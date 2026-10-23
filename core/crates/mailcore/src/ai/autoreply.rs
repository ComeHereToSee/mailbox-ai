//! Auto replies (kv space `ai_autoreply`): the first matching enabled rule (in creation
//! order) drafts (or sends) an AI reply to new inbox mail, or — vacation rules — sends a
//! fixed text while the vacation lasts, once per sender. The time of the last auto reply
//! to each sender, and the messages already answered, live in `ai_autoreply_sent`.

use super::model::AutoReplyRule;
use super::{local_day, prompts, text_html};
use crate::Engine;
use crate::error::{Error, Result};
use crate::events::Event;
use crate::mime::Headers;
use crate::model::{ComposeMode, EmailAddress, FolderRole, MessageId};
use crate::store::{new_id, now};

/// Ids that sort in creation order (kv lists are ordered by id).
fn sortable_id() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{nanos:020}{}", &new_id()[..4])
}

const SPACE: &str = "ai_autoreply";
const SENT: &str = "ai_autoreply_sent";

/// Why a message must not get an automatic reply (None: it may).
pub fn skip_reason(from_email: &str, own: &[String], h: &Headers) -> Option<&'static str> {
    let from = from_email.trim().to_lowercase();
    if from.is_empty() {
        return Some("没有发件人");
    }
    if own.iter().any(|o| *o == from) {
        return Some("自己发的邮件");
    }
    if ["noreply", "no-reply", "no_reply", "donotreply", "do-not-reply", "mailer-daemon"].iter().any(|p| from.contains(p)) {
        return Some("不接受回复的系统地址");
    }
    if h.unsubscribe.is_some() || h.header_names.iter().any(|n| n == "list-unsubscribe") {
        return Some("订阅邮件（List-Unsubscribe）");
    }
    if h.precedence.as_deref().is_some_and(|p| matches!(p, "bulk" | "list" | "junk")) {
        return Some("群发邮件（Precedence）");
    }
    if h.auto_submitted {
        return Some("自动发送的邮件（Auto-Submitted）");
    }
    None
}

/// Account / sender / subject conditions of a rule (the AI condition is checked separately).
pub fn rule_matches(r: &AutoReplyRule, account_id: &str, from: &EmailAddress, subject: &str) -> bool {
    if !r.enabled {
        return false;
    }
    if let Some(a) = r.account_id.as_deref().filter(|a| !a.is_empty()) {
        if a != account_id {
            return false;
        }
    }
    let fc = r.from_contains.trim().to_lowercase();
    if !fc.is_empty() {
        let hay = format!("{} {}", from.name.clone().unwrap_or_default(), from.email).to_lowercase();
        if !hay.contains(&fc) {
            return false;
        }
    }
    let sc = r.subject_contains.trim().to_lowercase();
    if !sc.is_empty() && !subject.to_lowercase().contains(&sc) {
        return false;
    }
    true
}

/// Whether a vacation rule is active at `now` (open ends when missing).
pub fn vacation_active(r: &AutoReplyRule, now: i64) -> bool {
    let started = match r.start {
        Some(s) => now >= s,
        None => true,
    };
    let ended = match r.end {
        Some(e) => now > e,
        None => false,
    };
    started && !ended
}

/// kv key remembering that a sender got the reply of this vacation (period).
pub fn vacation_key(r: &AutoReplyRule, sender: &str) -> String {
    format!("vacation:{}:{}:{}", r.id, r.start.unwrap_or(0), sender)
}

/// 「自动回复：原主题」
pub fn vacation_subject(original: &str) -> String {
    let o = original.trim();
    if o.is_empty() { "自动回复".to_string() } else { format!("自动回复：{o}") }
}

/// Runs the auto reply rules for a new inbox message.
pub(crate) async fn process(e: &Engine, id: MessageId) -> Result<()> {
    let rules: Vec<AutoReplyRule> = e.db.kv_list::<AutoReplyRule>(SPACE)?.into_iter().filter(|r| r.enabled).collect();
    if rules.is_empty() {
        return Ok(());
    }
    let m = e.db.message(id)?;
    if m.folder_role != FolderRole::Inbox {
        return Ok(());
    }
    let own = e.db.own_addresses()?;
    let raw = e.raw_message(id).await?;
    let h = crate::mime::parse_headers(&raw);
    if let Some(reason) = skip_reason(&m.summary.from.email, &own, &h) {
        tracing::debug!(message_id = id, reason, "auto reply skipped");
        return Ok(());
    }
    let today = local_day();
    let sender = m.summary.from.email.trim().to_lowercase();
    let sender_key = format!("sender:{sender}");
    let message_key = format!("message:{id}");
    if e.db.kv_get::<i64>(SENT, &message_key)?.is_some() {
        return Ok(());
    }
    for mut rule in rules {
        if !rule_matches(&rule, &m.summary.account_id, &m.summary.from, &m.summary.subject) {
            continue;
        }
        if rule.kind == "vacation" {
            if !vacation_active(&rule, now()) {
                continue;
            }
            // Once per sender for the whole vacation.
            let key = vacation_key(&rule, &sender);
            if e.db.kv_get::<i64>(SENT, &key)?.is_some() {
                continue;
            }
            e.ai_vacation_reply(rule, id).await?;
            let t = now();
            e.db.kv_put(SENT, &key, &t, 0)?;
            e.db.kv_put(SENT, &message_key, &t, 0)?;
            return Ok(());
        }
        if rule.day != today {
            rule.day = today.clone();
            rule.sent_today = 0;
        }
        if rule.max_per_day > 0 && rule.sent_today >= rule.max_per_day {
            continue;
        }
        if rule.once_per_sender_hours > 0 {
            if let Some(last) = e.db.kv_get::<i64>(SENT, &sender_key)? {
                if now() - last < rule.once_per_sender_hours as i64 * 3600 {
                    continue;
                }
            }
        }
        if !rule.ai_match.trim().is_empty() && !e.ai_autoreply_matches(&rule, id).await? {
            continue;
        }
        // The first matching rule wins.
        e.ai_autoreply_apply(rule, id).await?;
        let t = now();
        e.db.kv_put(SENT, &sender_key, &t, 0)?;
        e.db.kv_put(SENT, &message_key, &t, 0)?;
        return Ok(());
    }
    Ok(())
}

impl Engine {
    pub fn ai_autoreply_rules(&self) -> Vec<AutoReplyRule> {
        self.db.kv_list(SPACE).unwrap_or_default()
    }

    pub fn ai_save_autoreply(&self, mut r: AutoReplyRule) -> Result<AutoReplyRule> {
        r.instruction = r.instruction.trim().to_string();
        r.reply_text = r.reply_text.trim().to_string();
        if r.kind == "vacation" {
            if r.reply_text.is_empty() {
                return Err(Error::Invalid("请填写休假回复的内容".into()));
            }
            if let (Some(start), Some(end)) = (r.start, r.end) {
                if end <= start {
                    return Err(Error::Invalid("休假结束时间应晚于开始时间".into()));
                }
            }
            // Vacation replies always go out directly.
            r.mode = "send".into();
        } else {
            r.kind = "ai".into();
            if r.instruction.is_empty() {
                return Err(Error::Invalid("请填写怎样回复".into()));
            }
        }
        r.name = r.name.trim().to_string();
        if r.name.is_empty() {
            r.name = if r.kind == "vacation" { "休假回复".into() } else { "自动回复".into() };
        }
        if r.mode != "send" {
            r.mode = "draft".into();
        }
        if r.id.trim().is_empty() {
            r.id = sortable_id();
            r.sent_today = 0;
            r.day = String::new();
        } else if let Some(old) = self.db.kv_get::<AutoReplyRule>(SPACE, &r.id)? {
            // Internal counters are not editable.
            r.sent_today = old.sent_today;
            r.day = old.day;
        }
        self.db.kv_put(SPACE, &r.id, &r, 0)?;
        Ok(r)
    }

    pub fn ai_delete_autoreply(&self, id: &str) -> Result<()> {
        if !self.db.kv_delete(SPACE, id)? {
            return Err(Error::NotFound("规则不存在".into()));
        }
        Ok(())
    }

    async fn ai_autoreply_matches(&self, rule: &AutoReplyRule, id: MessageId) -> Result<bool> {
        let model = self.ai_model(true)?;
        let local = self.ai_model_is_local(&model);
        let mail = self.ai_mail_text(id, 4000, true, local).await?;
        let (system, user) = prompts::rule_match(rule.ai_match.trim(), &mail);
        let v = self.ai_json(&model, "autoreply", system, user).await?;
        Ok(match &v["match"] {
            serde_json::Value::Bool(b) => *b,
            serde_json::Value::String(s) => s.trim().eq_ignore_ascii_case("true"),
            _ => false,
        })
    }

    async fn ai_autoreply_apply(&self, mut rule: AutoReplyRule, id: MessageId) -> Result<()> {
        let model = self.ai_model(false)?;
        let text = self.ai_reply_text(id, &rule.instruction, &model, "autoreply", None).await?;
        let send = rule.mode == "send";
        let (draft_id, subject) = if send {
            let d = self.ai_reply_draft(id, &text, ComposeMode::Reply, true).await?;
            let subject = d.subject.clone();
            self.send_draft(d, None)?;
            (None, subject)
        } else {
            let d = self.ai_save_reply_draft(id, &text).await?;
            (Some(d.id.clone()), d.subject.clone())
        };
        rule.sent_today += 1;
        // Only the counters change; the user may have edited the rule meanwhile.
        if let Some(mut cur) = self.db.kv_get::<AutoReplyRule>(SPACE, &rule.id)? {
            cur.sent_today = rule.sent_today;
            cur.day = rule.day.clone();
            self.db.kv_put(SPACE, &cur.id, &cur, 0)?;
        }
        self.events.emit(Event::AiAutoreply { message_id: id, rule_name: rule.name.clone(), draft_id, sent: send, subject });
        Ok(())
    }

    /// Vacation rule: the fixed text, sent right away (through the outbox, with
    /// `Auto-Submitted: auto-replied`), subject 「自动回复：原主题」. No AI involved.
    async fn ai_vacation_reply(&self, rule: AutoReplyRule, id: MessageId) -> Result<()> {
        let original = self.db.message(id)?.summary.subject;
        let mut d = self.reply_draft(id, ComposeMode::Reply).await?;
        d.subject = vacation_subject(&original);
        d.html = text_html(&rule.reply_text);
        d.text = Some(rule.reply_text.clone());
        d.auto_submitted = true;
        let subject = d.subject.clone();
        self.send_draft(d, None)?;
        let today = local_day();
        if let Some(mut cur) = self.db.kv_get::<AutoReplyRule>(SPACE, &rule.id)? {
            if cur.day != today {
                cur.day = today;
                cur.sent_today = 0;
            }
            cur.sent_today += 1;
            self.db.kv_put(SPACE, &cur.id, &cur, 0)?;
        }
        self.events.emit(Event::AiAutoreply { message_id: id, rule_name: rule.name.clone(), draft_id: None, sent: true, subject });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Unsubscribe;

    #[test]
    fn skips_bulk_and_noreply() {
        let own = vec!["me@x.cn".to_string()];
        let plain = Headers::default();
        assert_eq!(skip_reason("friend@x.cn", &own, &plain), None);
        assert!(skip_reason("Me@X.cn", &own, &plain).is_some());
        assert!(skip_reason("noreply@github.com", &own, &plain).is_some());
        assert!(skip_reason("no-reply@shop.cn", &own, &plain).is_some());
        assert!(skip_reason("MAILER-DAEMON@x.cn", &own, &plain).is_some());
        let bulk = Headers { precedence: Some("bulk".into()), ..Default::default() };
        assert!(skip_reason("news@shop.cn", &own, &bulk).is_some());
        let list = Headers { unsubscribe: Some(Unsubscribe { mailto: Some("mailto:u@shop.cn".into()), ..Default::default() }), ..Default::default() };
        assert!(skip_reason("news@shop.cn", &own, &list).is_some());
        let auto = Headers { auto_submitted: true, ..Default::default() };
        assert!(skip_reason("bot@x.cn", &own, &auto).is_some());
    }

    #[test]
    fn rule_conditions() {
        let r = AutoReplyRule { from_contains: "张三".into(), subject_contains: "吃饭".into(), instruction: "x".into(), ..Default::default() };
        let from = EmailAddress::new(Some("张三"), "zs@x.cn");
        assert!(rule_matches(&r, "a", &from, "明天一起吃饭？"));
        assert!(!rule_matches(&r, "a", &from, "周报"));
        assert!(!rule_matches(&r, "a", &EmailAddress::new(None, "ls@x.cn"), "吃饭"));
        let other_account = AutoReplyRule { account_id: Some("b".into()), ..r.clone() };
        assert!(!rule_matches(&other_account, "a", &from, "吃饭"));
        let disabled = AutoReplyRule { enabled: false, ..r };
        assert!(!rule_matches(&disabled, "a", &from, "吃饭"));
    }

    #[test]
    fn vacation_window() {
        let r = AutoReplyRule { id: "v".into(), kind: "vacation".into(), start: Some(100), end: Some(200), ..Default::default() };
        assert!(!vacation_active(&r, 99));
        assert!(vacation_active(&r, 100) && vacation_active(&r, 200));
        assert!(!vacation_active(&r, 201));
        let open = AutoReplyRule { start: None, end: None, ..r.clone() };
        assert!(vacation_active(&open, 0) && vacation_active(&open, i64::MAX));
        assert_eq!(vacation_key(&r, "a@x.cn"), "vacation:v:100:a@x.cn");
        assert_eq!(vacation_subject(" 周报 "), "自动回复：周报");
        assert_eq!(vacation_subject(""), "自动回复");
    }
}

//! Smart reminders: analysis of new mail (kv space `ai_insight`, id = message id), due
//! reminders (`ai_reminder` events) and the "待处理" list.

use super::model::{AiSettings, Insight};
use super::{local_tz, prompts, truncate_plain};
use crate::Engine;
use crate::error::{Error, Result};
use crate::events::Event;
use crate::model::{Category, FolderRole, MessageId, MessageSummary};
use crate::store::now;
use chrono::TimeZone;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

pub(crate) const SPACE: &str = "ai_insight";

/// Reminder time: 9:00 on the day before the deadline; less than a day ahead: two hours
/// before. No deadline (or already past): none.
pub fn compute_remind_at<Z: TimeZone>(deadline: Option<i64>, now: i64, tz: &Z) -> Option<i64> {
    let d = deadline?;
    if d <= now {
        return None;
    }
    let two_hours_before = d - 2 * 3600;
    if d - now < 86_400 {
        return Some(two_hours_before.max(now));
    }
    let local = tz.timestamp_opt(d, 0).single()?;
    let day_before = local.date_naive().pred_opt()?;
    let at = tz.from_local_datetime(&day_before.and_hms_opt(9, 0, 0)?).earliest()?.timestamp();
    Some(if at > now { at } else { two_hours_before })
}

/// [`compute_remind_at`] in the system time zone.
pub fn compute_remind_at_local(deadline: Option<i64>, now: i64) -> Option<i64> {
    match local_tz() {
        Some(tz) => compute_remind_at(deadline, now, &tz),
        None => compute_remind_at(deadline, now, &chrono::Local),
    }
}

/// Parses the deadline written by the model ("YYYY-MM-DD HH:MM", a date, RFC 3339).
pub fn parse_deadline<Z: TimeZone>(s: &str, tz: &Z) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("null") {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    let norm = s.replace('T', " ").replace('/', "-");
    for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(&norm, f) {
            return tz.from_local_datetime(&n).earliest().map(|d| d.timestamp());
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(&norm, "%Y-%m-%d") {
        return tz.from_local_datetime(&d.and_hms_opt(18, 0, 0)?).earliest().map(|d| d.timestamp());
    }
    None
}

fn parse_deadline_local(s: &str) -> Option<i64> {
    match local_tz() {
        Some(tz) => parse_deadline(s, &tz),
        None => parse_deadline(s, &chrono::Local),
    }
}

pub(crate) fn bool_of(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => matches!(s.trim().to_lowercase().as_str(), "true" | "yes" | "是"),
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

pub(crate) fn text_of(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("null")).map(String::from)
}

/// Builds an insight from the model's JSON.
pub fn parse_insight(v: &Value, message_id: MessageId, now: i64) -> Insight {
    let priority = match v["priority"].as_str().unwrap_or("").trim().to_lowercase().as_str() {
        "high" | "高" | "urgent" => "high",
        "low" | "低" => "low",
        _ => "normal",
    }
    .to_string();
    let deadline = match &v["deadline"] {
        Value::Number(n) => n.as_i64().filter(|t| *t > 1_000_000_000),
        Value::String(s) => parse_deadline_local(s),
        _ => None,
    };
    let todos: Vec<String> = v["todos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(text_of)
        .take(10)
        .collect();
    let risk = parse_risk(&v["risk"]);
    let risk_reason = if risk.is_some() { truncate_plain(&text_of(&v["risk_reason"]).unwrap_or_default(), 120) } else { String::new() };
    Insight {
        message_id,
        needs_reply: bool_of(&v["needs_reply"]),
        priority,
        deadline,
        deadline_text: text_of(&v["deadline_text"]),
        todos,
        one_line: truncate_plain(&text_of(&v["one_line"]).unwrap_or_default(), 40),
        remind_at: compute_remind_at_local(deadline, now),
        done: false,
        created_at: now,
        reminded: false,
        events: super::calendar::parse_events_local(&v["events"]),
        risk,
        risk_reason,
    }
}

/// "phishing" | "scam" from the model's `risk` (anything else: no risk).
pub fn parse_risk(v: &Value) -> Option<String> {
    let s = v.as_str().unwrap_or("").trim().to_lowercase();
    if s.contains("phish") || s.contains("钓鱼") {
        Some("phishing".to_string())
    } else if s.contains("scam") || s.contains("fraud") || s.contains("诈骗") || s.contains("欺诈") {
        Some("scam".to_string())
    } else {
        None
    }
}

/// Whether a new message should be analysed (`smart_scope`).
pub(crate) fn in_scope(e: &Engine, s: &AiSettings, id: MessageId) -> bool {
    let Ok(m) = e.db.message(id) else { return false };
    if m.folder_role != FolderRole::Inbox {
        return false;
    }
    let own = e.db.own_addresses().unwrap_or_default();
    if own.contains(&m.summary.from.email.to_lowercase()) {
        return false;
    }
    let vip = m.summary.is_vip || e.db.is_vip(&m.summary.from.email).unwrap_or(false);
    match s.smart_scope.as_str() {
        "all" => true,
        "vip" => vip,
        _ => m.summary.category == Category::Primary || vip,
    }
}

#[derive(Serialize, Debug, Clone)]
pub struct ReminderItem {
    /// "insight" | "follow_up"
    pub kind: String,
    pub message: MessageSummary,
    pub account_label: String,
    pub insight: Insight,
}

impl Engine {
    pub fn ai_insight(&self, id: MessageId) -> Option<Insight> {
        self.db.kv_get::<Insight>(SPACE, &id.to_string()).ok().flatten()
    }

    pub(crate) fn ai_put_insight(&self, ins: &Insight) -> Result<()> {
        self.db.kv_put(SPACE, &ins.message_id.to_string(), ins, 0)
    }

    /// Analyses a message now (quick model, JSON) and stores the result; the calendar
    /// entries found go into the `ai_events` cache as well.
    pub async fn ai_analyze(&self, id: MessageId) -> Result<Insight> {
        let s = self.ai_settings();
        let model = self.ai_model(true)?;
        let local = self.ai_model_is_local(&model);
        let mail = self.ai_mail_text(id, s.max_body_chars, true, local).await?;
        let (system, user) = prompts::insight(&s, &mail);
        let v = self.ai_json(&model, "insight", system, user).await?;
        let mut ins = parse_insight(&v, id, now());
        if self.db.message(id)?.summary.flags.answered {
            ins.needs_reply = false;
        }
        if let Some(old) = self.ai_insight(id) {
            ins.done = old.done;
            ins.reminded = old.reminded && old.remind_at == ins.remind_at;
        }
        self.ai_put_insight(&ins)?;
        if let Err(err) = self.ai_store_events(id, &ins.events, &model.label()) {
            tracing::warn!(error = %err, "could not cache extracted events");
        }
        self.events.emit(Event::AiInsight { message_id: id });
        Ok(ins)
    }

    /// `GET /v1/ai/reminders`: open items that need a reply, have a deadline, todos or a
    /// phishing / scam risk, plus follow-ups of own sent mail (`kind: "follow_up"`).
    /// Deadlines first (soonest first), then the rest by message date, newest first.
    pub fn ai_reminders(&self) -> Result<Vec<ReminderItem>> {
        let labels: HashMap<String, String> = self.db.accounts()?.into_iter().map(|a| (a.id, a.label)).collect();
        let mut items = Vec::new();
        for mut ins in self.db.kv_list::<Insight>(SPACE)? {
            if ins.done {
                continue;
            }
            let Ok(m) = self.db.message(ins.message_id) else { continue };
            if matches!(m.folder_role, FolderRole::Trash | FolderRole::Junk) {
                continue;
            }
            if m.summary.flags.answered && ins.needs_reply {
                ins.needs_reply = false;
                let _ = self.ai_put_insight(&ins);
            }
            if !(ins.needs_reply || ins.deadline.is_some() || !ins.todos.is_empty() || ins.risk.is_some()) {
                continue;
            }
            let account_label = labels.get(&m.summary.account_id).cloned().unwrap_or_default();
            items.push(ReminderItem { kind: "insight".into(), message: m.summary, account_label, insight: ins });
        }
        items.extend(super::followup::reminder_items(self, &labels));
        items.sort_by(|a, b| match (a.insight.deadline, b.insight.deadline) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.message.date.cmp(&a.message.date),
        });
        Ok(items)
    }

    /// `POST /v1/ai/reminders/{message_id}/done` (smart reminders and follow-ups).
    pub fn ai_set_reminder_done(&self, id: MessageId, done: bool) -> Result<Insight> {
        let follow_up = super::followup::set_done(self, id, done);
        match self.ai_insight(id) {
            Some(mut ins) => {
                ins.done = done;
                self.ai_put_insight(&ins)?;
                self.events.emit(Event::AiInsight { message_id: id });
                Ok(ins)
            }
            None => match follow_up {
                Some(ins) => {
                    self.events.emit(Event::AiInsight { message_id: id });
                    Ok(ins)
                }
                None => Err(Error::NotFound("这封邮件还没有智能分析结果".into())),
            },
        }
    }
}

/// The user replied (the engine set the answered flag): nothing in the conversation waits
/// for a reply any more.
pub(crate) fn mark_answered(e: &Engine, id: MessageId) {
    let ids = e.ai_thread_ids(id).unwrap_or_else(|_| vec![id]);
    for mid in ids {
        if let Some(mut ins) = e.ai_insight(mid) {
            if ins.needs_reply {
                ins.needs_reply = false;
                if e.ai_put_insight(&ins).is_ok() {
                    e.events.emit(Event::AiInsight { message_id: mid });
                }
            }
        }
    }
}

/// Sends `ai_reminder` for due insights (once each).
pub(crate) fn check_reminders(e: &Engine, now: i64) {
    let list: Vec<Insight> = e.db.kv_list(SPACE).unwrap_or_default();
    for mut ins in list {
        if ins.done || ins.reminded {
            continue;
        }
        let Some(at) = ins.remind_at else { continue };
        if at > now {
            continue;
        }
        ins.reminded = true;
        if e.ai_put_insight(&ins).is_err() {
            continue;
        }
        let Ok(m) = e.db.message(ins.message_id) else { continue };
        let label = e.db.account(&m.summary.account_id).map(|a| a.label).unwrap_or_default();
        let title = format!("⏰ [{}] {}", label, m.summary.from.display());
        let subject = if m.summary.subject.trim().is_empty() { "（无主题）".to_string() } else { m.summary.subject.clone() };
        let body = match ins.deadline_text.as_deref().filter(|t| !t.is_empty()) {
            Some(t) => format!("{subject} · 截止 {t}"),
            None => subject,
        };
        e.events.emit(Event::AiReminder { message_id: ins.message_id, title, body });
    }
}

/// Daily: drops insights of deleted messages and old finished ones.
pub(crate) fn prune(e: &Engine, now: i64) {
    let list: Vec<Insight> = e.db.kv_list(SPACE).unwrap_or_default();
    for ins in list {
        let gone = matches!(e.db.message(ins.message_id), Err(Error::NotFound(_)));
        let stale = ins.done && ins.created_at < now - 60 * 86_400;
        if gone || stale {
            let _ = e.db.kv_delete(SPACE, &ins.message_id.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Asia::Shanghai;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Shanghai.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp()
    }

    #[test]
    fn remind_at_rules() {
        // Monday 2026-10-19 10:00 in Shanghai.
        let now = at(2026, 10, 19, 10, 0);
        // Friday 18:00 → Thursday 09:00.
        assert_eq!(compute_remind_at(Some(at(2026, 10, 23, 18, 0)), now, &Shanghai), Some(at(2026, 10, 22, 9, 0)));
        // Less than a day ahead → two hours before.
        assert_eq!(compute_remind_at(Some(at(2026, 10, 19, 20, 0)), now, &Shanghai), Some(at(2026, 10, 19, 18, 0)));
        // A day and a half ahead, but 9:00 of the day before already passed → two hours before.
        assert_eq!(compute_remind_at(Some(at(2026, 10, 20, 22, 0)), now, &Shanghai), Some(at(2026, 10, 20, 20, 0)));
        // Within two hours → now.
        assert_eq!(compute_remind_at(Some(now + 3600), now, &Shanghai), Some(now));
        assert_eq!(compute_remind_at(None, now, &Shanghai), None);
        assert_eq!(compute_remind_at(Some(now - 60), now, &Shanghai), None);
    }

    #[test]
    fn parses_model_deadlines() {
        assert_eq!(parse_deadline("2026-10-23 18:00", &Shanghai), Some(at(2026, 10, 23, 18, 0)));
        assert_eq!(parse_deadline("2026-10-23", &Shanghai), Some(at(2026, 10, 23, 18, 0)));
        assert_eq!(parse_deadline("2026-10-23T09:30:00", &Shanghai), Some(at(2026, 10, 23, 9, 30)));
        assert_eq!(parse_deadline("2026-10-23T18:00:00+08:00", &Shanghai), Some(at(2026, 10, 23, 18, 0)));
        assert_eq!(parse_deadline("null", &Shanghai), None);
        assert_eq!(parse_deadline("下周", &Shanghai), None);
    }

    #[test]
    fn insight_from_json() {
        let v: Value = serde_json::from_str(
            r#"{"needs_reply":"true","priority":"HIGH","deadline":null,"deadline_text":"null","todos":["确认分工",""," "],"one_line":"一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十一二三"}"#,
        )
        .unwrap();
        let ins = parse_insight(&v, 7, 1_000);
        assert!(ins.needs_reply);
        assert_eq!(ins.priority, "high");
        assert_eq!(ins.deadline, None);
        assert_eq!(ins.deadline_text, None);
        assert_eq!(ins.todos, vec!["确认分工"]);
        assert_eq!(ins.one_line.chars().count(), 41); // 40 + "…"
        assert_eq!(ins.remind_at, None);
        assert!(ins.events.is_empty() && ins.risk.is_none() && ins.risk_reason.is_empty());
    }

    #[test]
    fn insight_with_risk_and_events() {
        let v: Value = serde_json::from_str(
            r#"{"needs_reply":false,"priority":"low","risk":"phishing","risk_reason":"冒充 Apple 索要验证码",
                "events":[{"title":"评审会","start":"2099-01-05 15:00","end":"2099-01-05 16:00","all_day":false,"location":null,
                           "notes":"Q4 规划","source_text":"下周一下午 3 点"}]}"#,
        )
        .unwrap();
        let ins = parse_insight(&v, 9, 1_000);
        assert_eq!(ins.risk.as_deref(), Some("phishing"));
        assert_eq!(ins.risk_reason, "冒充 Apple 索要验证码");
        assert_eq!(ins.events.len(), 1);
        assert_eq!(ins.events[0].title, "评审会");
        assert_eq!(ins.events[0].end, Some(ins.events[0].start + 3600));
        assert_eq!(parse_risk(&serde_json::json!("SCAM")).as_deref(), Some("scam"));
        assert_eq!(parse_risk(&serde_json::json!("诈骗")).as_deref(), Some("scam"));
        assert_eq!(parse_risk(&serde_json::json!("none")), None);
        assert_eq!(parse_risk(&Value::Null), None);
        let v: Value = serde_json::from_str(r#"{"risk":null,"risk_reason":"看起来正常"}"#).unwrap();
        assert!(parse_insight(&v, 1, 0).risk_reason.is_empty());
    }
}

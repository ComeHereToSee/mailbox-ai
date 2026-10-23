//! Follow-up reminders (kv space `ai_followup`, id = message id): the user's own mail of
//! the last 14 days that got no answer for `followup_days` days and — as the quick model
//! judges — waits for one. Checked once an hour; a reply in the conversation removes it.

use super::insight::{ReminderItem, bool_of, text_of};
use super::model::Insight;
use super::{policy, prompts, truncate_plain};
use crate::Engine;
use crate::error::{Error, Result};
use crate::events::Event;
use crate::model::{FolderRole, Mailbox, MessageId, MessageSummary};
use crate::store::{ListQuery, now};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::Ordering;

pub(crate) const SPACE: &str = "ai_followup";
/// Seconds between two checks.
pub(crate) const INTERVAL: i64 = 3600;
/// Only mail sent within this many days is looked at.
const WINDOW_DAYS: i64 = 14;
/// Model calls per check at most (the rest waits for the next hour).
const MAX_CHECKS: usize = 20;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct FollowUp {
    pub message_id: MessageId,
    pub awaits_reply: bool,
    pub reason: String,
    pub checked_at: i64,
    pub done: bool,
    /// The `ai_reminder` event was sent.
    pub reminded: bool,
}

/// Someone else wrote in the conversation after the sent message.
pub fn has_reply(sent: &MessageSummary, thread: &[MessageSummary], own: &[String]) -> bool {
    thread.iter().any(|m| m.id != sent.id && m.date > sent.date && !own.contains(&m.from.email.to_lowercase()))
}

/// Sent at least `days` days ago, and within the last 14 days. `days == 0`: never.
pub fn in_window(sent_date: i64, now: i64, days: u32) -> bool {
    days > 0 && sent_date <= now - days as i64 * 86_400 && sent_date >= now - WINDOW_DAYS * 86_400
}

/// Whole days since the message was sent (at least 1).
pub fn days_waiting(sent_date: i64, now: i64) -> i64 {
    ((now - sent_date) / 86_400).max(1)
}

/// Who the reply is expected from: the first recipient.
pub fn recipient(m: &MessageSummary) -> String {
    m.to.first().map(|a| a.display()).filter(|s| !s.is_empty()).unwrap_or_else(|| "对方".to_string())
}

/// 「N 天没收到 收件人 的回复」
pub fn one_line(m: &MessageSummary, now: i64) -> String {
    format!("{} 天没收到 {} 的回复", days_waiting(m.date, now), recipient(m))
}

fn insight_of(f: &FollowUp, m: &MessageSummary, now: i64) -> Insight {
    Insight {
        message_id: f.message_id,
        priority: "normal".into(),
        one_line: one_line(m, now),
        done: f.done,
        created_at: f.checked_at,
        reminded: f.reminded,
        ..Default::default()
    }
}

/// Housekeeping (once a minute): starts the hourly check in the background.
pub(crate) fn schedule(e: &Engine, now: i64) {
    if now - e.ai.last_followup.load(Ordering::Relaxed) < INTERVAL {
        return;
    }
    if e.ai.followup_running.swap(true, Ordering::SeqCst) {
        return;
    }
    e.ai.last_followup.store(now, Ordering::Relaxed);
    let e = e.clone();
    tokio::spawn(async move {
        match e.ai_check_followups().await {
            Ok(n) if n > 0 => tracing::info!(count = n, "new follow-up reminders"),
            Ok(_) | Err(Error::AiNotConfigured(_)) => {}
            Err(err) => tracing::debug!(error = %err, "follow-up check failed"),
        }
        e.ai.followup_running.store(false, Ordering::SeqCst);
    });
}

/// A new message from someone else answers the follow-ups of its conversation.
pub(crate) fn on_new_mail(e: &Engine, id: MessageId) {
    let Ok(m) = e.db.message(id) else { return };
    let own = e.db.own_addresses().unwrap_or_default();
    if own.contains(&m.summary.from.email.to_lowercase()) {
        return;
    }
    for mid in e.db.thread_ids(&m.summary.thread_id).unwrap_or_default() {
        if mid != id {
            let _ = e.db.kv_delete(SPACE, &mid.to_string());
        }
    }
}

/// Open follow-ups for `GET /v1/ai/reminders` (answered ones are removed on the way).
pub(crate) fn reminder_items(e: &Engine, labels: &HashMap<String, String>) -> Vec<ReminderItem> {
    let own = e.db.own_addresses().unwrap_or_default();
    let t = now();
    let mut out = Vec::new();
    for f in e.db.kv_list::<FollowUp>(SPACE).unwrap_or_default() {
        if !f.awaits_reply || f.done {
            continue;
        }
        let Ok(m) = e.db.message(f.message_id) else { continue };
        if matches!(m.folder_role, FolderRole::Trash | FolderRole::Junk) {
            continue;
        }
        let thread = e.db.thread(&m.summary.thread_id, false).unwrap_or_default();
        if has_reply(&m.summary, &thread, &own) {
            let _ = e.db.kv_delete(SPACE, &f.message_id.to_string());
            continue;
        }
        let account_label = labels.get(&m.summary.account_id).cloned().unwrap_or_default();
        let insight = insight_of(&f, &m.summary, t);
        out.push(ReminderItem { kind: "follow_up".into(), message: m.summary, account_label, insight });
    }
    out
}

/// Marks a follow-up done (None when the message has none).
pub(crate) fn set_done(e: &Engine, id: MessageId, done: bool) -> Option<Insight> {
    let mut f = e.db.kv_get::<FollowUp>(SPACE, &id.to_string()).ok().flatten()?;
    f.done = done;
    e.db.kv_put(SPACE, &id.to_string(), &f, 0).ok()?;
    let m = e.db.message(id).ok()?;
    Some(insight_of(&f, &m.summary, now()))
}

/// Daily: drops follow-ups of deleted messages and old checks.
pub(crate) fn prune(e: &Engine, now: i64) {
    for f in e.db.kv_list::<FollowUp>(SPACE).unwrap_or_default() {
        let gone = matches!(e.db.message(f.message_id), Err(Error::NotFound(_)));
        if gone || f.checked_at < now - 30 * 86_400 {
            let _ = e.db.kv_delete(SPACE, &f.message_id.to_string());
        }
    }
}

impl Engine {
    /// The hourly check (also callable directly): own sent mail old enough and without an
    /// answer is judged by the quick model once; those waiting for a reply become
    /// reminders (`ai_reminder` the first time). Returns how many were added.
    pub async fn ai_check_followups(&self) -> Result<usize> {
        let s = self.ai_settings();
        if s.followup_days == 0 {
            return Ok(0);
        }
        let model = self.ai_model(true)?;
        let local = self.ai_model_is_local(&model);
        let own = self.db.own_addresses()?;
        let t = now();
        let sent = self.db.list_messages(&ListQuery {
            mailbox: Mailbox::Unified(FolderRole::Sent),
            search: Some(format!("newer_than:{WINDOW_DAYS}d")),
            limit: 500,
            ..Default::default()
        })?;
        let mut checks = 0usize;
        let mut added = 0usize;
        for m in sent {
            let thread = self.db.thread(&m.thread_id, false).unwrap_or_default();
            if has_reply(&m, &thread, &own) {
                let _ = self.db.kv_delete(SPACE, &m.id.to_string());
                continue;
            }
            if !in_window(m.date, t, s.followup_days) {
                continue;
            }
            if self.db.kv_get::<FollowUp>(SPACE, &m.id.to_string())?.is_some() {
                continue;
            }
            if !policy::allows(&s, &m.account_id, local) {
                continue;
            }
            if checks >= MAX_CHECKS {
                break;
            }
            checks += 1;
            let mail = match self.ai_mail_text(m.id, 4000, true, local).await {
                Ok(mail) => mail,
                Err(err) => {
                    tracing::debug!(message_id = m.id, error = %err, "follow-up: message not readable");
                    continue;
                }
            };
            let (system, user) = prompts::followup(&s, &mail, days_waiting(m.date, t));
            let v = match self.ai_json(&model, "insight", system, user).await {
                Ok(v) => v,
                Err(err) => {
                    tracing::debug!(message_id = m.id, error = %err, "follow-up: model failed");
                    continue;
                }
            };
            let awaits = bool_of(&v["awaits_reply"]);
            let f = FollowUp {
                message_id: m.id,
                awaits_reply: awaits,
                reason: truncate_plain(&text_of(&v["reason"]).unwrap_or_default(), 100),
                checked_at: t,
                done: false,
                reminded: awaits,
            };
            self.db.kv_put(SPACE, &m.id.to_string(), &f, 0)?;
            if awaits {
                added += 1;
                let label = self.db.account(&m.account_id).map(|a| a.label).unwrap_or_default();
                let subject = if m.subject.trim().is_empty() { "（无主题）".to_string() } else { m.subject.clone() };
                self.events.emit(Event::AiReminder {
                    message_id: m.id,
                    title: format!("⏰ [{label}] 等待 {} 回复", recipient(&m)),
                    body: format!("{subject} · {}", one_line(&m, t)),
                });
            }
        }
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EmailAddress;

    fn msg(id: i64, date: i64, from: &str) -> MessageSummary {
        MessageSummary { id, date, from: EmailAddress::new(None, from), to: vec![EmailAddress::new(Some("王五"), "w@x.cn")], ..Default::default() }
    }

    #[test]
    fn follow_up_rules() {
        let day = 86_400;
        let now = 100 * day;
        let own = vec!["me@x.cn".to_string()];
        let sent = msg(1, now - 5 * day, "me@x.cn");
        // Only my own later messages, or older ones from others: still waiting.
        let thread = vec![msg(0, now - 6 * day, "w@x.cn"), sent.clone(), msg(2, now - 4 * day, "Me@X.cn")];
        assert!(!has_reply(&sent, &thread, &own));
        let answered = vec![sent.clone(), msg(3, now - day, "w@x.cn")];
        assert!(has_reply(&sent, &answered, &own));
        assert!(in_window(sent.date, now, 3));
        assert!(!in_window(sent.date, now, 6), "not old enough yet");
        assert!(!in_window(now - 15 * day, now, 3), "older than two weeks");
        assert!(!in_window(sent.date, now, 0), "0 turns follow-ups off");
        assert_eq!(one_line(&sent, now), "5 天没收到 王五 的回复");
        assert_eq!(days_waiting(now - 10, now), 1);
        assert_eq!(recipient(&MessageSummary::default()), "对方");
    }
}

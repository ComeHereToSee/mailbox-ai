//! Notification modes (`AiSettings.notify_mode`). "annotate" needs nothing here (the helper
//! reacts to `ai_insight`). In "important" mode a new message inside the analysis scope is
//! announced with `notify: false` first; once the smart analysis is done an `ai_notify`
//! event pops it up only when it matters (high priority, needs a reply, VIP sender, risk).
//! A failed or missing analysis, or one slower than 90 seconds, falls back to a normal
//! notification (also as `ai_notify`).

use super::insight;
use super::model::Insight;
use crate::Engine;
use crate::events::Event;
use crate::model::MessageId;
use crate::store::now;
use std::time::Duration;

/// How long a held notification waits for the analysis.
const WAIT: Duration = Duration::from_secs(90);

/// Whether an analysed message deserves a notification in "important" mode.
pub fn is_important(ins: &Insight, vip: bool) -> bool {
    ins.priority == "high" || ins.needs_reply || vip || ins.risk.is_some()
}

impl Engine {
    /// Called when a new inbox message would be notified: holds the notification when the
    /// "important" mode applies to it. Returns true when held.
    pub(crate) fn ai_hold_notification(&self, id: MessageId) -> bool {
        let s = self.ai_settings();
        if !(s.smart_reminders && s.notify_mode == "important") {
            return false;
        }
        if !insight::in_scope(self, &s, id) {
            return false;
        }
        self.ai.held.lock().insert(id, now());
        let e = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(WAIT).await;
            e.ai_release_notification(id, None);
        });
        true
    }

    /// Ends a held notification: with the analysis (`Some`), or the plain fallback (`None`).
    /// Nothing happens when the message was not held (or was released already).
    pub(crate) fn ai_release_notification(&self, id: MessageId, ins: Option<&Insight>) {
        if self.ai.held.lock().remove(&id).is_none() {
            return;
        }
        let Ok(m) = self.db.message(id) else { return };
        let Ok(acc) = self.db.account(&m.summary.account_id) else { return };
        let vip = m.summary.is_vip || self.db.is_vip(&m.summary.from.email).unwrap_or(false);
        let body = match ins {
            Some(ins) => {
                if !is_important(ins, vip) {
                    return;
                }
                let line = if ins.one_line.trim().is_empty() { m.summary.snippet.clone() } else { ins.one_line.clone() };
                format!("✨ {line}")
            }
            None => m.summary.snippet.clone(),
        };
        let subtitle = if m.summary.subject.trim().is_empty() { "（无主题）".to_string() } else { m.summary.subject.clone() };
        self.events.emit(Event::AiNotify {
            message_id: id,
            title: format!("[{}] {}", acc.label, m.summary.from.display()),
            subtitle,
            body,
        });
        self.forward_notification(&acc, &m.summary, vip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_important() {
        let plain = Insight { priority: "normal".into(), ..Default::default() };
        assert!(!is_important(&plain, false));
        assert!(is_important(&plain, true), "VIP sender");
        assert!(is_important(&Insight { priority: "high".into(), ..Default::default() }, false));
        assert!(is_important(&Insight { needs_reply: true, ..Default::default() }, false));
        assert!(is_important(&Insight { risk: Some("phishing".into()), ..Default::default() }, false));
    }
}

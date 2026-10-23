//! New inbox mail: blocked senders, rules, notification decision and forwarders.

use super::{Action, Engine, detail, in_quiet_hours};
use crate::events::Event;
use crate::forward::{self, ForwardPayload};
use crate::imap::ImapConn;
use crate::model::*;
use crate::rules::{self, RuleInput};
use crate::store::now;

/// Handles messages that just arrived in an inbox (not during the first sync).
pub(super) async fn process(e: &Engine, acc: &Account, mut conn: Option<&mut ImapConn>, folder: &Folder, ids: &[MessageId]) {
    let rule_list: Vec<Rule> = e.db.kv_list("rules").unwrap_or_default();
    let needs_body = rule_list.iter().any(|r| r.enabled && r.conditions.iter().any(|c| c.field == "body"));
    for id in ids {
        let Ok(m) = e.db.message(*id) else { continue };
        if m.summary.flags.seen {
            continue;
        }
        // Small new messages are downloaded right away: snippet for the notification, body rules.
        let mut body_text = None;
        if m.blob.is_none() && m.size <= 1024 * 1024 {
            if let (Some(c), Some(uid)) = (conn.as_deref_mut(), m.uid) {
                if let Ok(f) = c.fetch_bodies(&folder.path, &[uid]).await {
                    if let Some(raw) = f.into_iter().find(|x| x.uid == uid).and_then(|x| x.body) {
                        if let Ok((_, b)) = detail::store_body(e, *id, &raw) {
                            body_text = Some(b.search_text);
                        }
                    }
                }
            }
        } else if needs_body {
            if let Some(raw) = m.blob.as_deref().and_then(|h| detail::read_blob(e, h)) {
                body_text = Some(crate::mime::parse_full(&raw).1.search_text);
            }
        }
        let m = e.db.message(*id).unwrap_or(m);

        if e.db.is_blocked(&m.summary.from.email).unwrap_or(false) {
            let _ = e.act(&[*id], Action::Trash).await;
            continue;
        }

        let to: Vec<String> = m.summary.to.iter().map(|a| a.email.clone()).collect();
        let cc: Vec<String> = m.cc.iter().map(|a| a.email.clone()).collect();
        let raw_headers = m.blob.as_deref().and_then(|h| detail::read_blob(e, h));
        let input = RuleInput {
            account_id: &acc.id,
            from: &m.summary.from.email,
            from_name: m.summary.from.name.as_deref().unwrap_or(""),
            to: &to,
            cc: &cc,
            subject: &m.summary.subject,
            body: body_text.as_deref(),
            has_attachment: m.summary.has_attachments,
            list_id: m.list_id.as_deref(),
            raw_headers: raw_headers.as_deref(),
        };
        let out = rules::evaluate(&rule_list, &input);
        let moved = apply_outcome(e, &acc.id, *id, &m.summary.from.email, &out).await;
        if moved || out.mark_read {
            continue;
        }
        let summary = e.db.message(*id).map(|m| m.summary).unwrap_or(m.summary);
        e.notify_new_message(&summary, out.notify).await;
        // Smart reminders / auto replies run in a background queue (never blocks sync).
        e.ai_enqueue(*id);
    }
    e.changed(Some(&acc.id));
}

/// Applies the actions of matched rules. Returns true when the message left the inbox.
pub(super) async fn apply_outcome(e: &Engine, account_id: &str, id: MessageId, from: &str, out: &rules::RuleOutcome) -> bool {
    if out.vip {
        let _ = e.db.set_vip(from, true);
    }
    if let Some(c) = &out.category {
        let _ = e.db.set_category(id, Category::parse(c));
    }
    if out.mark_read {
        let _ = e.act(&[id], Action::Read).await;
    }
    if out.flag {
        let _ = e.act(&[id], Action::Flag).await;
    }
    if !out.labels.is_empty() {
        let _ = e.act(&[id], Action::Label { add: out.labels.clone(), remove: vec![] }).await;
    }
    if !out.forward_to.is_empty() {
        let _ = e.forward_automatically(id, &out.forward_to).await;
    }
    if out.trash {
        e.act(&[id], Action::Trash).await.is_ok()
    } else if out.archive {
        e.act(&[id], Action::Archive).await.is_ok()
    } else if let Some(target) = &out.move_to {
        match e.find_folder(account_id, target) {
            Some(fid) => e.act(&[id], Action::Move { folder_id: fid }).await.is_ok(),
            None => false,
        }
    } else {
        false
    }
}

impl Engine {
    /// Finds a folder of an account by role name, path or display name.
    pub(crate) fn find_folder(&self, account_id: &str, target: &str) -> Option<FolderId> {
        let role = FolderRole::parse(target);
        let folders = self.db.folders(Some(account_id)).ok()?;
        if role != FolderRole::Custom {
            if let Some(f) = folders.iter().find(|f| f.role == role) {
                return Some(f.id);
            }
        }
        folders
            .iter()
            .find(|f| f.path == target || f.name == target || crate::imap::utf7::decode(&f.path) == target)
            .map(|f| f.id)
    }

    /// Decides whether a message deserves a notification and announces it.
    pub(crate) async fn notify_message(&self, summary: &MessageSummary, rule_override: Option<bool>) {
        self.announce_message(summary, rule_override, false);
    }

    /// New inbox mail: like [`Engine::notify_message`], but in the AI "important" mode the
    /// notification waits for the smart analysis (`ai_notify` event later).
    pub(crate) async fn notify_new_message(&self, summary: &MessageSummary, rule_override: Option<bool>) {
        self.announce_message(summary, rule_override, true);
    }

    fn announce_message(&self, summary: &MessageSummary, rule_override: Option<bool>, may_hold: bool) {
        let Ok(acc) = self.db.account(&summary.account_id) else { return };
        let settings = self.settings();
        let prefs = &acc.config.notify;
        let vip = summary.is_vip || self.db.is_vip(&summary.from.email).unwrap_or(false);
        let mut notify = prefs.enabled && !summary.flags.seen;
        if prefs.vip_only && !vip {
            notify = false;
        }
        if settings.categories_enabled && !prefs.categories.is_empty() && !vip && !prefs.categories.contains(&summary.category) {
            notify = false;
        }
        if let Some(o) = rule_override {
            notify = o;
        }
        let paused = settings.notifications_paused_until.is_some_and(|t| t > now());
        if (paused || in_quiet_hours(&settings.quiet_hours, chrono::Local::now())) && !vip {
            notify = false;
        }
        let held = notify && may_hold && self.ai_hold_notification(summary.id);
        self.events.emit(Event::NewMail {
            account_id: acc.id.clone(),
            account_label: acc.label.clone(),
            account_color: acc.color.clone(),
            message: summary.clone(),
            notify: notify && !held,
            sound: prefs.sound,
        });
        if !notify || held {
            return;
        }
        self.forward_notification(&acc, summary, vip);
    }

    /// Sends a notified message to the configured forwarders (Bark, webhooks …).
    pub(crate) fn forward_notification(&self, acc: &Account, summary: &MessageSummary, vip: bool) {
        let forwarders: Vec<Forwarder> = self.db.kv_list("forwarders").unwrap_or_default();
        if forwarders.is_empty() {
            return;
        }
        let payload = ForwardPayload {
            account_label: acc.label.clone(),
            from: summary.from.display(),
            subject: summary.subject.clone(),
            snippet: summary.snippet.clone(),
            date: summary.date,
            message_url: Some(format!("mailboxai://message/{}", summary.id)),
        };
        for f in forwarders {
            if !f.enabled || (f.vip_only && !vip) || (!f.accounts.is_empty() && !f.accounts.contains(&acc.id)) {
                continue;
            }
            let client = self.http.clone();
            let payload = payload.clone();
            tokio::spawn(async move {
                if let Err(err) = forward::send(&client, &f, &payload).await {
                    tracing::warn!(forwarder = %f.name, error = %err, "forward failed");
                }
            });
        }
    }
}

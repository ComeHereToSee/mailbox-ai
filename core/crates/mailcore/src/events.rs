use crate::model::{AccountStatus, MessageSummary, OutboxStatus};
use serde::Serialize;
use tokio::sync::broadcast;

/// Pushed to the app over the WebSocket and to the helper through the FFI callback.
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A new message arrived. `notify` tells whether a system notification should be shown.
    NewMail {
        account_id: String,
        account_label: String,
        account_color: String,
        message: MessageSummary,
        notify: bool,
        sound: bool,
    },
    /// Message list contents changed (flags, new mail, moves).
    MessagesChanged { account_id: Option<String> },
    FoldersChanged { account_id: String },
    AccountStatus { status: AccountStatus },
    AccountsChanged,
    Outbox { id: String, status: OutboxStatus, error: Option<String> },
    OAuth { state: String, ok: bool, email: Option<String>, error: Option<String> },
    UnreadCounts { total: i64, accounts: Vec<(String, i64)> },
    SettingsChanged,
    /// A snoozed message came back to the inbox.
    Snoozed { message_id: i64 },
    /// Smart analysis (insight) of a message was stored or changed.
    AiInsight { message_id: i64 },
    /// A smart reminder is due: the helper shows a notification opening the message.
    AiReminder { message_id: i64, title: String, body: String },
    /// Progress of an AI batch job.
    AiJob { job_id: String, done: usize, total: usize, status: String },
    /// An AI loop run finished. `preview` is the first 120 characters of the output as plain text.
    AiLoop { loop_id: String, run_id: String, name: String, status: String, preview: String },
    /// An auto reply was drafted (`draft_id`) or queued for sending (`sent`).
    AiAutoreply { message_id: i64, rule_name: String, draft_id: Option<String>, sent: bool, subject: String },
    /// An MCP client wants to send or delete mail: the app and the helper ask the user, who
    /// answers with `POST /v1/ai/confirm/{id}`.
    AiConfirm { id: String, kind: String, title: String, detail: String },
    /// Notification mode "important": a new message worth a notification after the smart
    /// analysis (or the plain fallback when the analysis failed or took too long).
    AiNotify { message_id: i64, title: String, subtitle: String, body: String },
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl Default for EventBus {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(512);
        EventBus { tx }
    }
}

impl EventBus {
    pub fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

//! Data types shared by the store, the engine and the HTTP API.
//!
//! Everything here is serialized as JSON with `snake_case` field names; the Swift
//! client mirrors these definitions in `macos/Shared/Models.swift`.

use serde::{Deserialize, Serialize};

pub type MessageId = i64;
pub type FolderId = i64;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    #[default]
    Imap,
    Pop3,
    Graph,
    Jmap,
    Web,
}

impl AccountKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccountKind::Imap => "imap",
            AccountKind::Pop3 => "pop3",
            AccountKind::Graph => "graph",
            AccountKind::Jmap => "jmap",
            AccountKind::Web => "web",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Security {
    #[default]
    Tls,
    StartTls,
    None,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// Login name; defaults to the account e-mail address when empty.
    pub username: Option<String>,
    pub accept_invalid_certs: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    #[default]
    Password,
    Oauth2,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct NotifyPrefs {
    pub enabled: bool,
    pub vip_only: bool,
    /// Categories that trigger a notification. Empty means all.
    pub categories: Vec<Category>,
    pub sound: bool,
}

impl Default for NotifyPrefs {
    fn default() -> Self {
        NotifyPrefs {
            enabled: true,
            vip_only: false,
            categories: vec![Category::Primary, Category::Updates],
            sound: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct AccountConfig {
    pub imap: Option<ServerConfig>,
    pub pop3: Option<ServerConfig>,
    pub smtp: Option<ServerConfig>,
    pub auth: AuthKind,
    /// `google` or `microsoft` when `auth == oauth2`.
    pub oauth_provider: Option<String>,
    /// Webmail URL for `web` accounts.
    pub web_url: Option<String>,
    /// JMAP session URL for `jmap` accounts.
    pub jmap_url: Option<String>,
    /// Send the IMAP `ID` command after login (required by NetEase).
    pub send_id: bool,
    /// Append a copy of sent mail to the Sent folder (off for servers that do it themselves).
    pub save_sent_copy: bool,
    /// Only keep messages newer than this many days locally (0 = everything).
    pub sync_days: u32,
    pub poll_interval_secs: u32,
    pub use_idle: bool,
    pub pop_leave_on_server: bool,
    pub pop_delete_after_days: Option<u32>,
    pub pop_delete_on_local_delete: bool,
    pub notify: NotifyPrefs,
}

impl Default for AccountConfig {
    fn default() -> Self {
        AccountConfig {
            imap: None,
            pop3: None,
            smtp: None,
            auth: AuthKind::Password,
            oauth_provider: None,
            web_url: None,
            jmap_url: None,
            send_id: true,
            save_sent_copy: true,
            sync_days: 90,
            poll_interval_secs: 300,
            use_idle: true,
            pop_leave_on_server: true,
            pop_delete_after_days: None,
            pop_delete_on_local_delete: false,
            notify: NotifyPrefs::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Account {
    pub id: String,
    pub kind: AccountKind,
    pub email: String,
    /// Display name used in the From header.
    pub name: String,
    /// Short label shown in notifications and the sidebar, e.g. `QQ·工作`.
    pub label: String,
    pub color: String,
    pub provider: Option<String>,
    pub config: AccountConfig,
    pub enabled: bool,
    pub sort_order: i64,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    #[default]
    Idle,
    Connecting,
    Syncing,
    Error,
    Offline,
    Disabled,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct AccountStatus {
    pub account_id: String,
    pub state: SyncState,
    pub error: Option<String>,
    pub last_sync: Option<i64>,
    pub unread: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    All,
    Flagged,
    Important,
    Outbox,
    #[default]
    Custom,
}

impl FolderRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            FolderRole::Inbox => "inbox",
            FolderRole::Sent => "sent",
            FolderRole::Drafts => "drafts",
            FolderRole::Trash => "trash",
            FolderRole::Junk => "junk",
            FolderRole::Archive => "archive",
            FolderRole::All => "all",
            FolderRole::Flagged => "flagged",
            FolderRole::Important => "important",
            FolderRole::Outbox => "outbox",
            FolderRole::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> FolderRole {
        match s {
            "inbox" => FolderRole::Inbox,
            "sent" => FolderRole::Sent,
            "drafts" => FolderRole::Drafts,
            "trash" => FolderRole::Trash,
            "junk" => FolderRole::Junk,
            "archive" => FolderRole::Archive,
            "all" => FolderRole::All,
            "flagged" => FolderRole::Flagged,
            "important" => FolderRole::Important,
            "outbox" => FolderRole::Outbox,
            _ => FolderRole::Custom,
        }
    }

    /// Order used for the sidebar.
    pub fn rank(&self) -> i32 {
        match self {
            FolderRole::Inbox => 0,
            FolderRole::Flagged => 1,
            FolderRole::Important => 2,
            FolderRole::Drafts => 3,
            FolderRole::Sent => 4,
            FolderRole::Outbox => 5,
            FolderRole::Archive => 6,
            FolderRole::All => 7,
            FolderRole::Junk => 8,
            FolderRole::Trash => 9,
            FolderRole::Custom => 10,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Folder {
    pub id: FolderId,
    pub account_id: String,
    /// Raw server name (modified UTF-7 for IMAP).
    pub path: String,
    /// Human readable, decoded name (last path component).
    pub name: String,
    pub delimiter: Option<String>,
    pub role: FolderRole,
    pub unread: i64,
    pub total: i64,
    pub selectable: bool,
    /// Folder only exists locally (POP3 accounts, local outbox).
    pub local: bool,
    #[serde(skip)]
    pub uidvalidity: Option<u32>,
    #[serde(skip)]
    pub uidnext: Option<u32>,
    #[serde(skip)]
    pub highest_modseq: Option<u64>,
    pub last_sync: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default, Hash)]
pub struct EmailAddress {
    #[serde(default)]
    pub name: Option<String>,
    pub email: String,
}

impl EmailAddress {
    pub fn new(name: Option<&str>, email: &str) -> Self {
        EmailAddress {
            name: name.filter(|n| !n.trim().is_empty()).map(|n| n.trim().to_string()),
            email: email.trim().to_string(),
        }
    }

    pub fn display(&self) -> String {
        match &self.name {
            Some(n) if !n.is_empty() => n.clone(),
            _ => self.email.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    #[default]
    Primary,
    Social,
    Promotions,
    Updates,
    Forums,
}

impl Category {
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Primary => "primary",
            Category::Social => "social",
            Category::Promotions => "promotions",
            Category::Updates => "updates",
            Category::Forums => "forums",
        }
    }

    pub fn parse(s: &str) -> Category {
        match s {
            "social" => Category::Social,
            "promotions" => Category::Promotions,
            "updates" => Category::Updates,
            "forums" => Category::Forums,
            _ => Category::Primary,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Flags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub forwarded: bool,
    pub draft: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct MessageSummary {
    pub id: MessageId,
    pub account_id: String,
    pub folder_id: FolderId,
    pub thread_id: String,
    pub subject: String,
    pub from: EmailAddress,
    pub to: Vec<EmailAddress>,
    pub date: i64,
    pub snippet: String,
    /// Position in lists (the date, or when a snoozed message came back).
    pub sort_date: i64,
    pub flags: Flags,
    pub has_attachments: bool,
    pub labels: Vec<String>,
    pub category: Category,
    pub pinned: bool,
    pub snoozed_until: Option<i64>,
    pub is_vip: bool,
    /// Number of messages in the thread when listing conversations.
    pub thread_count: i64,
    pub thread_unread: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Attachment {
    pub id: i64,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
    pub content_id: Option<String>,
    pub inline: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Unsubscribe {
    pub http: Option<String>,
    pub mailto: Option<String>,
    /// RFC 8058 one-click (POST) supported.
    pub one_click: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct CalendarInvite {
    pub method: String,
    pub uid: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub all_day: bool,
    pub organizer: Option<EmailAddress>,
    pub attendees: Vec<EmailAddress>,
    /// Our own participation status if we are an attendee.
    pub my_status: Option<String>,
    pub raw: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct MessageDetail {
    #[serde(flatten)]
    pub summary: MessageSummary,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    pub reply_to: Vec<EmailAddress>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// Sanitized HTML (cid: links rewritten to `mailbox-cid://<id>/<cid>`).
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub has_remote_content: bool,
    /// Remote images may be loaded right away (settings / trusted sender).
    pub allow_remote: bool,
    pub trackers: i64,
    pub unsubscribe: Option<Unsubscribe>,
    pub invite: Option<CalendarInvite>,
    pub read_receipt_to: Option<String>,
    /// Phishing / spoofing hints shown as a banner.
    pub warnings: Vec<String>,
    pub body_loaded: bool,
    pub folder_role: FolderRole,
    pub encrypted: bool,
    pub signed: bool,
    /// The message was encrypted and has been decrypted for display.
    pub decrypted: bool,
    pub decrypt_error: Option<String>,
    pub signature: Option<SignatureInfo>,
}

/// Result of checking a PGP or S/MIME signature.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct SignatureInfo {
    /// `"pgp"` or `"smime"`.
    pub kind: String,
    pub valid: bool,
    pub signer: Option<String>,
    pub key_id: Option<String>,
    pub message: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Identity {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub email: String,
    pub reply_to: Option<String>,
    pub signature_html: String,
    pub is_default: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct DraftAttachment {
    /// Local file to attach.
    pub path: Option<String>,
    /// Or an attachment of an existing message (forwarding).
    pub message_id: Option<MessageId>,
    pub attachment_id: Option<i64>,
    pub filename: String,
    pub mime_type: Option<String>,
    pub size: Option<i64>,
    /// Inline image referenced from the HTML via `cid:`.
    pub content_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ComposeMode {
    #[default]
    New,
    Reply,
    ReplyAll,
    Forward,
    ForwardAsAttachment,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Draft {
    pub id: String,
    pub account_id: String,
    pub identity_id: Option<String>,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    pub subject: String,
    pub html: String,
    /// Plain-text alternative. Generated from the HTML when missing.
    pub text: Option<String>,
    /// When set the body is Markdown source and `html` is ignored.
    pub markdown: Option<String>,
    pub plain_text_only: bool,
    pub attachments: Vec<DraftAttachment>,
    pub mode: ComposeMode,
    /// The message being replied to / forwarded.
    pub source_message_id: Option<MessageId>,
    /// When editing a draft that exists on the server (Drafts folder), that message.
    pub draft_message_id: Option<MessageId>,
    pub request_read_receipt: bool,
    pub priority_high: bool,
    /// iMIP calendar part (method, ics) for invitation replies.
    pub calendar: Option<(String, String)>,
    /// Sign / encrypt with PGP or S/MIME when sending.
    pub sign: bool,
    pub encrypt: bool,
    pub updated_at: i64,
    /// Written by an AI auto reply rule: adds `Auto-Submitted: auto-replied` (RFC 3834).
    pub auto_submitted: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutboxStatus {
    #[default]
    Queued,
    Sending,
    Sent,
    Failed,
    Cancelled,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct OutboxItem {
    pub id: String,
    pub account_id: String,
    pub subject: String,
    pub to: Vec<EmailAddress>,
    pub send_at: i64,
    pub status: OutboxStatus,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Contact {
    pub email: String,
    pub name: Option<String>,
    pub use_count: i64,
    pub last_used: i64,
    pub vip: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct RuleCondition {
    /// from | to | cc | recipients | subject | body | header:<Name> | has_attachment | account | list_id
    pub field: String,
    /// contains | not_contains | equals | starts_with | ends_with | regex | exists
    pub op: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct RuleAction {
    /// move | archive | trash | mark_read | flag | label | category | forward | vip | no_notify | notify | stop
    pub kind: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub account_id: Option<String>,
    pub match_all: bool,
    pub conditions: Vec<RuleCondition>,
    pub actions: Vec<RuleAction>,
    pub sort_order: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub subject: String,
    pub html: String,
    pub shortcut: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct SmartFolder {
    pub id: String,
    pub name: String,
    pub query: String,
    pub icon: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Forwarder {
    pub id: String,
    /// bark | serverchan | pushplus | wecom | feishu | dingtalk | telegram | webhook
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    /// Kind specific settings (url, key, chat_id, secret ...).
    pub config: serde_json::Map<String, serde_json::Value>,
    /// Only forward mail of these accounts (empty = all).
    pub accounts: Vec<String>,
    pub vip_only: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct QuietHours {
    pub enabled: bool,
    /// "22:00"
    pub start: String,
    pub end: String,
}

impl Default for QuietHours {
    fn default() -> Self {
        QuietHours { enabled: false, start: "22:00".into(), end: "08:00".into() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct OAuthClients {
    pub google_client_id: String,
    pub google_client_secret: String,
    pub microsoft_client_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub undo_send_secs: u32,
    /// never | always | contacts
    pub load_remote_images: String,
    pub conversation_view: bool,
    pub quiet_hours: QuietHours,
    /// Pause notifications until this unix time.
    pub notifications_paused_until: Option<i64>,
    pub mark_read_on_open: bool,
    pub mark_read_delay_secs: u32,
    /// Ask before deleting.
    pub confirm_delete: bool,
    /// Split the unified inbox into Primary / Social / Promotions / Updates / Forums.
    pub categories_enabled: bool,
    pub oauth: OAuthClients,
    pub language: String,
    pub list_density: String,
    pub show_snippets: bool,
    pub auto_bcc: String,
    pub reply_position: String,
    pub default_font: String,
    pub default_font_size: u32,
    pub block_trackers: bool,
    pub spotlight_index: bool,
    pub app_lock: bool,
    /// `"pgp"` or `"smime"`: which one to use when both could protect a message.
    pub crypto_prefer: String,
    /// New messages start with signing on (when the sender has a key).
    pub crypto_sign_default: bool,
    /// Encrypt automatically when every recipient has a key.
    pub crypto_auto_encrypt: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            undo_send_secs: 5,
            load_remote_images: "never".into(),
            conversation_view: true,
            quiet_hours: QuietHours::default(),
            notifications_paused_until: None,
            mark_read_on_open: true,
            mark_read_delay_secs: 0,
            confirm_delete: false,
            categories_enabled: true,
            oauth: OAuthClients::default(),
            language: "zh-Hans".into(),
            list_density: "comfortable".into(),
            show_snippets: true,
            auto_bcc: String::new(),
            reply_position: "top".into(),
            default_font: String::new(),
            default_font_size: 14,
            block_trackers: true,
            spotlight_index: true,
            app_lock: false,
            crypto_prefer: "pgp".into(),
            crypto_sign_default: false,
            crypto_auto_encrypt: true,
        }
    }
}

/// A location in the sidebar the message list can be opened on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mailbox {
    /// Unified view of a role across all accounts (`unified:inbox`).
    Unified(FolderRole),
    /// A role of one account (`account:<id>:inbox`).
    AccountRole(String, FolderRole),
    Folder(FolderId),
    Starred,
    Unread,
    Snoozed,
    Pinned,
    Attachments,
    Vip,
    Category(Category),
    Label(String, String),
    Smart(String),
    All,
}

impl Mailbox {
    pub fn parse(s: &str) -> Option<Mailbox> {
        let mut parts = s.splitn(3, ':');
        let head = parts.next()?;
        match head {
            "unified" => Some(Mailbox::Unified(FolderRole::parse(parts.next()?))),
            "account" => {
                let id = parts.next()?.to_string();
                Some(Mailbox::AccountRole(id, FolderRole::parse(parts.next().unwrap_or("inbox"))))
            }
            "folder" => parts.next()?.parse().ok().map(Mailbox::Folder),
            "starred" => Some(Mailbox::Starred),
            "unread" => Some(Mailbox::Unread),
            "snoozed" => Some(Mailbox::Snoozed),
            "pinned" => Some(Mailbox::Pinned),
            "attachments" => Some(Mailbox::Attachments),
            "vip" => Some(Mailbox::Vip),
            "category" => Some(Mailbox::Category(Category::parse(parts.next()?))),
            "label" => {
                let account = parts.next()?.to_string();
                Some(Mailbox::Label(account, parts.next()?.to_string()))
            }
            "smart" => Some(Mailbox::Smart(parts.next()?.to_string())),
            "all" => Some(Mailbox::All),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mailboxes() {
        assert_eq!(Mailbox::parse("unified:inbox"), Some(Mailbox::Unified(FolderRole::Inbox)));
        assert_eq!(
            Mailbox::parse("account:abc:sent"),
            Some(Mailbox::AccountRole("abc".into(), FolderRole::Sent))
        );
        assert_eq!(Mailbox::parse("folder:12"), Some(Mailbox::Folder(12)));
        assert_eq!(Mailbox::parse("label:a:Work/2024"), Some(Mailbox::Label("a".into(), "Work/2024".into())));
        assert_eq!(Mailbox::parse("nope"), None);
    }

    #[test]
    fn config_defaults_fill_missing_fields() {
        let c: AccountConfig = serde_json::from_str("{}").unwrap();
        assert!(c.send_id);
        assert_eq!(c.sync_days, 90);
        assert!(c.notify.enabled);
    }
}

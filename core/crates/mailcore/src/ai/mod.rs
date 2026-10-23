//! AI features (contract: `docs/AI.md`): providers and models, streaming chat over HTTP
//! APIs and the local `claude` / `codex` command lines, writing help, summaries, smart
//! reminders, batch jobs, scheduled loops, auto replies and "ask the mailbox"; second batch
//! (§8): calendar extraction, the built-in MCP server and external MCP tools, several keys
//! per provider, model capabilities, usage statistics, attachment explanations, account
//! policies, notification modes, phishing hints, follow-ups, vacation replies and a prompt
//! library.

pub mod ask;
pub mod autoreply;
pub mod batch;
pub mod calendar;
pub mod cli;
pub mod client;
pub mod compose;
mod demo;
pub mod explain;
pub mod followup;
pub mod insight;
pub mod loops;
pub mod mcp_client;
pub mod mcp_server;
pub mod model;
pub mod notify;
pub mod policy;
pub mod prompts;
pub mod prompts_lib;
pub mod providers;
pub mod summary;
pub mod tools;
pub mod usage;

pub use model::*;

use crate::Engine;
use crate::error::{Error, Result};
use crate::model::{FolderRole, MessageId};
use chrono::{Datelike, TimeZone};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const NOT_CONFIGURED: &str = "还没有设置 AI 模型：请在 设置 → AI 服务 中选择";
pub const ENCRYPTED_REFUSED: &str = "这封邮件是加密的，未允许交给 AI 处理";

/// Capacity of the new-mail analysis queue; when full, new messages are skipped.
const QUEUE_CAPACITY: usize = 256;

/// Text deltas of a streaming generation.
pub type DeltaTx = Option<mpsc::Sender<String>>;

/// Runtime state kept in the engine (`Inner.ai`).
pub struct AiState {
    /// Batch jobs in memory (the most recent 20).
    jobs: Mutex<HashMap<String, BatchJob>>,
    job_cancel: Mutex<HashMap<String, CancellationToken>>,
    queue_tx: mpsc::Sender<MessageId>,
    queue_rx: Mutex<Option<mpsc::Receiver<MessageId>>>,
    running_loops: Mutex<HashSet<String>>,
    last_minute: AtomicI64,
    last_prune: AtomicI64,
    /// HTTP client without a total timeout (responses stream for minutes); 15 s to connect.
    http: reqwest::Client,
    /// Key rotation: the next start index per provider.
    key_cursor: Mutex<HashMap<String, usize>>,
    /// OpenAI compatible providers that rejected `stream_options`.
    no_stream_options: Mutex<HashSet<String>>,
    /// Serialises the read-modify-write of usage rows.
    usage_lock: Mutex<()>,
    /// MCP send / delete requests waiting for the user.
    confirms: Mutex<HashMap<String, mcp_server::PendingConfirm>>,
    /// Get-or-create of the MCP token.
    mcp_token_lock: Mutex<()>,
    /// Notifications held for the smart analysis ("important" mode): message → since.
    held: Mutex<HashMap<MessageId, i64>>,
    last_followup: AtomicI64,
    followup_running: AtomicBool,
}

impl AiState {
    pub fn new() -> AiState {
        let (queue_tx, queue_rx) = mpsc::channel(QUEUE_CAPACITY);
        AiState {
            jobs: Mutex::new(HashMap::new()),
            job_cancel: Mutex::new(HashMap::new()),
            queue_tx,
            queue_rx: Mutex::new(Some(queue_rx)),
            running_loops: Mutex::new(HashSet::new()),
            last_minute: AtomicI64::new(0),
            last_prune: AtomicI64::new(0),
            http: ai_http_client(),
            key_cursor: Mutex::new(HashMap::new()),
            no_stream_options: Mutex::new(HashSet::new()),
            usage_lock: Mutex::new(()),
            confirms: Mutex::new(HashMap::new()),
            mcp_token_lock: Mutex::new(()),
            held: Mutex::new(HashMap::new()),
            // The first follow-up check runs about five minutes after start.
            last_followup: AtomicI64::new(crate::store::now() - followup::INTERVAL + 300),
            followup_running: AtomicBool::new(false),
        }
    }
}

impl Default for AiState {
    fn default() -> Self {
        AiState::new()
    }
}

fn ai_http_client() -> reqwest::Client {
    crate::net::install_crypto_provider();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("MailboxAI/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|e| {
            tracing::warn!("构建 AI HTTP 客户端失败, 使用默认配置: {e}");
            reqwest::Client::new()
        })
}

/// Last line of a streaming generation: `{"done":true,"text","model",...extra}`.
#[derive(Debug, Clone, Default)]
pub struct StreamDone {
    pub text: String,
    /// `provider/model`
    pub model: String,
    pub extra: Map<String, Value>,
}

impl StreamDone {
    pub fn new(text: String, model: String) -> StreamDone {
        StreamDone { text, model, extra: Map::new() }
    }

    pub fn with(mut self, key: &str, value: Value) -> StreamDone {
        self.extra.insert(key.to_string(), value);
        self
    }
}

/// Plain text of one message prepared for a prompt.
#[derive(Debug, Clone, Default)]
pub struct MailText {
    pub id: MessageId,
    pub account_id: String,
    pub subject: String,
    /// `名字 <email>`
    pub from: String,
    pub from_email: String,
    pub date: i64,
    /// `名字 <email>, …`
    pub to: String,
    pub cc: String,
    /// File names of the (non-inline) attachments.
    pub attachments: Vec<String>,
    pub text: String,
}

fn addr_text(list: &[crate::model::EmailAddress]) -> String {
    list.iter()
        .map(|a| match &a.name {
            Some(n) if !n.is_empty() => format!("{n} <{}>", a.email),
            _ => a.email.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ---- engine plumbing --------------------------------------------------------------------

/// Starts the background worker of the new-mail analysis queue. Called by `Engine::start`.
pub(crate) fn start(e: &Engine) {
    let Some(mut rx) = e.ai.queue_rx.lock().take() else { return };
    let e = e.clone();
    tokio::spawn(async move {
        let cancel = e.shutdown_token();
        loop {
            let id = tokio::select! {
                _ = cancel.cancelled() => return,
                v = rx.recv() => match v {
                    Some(id) => id,
                    None => return,
                },
            };
            e.ai_process_new_mail(id).await;
        }
    });
}

/// Housekeeping hook (every 30 s): due reminders and loops once a minute, follow-ups once
/// an hour, cleanup once a day.
pub(crate) fn tick(e: &Engine) {
    let now = crate::store::now();
    let minute = now / 60;
    if e.ai.last_minute.swap(minute, Ordering::Relaxed) == minute {
        return;
    }
    insight::check_reminders(e, now);
    loops::schedule_due(e, now);
    followup::schedule(e, now);
    if now - e.ai.last_prune.load(Ordering::Relaxed) > 86_400 {
        e.ai.last_prune.store(now, Ordering::Relaxed);
        insight::prune(e, now);
        calendar::prune(e);
        followup::prune(e, now);
        usage::prune(e);
    }
}

impl Engine {
    /// Queues a new inbox message for smart analysis and auto replies. Never blocks.
    pub(crate) fn ai_enqueue(&self, id: MessageId) {
        if self.ai.queue_tx.try_send(id).is_err() {
            tracing::debug!(message_id = id, "ai queue full or closed, message skipped");
        }
    }

    /// What the background queue does for one new inbox message: follow-ups answered by it
    /// go away, smart analysis (when enabled and in scope) and the notification held for it
    /// ("important" mode), then auto reply rules. Errors are logged, not returned.
    pub async fn ai_process_new_mail(&self, id: MessageId) {
        followup::on_new_mail(self, id);
        let s = self.ai_settings();
        let mut analysed: Option<Insight> = None;
        if s.smart_reminders && insight::in_scope(self, &s, id) {
            match self.ai_analyze(id).await {
                Ok(ins) => analysed = Some(ins),
                Err(Error::AiNotConfigured(_)) | Err(Error::AiRefused(_)) => {}
                Err(err) => tracing::warn!(message_id = id, error = %err, "smart analysis failed"),
            }
        }
        self.ai_release_notification(id, analysed.as_ref());
        if let Err(err) = autoreply::process(self, id).await {
            match err {
                Error::AiNotConfigured(_) | Error::AiRefused(_) => {}
                other => tracing::warn!(message_id = id, error = %other, "auto reply failed"),
            }
        }
    }

    pub fn ai_settings(&self) -> AiSettings {
        self.db.kv_get("ai", "settings").ok().flatten().unwrap_or_default()
    }

    pub fn save_ai_settings(&self, s: &AiSettings) -> Result<()> {
        let mut s = s.clone();
        if s.max_body_chars == 0 {
            s.max_body_chars = AiSettings::default().max_body_chars;
        }
        s.max_body_chars = s.max_body_chars.clamp(1000, 200_000);
        if !(s.tts_rate >= 0.0 && s.tts_rate <= 1.0) {
            s.tts_rate = 0.5;
        }
        if s.language.trim().is_empty() {
            s.language = "zh-CN".into();
        }
        if !matches!(s.notify_mode.as_str(), "normal" | "annotate" | "important") {
            s.notify_mode = "normal".into();
        }
        s.followup_days = s.followup_days.min(60);
        s.account_policies.retain(|k, v| !k.trim().is_empty() && matches!(v.as_str(), "on" | "off" | "local_only"));
        self.db.kv_put("ai", "settings", &s, 0)
    }

    /// The model for a feature: writing / chat use `default_model`, quick tasks prefer
    /// `quick_model`; each falls back to the other. The provider must be enabled (and have
    /// a key when it needs one).
    pub fn ai_model(&self, quick: bool) -> Result<ModelRef> {
        let s = self.ai_settings();
        let order = if quick { [s.quick_model, s.default_model] } else { [s.default_model, s.quick_model] };
        let providers = self.ai_providers();
        let mut key_error: Option<Error> = None;
        for m in order.into_iter().flatten() {
            if m.provider.is_empty() || m.model.is_empty() {
                continue;
            }
            let Some(p) = providers.iter().find(|p| p.id == m.provider) else { continue };
            if !p.enabled {
                continue;
            }
            if client::needs_key(p) && !p.has_key {
                if key_error.is_none() {
                    key_error = Some(Error::AiNotConfigured(format!("{} 还没有填写 API Key：请在 设置 → AI 服务 中填写", p.name)));
                }
                continue;
            }
            return Ok(m);
        }
        Err(key_error.unwrap_or_else(|| Error::AiNotConfigured(NOT_CONFIGURED.into())))
    }

    /// An explicitly requested model (its provider must exist), or [`Engine::ai_model`].
    pub fn ai_model_or(&self, explicit: Option<ModelRef>, quick: bool) -> Result<ModelRef> {
        if let Some(m) = explicit.filter(|m| !m.provider.is_empty() && !m.model.is_empty()) {
            if self.ai_provider(&m.provider).is_some() {
                return Ok(m);
            }
            return Err(Error::AiNotConfigured(format!("找不到 AI 服务商 {}", m.provider)));
        }
        self.ai_model(quick)
    }

    /// Streams text deltas into `tx`; returns the full text. Every call is counted in the
    /// usage statistics (`req.feature`, "chat" when empty).
    pub async fn ai_chat(&self, req: ChatRequest, tx: Option<mpsc::Sender<String>>) -> Result<String> {
        let provider = self
            .ai_provider(&req.model.provider)
            .ok_or_else(|| Error::AiNotConfigured(format!("找不到 AI 服务商 {}", req.model.provider)))?;
        if req.messages.is_empty() {
            return Err(Error::Invalid("没有要发送给 AI 的内容".into()));
        }
        if !req.images.is_empty() && provider.kind.is_cli() {
            return Err(Error::AiNotConfigured(format!("{} 不支持看图：{}", provider.name, explain::VISION_NEEDED)));
        }
        let feature = if req.feature.trim().is_empty() { "chat".to_string() } else { req.feature.clone() };
        tracing::debug!(provider = %provider.id, model = %req.model.model, feature = %feature, "ai request");
        let started = Instant::now();
        let result: Result<(String, TokenUsage)> = match provider.kind {
            ProviderKind::ClaudeCode => cli::claude_chat(self, &provider, &req, tx, None).await.map(|o| (o.text, o.usage)),
            ProviderKind::Codex => cli::codex_chat(self, &provider, &req, tx).await.map(|o| (o.text, o.usage)),
            _ => {
                let keys = self.ai_key_list(&provider.id);
                let start = self.ai_next_key(&provider.id);
                let stream_options = !self.ai.no_stream_options.lock().contains(&provider.id);
                match client::chat(&self.ai.http, &provider, client::Keys::new(&keys, start), stream_options, &req, tx).await {
                    Ok(out) => {
                        if out.stream_options_rejected {
                            self.ai.no_stream_options.lock().insert(provider.id.clone());
                        }
                        Ok((out.text, out.usage))
                    }
                    Err(err) => Err(err),
                }
            }
        };
        let ms = started.elapsed().as_millis() as u64;
        match result {
            Ok((text, usage)) => {
                self.ai_record_usage(&provider.id, &req.model.model, &feature, usage, ms, true);
                Ok(text)
            }
            Err(err) => {
                self.ai_record_usage(&provider.id, &req.model.model, &feature, TokenUsage::default(), ms, false);
                Err(err)
            }
        }
    }

    /// One system + one user message.
    pub(crate) async fn ai_complete(&self, model: &ModelRef, feature: &str, system: String, user: String, json: bool, tx: DeltaTx) -> Result<String> {
        let req = ChatRequest {
            model: model.clone(),
            system: Some(system),
            messages: vec![ChatMessage::user(user)],
            temperature: None,
            max_tokens: None,
            json,
            feature: feature.to_string(),
            images: Vec::new(),
        };
        self.ai_chat(req, tx).await
    }

    /// Asks for a JSON object and extracts it from the answer (models like ``` fences).
    pub(crate) async fn ai_json(&self, model: &ModelRef, feature: &str, system: String, user: String) -> Result<Value> {
        let text = self.ai_complete(model, feature, system, user, true, None).await?;
        // The answer may quote the message: it is not put into the error (errors get logged).
        extract_json(&text).ok_or_else(|| Error::Ai(format!("AI 返回的内容不是有效的 JSON（{} 个字符）", text.chars().count())))
    }

    /// `/v1/ai/chat`; with `tools` the enabled external MCP servers can be used.
    pub async fn ai_chat_stream(
        &self,
        model: ModelRef,
        system: Option<String>,
        messages: Vec<ChatMessage>,
        tools: bool,
        tx: DeltaTx,
    ) -> Result<StreamDone> {
        let label = model.label();
        let system = match system.filter(|s| !s.trim().is_empty()) {
            Some(s) => s,
            None => format!("{}\n{}", prompts::ASSISTANT, now_context()),
        };
        if tools {
            return self.ai_tool_chat(model, system, messages, "chat", tx).await;
        }
        let req = ChatRequest { model, system: Some(system), messages, feature: "chat".into(), ..Default::default() };
        let text = self.ai_chat(req, tx).await?;
        Ok(StreamDone::new(text, label))
    }

    /// Plain text of one message for a prompt (quoted history stripped when `strip`).
    /// The account's AI policy is checked first (`local`: the model runs on this machine);
    /// encrypted messages are refused unless `allow_encrypted`.
    pub(crate) async fn ai_mail_text(&self, id: MessageId, max_chars: usize, strip: bool, local: bool) -> Result<MailText> {
        let account_id = self.db.message(id)?.summary.account_id;
        self.ai_check_account(&account_id, local)?;
        let d = self.message_detail(id).await?;
        if d.encrypted {
            if !self.ai_settings().allow_encrypted {
                return Err(Error::AiRefused(ENCRYPTED_REFUSED.into()));
            }
            if !d.decrypted {
                return Err(Error::AiRefused("这封邮件是加密的，而且无法解密".into()));
            }
        }
        let mut text = match d.text.as_deref().filter(|t| !t.trim().is_empty()) {
            Some(t) => t.to_string(),
            None => crate::html::html_to_text(d.html.as_deref().unwrap_or("")),
        };
        if text.trim().is_empty() {
            text = d.summary.snippet.clone();
        }
        if strip {
            text = strip_quoted(&text);
        }
        let from = match &d.summary.from.name {
            Some(n) if !n.is_empty() => format!("{} <{}>", n, d.summary.from.email),
            _ => d.summary.from.email.clone(),
        };
        Ok(MailText {
            id,
            account_id: d.summary.account_id.clone(),
            subject: if d.summary.subject.is_empty() { "（无主题）".into() } else { d.summary.subject.clone() },
            from,
            from_email: d.summary.from.email.clone(),
            date: d.summary.date,
            to: addr_text(&d.summary.to),
            cc: addr_text(&d.cc),
            attachments: d.attachments.iter().filter(|a| !a.inline).map(|a| a.filename.clone()).collect(),
            text: truncate_chars(&text, max_chars),
        })
    }

    /// Ids of the conversation of a message (oldest first, one copy per Message-ID).
    pub(crate) fn ai_thread_ids(&self, id: MessageId) -> Result<Vec<MessageId>> {
        let m = self.db.message(id)?;
        let include_trash = matches!(m.folder_role, FolderRole::Trash | FolderRole::Junk);
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        for s in self.db.thread(&m.summary.thread_id, include_trash)? {
            if let Ok(sm) = self.db.message(s.id) {
                if let Some(mid) = sm.message_id {
                    if !seen.insert(mid) && s.id != id {
                        continue;
                    }
                }
            }
            ids.push(s.id);
        }
        if !ids.contains(&id) {
            ids.push(id);
        }
        Ok(ids)
    }

    /// The message (or its whole conversation) as numbered plain-text blocks. Messages the
    /// account policy keeps from this model are left out (the asked one is an error).
    pub(crate) async fn ai_conversation(&self, id: MessageId, thread: bool, budget: usize, local: bool) -> Result<String> {
        let mut ids = if thread { self.ai_thread_ids(id)? } else { vec![id] };
        // Long conversations: the newest messages matter most.
        if ids.len() > 12 {
            ids = ids[ids.len() - 12..].to_vec();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let per = (budget / ids.len().max(1)).max(1500);
        let mut parts = Vec::new();
        for (i, mid) in ids.iter().enumerate() {
            let block = match self.ai_mail_text(*mid, per, true, local).await {
                Ok(t) => format!(
                    "【邮件 {}】\n发件人：{}\n时间：{}\n主题：{}\n\n{}",
                    i + 1,
                    t.from,
                    fmt_time(t.date),
                    t.subject,
                    t.text
                ),
                Err(err) if *mid == id => return Err(err),
                Err(_) => format!("【邮件 {}】（这封邮件无法读取或是加密邮件，已略过）", i + 1),
            };
            parts.push(block);
        }
        Ok(parts.join("\n\n"))
    }
}

// ---- text helpers -----------------------------------------------------------------------

static QUOTE_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*(-{2,}\s*(Original Message|原始邮件)\s*-{2,}|On [^\n]{4,200}wrote:|在[^\n]{2,100}写道[:：])")
        .expect("quote regex")
});

/// Removes quoted reply history: lines starting with `>` and everything after common
/// "On … wrote:" / "在 … 写道：" / "-----Original Message-----" markers.
pub fn strip_quoted(text: &str) -> String {
    let cut = match QUOTE_START.find(text) {
        Some(m) if !text[..m.start()].trim().is_empty() => &text[..m.start()],
        _ => text,
    };
    let kept: Vec<&str> = cut.lines().filter(|l| !l.trim_start().starts_with('>')).collect();
    let out = kept.join("\n").trim().to_string();
    if out.is_empty() { text.trim().to_string() } else { out }
}

/// At most `max` characters (0 = unlimited), with a marker when cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 || s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…（内容过长，已截断）");
    out
}

/// At most `max` characters, `…` when cut (for previews and error messages).
pub fn truncate_plain(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// The first JSON object in a model answer (tolerates ``` fences and surrounding text).
pub fn extract_json(s: &str) -> Option<Value> {
    let mut from = 0;
    while let Some(rel) = s[from..].find('{') {
        let start = from + rel;
        if let Some(end) = matching_brace(&s[start..]) {
            if let Ok(v) = serde_json::from_str::<Value>(&s[start..start + end + 1]) {
                if v.is_object() {
                    return Some(v);
                }
            }
        }
        from = start + 1;
    }
    None
}

/// Byte offset of the `}` closing the `{` at the start of `s`.
fn matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Strips a code fence around the whole answer and a preamble line like "以下是润色后的正文：".
pub fn clean_output(s: &str) -> String {
    let mut t = s.trim().to_string();
    if t.len() >= 6 && t.starts_with("```") && t.ends_with("```") {
        let inner = t[3..t.len() - 3].to_string();
        let inner = match inner.find('\n') {
            Some(p) if inner[..p].trim().chars().all(|c| c.is_ascii_alphanumeric()) => inner[p + 1..].to_string(),
            _ => inner,
        };
        t = inner.trim().to_string();
    }
    let mut replaced: Option<String> = None;
    if let Some((first, rest)) = t.split_once('\n') {
        let f = first.trim();
        let lower = f.to_lowercase();
        let starts = ["以下是", "下面是", "好的", "当然", "here is", "here's", "sure"].iter().any(|p| lower.starts_with(p));
        let ends = f.ends_with('：') || f.ends_with(':');
        if starts && ends && f.chars().count() <= 40 && !rest.trim().is_empty() {
            replaced = Some(rest.trim().to_string());
        }
    }
    if let Some(r) = replaced {
        t = r;
    }
    t
}

/// Splits a trailing (or leading) `主题：…` / `Subject: …` line off a generated email.
pub fn split_subject(text: &str) -> (String, Option<String>) {
    const PREFIXES: [&str; 4] = ["主题：", "主题:", "Subject:", "subject:"];
    let lines: Vec<&str> = text.trim().lines().collect();
    let strip = |line: &str| -> Option<String> {
        let l = line.trim().trim_matches('*').trim();
        for p in PREFIXES {
            if let Some(rest) = l.strip_prefix(p) {
                let subject = rest.trim().trim_matches('*').trim().trim_matches(['「', '」', '"']).trim().to_string();
                return Some(subject);
            }
        }
        None
    };
    if let Some(pos) = lines.iter().rposition(|l| !l.trim().is_empty()) {
        if let Some(subject) = strip(lines[pos]) {
            let body = lines[..pos].join("\n").trim().to_string();
            return (body, Some(subject).filter(|s| !s.is_empty()));
        }
    }
    if let Some(pos) = lines.iter().position(|l| !l.trim().is_empty()) {
        if let Some(subject) = strip(lines[pos]) {
            let body = lines[pos + 1..].join("\n").trim().to_string();
            if !body.is_empty() {
                return (body, Some(subject).filter(|s| !s.is_empty()));
            }
        }
    }
    (text.trim().to_string(), None)
}

pub fn markdown_to_html(md: &str) -> String {
    let mut opts = pulldown_cmark::Options::empty();
    opts.insert(pulldown_cmark::Options::ENABLE_TABLES);
    opts.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    opts.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
    let parser = pulldown_cmark::Parser::new_ext(md, opts);
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, parser);
    out
}

/// Markdown → one line of plain text, at most `max` characters.
pub fn preview_text(md: &str, max: usize) -> String {
    let text = crate::html::html_to_text(&markdown_to_html(md));
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_plain(&one_line, max)
}

/// AI text (paragraphs separated by blank lines) as HTML for the rich editor.
pub fn text_html(text: &str) -> String {
    text.trim()
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| format!("<div>{}</div><div><br></div>", crate::html::escape_text(p).replace('\n', "<br>")))
        .collect::<String>()
}

// ---- time helpers -----------------------------------------------------------------------

/// The system time zone as an IANA zone (`TZ`, then the `/etc/localtime` link).
pub fn local_tz() -> Option<chrono_tz::Tz> {
    if let Ok(tz) = std::env::var("TZ") {
        if let Ok(z) = tz.trim().trim_start_matches(':').parse::<chrono_tz::Tz>() {
            return Some(z);
        }
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    let s = target.to_string_lossy().to_string();
    let idx = s.find("zoneinfo/")?;
    s[idx + "zoneinfo/".len()..].parse::<chrono_tz::Tz>().ok()
}

/// Local wall-clock time → unix seconds (system time zone).
pub fn local_timestamp(naive: &chrono::NaiveDateTime) -> Option<i64> {
    match local_tz() {
        Some(tz) => tz.from_local_datetime(naive).earliest().map(|d| d.timestamp()),
        None => chrono::Local.from_local_datetime(naive).earliest().map(|d| d.timestamp()),
    }
}

pub fn weekday_zh(n: u32) -> &'static str {
    match n {
        1 => "星期一",
        2 => "星期二",
        3 => "星期三",
        4 => "星期四",
        5 => "星期五",
        6 => "星期六",
        _ => "星期日",
    }
}

/// "现在是 2026-10-23 14:30（星期五），时区 Asia/Shanghai（UTC+08:00）。"
pub fn now_context() -> String {
    let now = chrono::Local::now();
    let offset = now.format("%:z").to_string();
    let zone = local_tz().map(|t| t.name().to_string()).unwrap_or_else(|| format!("UTC{offset}"));
    format!(
        "现在是 {}（{}），时区 {}（UTC{}）。",
        now.format("%Y-%m-%d %H:%M"),
        weekday_zh(now.weekday().number_from_monday()),
        zone,
        offset
    )
}

/// Local "2026-10-23 14:30".
pub fn fmt_time(ts: i64) -> String {
    chrono::Local.timestamp_opt(ts, 0).single().map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// Local "2026-10-23".
pub fn local_day() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_json_from_fenced_answers() {
        let v = extract_json("好的：\n```json\n{\"a\": {\"b\": \"}\"}, \"c\": [1]}\n```").unwrap();
        assert_eq!(v["a"]["b"], "}");
        assert_eq!(v["c"][0], 1);
        assert!(extract_json("没有 JSON").is_none());
        let v = extract_json("{bad} then {\"ok\":true}").unwrap();
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn strips_quotes_and_truncates() {
        let t = "好的，没问题。\n\n在 2026年10月22日 10:00，张三 <z@x.cn> 写道：\n> 明天开会？\n> 谢谢";
        assert_eq!(strip_quoted(t), "好的，没问题。");
        let t = "Sounds good.\n> quoted\nThanks\n\nOn Thu, Oct 22, 2026 at 10:00 Bob <b@x.com> wrote:\nold text";
        assert_eq!(strip_quoted(t), "Sounds good.\nThanks");
        assert_eq!(truncate_chars("一二三四五", 3), "一二三\n…（内容过长，已截断）");
        assert_eq!(truncate_chars("一二三", 3), "一二三");
    }

    #[test]
    fn cleans_output_and_splits_subject() {
        assert_eq!(clean_output("以下是润色后的正文：\n你好，\n\n谢谢"), "你好，\n\n谢谢");
        assert_eq!(clean_output("```\n正文\n```"), "正文");
        let (body, subject) = split_subject("你好，\n\n下周二可以。\n\n主题：会议时间确认");
        assert_eq!(body, "你好，\n\n下周二可以。");
        assert_eq!(subject.as_deref(), Some("会议时间确认"));
        let (body, subject) = split_subject("只有正文");
        assert_eq!((body.as_str(), subject), ("只有正文", None));
    }

    #[test]
    fn text_to_editor_html() {
        assert_eq!(text_html("第一段\n第二行\n\n<b>"), "<div>第一段<br>第二行</div><div><br></div><div>&lt;b&gt;</div><div><br></div>");
        let p = preview_text("**一句话**：测试\n\n- 要点", 8);
        assert!(p.starts_with("一句话：测试"), "{p}");
        assert!(p.ends_with('…'), "{p}");
    }
}

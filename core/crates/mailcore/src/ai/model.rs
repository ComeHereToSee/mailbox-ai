//! AI data types. Everything is JSON with snake_case names (see `docs/AI.md` §2 and §8).

use crate::model::MessageId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which wire protocol a provider speaks.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Any OpenAI compatible API (OpenAI, DeepSeek, Qwen, Kimi, GLM, Ollama, LM Studio …).
    #[default]
    Openai,
    /// Anthropic Messages API.
    Anthropic,
    /// Google Generative Language API.
    Gemini,
    AzureOpenai,
    /// Local `claude` CLI (subscription).
    ClaudeCode,
    /// Local `codex` CLI (subscription).
    Codex,
}

impl ProviderKind {
    pub fn is_cli(&self) -> bool {
        matches!(self, ProviderKind::ClaudeCode | ProviderKind::Codex)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct ProviderPreset {
    pub id: String,
    pub kind: ProviderKind,
    pub name: String,
    pub base_url: String,
    pub website: String,
    pub key_url: Option<String>,
    pub needs_key: bool,
    pub color: String,
    pub models: Vec<String>,
    pub description: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct AiModel {
    /// Model id sent to the provider; for the CLI providers `default` means "no --model / -m".
    pub id: String,
    pub name: String,
    pub group: String,
    pub enabled: bool,
    /// "vision" | "reasoning" | "tools" | "embedding" (inferred from the id, editable).
    pub capabilities: Vec<String>,
}

impl Default for AiModel {
    fn default() -> Self {
        AiModel { id: String::new(), name: String::new(), group: String::new(), enabled: true, capabilities: Vec::new() }
    }
}

impl AiModel {
    /// A model with its group and capabilities inferred from the id.
    pub fn new(id: &str) -> AiModel {
        AiModel { id: id.to_string(), name: id.to_string(), group: model_group(id), enabled: true, capabilities: infer_capabilities(id) }
    }
}

/// UI group of a model id: the part before the first `-` or `/`.
pub fn model_group(id: &str) -> String {
    id.split(['-', '/']).next().unwrap_or("").to_string()
}

/// Capabilities guessed from a model id (case-insensitive). Embedding models get only
/// "embedding".
pub fn infer_capabilities(id: &str) -> Vec<String> {
    let s = id.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| s.contains(w));
    if s.contains("embed") {
        return vec!["embedding".to_string()];
    }
    let mut out = Vec::new();
    if has(&["vision", "vl", "gpt-4o", "gpt-4.1", "gpt-5", "claude", "gemini", "glm-4v", "qwen-vl", "omni", "pixtral", "llava"]) {
        out.push("vision".to_string());
    }
    if has(&["reason", "r1", "o1", "o3", "o4", "thinking", "qwq"]) {
        out.push("reasoning".to_string());
    }
    if has(&["gpt", "claude", "gemini", "qwen", "deepseek-chat", "deepseek-v", "glm", "kimi", "moonshot", "mistral", "grok"]) {
        out.push("tools".to_string());
    }
    out
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct AiProvider {
    pub id: String,
    pub kind: ProviderKind,
    pub name: String,
    /// API base URL; for the CLI providers an optional path to the executable.
    pub base_url: String,
    /// Azure only.
    pub api_version: Option<String>,
    pub enabled: bool,
    pub models: Vec<AiModel>,
    /// Output only.
    pub has_key: bool,
    /// Custom providers can be deleted.
    pub custom: bool,
    /// Output only: how many API keys are stored (comma / newline separated, used in turn).
    pub key_count: usize,
}

/// Body of `PUT /v1/ai/providers/{id}`: the provider plus an optional key.
/// `api_key`: `Some("")` removes the key, `None` keeps it, `Some(k)` stores it.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct AiProviderInput {
    #[serde(flatten)]
    pub provider: AiProvider,
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

impl ModelRef {
    pub fn new(provider: &str, model: &str) -> ModelRef {
        ModelRef { provider: provider.to_string(), model: model.to_string() }
    }

    /// `provider/model`, used in the `model` field of `done` lines.
    pub fn label(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AiSettings {
    /// Writing, chat, ask the mailbox, auto replies.
    pub default_model: Option<ModelRef>,
    /// Summaries, translation, classification, smart reminders, loops. Falls back to `default_model`.
    pub quick_model: Option<ModelRef>,
    /// "auto" | "zh-CN" | "en" | …
    pub language: String,
    /// "professional" | "friendly" | "concise"
    pub tone: String,
    pub signature_name: String,
    pub smart_reminders: bool,
    /// "primary" | "all" | "vip"
    pub smart_scope: String,
    pub auto_summary: bool,
    pub allow_encrypted: bool,
    pub max_body_chars: usize,
    pub tts_voice: Option<String>,
    pub tts_rate: f32,
    /// Built-in MCP server (`POST /mcp`).
    pub mcp_enabled: bool,
    /// Explains image attachments; falls back to `default_model` when it can see images.
    pub vision_model: Option<ModelRef>,
    /// Semantic re-ranking for "ask the mailbox" (OpenAI / Gemini embeddings).
    pub embedding_model: Option<ModelRef>,
    /// account id → "on" (default) | "off" | "local_only".
    pub account_policies: BTreeMap<String, String>,
    /// "normal" | "annotate" | "important"
    pub notify_mode: String,
    /// Remind about own sent mail without an answer after this many days (0 = off).
    pub followup_days: u32,
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings {
            default_model: None,
            quick_model: None,
            language: "zh-CN".into(),
            tone: "professional".into(),
            signature_name: String::new(),
            smart_reminders: true,
            smart_scope: "primary".into(),
            auto_summary: false,
            allow_encrypted: false,
            max_body_chars: 12000,
            tts_voice: None,
            tts_rate: 0.5,
            mcp_enabled: false,
            vision_model: None,
            embedding_model: None,
            account_policies: BTreeMap::new(),
            notify_mode: "normal".into(),
            followup_days: 3,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ChatMessage {
    /// "system" | "user" | "assistant"
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> ChatMessage {
        ChatMessage { role: "user".into(), content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> ChatMessage {
        ChatMessage { role: "assistant".into(), content: content.into() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ChatRequest {
    pub model: ModelRef,
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Ask for a JSON object (OpenAI `response_format`, Gemini `responseMimeType`).
    pub json: bool,
    /// Feature for the usage statistics ("compose", "summary", …; empty = "chat").
    #[serde(default)]
    pub feature: String,
    /// Images sent along with the last user message (vision models only).
    #[serde(default)]
    pub images: Vec<ChatImage>,
}

/// An inline image for a vision model.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ChatImage {
    /// "image/png", "image/jpeg" …
    pub media_type: String,
    /// Base64 (standard alphabet, padded).
    pub data: String,
}

/// Tokens of one AI call, as reported by the provider (0 when unknown).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// One aggregated row of kv space `ai_usage` (date + provider + model + feature).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct UsageRow {
    /// Local date, "2026-10-23".
    pub date: String,
    pub provider: String,
    pub model: String,
    pub feature: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub errors: u64,
    pub duration_ms: u64,
}

/// A tool called during a chat with external MCP tools (`done.tool_calls`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ToolCallRecord {
    /// Display name of the MCP server.
    pub server: String,
    pub server_id: String,
    pub tool: String,
    pub ok: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct CliStatus {
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub logged_in: Option<bool>,
    pub hint: String,
}

// ---- smart reminders ---------------------------------------------------------------------

/// A calendar entry found in a message (`POST /v1/ai/messages/{id}/events`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ExtractedEvent {
    pub title: String,
    /// Unix seconds.
    pub start: i64,
    /// Missing: the UI uses one hour.
    pub end: Option<i64>,
    pub all_day: bool,
    pub location: Option<String>,
    /// One sentence.
    pub notes: String,
    /// How the time was phrased in the message, e.g. 「下周二下午 3 点」.
    pub source_text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Insight {
    pub message_id: MessageId,
    pub needs_reply: bool,
    /// "high" | "normal" | "low"
    pub priority: String,
    /// Unix seconds, inferred from the body ("周五前" → this Friday 18:00 local time).
    pub deadline: Option<i64>,
    /// How the deadline was phrased in the message.
    pub deadline_text: Option<String>,
    pub todos: Vec<String>,
    /// One sentence (≤ 40 characters).
    pub one_line: String,
    /// When to remind (see `insight::compute_remind_at`).
    pub remind_at: Option<i64>,
    pub done: bool,
    pub created_at: i64,
    /// The `ai_reminder` event was sent.
    pub reminded: bool,
    /// Calendar entries found by the analysis (also cached in `ai_events`).
    pub events: Vec<ExtractedEvent>,
    /// "phishing" | "scam"; None when the message looks harmless.
    pub risk: Option<String>,
    pub risk_reason: String,
}

// ---- summaries ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct SummaryCache {
    pub text: String,
    pub model: String,
    pub created_at: i64,
    /// Messages in the conversation when the summary was made (a new message invalidates it).
    pub thread_count: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ReplySuggestion {
    pub label: String,
    pub text: String,
}

// ---- batch jobs --------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct BatchJob {
    pub id: String,
    pub op: String,
    /// running | done | failed | cancelled
    pub status: String,
    pub total: usize,
    pub done: usize,
    pub items: Vec<BatchItem>,
    pub summary: Option<String>,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct BatchItem {
    pub message_id: MessageId,
    pub subject: String,
    pub from: String,
    pub output: String,
    pub error: Option<String>,
    pub suggested: Vec<SuggestedAction>,
    pub draft_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct SuggestedAction {
    /// archive | trash | star | read | label | move | category
    pub action: String,
    pub value: Option<String>,
    pub reason: String,
}

// ---- loops -------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct LoopSchedule {
    /// "daily" | "weekdays" | "weekly" | "interval"
    pub kind: String,
    /// "08:30"
    pub time: String,
    /// 1 = Monday … 7 = Sunday (weekly).
    pub weekday: Option<u8>,
    /// Interval in minutes (≥ 30).
    pub every_minutes: Option<u32>,
}

impl Default for LoopSchedule {
    fn default() -> Self {
        LoopSchedule { kind: "daily".into(), time: "08:00".into(), weekday: None, every_minutes: None }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct AiLoop {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub schedule: LoopSchedule,
    /// Search syntax, e.g. `is:unread newer_than:1d`; empty = recent mail of the unified inbox.
    pub query: String,
    pub instruction: String,
    pub max_messages: usize,
    pub notify: bool,
    /// Also deliver the result as a message into the local inbox.
    pub email_digest: bool,
    /// Defaults to `quick_model`.
    pub model: Option<ModelRef>,
    pub last_run: Option<i64>,
    pub next_run: Option<i64>,
}

impl Default for AiLoop {
    fn default() -> Self {
        AiLoop {
            id: String::new(),
            name: String::new(),
            enabled: true,
            schedule: LoopSchedule::default(),
            query: String::new(),
            instruction: String::new(),
            max_messages: 30,
            notify: true,
            email_digest: false,
            model: None,
            last_run: None,
            next_run: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct LoopRun {
    pub id: String,
    pub loop_id: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// running | done | failed
    pub status: String,
    pub message_count: usize,
    /// Markdown.
    pub output: String,
    pub error: Option<String>,
}

// ---- auto reply --------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct AutoReplyRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub account_id: Option<String>,
    /// Empty = any.
    pub from_contains: String,
    pub subject_contains: String,
    /// Optional: the quick model decides whether the message is about this.
    pub ai_match: String,
    /// How to reply.
    pub instruction: String,
    /// "draft" (default) | "send"
    pub mode: String,
    pub max_per_day: u32,
    /// Reply to the same sender at most once in this many hours.
    pub once_per_sender_hours: u32,
    /// Internal counters.
    pub sent_today: u32,
    pub day: String,
    /// "ai" (default) | "vacation"
    pub kind: String,
    /// vacation: the fixed reply (no AI).
    pub reply_text: String,
    /// vacation: active from / until (unix seconds, open when missing).
    pub start: Option<i64>,
    pub end: Option<i64>,
}

impl Default for AutoReplyRule {
    fn default() -> Self {
        AutoReplyRule {
            id: String::new(),
            name: String::new(),
            enabled: true,
            account_id: None,
            from_contains: String::new(),
            subject_contains: String::new(),
            ai_match: String::new(),
            instruction: String::new(),
            mode: "draft".into(),
            max_per_day: 20,
            once_per_sender_hours: 24,
            sent_today: 0,
            day: String::new(),
            kind: "ai".into(),
            reply_text: String::new(),
            start: None,
            end: None,
        }
    }
}

// ---- MCP ---------------------------------------------------------------------------------

/// A send / delete requested by an MCP client, waiting for the user (`GET /v1/ai/confirm`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Confirmation {
    pub id: String,
    /// "send" | "delete"
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub draft_id: Option<String>,
    pub message_ids: Vec<MessageId>,
    pub created_at: i64,
    pub expires_at: i64,
}

/// An external MCP server whose tools the assistant may use (kv space `ai_mcp_server`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// "stdio" | "http"
    pub transport: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: String,
    pub headers: BTreeMap<String, String>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        McpServerConfig {
            id: String::new(),
            name: String::new(),
            enabled: true,
            transport: "stdio".into(),
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: String::new(),
            headers: BTreeMap::new(),
        }
    }
}

// ---- prompt library ----------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct PromptPreset {
    pub id: String,
    pub name: String,
    /// "compose" | "reader" | "batch"
    pub scope: String,
    pub prompt: String,
    pub sort: i64,
}

// ---- ask the mailbox ---------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct AskSource {
    pub index: usize,
    pub message_id: MessageId,
    pub subject: String,
    pub from: String,
    pub date: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_names() {
        assert_eq!(serde_json::to_string(&ProviderKind::AzureOpenai).unwrap(), "\"azure_openai\"");
        assert_eq!(serde_json::to_string(&ProviderKind::ClaudeCode).unwrap(), "\"claude_code\"");
        let s: AiSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.language, "zh-CN");
        assert_eq!(s.max_body_chars, 12000);
        assert!(s.smart_reminders);
        let input: AiProviderInput =
            serde_json::from_str(r#"{"id":"x","kind":"anthropic","name":"X","enabled":true,"api_key":"k"}"#).unwrap();
        assert_eq!(input.provider.kind, ProviderKind::Anthropic);
        assert_eq!(input.api_key.as_deref(), Some("k"));
        let m: AiModel = serde_json::from_str(r#"{"id":"gpt-4o"}"#).unwrap();
        assert!(m.enabled);
        assert_eq!(model_group("gpt-4o"), "gpt");
        assert_eq!(model_group("Qwen/Qwen2.5-7B"), "Qwen");
        // Batch 1 JSON (no new fields) still loads.
        let s: AiSettings = serde_json::from_str(r#"{"language":"en","smart_scope":"all"}"#).unwrap();
        assert_eq!(s.notify_mode, "normal");
        assert_eq!(s.followup_days, 3);
        assert!(!s.mcp_enabled && s.account_policies.is_empty());
        let r: AutoReplyRule = serde_json::from_str(r#"{"id":"1","instruction":"x"}"#).unwrap();
        assert_eq!(r.kind, "ai");
        let ins: Insight = serde_json::from_str(r#"{"message_id":3,"priority":"high"}"#).unwrap();
        assert!(ins.events.is_empty() && ins.risk.is_none());
        assert!(m.capabilities.is_empty(), "stored models keep what they had");
    }

    #[test]
    fn capabilities_from_model_ids() {
        let caps = |id: &str| infer_capabilities(id);
        assert_eq!(caps("gpt-4o-mini"), vec!["vision", "tools"]);
        assert_eq!(caps("GPT-5"), vec!["vision", "tools"]);
        assert_eq!(caps("claude-sonnet-4-5"), vec!["vision", "tools"]);
        assert_eq!(caps("gemini-2.5-pro"), vec!["vision", "tools"]);
        assert_eq!(caps("deepseek-chat"), vec!["tools"]);
        assert_eq!(caps("deepseek-reasoner"), vec!["reasoning"]);
        assert_eq!(caps("deepseek-r1"), vec!["reasoning"]);
        assert_eq!(caps("QwQ-32B"), vec!["reasoning"]);
        assert_eq!(caps("qwen-vl-max"), vec!["vision", "tools"]);
        assert_eq!(caps("o3-mini"), vec!["reasoning"]);
        assert_eq!(caps("glm-4v-plus"), vec!["vision", "tools"]);
        assert_eq!(caps("text-embedding-3-small"), vec!["embedding"]);
        assert_eq!(caps("gemini-embedding-001"), vec!["embedding"]);
        assert_eq!(caps("moonshot-v1-8k"), vec!["tools"]);
        assert!(caps("sonnet").is_empty());
        assert!(caps("default").is_empty());
        assert_eq!(AiModel::new("grok-4").capabilities, vec!["tools"]);
    }
}

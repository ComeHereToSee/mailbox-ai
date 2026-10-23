//! HTTP providers: OpenAI compatible, Anthropic, Gemini and Azure OpenAI, streamed as SSE
//! and read with `Response::chunk()`.

use super::model::*;
use super::providers;
use crate::error::{Error, Result};
use serde_json::{Map, Value, json};
use std::time::Duration;
use tokio::sync::mpsc;

/// Longest silence allowed between two chunks of a response (and before the headers).
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Collects the generated text and forwards each delta to the stream (if any).
pub(crate) struct Sink {
    pub text: String,
    tx: Option<mpsc::Sender<String>>,
}

impl Sink {
    pub fn new(tx: Option<mpsc::Sender<String>>) -> Sink {
        Sink { text: String::new(), tx }
    }

    pub async fn push(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.text.push_str(s);
        if let Some(tx) = &self.tx {
            let _ = tx.send(s.to_string()).await;
        }
    }
}

// ---- SSE ----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental Server-Sent Events parser; chunks may split lines (and UTF-8 characters)
/// anywhere.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.buf.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&raw).to_string();
            self.line(text.trim_end_matches(['\n', '\r']), &mut out);
        }
        out
    }

    /// Flushes what is left when the body ends.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let text = String::from_utf8_lossy(&rest).to_string();
            self.line(text.trim_end_matches(['\n', '\r']), &mut out);
        }
        self.dispatch(&mut out);
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseEvent>) {
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "data" => {
                // Some servers leave out the blank line between events: a complete JSON
                // document already pending means this line starts a new event.
                if !self.data.is_empty() && serde_json::from_str::<Value>(&self.data.join("\n")).is_ok() {
                    self.dispatch(out);
                }
                self.data.push(value.to_string());
            }
            "event" => self.event = Some(value.to_string()),
            _ => {}
        }
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        if self.data.is_empty() {
            self.event = None;
            return;
        }
        let data = std::mem::take(&mut self.data).join("\n");
        out.push(SseEvent { event: self.event.take(), data });
    }
}

// ---- helpers ------------------------------------------------------------------------------

/// Whether calls need an API key (CLI providers and local servers do not).
pub fn needs_key(p: &AiProvider) -> bool {
    if p.kind.is_cli() {
        return false;
    }
    match providers::preset(&p.id) {
        Some(pr) => pr.needs_key,
        None => matches!(p.kind, ProviderKind::Anthropic | ProviderKind::Gemini | ProviderKind::AzureOpenai),
    }
}

/// System prompt (top-level + `system` messages) and the remaining conversation.
pub(crate) fn split_system(req: &ChatRequest) -> (Option<String>, Vec<ChatMessage>) {
    let mut system: Vec<String> = req.system.iter().filter(|s| !s.trim().is_empty()).cloned().collect();
    let mut msgs = Vec::new();
    for m in &req.messages {
        if m.role == "system" {
            if !m.content.trim().is_empty() {
                system.push(m.content.clone());
            }
        } else {
            msgs.push(m.clone());
        }
    }
    (if system.is_empty() { None } else { Some(system.join("\n\n")) }, msgs)
}

fn base(p: &AiProvider) -> Result<String> {
    let b = p.base_url.trim().trim_end_matches('/').to_string();
    if !b.is_empty() {
        return Ok(b);
    }
    match p.kind {
        ProviderKind::Anthropic => Ok("https://api.anthropic.com".into()),
        ProviderKind::Gemini => Ok("https://generativelanguage.googleapis.com".into()),
        _ => Err(Error::AiNotConfigured(format!("{} 还没有填写 API 地址：请在 设置 → AI 服务 中填写", p.name))),
    }
}

/// Anthropic / Gemini bases are accepted with or without the version suffix.
fn strip_version(b: &str, suffix: &str) -> String {
    b.strip_suffix(suffix).unwrap_or(b).to_string()
}

fn azure_version(p: &AiProvider) -> String {
    p.api_version.clone().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| "2024-10-21".into())
}

fn auth(mut b: reqwest::RequestBuilder, p: &AiProvider, key: Option<&str>) -> reqwest::RequestBuilder {
    let key = key.map(str::trim).filter(|k| !k.is_empty());
    match p.kind {
        ProviderKind::AzureOpenai => {
            if let Some(k) = key {
                b = b.header("api-key", k);
            }
        }
        ProviderKind::Anthropic => {
            if let Some(k) = key {
                b = b.header("x-api-key", k);
            }
            b = b.header("anthropic-version", "2023-06-01");
        }
        ProviderKind::Gemini => {
            if let Some(k) = key {
                b = b.header("x-goog-api-key", k);
            }
        }
        _ => {
            if let Some(k) = key {
                b = b.bearer_auth(k);
            }
        }
    }
    b
}

async fn send(b: reqwest::RequestBuilder) -> Result<reqwest::Response> {
    match tokio::time::timeout(IDLE_TIMEOUT, b.send()).await {
        Err(_) => Err(Error::Ai("AI 服务超过 120 秒没有响应".into())),
        Ok(Err(e)) => Err(Error::Net(format!("无法连接 AI 服务：{e}"))),
        Ok(Ok(r)) => Ok(r),
    }
}

async fn next_chunk(resp: &mut reqwest::Response) -> Result<Option<Vec<u8>>> {
    match tokio::time::timeout(IDLE_TIMEOUT, resp.chunk()).await {
        Err(_) => Err(Error::Ai("AI 服务超过 120 秒没有返回数据".into())),
        Ok(Err(e)) => Err(Error::Net(format!("读取 AI 响应失败：{e}"))),
        Ok(Ok(c)) => Ok(c.map(|b| b.to_vec())),
    }
}

fn error_detail(v: &Value) -> Option<String> {
    let from = |x: &Value| -> Option<String> {
        x.pointer("/error/message")
            .and_then(|m| m.as_str())
            .or_else(|| x.get("error").and_then(|e| e.as_str()))
            .or_else(|| x.get("message").and_then(|e| e.as_str()))
            .or_else(|| x.get("msg").and_then(|e| e.as_str()))
            .map(String::from)
    };
    match v.as_array() {
        Some(list) => list.first().and_then(from),
        None => from(v),
    }
}

/// `error` object / string of an error event.
fn error_text(err: &Value) -> String {
    err.get("message")
        .and_then(|m| m.as_str())
        .map(String::from)
        .or_else(|| err.as_str().map(String::from))
        .unwrap_or_else(|| err.to_string())
}

pub(crate) fn status_error(status: u16, detail: &str) -> Error {
    let detail = detail.trim();
    let with = |head: &str| if detail.is_empty() { head.to_string() } else { format!("{head}（{detail}）") };
    Error::Ai(match status {
        401 | 403 => with("API Key 无效或没有权限"),
        429 => with("请求太频繁或额度不足"),
        _ => {
            if detail.is_empty() {
                format!("AI 服务返回错误 {status}")
            } else {
                format!("AI 服务返回错误 {status}：{detail}")
            }
        }
    })
}

async fn http_error(resp: reqwest::Response) -> Error {
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| error_detail(&v))
        .unwrap_or_else(|| super::truncate_plain(body.trim(), 300));
    status_error(status, &super::truncate_plain(&detail, 300))
}

fn is_json_response(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"))
}

// ---- chat ---------------------------------------------------------------------------------

/// Runs a chat request against an HTTP provider; deltas go to `tx`, the full text is returned.
pub async fn chat(
    http: &reqwest::Client,
    p: &AiProvider,
    key: Option<&str>,
    req: &ChatRequest,
    tx: Option<mpsc::Sender<String>>,
) -> Result<String> {
    let key = key.map(str::trim).filter(|k| !k.is_empty());
    if key.is_none() && needs_key(p) {
        return Err(Error::AiNotConfigured(format!("{} 还没有填写 API Key：请在 设置 → AI 服务 中填写", p.name)));
    }
    let mut sink = Sink::new(tx);
    match p.kind {
        ProviderKind::Openai | ProviderKind::AzureOpenai => openai(http, p, key, req, &mut sink).await?,
        ProviderKind::Anthropic => anthropic(http, p, key, req, &mut sink).await?,
        ProviderKind::Gemini => gemini(http, p, key, req, &mut sink).await?,
        ProviderKind::ClaudeCode | ProviderKind::Codex => return Err(Error::Invalid("命令行服务商不走 HTTP".into())),
    }
    if sink.text.trim().is_empty() {
        return Err(Error::Ai("AI 没有返回任何内容".into()));
    }
    Ok(sink.text)
}

async fn openai(http: &reqwest::Client, p: &AiProvider, key: Option<&str>, req: &ChatRequest, sink: &mut Sink) -> Result<()> {
    let (system, msgs) = split_system(req);
    let mut messages: Vec<Value> = Vec::new();
    if let Some(s) = &system {
        messages.push(json!({ "role": "system", "content": s }));
    }
    for m in &msgs {
        let role = if m.role == "assistant" { "assistant" } else { "user" };
        messages.push(json!({ "role": role, "content": m.content }));
    }
    let mut body = json!({ "model": req.model.model, "messages": messages, "stream": true });
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(n) = req.max_tokens {
        body["max_tokens"] = json!(n);
    }
    if req.json {
        body["response_format"] = json!({ "type": "json_object" });
    }
    let b = base(p)?;
    let url = if p.kind == ProviderKind::AzureOpenai {
        format!("{b}/openai/deployments/{}/chat/completions?api-version={}", req.model.model, azure_version(p))
    } else {
        format!("{b}/chat/completions")
    };
    let mut resp = send(auth(http.post(url.as_str()).json(&body), p, key)).await?;
    if resp.status().as_u16() == 400 && req.json {
        // Not every compatible server knows `response_format`: retry once without it.
        if let Some(obj) = body.as_object_mut() {
            obj.remove("response_format");
        }
        resp = send(auth(http.post(url.as_str()).json(&body), p, key)).await?;
    }
    if !resp.status().is_success() {
        return Err(http_error(resp).await);
    }
    if is_json_response(&resp) {
        // A server that ignored `stream: true`.
        let v: Value = resp.json().await.map_err(|e| Error::Ai(format!("无法解析 AI 响应：{e}")))?;
        if let Some(err) = v.get("error") {
            return Err(Error::Ai(format!("AI 服务返回错误：{}", error_text(err))));
        }
        if let Some(t) = v.pointer("/choices/0/message/content").and_then(|c| c.as_str()) {
            sink.push(t).await;
        }
        return Ok(());
    }
    let mut parser = SseParser::default();
    let mut finished = false;
    while !finished {
        let Some(chunk) = next_chunk(&mut resp).await? else { break };
        for ev in parser.push(&chunk) {
            if openai_event(&ev, sink).await? {
                finished = true;
                break;
            }
        }
    }
    if !finished {
        for ev in parser.finish() {
            if openai_event(&ev, sink).await? {
                break;
            }
        }
    }
    Ok(())
}

/// Handles one OpenAI SSE event; returns true at `[DONE]`.
async fn openai_event(ev: &SseEvent, sink: &mut Sink) -> Result<bool> {
    let data = ev.data.trim();
    if data == "[DONE]" {
        return Ok(true);
    }
    let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(false) };
    if let Some(err) = v.get("error") {
        return Err(Error::Ai(format!("AI 服务返回错误：{}", error_text(err))));
    }
    // `reasoning_content` (DeepSeek & co.) is ignored on purpose.
    if let Some(t) = v.pointer("/choices/0/delta/content").and_then(|c| c.as_str()) {
        sink.push(t).await;
    }
    Ok(false)
}

async fn anthropic(http: &reqwest::Client, p: &AiProvider, key: Option<&str>, req: &ChatRequest, sink: &mut Sink) -> Result<()> {
    let (system, msgs) = split_system(req);
    let mut messages: Vec<Value> = Vec::new();
    for m in &msgs {
        let role = if m.role == "assistant" { "assistant" } else { "user" };
        messages.push(json!({ "role": role, "content": m.content }));
    }
    let mut body = json!({
        "model": req.model.model,
        "messages": messages,
        "max_tokens": req.max_tokens.unwrap_or(4096),
        "stream": true,
    });
    if let Some(s) = system {
        body["system"] = json!(s);
    }
    let url = format!("{}/v1/messages", strip_version(&base(p)?, "/v1"));
    let mut resp = send(auth(http.post(url.as_str()).json(&body), p, key)).await?;
    if !resp.status().is_success() {
        return Err(http_error(resp).await);
    }
    if is_json_response(&resp) {
        let v: Value = resp.json().await.map_err(|e| Error::Ai(format!("无法解析 AI 响应：{e}")))?;
        if let Some(err) = v.get("error") {
            return Err(Error::Ai(format!("AI 服务返回错误：{}", error_text(err))));
        }
        for part in v["content"].as_array().into_iter().flatten() {
            if part["type"] == "text" {
                if let Some(t) = part["text"].as_str() {
                    sink.push(t).await;
                }
            }
        }
        return Ok(());
    }
    let mut parser = SseParser::default();
    let mut state = AnthropicState::default();
    while !state.stopped {
        let Some(chunk) = next_chunk(&mut resp).await? else { break };
        for ev in parser.push(&chunk) {
            anthropic_event(&ev, sink, &mut state).await?;
            if state.stopped {
                break;
            }
        }
    }
    if !state.stopped {
        for ev in parser.finish() {
            anthropic_event(&ev, sink, &mut state).await?;
        }
    }
    if state.refused && sink.text.trim().is_empty() {
        return Err(Error::Ai("模型拒绝回答这个请求".into()));
    }
    Ok(())
}

#[derive(Default)]
struct AnthropicState {
    stopped: bool,
    refused: bool,
}

async fn anthropic_event(ev: &SseEvent, sink: &mut Sink, state: &mut AnthropicState) -> Result<()> {
    let Ok(v) = serde_json::from_str::<Value>(ev.data.trim()) else { return Ok(()) };
    let kind = v["type"].as_str().or(ev.event.as_deref()).unwrap_or("");
    match kind {
        "content_block_delta" => {
            if v["delta"]["type"] == "text_delta" {
                if let Some(t) = v["delta"]["text"].as_str() {
                    sink.push(t).await;
                }
            }
        }
        "message_delta" => {
            if v["delta"]["stop_reason"] == "refusal" {
                state.refused = true;
            }
        }
        "message_stop" => state.stopped = true,
        "error" => return Err(Error::Ai(format!("AI 服务返回错误：{}", error_text(&v["error"])))),
        _ => {}
    }
    Ok(())
}

async fn gemini(http: &reqwest::Client, p: &AiProvider, key: Option<&str>, req: &ChatRequest, sink: &mut Sink) -> Result<()> {
    let (system, msgs) = split_system(req);
    let mut contents: Vec<Value> = Vec::new();
    for m in &msgs {
        let role = if m.role == "assistant" { "model" } else { "user" };
        contents.push(json!({ "role": role, "parts": [{ "text": m.content }] }));
    }
    let mut body = json!({ "contents": contents });
    if let Some(s) = system {
        body["systemInstruction"] = json!({ "parts": [{ "text": s }] });
    }
    let mut config = Map::new();
    if let Some(t) = req.temperature {
        config.insert("temperature".into(), json!(t));
    }
    if let Some(n) = req.max_tokens {
        config.insert("maxOutputTokens".into(), json!(n));
    }
    if req.json {
        config.insert("responseMimeType".into(), json!("application/json"));
    }
    if !config.is_empty() {
        body["generationConfig"] = Value::Object(config);
    }
    let model = req.model.model.trim().trim_start_matches("models/");
    let url = format!("{}/v1beta/models/{model}:streamGenerateContent?alt=sse", strip_version(&base(p)?, "/v1beta"));
    let mut resp = send(auth(http.post(url.as_str()).json(&body), p, key)).await?;
    if !resp.status().is_success() {
        return Err(http_error(resp).await);
    }
    let mut parser = SseParser::default();
    let mut blocked: Option<String> = None;
    loop {
        let Some(chunk) = next_chunk(&mut resp).await? else { break };
        for ev in parser.push(&chunk) {
            gemini_event(&ev, sink, &mut blocked).await?;
        }
    }
    for ev in parser.finish() {
        gemini_event(&ev, sink, &mut blocked).await?;
    }
    if let Some(reason) = blocked {
        if sink.text.trim().is_empty() {
            return Err(Error::Ai(format!("Gemini 拒绝回答（{reason}）")));
        }
    }
    Ok(())
}

async fn gemini_event(ev: &SseEvent, sink: &mut Sink, blocked: &mut Option<String>) -> Result<()> {
    let Ok(v) = serde_json::from_str::<Value>(ev.data.trim()) else { return Ok(()) };
    let v = match v {
        Value::Array(mut list) if !list.is_empty() => list.remove(0),
        other => other,
    };
    if let Some(err) = v.get("error") {
        return Err(Error::Ai(format!("AI 服务返回错误：{}", error_text(err))));
    }
    if let Some(reason) = v.pointer("/promptFeedback/blockReason").and_then(|r| r.as_str()) {
        *blocked = Some(reason.to_string());
    }
    let mut text = String::new();
    for part in v.pointer("/candidates/0/content/parts").and_then(|p| p.as_array()).into_iter().flatten() {
        if part["thought"] == true {
            continue;
        }
        if let Some(t) = part["text"].as_str() {
            text.push_str(t);
        }
    }
    sink.push(&text).await;
    Ok(())
}

// ---- model lists --------------------------------------------------------------------------

/// `POST /v1/ai/providers/{id}/models` for HTTP providers.
pub async fn list_models(http: &reqwest::Client, p: &AiProvider, key: Option<&str>) -> Result<Vec<AiModel>> {
    let key = key.map(str::trim).filter(|k| !k.is_empty());
    let b = base(p)?;
    let url = match p.kind {
        ProviderKind::Anthropic => format!("{}/v1/models?limit=1000", strip_version(&b, "/v1")),
        ProviderKind::Gemini => format!("{}/v1beta/models?pageSize=1000", strip_version(&b, "/v1beta")),
        ProviderKind::AzureOpenai => format!("{b}/openai/models?api-version={}", azure_version(p)),
        _ => format!("{b}/models"),
    };
    let resp = send(auth(http.get(url.as_str()).timeout(Duration::from_secs(30)), p, key)).await?;
    if !resp.status().is_success() {
        return Err(http_error(resp).await);
    }
    let v: Value = resp.json().await.map_err(|e| Error::Ai(format!("无法解析模型列表：{e}")))?;
    let mut out: Vec<AiModel> = Vec::new();
    if p.kind == ProviderKind::Gemini {
        for m in v["models"].as_array().into_iter().flatten() {
            let supported = m["supportedGenerationMethods"]
                .as_array()
                .map(|a| a.iter().any(|x| x == "generateContent"))
                .unwrap_or(true);
            let Some(name) = m["name"].as_str() else { continue };
            if !supported {
                continue;
            }
            let id = name.trim_start_matches("models/").to_string();
            let mut model = AiModel::new(&id);
            if let Some(d) = m["displayName"].as_str().filter(|d| !d.is_empty()) {
                model.name = d.to_string();
            }
            out.push(model);
        }
    } else {
        for m in v["data"].as_array().into_iter().flatten() {
            let Some(id) = m["id"].as_str().filter(|i| !i.is_empty()) else { continue };
            let mut model = AiModel::new(id);
            if let Some(d) = m["display_name"].as_str().filter(|d| !d.is_empty()) {
                model.name = d.to_string();
            }
            out.push(model);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut p = SseParser::default();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(p.push(c));
        }
        out.extend(p.finish());
        out
    }

    #[test]
    fn sse_handles_any_chunk_boundary() {
        let stream = "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"你好\"}}\r\n\r\n: ping\n\ndata: {\"a\":1}\ndata: [DONE]\n\n".as_bytes();
        let whole = feed(&[stream]);
        assert_eq!(whole.len(), 3, "{whole:?}");
        assert_eq!(whole[0].event.as_deref(), Some("content_block_delta"));
        assert!(whole[0].data.contains("你好"));
        assert_eq!(whole[1].data, "{\"a\":1}");
        assert_eq!(whole[2].data, "[DONE]");
        // Every split position, including inside the multi-byte characters.
        for i in 1..stream.len() {
            let (a, b) = stream.split_at(i);
            assert_eq!(feed(&[a, b]), whole, "split at {i}");
        }
        // Byte by byte.
        let bytes: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(feed(&bytes), whole);
    }

    #[test]
    fn sse_multiline_data_and_missing_trailing_newline() {
        let ev = feed(&[b"data: line1\ndata: line2\n\ndata: {\"x\":2}"]);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].data, "line1\nline2");
        assert_eq!(ev[1].data, "{\"x\":2}");
    }

    #[test]
    fn http_errors_are_readable() {
        assert!(status_error(401, "bad key").to_string().starts_with("API Key 无效或没有权限"));
        assert!(status_error(429, "").to_string().starts_with("请求太频繁或额度不足"));
        let v: Value = serde_json::from_str(r#"{"error":{"message":"model not found","type":"x"}}"#).unwrap();
        assert_eq!(error_detail(&v).as_deref(), Some("model not found"));
        let v: Value = serde_json::from_str(r#"[{"error":{"code":400,"message":"bad"}}]"#).unwrap();
        assert_eq!(error_detail(&v).as_deref(), Some("bad"));
    }

    #[test]
    fn system_messages_are_merged() {
        let req = ChatRequest {
            system: Some("A".into()),
            messages: vec![
                ChatMessage { role: "system".into(), content: "B".into() },
                ChatMessage::user("hi"),
            ],
            ..Default::default()
        };
        let (system, msgs) = split_system(&req);
        assert_eq!(system.as_deref(), Some("A\n\nB"));
        assert_eq!(msgs.len(), 1);
    }
}

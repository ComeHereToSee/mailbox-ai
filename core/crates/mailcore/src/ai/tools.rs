//! Chats with external MCP tools (`tools: true` on `/v1/ai/chat` and `/v1/ai/ask`).
//! - OpenAI compatible / Azure / Anthropic / Gemini: a non-streaming loop — the model asks
//!   for tools, the MCP servers run them, the results go back — at most 8 rounds; the final
//!   text is sent as one delta.
//! - Claude Code: the servers are handed over as `--mcp-config` (`--allowedTools
//!   mcp__<server>`, `--max-turns 8`).
//! - Codex: not supported; `done.tools_note` says so.
//!
//! Tool names are `<server>__<tool>` with only `[A-Za-z0-9_-]`, at most 64 characters.

use super::client::{self, Keys};
use super::mcp_client::McpClient;
use super::model::*;
use super::{DeltaTx, StreamDone, cli, truncate_plain};
use crate::Engine;
use crate::error::{Error, Result};
use serde_json::{Map, Value, json};
use std::time::Instant;

pub const MAX_ROUNDS: usize = 8;
pub const CODEX_NOTE: &str = "Codex 暂不支持外部工具";
const MAX_NAME: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

/// One step of a tool conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum Turn {
    User(String),
    Assistant { text: String, calls: Vec<ToolCall> },
    Results(Vec<ToolOutput>),
}

/// Only `[A-Za-z0-9_-]`, at most `max` characters.
pub fn sanitize_name(s: &str, max: usize) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(max).collect()
}

/// Config keys of the servers (`<server>` in tool names, Claude's `mcpServers` keys): the
/// sanitized name, or the id when the name has no usable characters; unique, starting with
/// a letter.
pub fn server_keys(servers: &[McpServerConfig]) -> Vec<String> {
    let mut used: Vec<String> = Vec::new();
    for s in servers {
        let mut key = sanitize_name(&s.name, 32);
        if key.is_empty() {
            key = sanitize_name(&s.id, 32);
        }
        if key.is_empty() {
            key = "server".to_string();
        }
        if !key.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            key = format!("s{key}");
        }
        let base = key.clone();
        let mut n = 2;
        while used.contains(&key) {
            key = format!("{base}{n}");
            n += 1;
        }
        used.push(key);
    }
    used
}

/// `<key>__<tool>`, sanitized, unique among `used`.
pub fn tool_name(key: &str, tool: &str, used: &[String]) -> String {
    let base = sanitize_name(&format!("{key}__{tool}"), MAX_NAME);
    let mut name = base.clone();
    let mut n = 2;
    while used.contains(&name) {
        let suffix = format!("_{n}");
        let keep: String = base.chars().take(MAX_NAME - suffix.len()).collect();
        name = format!("{keep}{suffix}");
        n += 1;
    }
    name
}

/// `{"mcpServers": {...}}` for `claude --mcp-config`.
pub fn claude_mcp_config(servers: &[McpServerConfig], keys: &[String]) -> Value {
    let mut map = Map::new();
    for (s, key) in servers.iter().zip(keys) {
        let v = if s.transport == "http" {
            json!({ "type": "http", "url": s.url, "headers": s.headers })
        } else {
            let command = cli::expand(&s.command).to_string_lossy().to_string();
            let args: Vec<String> = s.args.iter().map(|a| cli::expand(a).to_string_lossy().to_string()).collect();
            json!({ "type": "stdio", "command": command, "args": args, "env": s.env })
        };
        map.insert(key.clone(), v);
    }
    json!({ "mcpServers": Value::Object(map) })
}

// ---- provider formats ---------------------------------------------------------------------

/// Arguments of a call: a JSON object (OpenAI sends them as a string).
fn parse_args(v: &Value) -> Value {
    match v {
        Value::Object(_) => v.clone(),
        Value::String(s) if s.trim().is_empty() => json!({}),
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(x) if x.is_object() => x,
            _ => json!({ "input": s }),
        },
        _ => json!({}),
    }
}

pub fn openai_body(model: &str, system: &str, turns: &[Turn], tools: &[ToolSpec]) -> Value {
    let mut messages: Vec<Value> = vec![json!({ "role": "system", "content": system })];
    for t in turns {
        match t {
            Turn::User(text) => messages.push(json!({ "role": "user", "content": text })),
            Turn::Assistant { text, calls } => {
                let content = if text.is_empty() { Value::Null } else { json!(text) };
                let mut m = json!({ "role": "assistant", "content": content });
                if !calls.is_empty() {
                    let list: Vec<Value> = calls
                        .iter()
                        .map(|c| json!({ "id": c.id, "type": "function", "function": { "name": c.name, "arguments": c.arguments.to_string() } }))
                        .collect();
                    m["tool_calls"] = Value::Array(list);
                }
                messages.push(m);
            }
            Turn::Results(results) => {
                for r in results {
                    messages.push(json!({ "role": "tool", "tool_call_id": r.call_id, "content": r.content }));
                }
            }
        }
    }
    let mut body = json!({ "model": model, "messages": messages, "stream": false });
    if !tools.is_empty() {
        let list: Vec<Value> = tools
            .iter()
            .map(|t| json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.schema } }))
            .collect();
        body["tools"] = Value::Array(list);
    }
    body
}

pub fn parse_openai(v: &Value) -> (String, Vec<ToolCall>, TokenUsage) {
    let msg = &v["choices"][0]["message"];
    let text = msg["content"].as_str().unwrap_or("").to_string();
    let mut calls = Vec::new();
    for (i, c) in msg["tool_calls"].as_array().into_iter().flatten().enumerate() {
        let Some(name) = c["function"]["name"].as_str().filter(|n| !n.is_empty()) else { continue };
        let id = c["id"].as_str().filter(|x| !x.is_empty()).map(String::from).unwrap_or_else(|| format!("call_{i}"));
        calls.push(ToolCall { id, name: name.to_string(), arguments: parse_args(&c["function"]["arguments"]) });
    }
    let usage = TokenUsage {
        input_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
    };
    (text, calls, usage)
}

pub fn anthropic_body(model: &str, system: &str, turns: &[Turn], tools: &[ToolSpec]) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for t in turns {
        match t {
            Turn::User(text) => messages.push(json!({ "role": "user", "content": text })),
            Turn::Assistant { text, calls } => {
                let mut content: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    content.push(json!({ "type": "text", "text": text }));
                }
                for c in calls {
                    content.push(json!({ "type": "tool_use", "id": c.id, "name": c.name, "input": c.arguments }));
                }
                if content.is_empty() {
                    content.push(json!({ "type": "text", "text": "…" }));
                }
                messages.push(json!({ "role": "assistant", "content": content }));
            }
            Turn::Results(results) => {
                let content: Vec<Value> = results
                    .iter()
                    .map(|r| json!({ "type": "tool_result", "tool_use_id": r.call_id, "content": r.content, "is_error": r.is_error }))
                    .collect();
                messages.push(json!({ "role": "user", "content": content }));
            }
        }
    }
    let mut body = json!({ "model": model, "max_tokens": 4096, "system": system, "messages": messages });
    if !tools.is_empty() {
        let list: Vec<Value> =
            tools.iter().map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.schema })).collect();
        body["tools"] = Value::Array(list);
    }
    body
}

pub fn parse_anthropic(v: &Value) -> (String, Vec<ToolCall>, TokenUsage) {
    let mut text = String::new();
    let mut calls = Vec::new();
    for (i, part) in v["content"].as_array().into_iter().flatten().enumerate() {
        match part["type"].as_str().unwrap_or("") {
            "text" => text.push_str(part["text"].as_str().unwrap_or("")),
            "tool_use" => {
                let Some(name) = part["name"].as_str().filter(|n| !n.is_empty()) else { continue };
                let id = part["id"].as_str().filter(|x| !x.is_empty()).map(String::from).unwrap_or_else(|| format!("toolu_{i}"));
                calls.push(ToolCall { id, name: name.to_string(), arguments: parse_args(&part["input"]) });
            }
            _ => {}
        }
    }
    let n = |k: &str| v["usage"][k].as_u64().unwrap_or(0);
    let usage = TokenUsage {
        input_tokens: n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens"),
        output_tokens: n("output_tokens"),
    };
    (text, calls, usage)
}

/// The JSON Schema subset Gemini accepts (`$schema`, `additionalProperties` … removed,
/// `"type": ["string", "null"]` → `nullable`).
pub fn gemini_schema(v: &Value) -> Value {
    const KEEP: &[&str] =
        &["type", "description", "properties", "items", "required", "enum", "format", "nullable", "minimum", "maximum", "minItems", "maxItems", "anyOf"];
    let Some(obj) = v.as_object() else { return v.clone() };
    let mut out = Map::new();
    for (k, val) in obj {
        if !KEEP.contains(&k.as_str()) {
            continue;
        }
        let cleaned = match k.as_str() {
            "properties" => match val.as_object() {
                Some(props) => Value::Object(props.iter().map(|(pk, pv)| (pk.clone(), gemini_schema(pv))).collect()),
                None => continue,
            },
            "items" => gemini_schema(val),
            "anyOf" => Value::Array(val.as_array().into_iter().flatten().map(gemini_schema).collect()),
            "type" => match val {
                Value::Array(types) => {
                    if types.iter().any(|t| t == "null") {
                        out.insert("nullable".to_string(), json!(true));
                    }
                    json!(types.iter().filter_map(|t| t.as_str()).find(|t| *t != "null").unwrap_or("string"))
                }
                other => other.clone(),
            },
            _ => val.clone(),
        };
        out.insert(k.clone(), cleaned);
    }
    Value::Object(out)
}

pub fn gemini_body(system: &str, turns: &[Turn], tools: &[ToolSpec]) -> Value {
    let mut contents: Vec<Value> = Vec::new();
    for t in turns {
        match t {
            Turn::User(text) => contents.push(json!({ "role": "user", "parts": [{ "text": text }] })),
            Turn::Assistant { text, calls } => {
                let mut parts: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    parts.push(json!({ "text": text }));
                }
                for c in calls {
                    parts.push(json!({ "functionCall": { "name": c.name, "args": c.arguments } }));
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            }
            Turn::Results(results) => {
                let parts: Vec<Value> =
                    results.iter().map(|r| json!({ "functionResponse": { "name": r.name, "response": { "result": r.content } } })).collect();
                contents.push(json!({ "role": "user", "parts": parts }));
            }
        }
    }
    let mut body = json!({ "contents": contents, "systemInstruction": { "parts": [{ "text": system }] } });
    if !tools.is_empty() {
        let mut decls: Vec<Value> = Vec::new();
        for t in tools {
            let mut d = json!({ "name": t.name, "description": t.description });
            // Gemini rejects OBJECT parameters without properties: leave them out then.
            if t.schema["properties"].as_object().is_some_and(|p| !p.is_empty()) {
                d["parameters"] = gemini_schema(&t.schema);
            }
            decls.push(d);
        }
        body["tools"] = json!([{ "functionDeclarations": decls }]);
    }
    body
}

pub fn parse_gemini(v: &Value) -> (String, Vec<ToolCall>, TokenUsage) {
    let mut text = String::new();
    let mut calls = Vec::new();
    for part in v.pointer("/candidates/0/content/parts").and_then(|p| p.as_array()).into_iter().flatten() {
        if part["thought"] == true {
            continue;
        }
        if let Some(t) = part["text"].as_str() {
            text.push_str(t);
        }
        if let Some(name) = part["functionCall"]["name"].as_str().filter(|n| !n.is_empty()) {
            let id = format!("call_{}", calls.len());
            calls.push(ToolCall { id, name: name.to_string(), arguments: parse_args(&part["functionCall"]["args"]) });
        }
    }
    let n = |k: &str| v["usageMetadata"][k].as_u64().unwrap_or(0);
    let usage = TokenUsage { input_tokens: n("promptTokenCount"), output_tokens: n("candidatesTokenCount") + n("thoughtsTokenCount") };
    (text, calls, usage)
}

// ---- the tool box -------------------------------------------------------------------------

struct Entry {
    spec: ToolSpec,
    client: usize,
    tool: String,
}

/// The connected servers of one conversation and their tools.
pub(crate) struct ToolBox {
    clients: Vec<McpClient>,
    /// (display name, id) per client.
    servers: Vec<(String, String)>,
    entries: Vec<Entry>,
    /// Servers that could not be used: "name（error）".
    pub failures: Vec<String>,
}

impl ToolBox {
    pub async fn open(e: &Engine, servers: &[McpServerConfig]) -> ToolBox {
        let keys = server_keys(servers);
        let mut tb = ToolBox { clients: Vec::new(), servers: Vec::new(), entries: Vec::new(), failures: Vec::new() };
        let mut used: Vec<String> = Vec::new();
        for (cfg, key) in servers.iter().zip(keys.iter()) {
            let mut c = match McpClient::connect(e, cfg).await {
                Ok(c) => c,
                Err(err) => {
                    tb.failures.push(format!("{}（{err}）", cfg.name));
                    continue;
                }
            };
            let tools = match c.list_tools().await {
                Ok(t) => t,
                Err(err) => {
                    tb.failures.push(format!("{}（{err}）", cfg.name));
                    c.close().await;
                    continue;
                }
            };
            let index = tb.clients.len();
            for t in tools {
                let name = tool_name(key, &t.name, &used);
                used.push(name.clone());
                tb.entries.push(Entry {
                    spec: ToolSpec { name, description: truncate_plain(&t.description, 1000), schema: t.input_schema },
                    client: index,
                    tool: t.name,
                });
            }
            tb.clients.push(c);
            tb.servers.push((cfg.name.clone(), cfg.id.clone()));
        }
        tb
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.entries.iter().map(|x| x.spec.clone()).collect()
    }

    /// Runs one call of the model.
    pub async fn call(&mut self, call: &ToolCall) -> (ToolOutput, Option<ToolCallRecord>) {
        let found = self.entries.iter().find(|x| x.spec.name == call.name).map(|x| (x.client, x.tool.clone()));
        let Some((index, tool)) = found else {
            let out = ToolOutput { call_id: call.id.clone(), name: call.name.clone(), content: format!("没有名为 {} 的工具", call.name), is_error: true };
            return (out, None);
        };
        let (server, server_id) = self.servers.get(index).cloned().unwrap_or_default();
        let result = match self.clients.get_mut(index) {
            Some(c) => c.call_tool(&tool, call.arguments.clone()).await,
            None => Err(Error::Ai("MCP 服务器不可用".into())),
        };
        let (content, ok) = match result {
            Ok((text, ok)) => (if text.trim().is_empty() { "（没有输出）".to_string() } else { text }, ok),
            Err(err) => (format!("工具调用失败：{err}"), false),
        };
        let out = ToolOutput { call_id: call.id.clone(), name: call.name.clone(), content, is_error: !ok };
        (out, Some(ToolCallRecord { server, server_id, tool, ok }))
    }

    pub async fn close(self) {
        for c in self.clients {
            c.close().await;
        }
    }
}

impl Engine {
    /// One non-streaming request of the tool loop (usage counted as "mcp").
    async fn ai_tool_turn(&self, p: &AiProvider, model: &ModelRef, system: &str, turns: &[Turn], tools: &[ToolSpec]) -> Result<(String, Vec<ToolCall>)> {
        let keys = self.ai_key_list(&p.id);
        let start = self.ai_next_key(&p.id);
        let url = client::chat_url(p, &model.model, false)?;
        let body = match p.kind {
            ProviderKind::Anthropic => anthropic_body(&model.model, system, turns, tools),
            ProviderKind::Gemini => gemini_body(system, turns, tools),
            _ => openai_body(&model.model, system, turns, tools),
        };
        let started = Instant::now();
        let r = client::post_json(&self.ai.http, p, Keys::new(&keys, start), &url, &body).await;
        let ms = started.elapsed().as_millis() as u64;
        match r {
            Ok(v) => {
                let (text, calls, usage) = match p.kind {
                    ProviderKind::Anthropic => parse_anthropic(&v),
                    ProviderKind::Gemini => parse_gemini(&v),
                    _ => parse_openai(&v),
                };
                self.ai_record_usage(&p.id, &model.model, "mcp", usage, ms, true);
                Ok((text, calls))
            }
            Err(err) => {
                self.ai_record_usage(&p.id, &model.model, "mcp", TokenUsage::default(), ms, false);
                Err(err)
            }
        }
    }

    /// A chat that may use the enabled external MCP servers. `done` carries `tool_calls`
    /// (and `tools_note` for Codex, or when servers could not be reached).
    pub async fn ai_tool_chat(&self, model: ModelRef, system: String, messages: Vec<ChatMessage>, feature: &str, tx: DeltaTx) -> Result<StreamDone> {
        let label = model.label();
        let provider = self.ai_provider(&model.provider).ok_or_else(|| Error::AiNotConfigured(format!("找不到 AI 服务商 {}", model.provider)))?;
        let servers: Vec<McpServerConfig> = self.ai_mcp_servers().into_iter().filter(|s| s.enabled).collect();

        if provider.kind == ProviderKind::Codex || servers.is_empty() {
            let req = ChatRequest { model, system: Some(system), messages, feature: feature.to_string(), ..Default::default() };
            let text = self.ai_chat(req, tx).await?;
            let mut done = StreamDone::new(text, label).with("tool_calls", json!([]));
            if provider.kind == ProviderKind::Codex {
                done = done.with("tools_note", json!(CODEX_NOTE));
            }
            return Ok(done);
        }

        if provider.kind == ProviderKind::ClaudeCode {
            let keys = server_keys(&servers);
            let config = claude_mcp_config(&servers, &keys);
            let file = self.paths.tmp().join(format!("mcp-{}.json", uuid::Uuid::new_v4().simple()));
            crate::paths::write_private(&file, config.to_string().as_bytes())?;
            let tools = cli::ClaudeTools {
                config_file: file.clone(),
                servers: servers.iter().zip(keys).map(|(s, k)| (k, s.name.clone(), s.id.clone())).collect(),
            };
            let req = ChatRequest { model, system: Some(system), messages, feature: feature.to_string(), ..Default::default() };
            let started = Instant::now();
            let result = cli::claude_chat(self, &provider, &req, tx, Some(&tools)).await;
            let _ = std::fs::remove_file(&file);
            let ms = started.elapsed().as_millis() as u64;
            // Counted like the HTTP tool loop.
            return match result {
                Ok(out) => {
                    self.ai_record_usage(&provider.id, &req.model.model, "mcp", out.usage, ms, true);
                    Ok(StreamDone::new(out.text, label).with("tool_calls", json!(out.tool_calls)))
                }
                Err(err) => {
                    self.ai_record_usage(&provider.id, &req.model.model, "mcp", TokenUsage::default(), ms, false);
                    Err(err)
                }
            };
        }

        // HTTP providers: the tool loop.
        let mut tb = ToolBox::open(self, &servers).await;
        let note = if tb.failures.is_empty() { None } else { Some(format!("无法使用 MCP 服务器：{}", tb.failures.join("；"))) };
        let specs = tb.specs();
        if specs.is_empty() {
            tb.close().await;
            let req = ChatRequest { model, system: Some(system), messages, feature: feature.to_string(), ..Default::default() };
            let text = self.ai_chat(req, tx).await?;
            let mut done = StreamDone::new(text, label).with("tool_calls", json!([]));
            if let Some(n) = note {
                done = done.with("tools_note", json!(n));
            }
            return Ok(done);
        }
        let mut system = system;
        let mut turns: Vec<Turn> = Vec::new();
        for m in messages {
            if m.content.trim().is_empty() {
                continue;
            }
            match m.role.as_str() {
                "system" => {
                    system.push_str("\n\n");
                    system.push_str(&m.content);
                }
                "assistant" => turns.push(Turn::Assistant { text: m.content, calls: Vec::new() }),
                _ => turns.push(Turn::User(m.content)),
            }
        }
        let mut records: Vec<ToolCallRecord> = Vec::new();
        let mut answer: Option<String> = None;
        for _ in 0..MAX_ROUNDS {
            let (text, calls) = match self.ai_tool_turn(&provider, &model, &system, &turns, &specs).await {
                Ok(x) => x,
                Err(err) => {
                    tb.close().await;
                    return Err(err);
                }
            };
            if calls.is_empty() {
                answer = Some(text);
                break;
            }
            let mut outputs: Vec<ToolOutput> = Vec::new();
            for c in &calls {
                let (out, record) = tb.call(c).await;
                if let Some(r) = record {
                    records.push(r);
                }
                outputs.push(out);
            }
            turns.push(Turn::Assistant { text, calls });
            turns.push(Turn::Results(outputs));
        }
        tb.close().await;
        let text = match answer {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            Some(_) => return Err(Error::Ai("AI 没有返回任何内容".into())),
            None => format!("（已经调用了 {MAX_ROUNDS} 轮工具，仍没有得到最终回答，已停止）"),
        };
        if let Some(tx) = &tx {
            let _ = tx.send(text.clone()).await;
        }
        let mut done = StreamDone::new(text, label).with("tool_calls", json!(records));
        if let Some(n) = note {
            done = done.with("tools_note", json!(n));
        }
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(id: &str, name: &str) -> McpServerConfig {
        McpServerConfig { id: id.into(), name: name.into(), ..Default::default() }
    }

    #[test]
    fn names_are_safe_and_unique() {
        assert_eq!(sanitize_name("Fake Stdio!", 64), "FakeStdio");
        let keys = server_keys(&[server("a1", "文件 系统"), server("9f", "Notion"), server("x", "Notion"), server("", "")]);
        assert_eq!(keys, vec!["a1", "Notion", "Notion2", "server"]);
        assert_eq!(server_keys(&[server("9f", "")]), vec!["s9f"]);
        let used = vec!["Notion__search".to_string()];
        assert_eq!(tool_name("Notion", "search", &[]), "Notion__search");
        assert_eq!(tool_name("Notion", "search", &used), "Notion__search_2");
        let long = tool_name("k", &"x".repeat(100), &[]);
        assert_eq!(long.chars().count(), 64);
        let longer = tool_name("k", &"x".repeat(100), &[long.clone()]);
        assert_eq!(longer.chars().count(), 64);
        assert!(longer.ends_with("_2"));
    }

    #[test]
    fn provider_formats_round_trip() {
        let tools = vec![ToolSpec {
            name: "files__read".into(),
            description: "读文件".into(),
            schema: json!({"type": "object", "$schema": "x", "additionalProperties": false,
                           "properties": {"path": {"type": ["string", "null"], "description": "路径"}}, "required": ["path"]}),
        }];
        let turns = vec![
            Turn::User("读一下".into()),
            Turn::Assistant { text: String::new(), calls: vec![ToolCall { id: "c1".into(), name: "files__read".into(), arguments: json!({"path": "/a"}) }] },
            Turn::Results(vec![ToolOutput { call_id: "c1".into(), name: "files__read".into(), content: "内容".into(), is_error: false }]),
        ];
        let o = openai_body("m", "S", &turns, &tools);
        assert_eq!(o["messages"][0]["role"], "system");
        assert_eq!(o["messages"][2]["tool_calls"][0]["function"]["arguments"], "{\"path\":\"/a\"}");
        assert!(o["messages"][2]["content"].is_null());
        assert_eq!(o["messages"][3]["role"], "tool");
        assert_eq!(o["tools"][0]["function"]["name"], "files__read");
        assert_eq!(o["stream"], false);
        let a = anthropic_body("m", "S", &turns, &tools);
        assert_eq!(a["system"], "S");
        assert_eq!(a["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(a["messages"][2]["content"][0]["tool_use_id"], "c1");
        assert_eq!(a["tools"][0]["input_schema"]["required"][0], "path");
        let g = gemini_body("S", &turns, &tools);
        assert_eq!(g["contents"][1]["role"], "model");
        assert_eq!(g["contents"][1]["parts"][0]["functionCall"]["args"]["path"], "/a");
        assert_eq!(g["contents"][2]["parts"][0]["functionResponse"]["response"]["result"], "内容");
        let params = &g["tools"][0]["functionDeclarations"][0]["parameters"];
        assert!(params.get("$schema").is_none() && params.get("additionalProperties").is_none());
        assert_eq!(params["properties"]["path"]["type"], "string");
        assert_eq!(params["properties"]["path"]["nullable"], true);

        let v = json!({"choices": [{"message": {"content": null, "tool_calls": [{"id": "x", "type": "function",
                       "function": {"name": "files__read", "arguments": "{\"path\":\"/b\"}"}}]}}], "usage": {"prompt_tokens": 5, "completion_tokens": 2}});
        let (text, calls, usage) = parse_openai(&v);
        assert!(text.is_empty());
        assert_eq!(calls[0].arguments["path"], "/b");
        assert_eq!(usage, TokenUsage { input_tokens: 5, output_tokens: 2 });
        let v = json!({"content": [{"type": "text", "text": "我来读"}, {"type": "tool_use", "id": "t", "name": "files__read", "input": {"path": "/c"}}],
                       "usage": {"input_tokens": 3, "output_tokens": 1}});
        let (text, calls, _) = parse_anthropic(&v);
        assert_eq!(text, "我来读");
        assert_eq!((calls[0].id.as_str(), calls[0].arguments["path"].as_str()), ("t", Some("/c")));
        let v = json!({"candidates": [{"content": {"parts": [{"functionCall": {"name": "files__read", "args": {"path": "/d"}}}]}}]});
        let (_, calls, _) = parse_gemini(&v);
        assert_eq!(calls[0].id, "call_0");
        assert_eq!(parse_args(&json!("not json")), json!({"input": "not json"}));
    }

    #[test]
    fn claude_config() {
        let mut http = server("h", "Web");
        http.transport = "http".into();
        http.url = "http://127.0.0.1:9/mcp".into();
        let mut stdio = server("s", "Files");
        stdio.command = "npx".into();
        stdio.args = vec!["-y".into(), "pkg".into()];
        let keys = server_keys(&[http.clone(), stdio.clone()]);
        let c = claude_mcp_config(&[http, stdio], &keys);
        assert_eq!(c["mcpServers"]["Web"]["type"], "http");
        assert_eq!(c["mcpServers"]["Files"]["command"], "npx");
        assert_eq!(c["mcpServers"]["Files"]["args"][1], "pkg");
    }
}

//! External MCP servers (kv space `ai_mcp_server`) and a small client for them:
//! - stdio: a child process (PATH as for the subscription CLIs) speaking line-delimited
//!   JSON-RPC on stdin / stdout;
//! - http (Streamable HTTP): JSON-RPC POSTs answered with JSON or `text/event-stream`
//!   (the event with our id is taken); the `Mcp-Session-Id` header is kept.
//!
//! `initialize` comes first, then `notifications/initialized`. Every call times out after
//! 60 seconds. Connections live for one conversation only (no resident processes).

use super::client::SseParser;
use super::model::McpServerConfig;
use super::{cli, truncate_plain};
use crate::Engine;
use crate::error::{Error, Result};
use crate::store::new_id;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const SPACE: &str = "ai_mcp_server";
/// Protocol version this client asks for.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Tool results longer than this are cut (they go back to the model).
const MAX_RESULT_CHARS: usize = 20_000;

#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

enum Transport {
    Stdio {
        child: Child,
        stdin: ChildStdin,
        lines: tokio::io::Lines<BufReader<ChildStdout>>,
        /// The last few KB of stderr, for error messages.
        stderr: Arc<Mutex<String>>,
    },
    Http {
        http: reqwest::Client,
        url: String,
        headers: BTreeMap<String, String>,
        session: Option<String>,
        protocol: Option<String>,
    },
}

/// A connected (and initialised) MCP server.
pub struct McpClient {
    transport: Transport,
    next_id: i64,
    pub name: String,
}

fn id_matches(v: &Value, id: i64) -> bool {
    v.as_i64() == Some(id) || v.as_str().is_some_and(|s| s == id.to_string())
}

/// Text of a `tools/call` result (text parts; other parts as short markers).
pub fn content_text(result: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in result["content"].as_array().into_iter().flatten() {
        match c["type"].as_str().unwrap_or("") {
            "text" => parts.push(c["text"].as_str().unwrap_or("").to_string()),
            "image" => parts.push("[图片]".to_string()),
            "audio" => parts.push("[音频]".to_string()),
            "resource" => match c["resource"]["text"].as_str() {
                Some(t) => parts.push(t.to_string()),
                None => parts.push(format!("[资源 {}]", c["resource"]["uri"].as_str().unwrap_or(""))),
            },
            "resource_link" => parts.push(format!("[链接 {}]", c["uri"].as_str().unwrap_or(""))),
            _ => parts.push(c.to_string()),
        }
    }
    if parts.is_empty() {
        if let Some(sc) = result.get("structuredContent").filter(|v| !v.is_null()) {
            return sc.to_string();
        }
    }
    parts.join("\n")
}

/// A bare command name is looked up in the same directories as the CLIs.
fn resolve_command(command: &str) -> PathBuf {
    if command.contains('/') {
        return cli::expand(command);
    }
    for dir in cli::search_path(None).split(':') {
        let p = Path::new(dir).join(command);
        if cli::is_exec(&p) {
            return p;
        }
    }
    PathBuf::from(command)
}

fn spawn_stdio(cfg: &McpServerConfig) -> Result<Transport> {
    let command = cfg.command.trim();
    if command.is_empty() {
        return Err(Error::Invalid("请填写 MCP 服务器的启动命令".into()));
    }
    let exe = resolve_command(command);
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"));
    let mut cmd = Command::new(&exe);
    cmd.args(cfg.args.iter().map(|a| cli::expand(a)))
        .env("PATH", cli::search_path(exe.parent()))
        .envs(cfg.env.iter())
        .current_dir(&home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|err| Error::Ai(format!("无法启动 MCP 服务器 {}：{err}", exe.display())))?;
    let stdin = child.stdin.take().ok_or_else(|| Error::Ai("无法写入 MCP 服务器".into()))?;
    let stdout = child.stdout.take().ok_or_else(|| Error::Ai("无法读取 MCP 服务器的输出".into()))?;
    let stderr = Arc::new(Mutex::new(String::new()));
    if let Some(mut pipe) = child.stderr.take() {
        let buf = stderr.clone();
        tokio::spawn(async move {
            let mut chunk = [0u8; 2048];
            loop {
                let n = match pipe.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let text = String::from_utf8_lossy(&chunk[..n]).to_string();
                let mut b = buf.lock();
                b.push_str(&text);
                if b.chars().count() > 8000 {
                    let keep: Vec<char> = b.chars().rev().take(4000).collect();
                    *b = keep.into_iter().rev().collect();
                }
            }
        });
    }
    Ok(Transport::Stdio { child, stdin, lines: BufReader::new(stdout).lines(), stderr })
}

fn http_request(
    http: &reqwest::Client,
    url: &str,
    headers: &BTreeMap<String, String>,
    session: Option<&str>,
    protocol: Option<&str>,
    body: String,
) -> reqwest::RequestBuilder {
    let mut b = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
        .body(body);
    for (k, v) in headers {
        b = b.header(k.as_str(), v.as_str());
    }
    if let Some(s) = session {
        b = b.header("Mcp-Session-Id", s);
    }
    if let Some(p) = protocol {
        b = b.header("MCP-Protocol-Version", p);
    }
    b
}

fn last_lines(s: &str) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let start = lines.len().saturating_sub(3);
    truncate_plain(&lines[start..].join(" "), 300)
}

impl McpClient {
    /// Starts / reaches the server and runs the `initialize` handshake.
    pub async fn connect(e: &Engine, cfg: &McpServerConfig) -> Result<McpClient> {
        let transport = if cfg.transport == "http" {
            let url = cfg.url.trim().to_string();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(Error::Invalid("MCP 服务器地址应以 http:// 或 https:// 开头".into()));
            }
            Transport::Http { http: e.ai.http.clone(), url, headers: cfg.headers.clone(), session: None, protocol: None }
        } else {
            spawn_stdio(cfg)?
        };
        let name = if cfg.name.trim().is_empty() { cfg.id.clone() } else { cfg.name.clone() };
        let mut c = McpClient { transport, next_id: 1, name };
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "mailbox-ai", "version": env!("CARGO_PKG_VERSION") },
        });
        let init = c.request("initialize", params).await?;
        let version = init["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION).to_string();
        if let Transport::Http { protocol, .. } = &mut c.transport {
            *protocol = Some(version);
        }
        c.notify("notifications/initialized", json!({})).await?;
        Ok(c)
    }

    /// One JSON-RPC request → its `result` (an `error` becomes `Err`).
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let outcome = tokio::time::timeout(CALL_TIMEOUT, self.exchange(msg, id)).await;
        let resp = match outcome {
            Err(_) => return Err(Error::Ai(format!("MCP 服务器 {} 超过 60 秒没有响应（{method}）", self.name))),
            Ok(r) => r?,
        };
        if let Some(err) = resp.get("error").filter(|e| !e.is_null()) {
            let message = err["message"].as_str().map(String::from).unwrap_or_else(|| err.to_string());
            return Err(Error::Ai(format!("MCP 服务器 {} 返回错误：{message}", self.name)));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Sends a request and waits for the response with the same id.
    async fn exchange(&mut self, msg: Value, id: i64) -> Result<Value> {
        let name = self.name.clone();
        match &mut self.transport {
            Transport::Stdio { stdin, lines, stderr, .. } => {
                let mut line = serde_json::to_string(&msg)?;
                line.push('\n');
                stdin.write_all(line.as_bytes()).await.map_err(|e| Error::Ai(format!("无法写入 MCP 服务器 {name}：{e}")))?;
                stdin.flush().await?;
                loop {
                    let next = lines.next_line().await.map_err(|e| Error::Ai(format!("读取 MCP 服务器 {name} 的输出失败：{e}")))?;
                    let Some(text) = next else {
                        let tail = last_lines(&stderr.lock());
                        let detail = if tail.is_empty() { String::new() } else { format!("：{tail}") };
                        return Err(Error::Ai(format!("MCP 服务器 {name} 已退出{detail}")));
                    };
                    let Ok(v) = serde_json::from_str::<Value>(text.trim()) else { continue };
                    if v.get("method").is_some() {
                        // A request or notification of the server: requests get "not supported".
                        if let Some(req_id) = v.get("id").filter(|x| !x.is_null()) {
                            let reply = json!({ "jsonrpc": "2.0", "id": req_id, "error": { "code": -32601, "message": "Method not supported" } });
                            let mut out = reply.to_string();
                            out.push('\n');
                            let _ = stdin.write_all(out.as_bytes()).await;
                            let _ = stdin.flush().await;
                        }
                        continue;
                    }
                    if id_matches(&v["id"], id) {
                        return Ok(v);
                    }
                }
            }
            Transport::Http { http, url, headers, session, protocol } => {
                let b = http_request(http, url, headers, session.as_deref(), protocol.as_deref(), msg.to_string());
                let mut resp = b.send().await.map_err(|e| Error::Net(format!("无法连接 MCP 服务器 {name}：{e}")))?;
                if let Some(s) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
                    *session = Some(s.to_string());
                }
                let status = resp.status().as_u16();
                if !resp.status().is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    return Err(Error::Ai(format!("MCP 服务器 {name} 返回错误 {status}：{}", truncate_plain(text.trim(), 300))));
                }
                let sse = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.contains("text/event-stream"));
                if !sse {
                    let text = resp.text().await?;
                    let v: Value = serde_json::from_str(&text).map_err(|_| Error::Ai(format!("MCP 服务器 {name} 返回的不是 JSON")))?;
                    if let Value::Array(list) = &v {
                        if let Some(m) = list.iter().find(|m| id_matches(&m["id"], id)) {
                            return Ok(m.clone());
                        }
                    }
                    return Ok(v);
                }
                let mut parser = SseParser::default();
                loop {
                    let chunk = resp.chunk().await.map_err(|e| Error::Net(format!("读取 MCP 服务器 {name} 的响应失败：{e}")))?;
                    let events = match &chunk {
                        Some(c) => parser.push(c),
                        None => parser.finish(),
                    };
                    for ev in events {
                        if let Ok(v) = serde_json::from_str::<Value>(ev.data.trim()) {
                            if v.get("method").is_none() && id_matches(&v["id"], id) {
                                return Ok(v);
                            }
                        }
                    }
                    if chunk.is_none() {
                        return Err(Error::Ai(format!("MCP 服务器 {name} 没有返回结果")));
                    }
                }
            }
        }
    }

    /// A notification (no answer expected).
    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        match &mut self.transport {
            Transport::Stdio { stdin, .. } => {
                let mut line = msg.to_string();
                line.push('\n');
                stdin.write_all(line.as_bytes()).await.map_err(|e| Error::Ai(format!("无法写入 MCP 服务器：{e}")))?;
                stdin.flush().await?;
            }
            Transport::Http { http, url, headers, session, protocol } => {
                let b = http_request(http, url, headers, session.as_deref(), protocol.as_deref(), msg.to_string());
                match tokio::time::timeout(CALL_TIMEOUT, b.send()).await {
                    Ok(Ok(r)) if !r.status().is_success() => {
                        tracing::debug!(status = r.status().as_u16(), "mcp notification not accepted");
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => return Err(Error::Net(format!("无法连接 MCP 服务器：{e}"))),
                    Err(_) => return Err(Error::Ai("MCP 服务器超过 60 秒没有响应".into())),
                }
            }
        }
        Ok(())
    }

    /// `tools/list` (all pages).
    pub async fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let r = self.request("tools/list", params).await?;
            for t in r["tools"].as_array().into_iter().flatten() {
                let Some(name) = t["name"].as_str().filter(|n| !n.is_empty()) else { continue };
                let input_schema = if t["inputSchema"].is_object() { t["inputSchema"].clone() } else { json!({ "type": "object", "properties": {} }) };
                out.push(McpTool { name: name.to_string(), description: t["description"].as_str().unwrap_or("").to_string(), input_schema });
            }
            cursor = r["nextCursor"].as_str().filter(|c| !c.is_empty()).map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// `tools/call` → (text, ok).
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<(String, bool)> {
        let arguments = if arguments.is_object() { arguments } else { json!({}) };
        let r = self.request("tools/call", json!({ "name": name, "arguments": arguments })).await?;
        let is_error = r["isError"].as_bool().unwrap_or(false);
        Ok((truncate_plain(&content_text(&r), MAX_RESULT_CHARS), !is_error))
    }

    /// Ends the connection: stdio servers get EOF (then a kill), HTTP sessions a DELETE.
    pub async fn close(self) {
        match self.transport {
            Transport::Stdio { mut child, stdin, .. } => {
                drop(stdin);
                if tokio::time::timeout(Duration::from_millis(500), child.wait()).await.is_err() {
                    let _ = child.kill().await;
                }
            }
            Transport::Http { http, url, headers, session: Some(session), .. } => {
                let mut b = http.delete(url.as_str()).header("Mcp-Session-Id", session.as_str());
                for (k, v) in &headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                let _ = tokio::time::timeout(Duration::from_secs(2), b.send()).await;
            }
            Transport::Http { .. } => {}
        }
    }
}

/// Connects, lists the tools and disconnects.
async fn probe(e: &Engine, cfg: &McpServerConfig) -> Result<Vec<McpTool>> {
    let mut c = McpClient::connect(e, cfg).await?;
    let tools = c.list_tools().await;
    c.close().await;
    tools
}

impl Engine {
    pub fn ai_mcp_servers(&self) -> Vec<McpServerConfig> {
        self.db.kv_list(SPACE).unwrap_or_default()
    }

    /// Saves a server; an empty id creates one.
    pub fn ai_save_mcp_server(&self, mut c: McpServerConfig) -> Result<McpServerConfig> {
        c.name = c.name.trim().to_string();
        if c.name.is_empty() {
            return Err(Error::Invalid("请填写 MCP 服务器的名称".into()));
        }
        c.transport = if c.transport.trim() == "http" { "http".to_string() } else { "stdio".to_string() };
        c.command = c.command.trim().to_string();
        c.url = c.url.trim().to_string();
        c.args.retain(|a| !a.is_empty());
        c.env.retain(|k, _| !k.trim().is_empty());
        c.headers.retain(|k, _| !k.trim().is_empty());
        if c.transport == "stdio" && c.command.is_empty() {
            return Err(Error::Invalid("请填写 MCP 服务器的启动命令".into()));
        }
        if c.transport == "http" && !(c.url.starts_with("http://") || c.url.starts_with("https://")) {
            return Err(Error::Invalid("MCP 服务器地址应以 http:// 或 https:// 开头".into()));
        }
        if c.id.trim().is_empty() {
            c.id = new_id();
        }
        self.db.kv_put(SPACE, &c.id, &c, 0)?;
        Ok(c)
    }

    pub fn ai_delete_mcp_server(&self, id: &str) -> Result<()> {
        if !self.db.kv_delete(SPACE, id)? {
            return Err(Error::NotFound("MCP 服务器不存在".into()));
        }
        Ok(())
    }

    /// `POST /v1/ai/mcp-servers/{id}/test` → `{ok, tools:[{name, description}], error?}`.
    pub async fn ai_test_mcp_server(&self, id: &str) -> Result<Value> {
        let cfg = self.db.kv_get::<McpServerConfig>(SPACE, id)?.ok_or_else(|| Error::NotFound("MCP 服务器不存在".into()))?;
        Ok(match probe(self, &cfg).await {
            Ok(tools) => {
                let list: Vec<Value> = tools.iter().map(|t| json!({ "name": t.name, "description": t.description })).collect();
                json!({ "ok": true, "tools": list })
            }
            Err(err) => json!({ "ok": false, "tools": [], "error": err.to_string() }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_as_text() {
        let r = json!({"content": [{"type": "text", "text": "第一段"}, {"type": "image", "data": "x"}, {"type": "text", "text": "第二段"}], "isError": false});
        assert_eq!(content_text(&r), "第一段\n[图片]\n第二段");
        assert_eq!(content_text(&json!({"content": [], "structuredContent": {"n": 1}})), "{\"n\":1}");
        assert_eq!(content_text(&json!({"content": [{"type": "resource", "resource": {"uri": "file:///a", "text": "内容"}}]})), "内容");
        assert!(id_matches(&json!(3), 3) && id_matches(&json!("3"), 3) && !id_matches(&json!(4), 3) && !id_matches(&Value::Null, 3));
        assert_eq!(resolve_command("/bin/sh"), PathBuf::from("/bin/sh"));
        assert_eq!(last_lines("a\n\nb\nc\nd\n"), "b c d");
    }
}

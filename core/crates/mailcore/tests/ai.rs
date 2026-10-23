//! AI features against fake providers, no network and no real models:
//! - an OpenAI compatible and an Anthropic server (axum on 127.0.0.1:0) answering with SSE
//!   split into small chunks, plus routes for key rotation and tool calls,
//! - fake `claude` / `codex` shell scripts selected through the provider's `base_url`,
//! - fake MCP servers: a python3 stdio script and an axum Streamable HTTP server.

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use mailcore::events::Event;
use mailcore::model::{Account, AccountKind, FolderRole, Mailbox};
use mailcore::store::ListQuery;
use mailcore::{Engine, api};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---- fake providers ---------------------------------------------------------------------

#[derive(Clone, Default)]
struct Fake {
    requests: Arc<Mutex<Vec<Value>>>,
    auth: Arc<Mutex<Vec<String>>>,
}

/// All prompt text of a request (system + messages).
fn prompt_text(v: &Value) -> String {
    let mut s = String::new();
    if let Some(sys) = v["system"].as_str() {
        s.push_str(sys);
        s.push('\n');
    }
    for m in v["messages"].as_array().into_iter().flatten() {
        if let Some(c) = m["content"].as_str() {
            s.push_str(c);
            s.push('\n');
        }
    }
    s
}

const PLAIN_ANSWER: &str = "这是 AI 的回答 [1]。";

/// Picks a canned answer from markers in the prompt.
fn fake_answer(prompt: &str) -> String {
    if prompt.contains("needs_reply") {
        json!({"needs_reply": true, "priority": "high", "deadline": "2099-01-02 18:00", "deadline_text": "下周五前",
               "todos": ["确认分工"], "one_line": "测试一句话"})
        .to_string()
    } else if prompt.contains("awaits_reply") {
        json!({"awaits_reply": true, "reason": "在等对方确认报价"}).to_string()
    } else if prompt.contains("source_text") {
        json!({"events": [{"title": "西山爬山", "start": "2099-06-06 08:00", "end": null, "all_day": false,
                           "location": "西山门口", "notes": "记得带水", "source_text": "周六早上 8 点"}]})
        .to_string()
    } else if prompt.contains("\"queries\"") {
        "```json\n{\"queries\": [\"纪要\"]}\n```".to_string()
    } else if prompt.contains("suggestions") {
        json!({"suggestions": [
            {"label": "好的", "text": "好的，收到。"},
            {"label": "稍后回复", "text": "我稍后回复你。"},
            {"label": "这次不行", "text": "抱歉，这次不行。"}
        ]})
        .to_string()
    } else if prompt.contains("promotions") {
        json!({"label": "测试标签", "category": "updates", "action": "archive", "reason": "测试理由"}).to_string()
    } else if prompt.contains("\"match\"") {
        "{\"match\": true}".to_string()
    } else if prompt.contains("写一封邮件的正文") {
        "以下是邮件正文：\n你好，\n\n这是 AI 写的正文。\n\n主题：项目进度".to_string()
    } else if prompt.contains("总结") && prompt.contains("一句话") {
        "**一句话**：测试摘要。\n\n- 要点一\n- 要点二".to_string()
    } else {
        PLAIN_ANSWER.to_string()
    }
}

/// Bytes in chunks of `n` (splitting lines and UTF-8 characters).
fn chunked(s: &str, n: usize) -> Vec<Vec<u8>> {
    s.as_bytes().chunks(n).map(|c| c.to_vec()).collect()
}

fn pieces(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(3).map(|c| c.iter().collect()).collect()
}

fn sse(chunks: Vec<Vec<u8>>) -> Response {
    let stream = futures::stream::iter(chunks.into_iter().map(Ok::<Vec<u8>, std::io::Error>));
    Response::builder().header("content-type", "text/event-stream").body(Body::from_stream(stream)).unwrap()
}

fn record(f: &Fake, headers: &HeaderMap, body: &Bytes, auth_header: &str) -> Value {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    f.requests.lock().unwrap().push(v.clone());
    if let Some(a) = headers.get(auth_header).and_then(|h| h.to_str().ok()) {
        f.auth.lock().unwrap().push(a.to_string());
    }
    v
}

fn openai_stream(v: &Value) -> Response {
    let mut s = String::new();
    s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n");
    s.push_str(": keep-alive\n\n");
    s.push_str(&format!("data: {}\n\n", json!({"choices": [{"index": 0, "delta": {"reasoning_content": "思考中……"}}]})));
    for p in pieces(&fake_answer(&prompt_text(v))) {
        s.push_str(&format!("data: {}\r\n\r\n", json!({"choices": [{"index": 0, "delta": {"content": p}}]})));
    }
    if v["stream_options"]["include_usage"] == true {
        s.push_str("data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}\n\n");
    }
    s.push_str("data: [DONE]\n\n");
    sse(chunked(&s, 7))
}

/// Rejects the key "key-a" (401), accepts any other.
async fn keyed_chat(State(f): State<Fake>, headers: HeaderMap, body: Bytes) -> Response {
    let v = record(&f, &headers, &body, "authorization");
    if headers.get("authorization").and_then(|h| h.to_str().ok()) == Some("Bearer key-a") {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": {"message": "invalid key"}}))).into_response();
    }
    openai_stream(&v)
}

/// A model that calls the first tool named `*__echo`, then answers with the tool result.
async fn tools_chat(State(f): State<Fake>, headers: HeaderMap, body: Bytes) -> Response {
    let v = record(&f, &headers, &body, "authorization");
    let messages = v["messages"].as_array().cloned().unwrap_or_default();
    if let Some(result) = messages.iter().find(|m| m["role"] == "tool") {
        let content = result["content"].as_str().unwrap_or("");
        return axum::Json(json!({
            "choices": [{"index": 0, "message": {"role": "assistant", "content": format!("工具说：{content}")}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 20, "completion_tokens": 5}
        }))
        .into_response();
    }
    let name = v["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["function"]["name"].as_str())
        .find(|n| n.ends_with("__echo"))
        .unwrap_or("missing")
        .to_string();
    axum::Json(json!({
        "choices": [{"index": 0, "message": {"role": "assistant", "content": null, "tool_calls": [
            {"id": "call_1", "type": "function", "function": {"name": name, "arguments": "{\"text\":\"你好\"}"}}
        ]}, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 30, "completion_tokens": 6}
    }))
    .into_response()
}

async fn openai_chat(State(f): State<Fake>, headers: HeaderMap, body: Bytes) -> Response {
    let v = record(&f, &headers, &body, "authorization");
    openai_stream(&v)
}

/// A compatible server that rejects `response_format`.
async fn strict_chat(State(f): State<Fake>, headers: HeaderMap, body: Bytes) -> Response {
    let v = record(&f, &headers, &body, "authorization");
    if v.get("response_format").is_some() {
        return (StatusCode::BAD_REQUEST, axum::Json(json!({"error": {"message": "response_format is not supported"}}))).into_response();
    }
    openai_stream(&v)
}

async fn openai_models() -> axum::Json<Value> {
    axum::Json(json!({"object": "list", "data": [{"id": "fake-mini"}, {"id": "fake-large"}]}))
}

async fn anthropic_messages(State(f): State<Fake>, headers: HeaderMap, body: Bytes) -> Response {
    let v = record(&f, &headers, &body, "x-api-key");
    let key = headers.get("x-api-key").and_then(|h| h.to_str().ok()).unwrap_or("");
    if key != "test-key" || headers.get("anthropic-version").is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}})),
        )
            .into_response();
    }
    let mut s = String::new();
    let mut ev = |name: &str, data: Value| s.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    ev("message_start", json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "content": []}}));
    ev("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}));
    ev("ping", json!({"type": "ping"}));
    for p in pieces(&fake_answer(&prompt_text(&v))) {
        ev("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": p}}));
    }
    ev("content_block_stop", json!({"type": "content_block_stop", "index": 0}));
    ev("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 9}}));
    ev("message_stop", json!({"type": "message_stop"}));
    sse(chunked(&s, 5))
}

async fn anthropic_models() -> axum::Json<Value> {
    axum::Json(json!({"data": [{"id": "claude-test-1", "display_name": "Claude Test"}], "has_more": false}))
}

async fn start_fake() -> (String, Fake) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(openai_chat))
        .route("/v1/models", get(openai_models))
        .route("/strict/v1/chat/completions", post(strict_chat))
        .route("/keys/v1/chat/completions", post(keyed_chat))
        .route("/tools/v1/chat/completions", post(tools_chat))
        .route("/anthropic/v1/messages", post(anthropic_messages))
        .route("/anthropic/v1/models", get(anthropic_models))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), fake)
}

// ---- API client -------------------------------------------------------------------------

struct Ctx {
    base: String,
    token: String,
    http: reqwest::Client,
    engine: Engine,
    dir: std::path::PathBuf,
}

impl Drop for Ctx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Ctx {
    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.http.request(method, format!("{}{path}", self.base)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        let status = r.status().as_u16();
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    async fn get(&self, path: &str) -> Value {
        let (status, v) = self.send(reqwest::Method::GET, path, None).await;
        assert!(status < 300, "GET {path}: {status} {v}");
        v
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let (status, v) = self.send(reqwest::Method::POST, path, Some(body)).await;
        assert!(status < 300, "POST {path}: {status} {v}");
        v
    }

    async fn put(&self, path: &str, body: Value) -> Value {
        let (status, v) = self.send(reqwest::Method::PUT, path, Some(body)).await;
        assert!(status < 300, "PUT {path}: {status} {v}");
        v
    }

    /// NDJSON endpoint → (HTTP status, parsed lines).
    async fn stream(&self, path: &str, body: Value) -> (u16, Vec<Value>) {
        let r = self.http.post(format!("{}{path}", self.base)).bearer_auth(&self.token).json(&body).send().await.unwrap();
        let status = r.status().as_u16();
        let ndjson = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").contains("ndjson");
        let text = r.text().await.unwrap();
        if !ndjson {
            return (status, vec![serde_json::from_str(&text).unwrap_or(Value::String(text))]);
        }
        let lines = text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).expect(l)).collect();
        (status, lines)
    }

    /// A successful stream: (joined deltas, done line).
    async fn generate(&self, path: &str, body: Value) -> (String, Value) {
        let (status, lines) = self.stream(path, body).await;
        assert_eq!(status, 200, "{path}: {lines:?}");
        let done = lines.last().cloned().unwrap_or(Value::Null);
        assert_eq!(done["done"], true, "{path}: {lines:?}");
        let deltas: String = lines.iter().filter_map(|l| l["delta"].as_str()).collect();
        (deltas, done)
    }

    async fn wait_for(&self, path: &str, f: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..200 {
            let v = self.get(path).await;
            if f(&v) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out waiting for {path}");
    }
}

async fn setup(demo: bool) -> Ctx {
    unsafe { std::env::set_var("MAILBOX_KEY_FILE", "1") };
    let dir = std::env::temp_dir().join(format!("mailbox-ai-{}", uuid::Uuid::new_v4().simple()));
    let engine = Engine::open(&dir).unwrap();
    let h = api::serve(engine.clone(), Some(0)).await.unwrap();
    engine.start();
    let c = Ctx { base: format!("http://127.0.0.1:{}", h.port), token: h.token, http: reqwest::Client::new(), engine, dir };
    if demo {
        c.post("/v1/demo", json!({})).await;
    }
    c
}

/// Registers the fake servers as custom providers and makes them the default / quick model.
async fn use_fakes(c: &Ctx, base: &str) {
    let p = c
        .put(
            "/v1/ai/providers/fake",
            json!({"name": "Fake", "kind": "openai", "base_url": format!("{base}/v1"), "enabled": true,
                   "models": [{"id": "fake-mini"}], "api_key": "sk-test"}),
        )
        .await;
    assert_eq!(p["has_key"], true);
    assert_eq!(p["custom"], true);
    let mut s = c.get("/v1/ai/settings").await;
    s["default_model"] = json!({"provider": "fake", "model": "fake-mini"});
    s["quick_model"] = json!({"provider": "fake", "model": "fake-mini"});
    c.put("/v1/ai/settings", s).await;
}

async fn message_id(c: &Ctx, needle: &str) -> i64 {
    let list = c.get("/v1/messages?mailbox=all&threads=false&limit=200").await;
    list["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["subject"].as_str().unwrap_or("").contains(needle))
        .unwrap_or_else(|| panic!("no demo message {needle}"))["id"]
        .as_i64()
        .unwrap()
}

// ---- tests ------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn providers_streaming_and_reading() {
    let (fake_base, fake) = start_fake().await;
    let c = setup(true).await;

    // Presets and the generated provider list.
    let presets = c.get("/v1/ai/presets").await;
    assert!(presets.as_array().unwrap().len() >= 22);
    let providers = c.get("/v1/ai/providers").await;
    let list = providers.as_array().unwrap();
    assert_eq!(list[0]["id"], "claude_code");
    assert_eq!(list[0]["enabled"], true);
    assert_eq!(list.iter().find(|p| p["id"] == "openai").unwrap()["enabled"], false);
    assert_eq!(list.iter().find(|p| p["id"] == "ollama").unwrap()["enabled"], true);

    // No model configured yet: a plain HTTP error before any streaming.
    let (status, lines) = c.stream("/v1/ai/compose", json!({"op": "polish", "text": "你好"})).await;
    assert_eq!(status, 400, "{lines:?}");
    assert_eq!(lines[0]["code"], "ai_not_configured");
    assert!(lines[0]["error"].as_str().unwrap().contains("还没有设置 AI 模型"));

    use_fakes(&c, &fake_base).await;
    let providers = c.get("/v1/ai/providers").await;
    assert!(!providers.to_string().contains("sk-test"), "keys are never returned");
    assert_eq!(providers.as_array().unwrap().last().unwrap()["id"], "fake");

    // Model list and connection check.
    let models = c.post("/v1/ai/providers/fake/models", json!({})).await;
    let ids: Vec<&str> = models.as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["fake-large", "fake-mini"]);
    assert_eq!(models[0]["group"], "fake");
    let test = c.post("/v1/ai/providers/fake/test", json!({"model": "fake-mini"})).await;
    assert_eq!(test["ok"], true, "{test}");
    assert_eq!(test["reply"], PLAIN_ANSWER);

    // Chat: deltas reassemble the text (chunk boundaries everywhere), reasoning is dropped.
    let (deltas, done) = c.generate("/v1/ai/chat", json!({"messages": [{"role": "user", "content": "你好"}]})).await;
    assert_eq!(deltas, PLAIN_ANSWER);
    assert_eq!(done["text"], PLAIN_ANSWER);
    assert_eq!(done["model"], "fake/fake-mini");
    assert!(fake.auth.lock().unwrap().iter().any(|a| a == "Bearer sk-test"));

    // Compose: write splits the subject line off and drops the preamble.
    let (_, done) = c.generate("/v1/ai/compose", json!({"op": "write", "instruction": "写一封项目进度邮件"})).await;
    assert_eq!(done["text"], "你好，\n\n这是 AI 写的正文。");
    assert_eq!(done["subject"], "项目进度");
    let (_, done) = c.generate("/v1/ai/compose", json!({"op": "polish", "text": "你好呀"})).await;
    assert_eq!(done["text"], PLAIN_ANSWER);
    assert!(done["subject"].is_null());
    let doc = message_id(&c, "接口文档").await;
    let (_, done) = c.generate("/v1/ai/compose", json!({"op": "reply", "message_id": doc, "instruction": "说我已经看过了"})).await;
    assert_eq!(done["text"], PLAIN_ANSWER);
    let (status, lines) = c.stream("/v1/ai/compose", json!({"op": "polish", "text": "  "})).await;
    assert_eq!(status, 400, "{lines:?}");

    // Encrypted mail is refused unless allowed.
    let secret = message_id(&c, "蓝鲸").await;
    let (status, lines) = c.stream(&format!("/v1/ai/messages/{secret}/summary"), json!({})).await;
    assert_eq!(status, 400, "{lines:?}");
    assert!(lines[0]["error"].as_str().unwrap().contains("加密"));

    // Quick model on a server that rejects response_format: retried without it.
    c.put(
        "/v1/ai/providers/strict",
        json!({"name": "Strict", "kind": "openai", "base_url": format!("{fake_base}/strict/v1"), "enabled": true,
               "models": [{"id": "fake-mini"}]}),
    )
    .await;
    let mut s = c.get("/v1/ai/settings").await;
    s["quick_model"] = json!({"provider": "strict", "model": "fake-mini"});
    c.put("/v1/ai/settings", s).await;

    // Smart analysis + reminders.
    let mut events = c.engine.events.subscribe();
    let notes = message_id(&c, "纪要").await;
    let ins = c.post(&format!("/v1/ai/messages/{notes}/insight"), json!({})).await;
    assert_eq!(ins["needs_reply"], true, "{ins}");
    assert_eq!(ins["priority"], "high");
    assert_eq!(ins["todos"], json!(["确认分工"]));
    assert!(ins["deadline"].as_i64().unwrap() > 4_000_000_000);
    assert!(ins["remind_at"].as_i64().unwrap() < ins["deadline"].as_i64().unwrap());
    let mut saw_event = false;
    while let Ok(ev) = events.try_recv() {
        if let Event::AiInsight { message_id } = ev {
            saw_event |= message_id == notes;
        }
    }
    assert!(saw_event);
    assert_eq!(c.get(&format!("/v1/ai/messages/{notes}/insight")).await["one_line"], "测试一句话");
    let reminders = c.get("/v1/ai/reminders").await;
    let item = reminders.as_array().unwrap().iter().find(|r| r["message"]["id"] == notes).expect("reminder listed").clone();
    assert_eq!(item["account_label"], "QQ·工作");
    assert_eq!(item["insight"]["deadline_text"], "下周五前");
    c.post(&format!("/v1/ai/reminders/{notes}/done"), json!({"done": true})).await;
    let reminders = c.get("/v1/ai/reminders").await;
    assert!(reminders.as_array().unwrap().iter().all(|r| r["message"]["id"] != notes));

    // Summary: generated, then served from the cache, regenerated with force.
    let (status, _) = c.send(reqwest::Method::GET, &format!("/v1/ai/messages/{notes}/summary"), None).await;
    assert_eq!(status, 404);
    let (deltas, done) = c.generate(&format!("/v1/ai/messages/{notes}/summary"), json!({})).await;
    assert_eq!(done["cached"], false);
    assert!(done["text"].as_str().unwrap().starts_with("**一句话**：测试摘要"));
    assert_eq!(deltas, done["text"].as_str().unwrap());
    let (deltas, done) = c.generate(&format!("/v1/ai/messages/{notes}/summary"), json!({"thread": true})).await;
    assert_eq!(done["cached"], true);
    assert_eq!(deltas, done["text"].as_str().unwrap());
    let cached = c.get(&format!("/v1/ai/messages/{notes}/summary")).await;
    assert_eq!(cached["text"], done["text"]);
    assert_eq!(cached["model"], "strict/fake-mini");
    let (_, done) = c.generate(&format!("/v1/ai/messages/{notes}/summary"), json!({"force": true})).await;
    assert_eq!(done["cached"], false);

    // Translation and suggested replies.
    let (_, done) = c.generate(&format!("/v1/ai/messages/{notes}/translate"), json!({"target_lang": "en"})).await;
    assert_eq!(done["text"], PLAIN_ANSWER);
    let replies = c.post(&format!("/v1/ai/messages/{notes}/replies"), json!({})).await;
    let suggestions = replies["suggestions"].as_array().unwrap();
    assert_eq!(suggestions.len(), 3);
    assert_eq!(suggestions[0]["label"], "好的");
    assert_eq!(suggestions[0]["text"], "好的，收到。");

    // Ask the mailbox: the rewritten query finds the meeting notes, cited as sources.
    let (_, done) = c.generate("/v1/ai/ask", json!({"question": "王经理的会议纪要让我做什么？"})).await;
    assert_eq!(done["text"], PLAIN_ANSWER);
    let sources = done["sources"].as_array().unwrap();
    assert!(!sources.is_empty(), "{done}");
    assert_eq!(sources[0]["index"], 1);
    assert!(sources.iter().any(|s| s["message_id"] == notes && s["subject"].as_str().unwrap().contains("纪要")), "{done}");

    // Anthropic Messages API.
    let anthropic = json!({"name": "Claude API", "kind": "anthropic", "base_url": format!("{fake_base}/anthropic"), "enabled": true,
                           "models": [{"id": "claude-test-1"}], "api_key": "test-key"});
    c.put("/v1/ai/providers/claude-test", anthropic.clone()).await;
    let models = c.post("/v1/ai/providers/claude-test/models", json!({})).await;
    assert_eq!(models[0]["id"], "claude-test-1");
    assert_eq!(models[0]["name"], "Claude Test");
    let chat = json!({"model": {"provider": "claude-test", "model": "claude-test-1"}, "system": "你是测试助手",
                      "messages": [{"role": "user", "content": "你好"}]});
    let (deltas, done) = c.generate("/v1/ai/chat", chat.clone()).await;
    assert_eq!(deltas, PLAIN_ANSWER);
    assert_eq!(done["model"], "claude-test/claude-test-1");
    let last = fake.requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last["system"], "你是测试助手");
    assert_eq!(last["max_tokens"], 4096);
    let mut wrong = anthropic.clone();
    wrong["api_key"] = json!("wrong-key");
    c.put("/v1/ai/providers/claude-test", wrong).await;
    let (status, lines) = c.stream("/v1/ai/chat", chat).await;
    assert_eq!(status, 200);
    let err = lines.last().unwrap()["error"].as_str().unwrap_or("").to_string();
    assert!(err.contains("API Key 无效"), "{lines:?}");

    // Deleting: a custom provider goes away, a preset is reset.
    c.send(reqwest::Method::DELETE, "/v1/ai/providers/claude-test", None).await;
    assert!(c.get("/v1/ai/providers").await.as_array().unwrap().iter().all(|p| p["id"] != "claude-test"));

    // Demo seeding for screenshots.
    let demo = c.post("/v1/ai/demo", json!({})).await;
    assert!(demo["insights"].as_i64().unwrap() >= 4, "{demo}");
    assert_eq!(c.get("/v1/ai/settings").await["default_model"], json!({"provider": "claude_code", "model": "default"}));
    assert!(!c.get("/v1/ai/reminders").await.as_array().unwrap().is_empty());
    let loop_id = demo["loop_id"].as_str().unwrap();
    let runs = c.get(&format!("/v1/ai/loops/{loop_id}/runs")).await;
    assert_eq!(runs[0]["status"], "done");
    let cached = c.get(&format!("/v1/ai/messages/{notes}/summary")).await;
    assert!(cached["text"].as_str().unwrap().contains("一句话"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batch_loops_and_auto_replies() {
    let (fake_base, _fake) = start_fake().await;
    let c = setup(true).await;
    use_fakes(&c, &fake_base).await;

    // Batch classify, then apply the suggestions of one message.
    let hike = message_id(&c, "周末爬山").await;
    let bill = message_id(&c, "10 月账单").await;
    let secret = message_id(&c, "蓝鲸").await;
    let r = c.post("/v1/ai/batch", json!({"ids": [hike, bill, secret], "op": "classify"})).await;
    let job_id = r["job_id"].as_str().unwrap().to_string();
    let job = c.wait_for(&format!("/v1/ai/jobs/{job_id}"), |j| j["status"] != "running").await;
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(job["total"], 3);
    assert_eq!(job["done"], 3);
    let items = job["items"].as_array().unwrap();
    let hike_item = items.iter().find(|i| i["message_id"] == hike).unwrap();
    let actions: Vec<&str> = hike_item["suggested"].as_array().unwrap().iter().map(|a| a["action"].as_str().unwrap()).collect();
    assert_eq!(actions, vec!["label", "category", "archive"]);
    let bill_item = items.iter().find(|i| i["message_id"] == bill).unwrap();
    assert!(bill_item["suggested"].as_array().unwrap().iter().all(|a| a["action"] != "category"), "already in updates");
    let secret_item = items.iter().find(|i| i["message_id"] == secret).unwrap();
    assert!(secret_item["error"].as_str().unwrap().contains("加密"));
    let applied = c.post(&format!("/v1/ai/jobs/{job_id}/apply"), json!({"message_ids": [hike]})).await;
    assert_eq!(applied["applied"], 1);
    let m = c.get(&format!("/v1/messages/{hike}")).await;
    assert_eq!(m["folder_role"], "archive");
    assert_eq!(m["category"], "updates");
    assert!(m["labels"].as_array().unwrap().iter().any(|l| l == "测试标签"), "{m}");

    // Batch summarize: one line per message plus an overall summary.
    let notes = message_id(&c, "纪要").await;
    let doc = message_id(&c, "接口文档").await;
    let r = c.post("/v1/ai/batch", json!({"ids": [notes, doc], "op": "summarize"})).await;
    let job = c.wait_for(&format!("/v1/ai/jobs/{}", r["job_id"].as_str().unwrap()), |j| j["status"] != "running").await;
    assert_eq!(job["status"], "done");
    assert_eq!(job["summary"], PLAIN_ANSWER);
    assert!(job["items"].as_array().unwrap().iter().all(|i| i["output"] == PLAIN_ANSWER));

    // Batch draft replies are saved as drafts.
    let mockups = message_id(&c, "mockups").await;
    let r = c.post("/v1/ai/batch", json!({"ids": [mockups], "op": "draft_replies"})).await;
    let job = c.wait_for(&format!("/v1/ai/jobs/{}", r["job_id"].as_str().unwrap()), |j| j["status"] != "running").await;
    let draft_id = job["items"][0]["draft_id"].as_str().expect("draft id").to_string();
    let draft = c.get(&format!("/v1/drafts/{draft_id}")).await;
    assert!(draft["html"].as_str().unwrap().starts_with("<div>这是 AI 的回答 [1]。</div>"), "{draft}");
    assert_eq!(draft["source_message_id"], mockups);
    let (status, _) = c.send(reqwest::Method::POST, "/v1/ai/batch", Some(json!({"ids": [notes], "op": "custom"}))).await;
    assert_eq!(status, 400, "custom needs an instruction");

    // Loops: templates, a run over matching mail, a run without matches, a digest.
    assert_eq!(c.get("/v1/ai/loop-templates").await.as_array().unwrap().len(), 4);
    let l = c
        .post(
            "/v1/ai/loops",
            json!({"name": "测试 Loop", "schedule": {"kind": "daily", "time": "08:00"}, "query": "纪要", "instruction": "整理一下要点",
                   "email_digest": true}),
        )
        .await;
    let loop_id = l["id"].as_str().unwrap().to_string();
    assert!(!loop_id.is_empty());
    assert!(l["next_run"].as_i64().unwrap() > chrono_now());
    let mut events = c.engine.events.subscribe();
    let run = c.post(&format!("/v1/ai/loops/{loop_id}/run"), json!({})).await;
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let runs = c.wait_for(&format!("/v1/ai/loops/{loop_id}/runs"), |r| r[0]["status"] != "running").await;
    assert_eq!(runs[0]["id"], run_id.as_str());
    assert_eq!(runs[0]["status"], "done", "{runs}");
    assert_eq!(runs[0]["message_count"], 1);
    assert_eq!(runs[0]["output"], PLAIN_ANSWER);
    let mut saw_loop = false;
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Ok(Event::AiLoop { loop_id: id, preview, .. })) if id == loop_id => {
                assert_eq!(preview, PLAIN_ANSWER);
                saw_loop = true;
                break;
            }
            Ok(Ok(_)) => {}
            _ => {}
        }
    }
    assert!(saw_loop);
    let digest = c.get(&format!("/v1/messages?mailbox=unified:inbox&threads=false&q={}", "from:ai@mailbox.local")).await;
    assert_eq!(digest["messages"].as_array().unwrap().len(), 1, "{digest}");
    let saved = c.get("/v1/ai/loops").await;
    let saved = saved.as_array().unwrap().iter().find(|x| x["id"] == loop_id.as_str()).unwrap().clone();
    assert!(saved["last_run"].as_i64().is_some());

    let empty = c
        .post("/v1/ai/loops", json!({"name": "空", "schedule": {"kind": "weekly", "time": "09:00", "weekday": 1}, "query": "完全不存在的关键词xyz", "instruction": "随便"}))
        .await;
    let empty_id = empty["id"].as_str().unwrap().to_string();
    c.post(&format!("/v1/ai/loops/{empty_id}/run"), json!({})).await;
    let runs = c.wait_for(&format!("/v1/ai/loops/{empty_id}/runs"), |r| r[0]["status"] != "running").await;
    assert_eq!(runs[0]["output"], "没有符合条件的邮件");
    assert_eq!(runs[0]["message_count"], 0);
    c.send(reqwest::Method::DELETE, &format!("/v1/ai/loops/{empty_id}"), None).await;
    assert!(c.get("/v1/ai/loops").await.as_array().unwrap().iter().all(|x| x["id"] != empty_id.as_str()));
    let (status, _) = c.send(reqwest::Method::POST, "/v1/ai/loops", Some(json!({"name": "x", "schedule": {"kind": "daily", "time": "8点"}, "instruction": "y"}))).await;
    assert_eq!(status, 400);

    // Auto replies on a local (POP3-style) inbox.
    let acc = c
        .engine
        .db
        .insert_account(Account {
            kind: AccountKind::Pop3,
            email: "me@pop-demo.cn".into(),
            name: "我".into(),
            label: "POP".into(),
            provider: Some("demo".into()),
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    let inbox = c.engine.db.ensure_local_folder(&acc.id, FolderRole::Inbox, "收件箱").unwrap();
    let eml_dir = c.dir.join("eml");
    std::fs::create_dir_all(&eml_dir).unwrap();
    let eml = |from: &str, subject: &str, extra: &str, mid: &str, body: &str| {
        format!(
            "From: {from}\r\nTo: me@pop-demo.cn\r\nSubject: {subject}\r\n{extra}Message-ID: <{mid}>\r\nDate: Thu, 22 Oct 2026 10:00:00 +0800\r\n\
             MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
        )
    };
    std::fs::write(eml_dir.join("1.eml"), eml("=?UTF-8?B?5byg5LiJ?= <zhangsan@friend-demo.cn>", "Dinner tomorrow?", "", "friend1@test", "明天一起吃饭吗？")).unwrap();
    std::fs::write(eml_dir.join("2.eml"), eml("Shop <no-reply@shop-demo.com>", "Your order", "", "noreply1@test", "订单已发货。")).unwrap();
    std::fs::write(
        eml_dir.join("3.eml"),
        eml("Shop News <news@shop-demo.com>", "Weekly deals", "List-Unsubscribe: <mailto:unsub@shop-demo.com>\r\nPrecedence: bulk\r\n", "bulk1@test", "本周特价。"),
    )
    .unwrap();
    std::fs::write(eml_dir.join("4.eml"), eml("Li Si <lisi@friend-demo.cn>", "Report", "", "friend2@test", "报告写好了吗？")).unwrap();
    assert_eq!(c.engine.import_messages(inbox, &eml_dir).await.unwrap(), 4);
    let local = c.engine.db.list_messages(&ListQuery { mailbox: Mailbox::Folder(inbox), limit: 10, ..Default::default() }).unwrap();
    assert_eq!(local.len(), 4);
    let id_of = |email: &str| local.iter().find(|m| m.from.email == email).unwrap().id;

    c.post(
        "/v1/ai/autoreply",
        json!({"name": "朋友", "enabled": true, "from_contains": "zhangsan", "ai_match": "朋友之间的邀约", "instruction": "礼貌地答应", "mode": "draft"}),
    )
    .await;
    c.post(
        "/v1/ai/autoreply",
        json!({"name": "李四", "enabled": true, "from_contains": "lisi", "instruction": "说我明天给他", "mode": "send"}),
    )
    .await;
    // Rules matching everything, to show that noreply / bulk mail is skipped anyway.
    c.post("/v1/ai/autoreply", json!({"name": "全部", "enabled": true, "instruction": "收到", "mode": "draft"})).await;

    let mut events = c.engine.events.subscribe();
    for m in &local {
        c.engine.ai_process_new_mail(m.id).await;
    }
    let drafts = c.engine.db.drafts().unwrap();
    let for_msg = |id: i64| drafts.iter().filter(|d| d.source_message_id == Some(id)).count();
    assert_eq!(for_msg(id_of("no-reply@shop-demo.com")), 0, "noreply is skipped");
    assert_eq!(for_msg(id_of("news@shop-demo.com")), 0, "bulk mail is skipped");
    let friend = id_of("zhangsan@friend-demo.cn");
    assert_eq!(for_msg(friend), 1);
    let d = drafts.iter().find(|d| d.source_message_id == Some(friend)).unwrap();
    assert!(d.html.starts_with("<div>这是 AI 的回答 [1]。</div>"), "{}", d.html);
    assert_eq!(d.to[0].email, "zhangsan@friend-demo.cn");
    assert!(!d.auto_submitted);

    // The "send" rule went through the outbox with Auto-Submitted.
    let outbox = c.get("/v1/outbox").await;
    let item = outbox.as_array().unwrap().iter().find(|o| o["to"][0]["email"] == "lisi@friend-demo.cn").expect("queued").clone();
    let cancelled = c.post(&format!("/v1/outbox/{}/cancel", item["id"].as_str().unwrap()), json!({})).await;
    assert_eq!(cancelled["draft"]["auto_submitted"], true, "{cancelled}");

    let mut sent_events = Vec::new();
    while let Ok(ev) = events.try_recv() {
        if let Event::AiAutoreply { message_id, sent, draft_id, rule_name, .. } = ev {
            sent_events.push((message_id, sent, draft_id.is_some(), rule_name));
        }
    }
    assert!(sent_events.contains(&(friend, false, true, "朋友".to_string())), "{sent_events:?}");
    assert!(sent_events.contains(&(id_of("lisi@friend-demo.cn"), true, false, "李四".to_string())), "{sent_events:?}");

    // Once per sender: a second pass adds nothing.
    c.engine.ai_process_new_mail(friend).await;
    assert_eq!(c.engine.db.drafts().unwrap().iter().filter(|d| d.source_message_id == Some(friend)).count(), 1);
    let rules = c.get("/v1/ai/autoreply").await;
    let friend_rule = rules.as_array().unwrap().iter().find(|r| r["name"] == "朋友").unwrap();
    assert_eq!(friend_rule["sent_today"], 1);
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

#[cfg(unix)]
fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscription_command_lines() {
    let bin = std::env::temp_dir().join(format!("mailbox-ai-cli-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&bin).unwrap();
    let claude = write_script(
        &bin,
        "claude",
        r#"#!/bin/sh
DIR="$(cd "$(dirname "$0")" && pwd)"
if [ "$1" = "--version" ]; then echo "2.1.3 (Claude Code)"; exit 0; fi
printf '%s\n' "$*" > "$DIR/claude.args"
cat > "$DIR/claude.stdin"
echo '{"type":"system","subtype":"init","session_id":"s1"}'
echo '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好，"}}}'
echo '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"来自 Claude"}}}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"你好，来自 Claude"}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"你好，来自 Claude"}'
"#,
    );
    let claude_old = write_script(
        &bin,
        "claude-old",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--include-partial-messages" ]; then
    echo "error: unknown option '--include-partial-messages'" >&2
    exit 1
  fi
done
cat > /dev/null
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"旧版 Claude 回复"}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"旧版 Claude 回复"}'
"#,
    );
    let claude_logged_out = write_script(
        &bin,
        "claude-logged-out",
        r#"#!/bin/sh
cat > /dev/null
echo '{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login"}'
exit 1
"#,
    );
    let codex = write_script(
        &bin,
        "codex",
        r#"#!/bin/sh
DIR="$(cd "$(dirname "$0")" && pwd)"
if [ "$1" = "--version" ]; then echo "codex-cli 0.50.0"; exit 0; fi
if [ "$1" = "login" ]; then echo "Logged in using ChatGPT"; exit 0; fi
out=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--output-last-message" ]; then out="$a"; fi
  prev="$a"
done
printf '%s\n' "$*" > "$DIR/codex.args"
cat > "$DIR/codex.stdin"
echo '{"type":"thread.started","thread_id":"t1"}'
echo '{"type":"item.completed","item":{"id":"i0","type":"agent_message","text":"Codex 回复"}}'
printf '%s' 'Codex 最终回复' > "$out"
"#,
    );

    let c = setup(false).await;
    let provider = |kind: &str, path: &std::path::Path| {
        json!({"name": kind, "kind": kind, "base_url": path.to_string_lossy(), "enabled": true,
               "models": [{"id": "default"}, {"id": "sonnet"}]})
    };
    c.put("/v1/ai/providers/claude_code", provider("claude_code", claude.as_path())).await;
    c.put("/v1/ai/providers/codex", provider("codex", codex.as_path())).await;

    // Status of both command lines.
    let status = c.get("/v1/ai/cli").await;
    assert_eq!(status["claude"]["found"], true, "{status}");
    assert_eq!(status["claude"]["version"], "2.1.3 (Claude Code)");
    assert!(status["claude"]["logged_in"].is_null());
    assert!(status["claude"]["hint"].as_str().unwrap().contains("已找到"));
    assert_eq!(status["codex"]["logged_in"], true, "{status}");
    assert_eq!(status["codex"]["version"], "codex-cli 0.50.0");

    // Claude Code: partial messages stream, the full assistant message is not repeated.
    let chat = |provider: &str, model: &str| {
        json!({"model": {"provider": provider, "model": model}, "system": "系统提示",
               "messages": [{"role": "user", "content": "你好"}]})
    };
    let (deltas, done) = c.generate("/v1/ai/chat", chat("claude_code", "default")).await;
    assert_eq!(deltas, "你好，来自 Claude");
    assert_eq!(done["text"], "你好，来自 Claude");
    let args = std::fs::read_to_string(bin.join("claude.args")).unwrap();
    assert!(args.starts_with("-p --output-format stream-json --verbose --include-partial-messages --max-turns 1"), "{args}");
    assert!(args.contains("--append-system-prompt 系统提示"), "{args}");
    assert!(args.contains("--disallowedTools Bash Edit Write Read Glob Grep WebFetch WebSearch NotebookEdit Task"), "{args}");
    assert!(!args.contains("--model"), "{args}");
    assert_eq!(std::fs::read_to_string(bin.join("claude.stdin")).unwrap(), "你好");
    c.generate("/v1/ai/chat", chat("claude_code", "sonnet")).await;
    assert!(std::fs::read_to_string(bin.join("claude.args")).unwrap().contains("--model sonnet"));
    let test = c.post("/v1/ai/providers/claude_code/test", json!({})).await;
    assert_eq!(test["ok"], true, "{test}");

    // An older CLI without --include-partial-messages: retried without it.
    c.put("/v1/ai/providers/claude_code", provider("claude_code", claude_old.as_path())).await;
    let (deltas, done) = c.generate("/v1/ai/chat", chat("claude_code", "default")).await;
    assert_eq!(done["text"], "旧版 Claude 回复");
    assert_eq!(deltas, "旧版 Claude 回复");

    // Not logged in.
    c.put("/v1/ai/providers/claude_code", provider("claude_code", claude_logged_out.as_path())).await;
    let (_, lines) = c.stream("/v1/ai/chat", chat("claude_code", "default")).await;
    let err = lines.last().unwrap()["error"].as_str().unwrap_or("").to_string();
    assert!(err.contains("Claude Code 未登录"), "{lines:?}");

    // Codex: agent messages stream, the last-message file is the final text.
    let (deltas, done) = c.generate("/v1/ai/chat", chat("codex", "default")).await;
    assert_eq!(deltas, "Codex 回复");
    assert_eq!(done["text"], "Codex 最终回复");
    let args = std::fs::read_to_string(bin.join("codex.args")).unwrap();
    assert!(args.starts_with("exec --skip-git-repo-check --sandbox read-only --color never --json --output-last-message "), "{args}");
    assert!(args.ends_with(" -\n"), "{args}");
    assert_eq!(std::fs::read_to_string(bin.join("codex.stdin")).unwrap(), "系统提示\n\n---\n\n你好");
    c.generate("/v1/ai/chat", chat("codex", "sonnet")).await;
    assert!(std::fs::read_to_string(bin.join("codex.args")).unwrap().contains("--json -m sonnet --output-last-message"));

    // A path that does not exist.
    c.put("/v1/ai/providers/codex", provider("codex", bin.join("missing").as_path())).await;
    let status = c.get("/v1/ai/cli").await;
    assert_eq!(status["codex"]["found"], false);
    let _ = std::fs::remove_dir_all(&bin);
}

// ---- second batch (docs/AI.md §8) ---------------------------------------------------------

/// The id of a message's first attachment whose MIME type starts with `prefix`.
async fn attachment_id(c: &Ctx, message: i64, prefix: &str) -> i64 {
    let detail = c.get(&format!("/v1/messages/{message}")).await;
    detail["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["mime_type"].as_str().unwrap_or("").starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix} attachment: {detail}"))["id"]
        .as_i64()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calendar_attachments_prompts_and_policies() {
    let (fake_base, fake) = start_fake().await;
    let c = setup(true).await;
    use_fakes(&c, &fake_base).await;

    // Calendar extraction: the model's JSON becomes events, cached until forced.
    let hike = message_id(&c, "周末爬山").await;
    let r = c.post(&format!("/v1/ai/messages/{hike}/events"), json!({})).await;
    assert_eq!(r["cached"], false, "{r}");
    let events = r["events"].as_array().unwrap().clone();
    assert_eq!(events.len(), 1, "{r}");
    assert_eq!(events[0]["title"], "西山爬山");
    assert_eq!(events[0]["location"], "西山门口");
    assert_eq!(events[0]["source_text"], "周六早上 8 点");
    assert_eq!(events[0]["all_day"], false);
    assert!(events[0]["start"].as_i64().unwrap() > 4_000_000_000);
    assert!(events[0]["end"].is_null());
    let again = c.post(&format!("/v1/ai/messages/{hike}/events"), json!({})).await;
    assert_eq!(again["cached"], true);
    assert_eq!(again["events"], json!(events));
    let forced = c.post(&format!("/v1/ai/messages/{hike}/events"), json!({"force": true})).await;
    assert_eq!(forced["cached"], false);
    // The smart analysis fills the same cache; risk is null for ordinary mail.
    let notes = message_id(&c, "纪要").await;
    let ins = c.post(&format!("/v1/ai/messages/{notes}/insight"), json!({})).await;
    assert!(ins["events"].as_array().unwrap().is_empty(), "{ins}");
    assert!(ins["risk"].is_null(), "{ins}");
    assert_eq!(ins["risk_reason"], "");
    let cached = c.post(&format!("/v1/ai/messages/{notes}/events"), json!({})).await;
    assert_eq!(cached["cached"], true, "{cached}");

    // Compose: a subject for the body.
    let (_, done) = c.generate("/v1/ai/compose", json!({"op": "subject", "text": "下周二下午三点开评审会，请准时参加。"})).await;
    assert_eq!(done["subject"], PLAIN_ANSWER);
    assert_eq!(done["text"], PLAIN_ANSWER);
    let (status, _) = c.stream("/v1/ai/compose", json!({"op": "subject", "text": " "})).await;
    assert_eq!(status, 400);

    // Prompt library: defaults on first read, add and delete, run one on a message.
    let prompts = c.get("/v1/ai/prompts").await;
    let list = prompts.as_array().unwrap();
    assert_eq!(list.len(), 5, "{prompts}");
    assert_eq!(list.iter().filter(|p| p["scope"] == "reader").count(), 2);
    assert_eq!(list.iter().filter(|p| p["scope"] == "compose").count(), 2);
    let saved = c.post("/v1/ai/prompts", json!({"name": "", "scope": "batch", "prompt": "列出所有附件"})).await;
    let pid = saved["id"].as_str().unwrap().to_string();
    assert_eq!(saved["name"], "列出所有附件");
    assert_eq!(c.get("/v1/ai/prompts").await.as_array().unwrap().last().unwrap()["id"], pid.as_str());
    let (status, _) = c.send(reqwest::Method::POST, "/v1/ai/prompts", Some(json!({"scope": "nope", "prompt": "x"}))).await;
    assert_eq!(status, 400);
    c.send(reqwest::Method::DELETE, &format!("/v1/ai/prompts/{pid}"), None).await;
    assert_eq!(c.get("/v1/ai/prompts").await.as_array().unwrap().len(), 5);
    let (_, done) = c.generate(&format!("/v1/ai/messages/{notes}/prompt"), json!({"prompt": "提取所有金额和日期", "thread": false})).await;
    assert_eq!(done["text"], PLAIN_ANSWER);
    let last = fake.requests.lock().unwrap().last().cloned().unwrap();
    assert!(prompt_text(&last).contains("提取所有金额和日期"));

    // Attachments: a PDF needs the text the app extracted.
    let pdf = attachment_id(&c, notes, "application/pdf").await;
    let path = format!("/v1/ai/messages/{notes}/attachments/{pdf}/explain");
    let (status, lines) = c.stream(&path, json!({})).await;
    assert_eq!(status, 400, "{lines:?}");
    let (_, done) = c.generate(&path, json!({"text": "会议纪要：移动端改版由张三负责。", "question": "谁负责移动端？"})).await;
    assert_eq!(done["text"], PLAIN_ANSWER);
    assert!(done["filename"].as_str().unwrap().ends_with(".pdf"), "{done}");
    let last = fake.requests.lock().unwrap().last().cloned().unwrap();
    let sent = prompt_text(&last);
    assert!(sent.contains("谁负责移动端？") && sent.contains("移动端改版由张三负责"), "{sent}");
    // An image needs a model that can see images.
    let mockups = message_id(&c, "mockups").await;
    let png = attachment_id(&c, mockups, "image/").await;
    let path = format!("/v1/ai/messages/{mockups}/attachments/{png}/explain");
    let (status, lines) = c.stream(&path, json!({})).await;
    assert_eq!(status, 400, "{lines:?}");
    assert_eq!(lines[0]["code"], "ai_not_configured");
    assert!(lines[0]["error"].as_str().unwrap().contains("看图"), "{lines:?}");
    let mut s = c.get("/v1/ai/settings").await;
    s["vision_model"] = json!({"provider": "fake", "model": "fake-mini"});
    c.put("/v1/ai/settings", s).await;
    let (_, done) = c.generate(&path, json!({"question": "图里是什么？"})).await;
    assert_eq!(done["text"], PLAIN_ANSWER);
    let last = fake.requests.lock().unwrap().last().cloned().unwrap();
    let parts = last["messages"].as_array().unwrap().last().unwrap()["content"].as_array().cloned().expect("content parts");
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[1]["type"], "image_url");
    assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,ZGVtbw==");

    // Model capabilities are inferred from the id; key_count is reported.
    c.put(
        "/v1/ai/providers/remote",
        json!({"name": "Remote", "kind": "openai", "base_url": "https://api.remote-demo.invalid/v1", "enabled": true,
               "models": [{"id": "gpt-4o-mini"}, {"id": "text-embedding-3-small"}], "api_key": "sk-remote"}),
    )
    .await;
    let providers = c.get("/v1/ai/providers").await;
    let remote = providers.as_array().unwrap().iter().find(|p| p["id"] == "remote").unwrap().clone();
    assert_eq!(remote["key_count"], 1);
    assert_eq!(remote["models"][0]["capabilities"], json!(["vision", "tools"]));
    assert_eq!(remote["models"][1]["capabilities"], json!(["embedding"]));
    let openai = providers.as_array().unwrap().iter().find(|p| p["id"] == "openai").unwrap().clone();
    assert_eq!(openai["models"][0]["capabilities"], json!(["vision", "tools"]), "{openai}");

    // Account policies: "off" refuses, "local_only" accepts local models only.
    let accounts = c.get("/v1/accounts").await;
    let qq = accounts.as_array().unwrap().iter().find(|a| a["label"] == "QQ·工作").unwrap()["id"].as_str().unwrap().to_string();
    let mut s = c.get("/v1/ai/settings").await;
    s["account_policies"][qq.as_str()] = json!("off");
    s["notify_mode"] = json!("loud");
    let saved = c.put("/v1/ai/settings", s).await;
    assert_eq!(saved["notify_mode"], "normal", "unknown modes fall back");
    assert_eq!(saved["account_policies"][qq.as_str()], "off");
    let (status, lines) = c.stream(&format!("/v1/ai/messages/{notes}/summary"), json!({"force": true})).await;
    assert_eq!(status, 400, "{lines:?}");
    assert_eq!(lines[0]["code"], "ai_refused");
    assert_eq!(lines[0]["error"], "这个账户已关闭 AI");
    let (_, done) = c.generate("/v1/ai/ask", json!({"question": "王经理的会议纪要让我做什么？"})).await;
    assert!(done["sources"].as_array().unwrap().iter().all(|src| src["message_id"] != notes), "{done}");
    let mut s = c.get("/v1/ai/settings").await;
    s["account_policies"][qq.as_str()] = json!("local_only");
    s["quick_model"] = json!({"provider": "remote", "model": "gpt-4o-mini"});
    c.put("/v1/ai/settings", s).await;
    let (status, lines) = c.stream(&format!("/v1/ai/messages/{notes}/summary"), json!({"force": true})).await;
    assert_eq!(status, 400, "{lines:?}");
    assert_eq!(lines[0]["error"], "这个账户只允许使用本地模型");
    // The fake server on 127.0.0.1 counts as local.
    let mut s = c.get("/v1/ai/settings").await;
    s["quick_model"] = json!({"provider": "fake", "model": "fake-mini"});
    c.put("/v1/ai/settings", s).await;
    let (_, done) = c.generate(&format!("/v1/ai/messages/{notes}/summary"), json!({"force": true})).await;
    assert!(done["text"].as_str().unwrap().contains("一句话"), "{done}");

    // Demo data of the second batch.
    let demo = c.post("/v1/ai/demo", json!({})).await;
    let phishing = demo["phishing_message_id"].as_i64().expect("phishing message");
    let ins = c.get(&format!("/v1/ai/messages/{phishing}/insight")).await;
    assert_eq!(ins["risk"], "phishing");
    let events = c.post(&format!("/v1/ai/messages/{}/events", demo["events_message_id"].as_i64().unwrap()), json!({})).await;
    assert_eq!(events["cached"], true);
    assert_eq!(events["events"][0]["title"], "西山爬山");
    let reminders = c.get("/v1/ai/reminders").await;
    let list = reminders.as_array().unwrap();
    assert!(list.iter().any(|r| r["kind"] == "follow_up" && r["message"]["id"] == demo["followup_message_id"]), "{reminders}");
    assert!(list.iter().any(|r| r["message"]["id"] == phishing && r["insight"]["risk"] == "phishing"), "{reminders}");
    assert_eq!(c.get("/v1/ai/settings").await["mcp_enabled"], true);
    let servers = c.get("/v1/ai/mcp-servers").await;
    assert!(servers.as_array().unwrap().iter().any(|s| s["id"] == "demo-filesystem" && s["enabled"] == false));
    let usage = c.get("/v1/ai/usage?days=7").await;
    assert_eq!(usage["days"].as_array().unwrap().len(), 7);
    assert!(usage["days"].as_array().unwrap().iter().all(|d| d["calls"].as_u64().unwrap() > 0), "{usage}");
    assert!(usage["by_feature"].as_array().unwrap().len() >= 5);
}

// ---- built-in MCP server ----------------------------------------------------------------

async fn mcp_post(http: &reqwest::Client, base: &str, token: Option<&str>, body: Value) -> (u16, Value) {
    let mut req = http.post(format!("{base}/mcp")).json(&body);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    let v = if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) };
    (status, v)
}

fn rpc(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn tool_call(id: i64, name: &str, args: Value) -> Value {
    rpc(id, "tools/call", json!({"name": name, "arguments": args}))
}

/// Text of a `tools/call` result.
fn tool_text(v: &Value) -> String {
    v["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("no tool text: {v}")).to_string()
}

/// The JSON object at the end of a tool text (`… {"draft_id":"…"}`).
fn trailing_json(text: &str) -> Value {
    let start = text.find('{').unwrap_or_else(|| panic!("no JSON in {text}"));
    serde_json::from_str(&text[start..]).unwrap()
}

/// Calls a tool in the background (it waits for a confirmation).
fn spawn_tool(c: &Ctx, token: &str, id: i64, name: &str, args: Value) -> tokio::task::JoinHandle<(u16, Value)> {
    let (http, base, token, body) = (c.http.clone(), c.base.clone(), token.to_string(), tool_call(id, name, args));
    tokio::spawn(async move { mcp_post(&http, &base, Some(token.as_str()), body).await })
}

async fn pending_confirmation(c: &Ctx) -> Value {
    let list = c.wait_for("/v1/ai/confirm", |v| v.as_array().is_some_and(|a| !a.is_empty())).await;
    list[0].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_server_and_confirmations() {
    // A short wait for the timeout case; no other test asks for confirmations.
    unsafe { std::env::set_var("MAILBOX_AI_CONFIRM_TIMEOUT_SECS", "5") };
    let c = setup(true).await;
    let info = c.get("/v1/ai/mcp").await;
    assert_eq!(info["enabled"], false);
    assert_eq!(info["url"], format!("{}/mcp", c.base), "the real port");
    assert_eq!(info["stdio_command"], "");
    let token = info["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 64);
    assert_eq!(c.get("/v1/ai/mcp").await["token"], token.as_str(), "the token is kept");

    let init = rpc(1, "initialize", json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
    // Off: 403 with a JSON-RPC error.
    let (status, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), init.clone()).await;
    assert_eq!(status, 403, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("MCP 未启用"), "{v}");
    let mut s = c.get("/v1/ai/settings").await;
    s["mcp_enabled"] = json!(true);
    c.put("/v1/ai/settings", s).await;

    // Tokens: none or a wrong one → 401; the MCP token and the API token both work.
    assert_eq!(mcp_post(&c.http, &c.base, None, init.clone()).await.0, 401);
    assert_eq!(mcp_post(&c.http, &c.base, Some("wrong"), init.clone()).await.0, 401);
    let (status, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), init.clone()).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["id"], 1);
    assert_eq!(v["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(v["result"]["serverInfo"]["name"], "mailbox-ai");
    assert!(v["result"]["capabilities"]["tools"].is_object());
    assert!(v["result"]["instructions"].as_str().unwrap().contains("确认"));
    let (status, v) = mcp_post(&c.http, &c.base, Some(c.token.as_str()), rpc(2, "initialize", json!({"protocolVersion": "1999-01-01"}))).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
    // Notifications: 202 without a body. GET: 405. Unknown method: -32601.
    let (status, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await;
    assert_eq!((status, v), (202, Value::Null));
    let get = c.http.get(format!("{}/mcp", c.base)).bearer_auth(&token).send().await.unwrap();
    assert_eq!(get.status().as_u16(), 405);
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), rpc(3, "resources/list", json!({}))).await;
    assert_eq!(v["error"]["code"], -32601, "{v}");
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), rpc(4, "ping", json!({}))).await;
    assert_eq!(v["result"], json!({}));

    // tools/list
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), rpc(5, "tools/list", json!({}))).await;
    let tools = v["result"]["tools"].as_array().unwrap().clone();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in [
        "list_accounts", "search_emails", "list_unread", "read_email", "read_thread", "get_pending", "draft_email", "reply_email",
        "send_email", "mark_emails", "delete_emails",
    ] {
        assert!(names.contains(&n), "missing tool {n}");
    }
    assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object" && !t["description"].as_str().unwrap().is_empty()));

    // Reading tools.
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), tool_call(6, "search_emails", json!({"query": "纪要"}))).await;
    assert_eq!(v["result"]["isError"], false, "{v}");
    let text = tool_text(&v);
    assert!(text.contains("Q4 产品规划评审会议纪要"), "{text}");
    let notes = message_id(&c, "纪要").await;
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), tool_call(7, "read_email", json!({"id": notes}))).await;
    let text = tool_text(&v);
    assert!(text.contains("王经理") && text.contains("周五前"), "{text}");
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), tool_call(8, "read_thread", json!({"id": notes.to_string()}))).await;
    assert!(tool_text(&v).contains("这个会话共"), "{v}");
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), tool_call(9, "list_accounts", json!({}))).await;
    assert!(tool_text(&v).contains("me@qq-demo.com"), "{v}");
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), tool_call(10, "no_such_tool", json!({}))).await;
    assert_eq!(v["error"]["code"], -32602, "{v}");
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), tool_call(11, "read_email", json!({}))).await;
    assert_eq!(v["result"]["isError"], true, "{v}");

    // Organising without confirmation.
    let doc = message_id(&c, "接口文档").await;
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), tool_call(12, "mark_emails", json!({"ids": [doc], "action": "star"}))).await;
    assert_eq!(v["result"]["isError"], false, "{v}");
    assert!(c.engine.db.message(doc).unwrap().summary.flags.flagged);
    let (_, v) = mcp_post(
        &c.http,
        &c.base,
        Some(token.as_str()),
        tool_call(13, "draft_email", json!({"to": ["李四 <lisi@corp-demo.cn>"], "subject": "MCP 草稿", "body": "第一段\n\n第二段"})),
    )
    .await;
    let draft_id = trailing_json(&tool_text(&v))["draft_id"].as_str().unwrap().to_string();
    let draft = c.get(&format!("/v1/drafts/{draft_id}")).await;
    assert_eq!(draft["subject"], "MCP 草稿");
    assert!(draft["html"].as_str().unwrap().starts_with("<div>第一段</div>"), "{draft}");
    assert_eq!(draft["to"][0]["name"], "李四");
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), tool_call(14, "reply_email", json!({"id": doc, "body": "收到，谢谢。"}))).await;
    let reply_id = trailing_json(&tool_text(&v))["draft_id"].as_str().unwrap().to_string();
    let reply = c.get(&format!("/v1/drafts/{reply_id}")).await;
    assert_eq!(reply["source_message_id"], doc);
    assert!(reply["html"].as_str().unwrap().starts_with("<div>收到，谢谢。</div>"), "{reply}");

    // send_email, approved in Mailbox AI.
    let mut events = c.engine.events.subscribe();
    let call = spawn_tool(&c, &token, 15, "send_email", json!({"to": ["lisi@corp-demo.cn"], "subject": "MCP 测试", "body": "你好\n\n这是测试。"}));
    let conf = pending_confirmation(&c).await;
    let conf_id = conf["id"].as_str().unwrap().to_string();
    assert_eq!(conf["kind"], "send");
    assert!(conf["detail"].as_str().unwrap().contains("lisi@corp-demo.cn"), "{conf}");
    assert!(conf["title"].as_str().unwrap().contains("MCP 测试"));
    assert!(conf["draft_id"].as_str().is_some());
    assert!(conf["expires_at"].as_i64().unwrap() > conf["created_at"].as_i64().unwrap());
    let mut saw_event = false;
    while let Ok(ev) = events.try_recv() {
        if let Event::AiConfirm { id, kind, .. } = ev {
            saw_event |= id == conf_id && kind == "send";
        }
    }
    assert!(saw_event);
    c.post(&format!("/v1/ai/confirm/{conf_id}"), json!({"approve": true})).await;
    let (status, v) = call.await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(v["result"]["isError"], false, "{v}");
    assert!(tool_text(&v).starts_with("已发送"), "{v}");
    let outbox = c.get("/v1/outbox").await;
    assert!(outbox.as_array().unwrap().iter().any(|o| o["subject"] == "MCP 测试"), "{outbox}");
    let (status, _) = c.send(reqwest::Method::POST, &format!("/v1/ai/confirm/{conf_id}"), Some(json!({"approve": true}))).await;
    assert_eq!(status, 404, "a decision counts once");

    // send_email nobody answers: times out (MAILBOX_AI_CONFIRM_TIMEOUT_SECS), the draft stays.
    let started = std::time::Instant::now();
    let call = spawn_tool(&c, &token, 16, "send_email", json!({"to": "lisi@corp-demo.cn", "subject": "无人确认", "body": "超时测试"}));
    let conf = pending_confirmation(&c).await;
    let kept = conf["draft_id"].as_str().unwrap().to_string();
    let (_, v) = call.await.unwrap();
    let text = tool_text(&v);
    assert!(text.contains("等待确认超时") && text.contains(&kept), "{text}");
    assert_eq!(v["result"]["isError"], false);
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert!(c.get("/v1/ai/confirm").await.as_array().unwrap().is_empty());
    assert_eq!(c.get(&format!("/v1/drafts/{kept}")).await["subject"], "无人确认");

    // delete_emails: refused, then approved.
    let call = spawn_tool(&c, &token, 17, "delete_emails", json!({"ids": [doc]}));
    let conf = pending_confirmation(&c).await;
    assert_eq!(conf["kind"], "delete");
    assert_eq!(conf["message_ids"], json!([doc]));
    c.post(&format!("/v1/ai/confirm/{}", conf["id"].as_str().unwrap()), json!({"approve": false})).await;
    let (_, v) = call.await.unwrap();
    assert!(tool_text(&v).contains("拒绝"), "{v}");
    assert_ne!(c.engine.db.message(doc).unwrap().folder_role, FolderRole::Trash);
    let call = spawn_tool(&c, &token, 18, "delete_emails", json!({"ids": [doc]}));
    let conf = pending_confirmation(&c).await;
    c.post(&format!("/v1/ai/confirm/{}", conf["id"].as_str().unwrap()), json!({"approve": true})).await;
    let (_, v) = call.await.unwrap();
    assert!(tool_text(&v).contains("废纸篓"), "{v}");
    assert_eq!(c.engine.db.message(doc).unwrap().folder_role, FolderRole::Trash);

    // Account policies apply to MCP clients (they count as non-local models).
    let accounts = c.get("/v1/accounts").await;
    let qq = accounts.as_array().unwrap().iter().find(|a| a["label"] == "QQ·工作").unwrap()["id"].as_str().unwrap().to_string();
    let mut s = c.get("/v1/ai/settings").await;
    s["account_policies"][qq.as_str()] = json!("local_only");
    c.put("/v1/ai/settings", s).await;
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), tool_call(19, "read_email", json!({"id": notes}))).await;
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert!(tool_text(&v).contains("只允许使用本地模型"), "{v}");
    let (_, v) = mcp_post(&c.http, &c.base, Some(token.as_str()), tool_call(20, "search_emails", json!({"query": "纪要"}))).await;
    assert!(!tool_text(&v).contains("Q4 产品规划评审会议纪要"), "{v}");

    // A new token replaces the old one.
    let fresh = c.post("/v1/ai/mcp/token", json!({})).await["token"].as_str().unwrap().to_string();
    assert_ne!(fresh, token);
    assert_eq!(mcp_post(&c.http, &c.base, Some(token.as_str()), rpc(21, "ping", json!({}))).await.0, 401);
    assert_eq!(mcp_post(&c.http, &c.base, Some(fresh.as_str()), rpc(22, "ping", json!({}))).await.0, 200);
}

// ---- external MCP servers and the tool loop ---------------------------------------------

#[cfg(unix)]
const FAKE_STDIO_MCP: &str = r#"#!/usr/bin/env python3
import json, sys

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

sys.stderr.write("fake mcp starting\n")
while True:
    raw = sys.stdin.buffer.readline()
    if not raw:
        break
    line = raw.decode("utf-8").strip()
    if not line:
        continue
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {"protocolVersion": msg["params"]["protocolVersion"], "capabilities": {"tools": {}},
                  "serverInfo": {"name": "fake-stdio", "version": "1"}}
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": "listing"}})
        result = {"tools": [{"name": "echo", "description": "Echo back the text",
                             "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}]}
    elif method == "tools/call":
        args = msg["params"].get("arguments") or {}
        result = {"content": [{"type": "text", "text": "echo: " + str(args.get("text", ""))}], "isError": False}
    else:
        send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": "not found"}})
        continue
    send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
"#;

#[derive(Clone, Default)]
struct McpFake {
    sessions: Arc<Mutex<Vec<String>>>,
}

/// Streamable HTTP MCP server: needs `Authorization: Bearer mcp-secret`, hands out a session
/// on `initialize` and requires it afterwards, answers `tools/list` as SSE.
async fn fake_mcp(State(f): State<McpFake>, headers: HeaderMap, body: Bytes) -> Response {
    if headers.get("authorization").and_then(|h| h.to_str().ok()) != Some("Bearer mcp-secret") {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(id) = v.get("id").cloned() else { return StatusCode::ACCEPTED.into_response() };
    let method = v["method"].as_str().unwrap_or("").to_string();
    if method == "initialize" {
        let resp = json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                          "serverInfo": {"name": "fake-http", "version": "1"}}});
        return Response::builder()
            .header("content-type", "application/json")
            .header("mcp-session-id", "sess-1")
            .body(Body::from(resp.to_string()))
            .unwrap();
    }
    let session = headers.get("mcp-session-id").and_then(|h| h.to_str().ok()).unwrap_or("").to_string();
    f.sessions.lock().unwrap().push(session.clone());
    if session != "sess-1" {
        return (StatusCode::BAD_REQUEST, "missing session").into_response();
    }
    match method.as_str() {
        "tools/list" => {
            let note = json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}});
            let resp = json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [{"name": "lookup", "description": "Look something up",
                              "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}}]}});
            sse(chunked(&format!("event: message\ndata: {note}\n\nevent: message\ndata: {resp}\n\n"), 9))
        }
        "tools/call" => {
            axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "lookup ok"}], "isError": false}}))
                .into_response()
        }
        _ => axum::Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "not found"}})).into_response(),
    }
}

async fn start_fake_mcp() -> (String, McpFake) {
    let fake = McpFake::default();
    let app = Router::new().route("/mcp", post(fake_mcp)).with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), fake)
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_mcp_tools() {
    let (fake_base, fake) = start_fake().await;
    let (mcp_base, mcp_fake) = start_fake_mcp().await;
    let bin = std::env::temp_dir().join(format!("mailbox-ai-mcp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&bin).unwrap();
    let script = write_script(&bin, "fake_mcp.py", FAKE_STDIO_MCP);
    let c = setup(false).await;
    c.put(
        "/v1/ai/providers/toolfake",
        json!({"name": "Tool Fake", "kind": "openai", "base_url": format!("{fake_base}/tools/v1"), "enabled": true,
               "models": [{"id": "fake-tools"}], "api_key": "sk-tools"}),
    )
    .await;
    let mut s = c.get("/v1/ai/settings").await;
    s["default_model"] = json!({"provider": "toolfake", "model": "fake-tools"});
    c.put("/v1/ai/settings", s).await;

    // Server configs: validation, then a stdio and an HTTP server that both work.
    let (status, _) = c.send(reqwest::Method::POST, "/v1/ai/mcp-servers", Some(json!({"name": "没有命令", "transport": "stdio"}))).await;
    assert_eq!(status, 400);
    let (status, _) = c.send(reqwest::Method::POST, "/v1/ai/mcp-servers", Some(json!({"name": "坏地址", "transport": "http", "url": "ftp://x"}))).await;
    assert_eq!(status, 400);
    let stdio = c
        .post("/v1/ai/mcp-servers", json!({"name": "Fake Stdio", "transport": "stdio", "command": "python3", "args": [script.to_string_lossy()]}))
        .await;
    let stdio_id = stdio["id"].as_str().unwrap().to_string();
    assert_eq!(stdio["enabled"], true);
    let t = c.post(&format!("/v1/ai/mcp-servers/{stdio_id}/test"), json!({})).await;
    assert_eq!(t["ok"], true, "{t}");
    assert_eq!(t["tools"], json!([{"name": "echo", "description": "Echo back the text"}]));
    let http = c
        .post(
            "/v1/ai/mcp-servers",
            json!({"name": "Fake HTTP", "transport": "http", "url": format!("{mcp_base}/mcp"), "headers": {"Authorization": "Bearer mcp-secret"}}),
        )
        .await;
    let http_id = http["id"].as_str().unwrap().to_string();
    let t = c.post(&format!("/v1/ai/mcp-servers/{http_id}/test"), json!({})).await;
    assert_eq!(t["ok"], true, "{t}");
    assert_eq!(t["tools"][0]["name"], "lookup");
    assert!(mcp_fake.sessions.lock().unwrap().iter().all(|s| s == "sess-1"), "the session id is sent back");
    let wrong = c.post("/v1/ai/mcp-servers", json!({"name": "Wrong", "enabled": false, "transport": "http", "url": format!("{mcp_base}/mcp")})).await;
    let t = c.post(&format!("/v1/ai/mcp-servers/{}/test", wrong["id"].as_str().unwrap()), json!({})).await;
    assert_eq!(t["ok"], false);
    assert!(t["error"].as_str().unwrap().contains("401"), "{t}");
    assert_eq!(c.get("/v1/ai/mcp-servers").await.as_array().unwrap().len(), 3);

    // The tool loop: the model calls echo on the stdio server, then answers with the result.
    let (deltas, done) = c.generate("/v1/ai/chat", json!({"messages": [{"role": "user", "content": "请调用工具"}], "tools": true})).await;
    assert_eq!(done["text"], "工具说：echo: 你好", "{done}");
    assert_eq!(deltas, "工具说：echo: 你好");
    assert_eq!(done["tool_calls"], json!([{"server": "Fake Stdio", "server_id": stdio_id, "tool": "echo", "ok": true}]));
    let requests = fake.requests.lock().unwrap().clone();
    let first = requests.iter().find(|r| r["tools"].is_array()).expect("a request with tools").clone();
    let names: Vec<String> = first["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect();
    assert!(names.contains(&"FakeStdio__echo".to_string()) && names.contains(&"FakeHTTP__lookup".to_string()), "{names:?}");
    assert!(!names.iter().any(|n| n.starts_with("Wrong")), "disabled servers are not used");
    assert_eq!(first["stream"], false);
    assert!(
        requests.iter().any(|r| r["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool" && m["content"] == "echo: 你好")),
        "the tool result goes back to the model"
    );
    let usage = c.get("/v1/ai/usage?days=1").await;
    assert!(usage["by_feature"].as_array().unwrap().iter().any(|f| f["feature"] == "mcp" && f["calls"] == 2), "{usage}");

    // Claude Code gets the servers as --mcp-config.
    let claude = write_script(
        &bin,
        "claude",
        r#"#!/bin/sh
DIR="$(cd "$(dirname "$0")" && pwd)"
printf '%s\n' "$*" > "$DIR/claude.args"
prev=""
for a in "$@"; do
  if [ "$prev" = "--mcp-config" ]; then cp "$a" "$DIR/claude.mcp.json"; fi
  prev="$a"
done
cat > /dev/null
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu1","name":"mcp__FakeStdio__echo","input":{"text":"hi"}}]}}'
echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"echo: hi","is_error":false}]}}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"完成"}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"完成","usage":{"input_tokens":5,"output_tokens":3}}'
"#,
    );
    c.put(
        "/v1/ai/providers/claude_code",
        json!({"name": "Claude Code", "kind": "claude_code", "base_url": claude.to_string_lossy(), "enabled": true, "models": [{"id": "default"}]}),
    )
    .await;
    let chat = |provider: &str| json!({"model": {"provider": provider, "model": "default"}, "messages": [{"role": "user", "content": "用工具"}], "tools": true});
    let (_, done) = c.generate("/v1/ai/chat", chat("claude_code")).await;
    assert_eq!(done["text"], "完成", "{done}");
    assert_eq!(done["tool_calls"], json!([{"server": "Fake Stdio", "server_id": stdio_id, "tool": "echo", "ok": true}]));
    let args = std::fs::read_to_string(bin.join("claude.args")).unwrap();
    assert!(args.contains("--max-turns 8") && args.contains("--mcp-config "), "{args}");
    assert!(args.contains("mcp__FakeStdio") && args.contains("mcp__FakeHTTP") && !args.contains("mcp__Wrong"), "{args}");
    assert!(args.contains("--disallowedTools Bash Edit Write Read"), "{args}");
    let config: Value = serde_json::from_str(&std::fs::read_to_string(bin.join("claude.mcp.json")).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["FakeStdio"]["command"], "python3");
    assert_eq!(config["mcpServers"]["FakeHTTP"]["headers"]["Authorization"], "Bearer mcp-secret");

    // Codex: no external tools.
    let codex = write_script(
        &bin,
        "codex",
        r#"#!/bin/sh
out=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--output-last-message" ]; then out="$a"; fi
  prev="$a"
done
cat > /dev/null
echo '{"type":"item.completed","item":{"id":"i0","type":"agent_message","text":"Codex 回复"}}'
echo '{"type":"turn.completed","usage":{"input_tokens":4,"output_tokens":2}}'
printf '%s' 'Codex 回复' > "$out"
"#,
    );
    c.put(
        "/v1/ai/providers/codex",
        json!({"name": "Codex", "kind": "codex", "base_url": codex.to_string_lossy(), "enabled": true, "models": [{"id": "default"}]}),
    )
    .await;
    let (_, done) = c.generate("/v1/ai/chat", chat("codex")).await;
    assert_eq!(done["text"], "Codex 回复");
    assert_eq!(done["tools_note"], "Codex 暂不支持外部工具");
    assert_eq!(done["tool_calls"], json!([]));

    // Deleting.
    c.send(reqwest::Method::DELETE, &format!("/v1/ai/mcp-servers/{http_id}"), None).await;
    let (status, _) = c.send(reqwest::Method::DELETE, &format!("/v1/ai/mcp-servers/{http_id}"), None).await;
    assert_eq!(status, 404);
    assert_eq!(c.get("/v1/ai/mcp-servers").await.as_array().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&bin);
}

// ---- keys, usage, follow-ups, vacation replies, batch unsubscribe ----------------------------

fn write_eml(dir: &std::path::Path, name: &str, headers: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let raw = format!("{headers}MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n");
    std::fs::write(dir.join(name), raw).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keys_usage_followups_and_vacation() {
    let (fake_base, fake) = start_fake().await;
    let c = setup(false).await;
    let mut st = c.get("/v1/settings").await;
    st["undo_send_secs"] = json!(120);
    c.put("/v1/settings", st).await;

    // Two keys: "key-a" is rejected (401) and the next one takes over; requests take turns.
    let p = c
        .put(
            "/v1/ai/providers/rot",
            json!({"name": "Rotating", "kind": "openai", "base_url": format!("{fake_base}/keys/v1"), "enabled": true,
                   "models": [{"id": "fake-mini"}], "api_key": "key-a, key-b"}),
        )
        .await;
    assert_eq!(p["key_count"], 2);
    assert_eq!(p["has_key"], true);
    assert!(!p.to_string().contains("key-b"), "keys are never returned");
    let mut s = c.get("/v1/ai/settings").await;
    s["default_model"] = json!({"provider": "rot", "model": "fake-mini"});
    s["quick_model"] = json!({"provider": "rot", "model": "fake-mini"});
    c.put("/v1/ai/settings", s).await;
    for _ in 0..2 {
        let (_, done) = c.generate("/v1/ai/chat", json!({"messages": [{"role": "user", "content": "你好"}]})).await;
        assert_eq!(done["text"], PLAIN_ANSWER);
    }
    let auth = fake.auth.lock().unwrap().clone();
    assert_eq!(auth, vec!["Bearer key-a", "Bearer key-b", "Bearer key-b"]);
    let last = fake.requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last["stream_options"]["include_usage"], true);

    // Usage: per day, model and feature, with the tokens of the usage chunk.
    let usage = c.get("/v1/ai/usage?days=7").await;
    let days = usage["days"].as_array().unwrap();
    assert_eq!(days.len(), 7);
    let today = days.last().unwrap();
    assert_eq!(today["calls"], 2, "{usage}");
    assert_eq!(today["input_tokens"], 22);
    assert_eq!(today["output_tokens"], 14);
    assert_eq!(today["errors"], 0);
    assert_eq!(days[0]["calls"], 0);
    assert_eq!(usage["by_model"][0]["provider"], "rot");
    assert_eq!(usage["by_model"][0]["model"], "fake-mini");
    assert_eq!(usage["by_feature"][0]["feature"], "chat");
    assert_eq!(usage["by_feature"][0]["calls"], 2);

    // A local (POP-style) account with a sent message that got no answer for five days.
    let acc = c
        .engine
        .db
        .insert_account(Account {
            kind: AccountKind::Pop3,
            email: "me@pop-demo.cn".into(),
            name: "我".into(),
            label: "POP".into(),
            provider: Some("demo".into()),
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    let inbox = c.engine.db.ensure_local_folder(&acc.id, FolderRole::Inbox, "收件箱").unwrap();
    let sent = c.engine.db.ensure_local_folder(&acc.id, FolderRole::Sent, "已发送").unwrap();
    let five_days_ago = (chrono::Utc::now() - chrono::Duration::days(5)).to_rfc2822();
    let now_date = chrono::Utc::now().to_rfc2822();
    write_eml(
        &c.dir.join("sent"),
        "1.eml",
        &format!(
            "From: =?UTF-8?B?5oiR?= <me@pop-demo.cn>\r\nTo: =?UTF-8?B?546L5LqU?= <wangwu@friend-demo.cn>\r\nSubject: =?UTF-8?B?5oql5Lu356Gu6K6k?=\r\n\
             Message-ID: <quote1@test>\r\nDate: {five_days_ago}\r\n"
        ),
        "王总，报价单已经发您了，请确认是否可以？",
    );
    assert_eq!(c.engine.import_messages(sent, &c.dir.join("sent")).await.unwrap(), 1);
    let sent_id = c.engine.db.list_messages(&ListQuery { mailbox: Mailbox::Folder(sent), limit: 10, ..Default::default() }).unwrap()[0].id;
    let mut events = c.engine.events.subscribe();
    assert_eq!(c.engine.ai_check_followups().await.unwrap(), 1);
    let mut reminder_title = String::new();
    while let Ok(ev) = events.try_recv() {
        if let Event::AiReminder { message_id, title, .. } = ev {
            if message_id == sent_id {
                reminder_title = title;
            }
        }
    }
    assert!(reminder_title.contains("POP") && reminder_title.contains("王五"), "{reminder_title}");
    assert_eq!(c.engine.ai_check_followups().await.unwrap(), 0, "judged once");
    let reminders = c.get("/v1/ai/reminders").await;
    let item = reminders.as_array().unwrap().iter().find(|r| r["kind"] == "follow_up").expect("follow-up listed").clone();
    assert_eq!(item["message"]["id"], sent_id);
    assert_eq!(item["message"]["subject"], "报价确认");
    assert_eq!(item["insight"]["one_line"], "5 天没收到 王五 的回复");
    assert_eq!(item["account_label"], "POP");
    // Done and undone.
    c.post(&format!("/v1/ai/reminders/{sent_id}/done"), json!({"done": true})).await;
    assert!(c.get("/v1/ai/reminders").await.as_array().unwrap().iter().all(|r| r["kind"] != "follow_up"));
    c.post(&format!("/v1/ai/reminders/{sent_id}/done"), json!({"done": false})).await;
    assert!(c.get("/v1/ai/reminders").await.as_array().unwrap().iter().any(|r| r["kind"] == "follow_up"));
    // The answer arrives: the follow-up goes away.
    write_eml(
        &c.dir.join("reply"),
        "1.eml",
        &format!(
            "From: =?UTF-8?B?546L5LqU?= <wangwu@friend-demo.cn>\r\nTo: me@pop-demo.cn\r\nSubject: Re: =?UTF-8?B?5oql5Lu356Gu6K6k?=\r\n\
             Message-ID: <reply1@test>\r\nIn-Reply-To: <quote1@test>\r\nReferences: <quote1@test>\r\nDate: {now_date}\r\n"
        ),
        "可以，就按这个报价。",
    );
    assert_eq!(c.engine.import_messages(inbox, &c.dir.join("reply")).await.unwrap(), 1);
    let reply_id = c.engine.db.list_messages(&ListQuery { mailbox: Mailbox::Folder(inbox), limit: 10, ..Default::default() }).unwrap()[0].id;
    c.engine.ai_process_new_mail(reply_id).await;
    assert!(c.get("/v1/ai/reminders").await.as_array().unwrap().iter().all(|r| r["kind"] != "follow_up"));

    // Vacation replies: no AI, sent directly, once per sender while the vacation lasts.
    let now = chrono_now();
    let (status, _) = c.send(reqwest::Method::POST, "/v1/ai/autoreply", Some(json!({"kind": "vacation", "reply_text": " "}))).await;
    assert_eq!(status, 400);
    let (status, _) = c
        .send(reqwest::Method::POST, "/v1/ai/autoreply", Some(json!({"kind": "vacation", "reply_text": "x", "start": now, "end": now - 10})))
        .await;
    assert_eq!(status, 400);
    let rule = c
        .post("/v1/ai/autoreply", json!({"name": "休假", "kind": "vacation", "reply_text": "我在休假，10 月 30 日回来。", "start": now - 3600, "end": now + 86_400}))
        .await;
    assert_eq!(rule["mode"], "send");
    c.post("/v1/ai/autoreply", json!({"name": "过期的休假", "kind": "vacation", "reply_text": "旧的", "start": now - 10 * 86_400, "end": now - 5 * 86_400}))
        .await;
    let dir = c.dir.join("vacation");
    for (i, (from, subject, mid)) in
        [("Zhao Liu <zhaoliu@friend-demo.cn>", "Lunch?", "v1@test"), ("Zhao Liu <zhaoliu@friend-demo.cn>", "Again", "v2@test"), ("Shop <noreply@shop-demo.com>", "Order", "v3@test")]
            .iter()
            .enumerate()
    {
        write_eml(&dir, &format!("{i}.eml"), &format!("From: {from}\r\nTo: me@pop-demo.cn\r\nSubject: {subject}\r\nMessage-ID: <{mid}>\r\nDate: {now_date}\r\n"), "你好");
    }
    write_eml(
        &dir,
        "9.eml",
        &format!(
            "From: Shop News <news@shop-demo.com>\r\nTo: me@pop-demo.cn\r\nSubject: Weekly deals\r\nList-Unsubscribe: <mailto:unsub@shop-demo.com>\r\n\
             Message-ID: <bulk1@test>\r\nDate: {now_date}\r\n"
        ),
        "本周特价。",
    );
    assert_eq!(c.engine.import_messages(inbox, &dir).await.unwrap(), 4);
    let local = c.engine.db.list_messages(&ListQuery { mailbox: Mailbox::Folder(inbox), limit: 20, ..Default::default() }).unwrap();
    let id_of = |subject: &str| local.iter().find(|m| m.subject == subject).unwrap().id;
    let mut events = c.engine.events.subscribe();
    for subject in ["Lunch?", "Again", "Order", "Weekly deals"] {
        c.engine.ai_process_new_mail(id_of(subject)).await;
    }
    let outbox = c.get("/v1/outbox").await;
    let to_zhao: Vec<Value> = outbox.as_array().unwrap().iter().filter(|o| o["to"][0]["email"] == "zhaoliu@friend-demo.cn").cloned().collect();
    assert_eq!(to_zhao.len(), 1, "once per sender: {outbox}");
    assert_eq!(to_zhao[0]["subject"], "自动回复：Lunch?");
    assert!(outbox.as_array().unwrap().iter().all(|o| o["to"][0]["email"] != "noreply@shop-demo.com" && o["to"][0]["email"] != "news@shop-demo.com"));
    let cancelled = c.post(&format!("/v1/outbox/{}/cancel", to_zhao[0]["id"].as_str().unwrap()), json!({})).await;
    assert_eq!(cancelled["draft"]["auto_submitted"], true, "{cancelled}");
    assert!(cancelled["draft"]["html"].as_str().unwrap().contains("我在休假"), "{cancelled}");
    let mut vacation_events = 0;
    while let Ok(ev) = events.try_recv() {
        if let Event::AiAutoreply { rule_name, sent, draft_id, .. } = ev {
            assert_eq!((rule_name.as_str(), sent, draft_id), ("休假", true, None));
            vacation_events += 1;
        }
    }
    assert_eq!(vacation_events, 1);

    // Batch unsubscribe (no AI): List-Unsubscribe mailto is handled, the rest is reported.
    let r = c.post("/v1/ai/batch", json!({"ids": [id_of("Weekly deals"), id_of("Order")], "op": "unsubscribe"})).await;
    let job = c.wait_for(&format!("/v1/ai/jobs/{}", r["job_id"].as_str().unwrap()), |j| j["status"] != "running").await;
    assert_eq!(job["status"], "done", "{job}");
    let items = job["items"].as_array().unwrap();
    let bulk = items.iter().find(|i| i["subject"] == "Weekly deals").unwrap();
    assert_eq!(bulk["output"], "已退订", "{job}");
    let plain = items.iter().find(|i| i["subject"] == "Order").unwrap();
    assert!(plain["error"].as_str().unwrap().contains("退订"), "{job}");
    assert!(job["summary"].as_str().unwrap().starts_with("已退订 1 封"), "{job}");
    let outbox = c.get("/v1/outbox").await;
    assert!(outbox.as_array().unwrap().iter().any(|o| o["to"][0]["email"] == "unsub@shop-demo.com"), "{outbox}");
}

//! AI features against fake providers, no network and no real models:
//! - an OpenAI compatible and an Anthropic server (axum on 127.0.0.1:0) answering with SSE
//!   split into small chunks,
//! - fake `claude` / `codex` shell scripts selected through the provider's `base_url`.

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
    s.push_str("data: [DONE]\n\n");
    sse(chunked(&s, 7))
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

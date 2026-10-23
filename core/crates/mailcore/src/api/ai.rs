//! `/v1/ai/*` routes (contract: `docs/AI.md` §4). Generating endpoints answer with an
//! NDJSON stream: `{"delta":…}` lines, then `{"done":true,"text","model",…}` or `{"error":…}`.
//! Errors found before the stream starts are ordinary `{"error","code"}` HTTP errors.

use super::{ApiError, ApiResult, AppState};
use crate::ai::compose::ComposeRequest;
use crate::ai::{AiLoop, AiProvider, AiProviderInput, AiSettings, AutoReplyRule, ChatMessage, ModelRef, StreamDone};
use crate::error::{Error, Result};
use crate::model::MessageId;
use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::future::Future;
use tokio::sync::mpsc;

type St = State<AppState>;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/ai/presets", get(presets))
        .route("/v1/ai/providers", get(providers))
        .route("/v1/ai/providers/{id}", put(save_provider).delete(delete_provider))
        .route("/v1/ai/providers/{id}/models", post(fetch_models))
        .route("/v1/ai/providers/{id}/test", post(test_provider))
        .route("/v1/ai/cli", get(cli_status))
        .route("/v1/ai/settings", get(get_settings).put(put_settings))
        .route("/v1/ai/chat", post(chat))
        .route("/v1/ai/compose", post(compose))
        .route("/v1/ai/messages/{id}/summary", get(get_summary).post(summary))
        .route("/v1/ai/messages/{id}/translate", post(translate))
        .route("/v1/ai/messages/{id}/replies", post(replies))
        .route("/v1/ai/messages/{id}/insight", get(get_insight).post(analyze))
        .route("/v1/ai/reminders", get(reminders))
        .route("/v1/ai/reminders/{id}/done", post(reminder_done))
        .route("/v1/ai/batch", post(batch))
        .route("/v1/ai/jobs/{id}", get(job))
        .route("/v1/ai/jobs/{id}/cancel", post(cancel_job))
        .route("/v1/ai/jobs/{id}/apply", post(apply_job))
        .route("/v1/ai/loops", get(loops).post(save_loop))
        .route("/v1/ai/loops/{id}", delete(delete_loop))
        .route("/v1/ai/loops/{id}/run", post(run_loop))
        .route("/v1/ai/loops/{id}/runs", get(loop_runs))
        .route("/v1/ai/loop-templates", get(loop_templates))
        .route("/v1/ai/autoreply", get(autoreplies).post(save_autoreply))
        .route("/v1/ai/autoreply/{id}", delete(delete_autoreply))
        .route("/v1/ai/ask", post(ask))
        .route("/v1/ai/demo", post(demo))
}

fn ok() -> Json<Value> {
    Json(json!({ "ok": true }))
}

/// JSON body; an empty body means "all defaults" (bodies are optional for most AI calls).
fn body<T: DeserializeOwned + Default>(b: &Bytes) -> ApiResult<T> {
    if b.iter().all(|c| c.is_ascii_whitespace()) {
        return Ok(T::default());
    }
    serde_json::from_slice(b).map_err(|e| ApiError(Error::Invalid(format!("请求格式错误：{e}"))))
}

fn line(v: &Value) -> String {
    let mut s = v.to_string();
    s.push('\n');
    s
}

/// Runs `make(tx)` in the background and streams its deltas as NDJSON. The last line is
/// `{"done":true,…}` or `{"error":…}`. When the client goes away the work is dropped.
fn ndjson<F, Fut>(make: F) -> Response
where
    F: FnOnce(mpsc::Sender<String>) -> Fut,
    Fut: Future<Output = Result<StreamDone>> + Send + 'static,
{
    let (line_tx, line_rx) = mpsc::channel::<String>(64);
    let (delta_tx, mut delta_rx) = mpsc::channel::<String>(256);
    let fut = make(delta_tx);
    tokio::spawn(async move {
        let mut fut = Box::pin(fut);
        let mut open = true;
        let result: Option<Result<StreamDone>> = loop {
            tokio::select! {
                biased;
                d = delta_rx.recv(), if open => match d {
                    Some(d) => {
                        if line_tx.send(line(&json!({ "delta": d }))).await.is_err() {
                            break None;
                        }
                    }
                    None => open = false,
                },
                r = &mut fut => break Some(r),
            }
        };
        // The client disconnected: dropping `fut` cancels the request / kills the CLI.
        let Some(result) = result else { return };
        while let Ok(d) = delta_rx.try_recv() {
            if line_tx.send(line(&json!({ "delta": d }))).await.is_err() {
                return;
            }
        }
        let last = match result {
            Ok(done) => {
                let mut obj = done.extra;
                obj.insert("done".into(), json!(true));
                obj.insert("text".into(), json!(done.text));
                obj.insert("model".into(), json!(done.model));
                Value::Object(obj)
            }
            Err(e) => json!({ "error": e.to_string() }),
        };
        let _ = line_tx.send(line(&last)).await;
    });
    let stream = futures::stream::unfold(line_rx, |mut rx| async move {
        let next = rx.recv().await;
        next.map(|l| (Ok::<String, std::io::Error>(l), rx))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ---- providers & settings ---------------------------------------------------------------

async fn presets() -> Json<Value> {
    Json(json!(crate::ai::providers::presets()))
}

async fn providers(State(st): St) -> Json<Vec<AiProvider>> {
    Json(st.engine.ai_providers())
}

async fn save_provider(State(st): St, Path(id): Path<String>, b: Bytes) -> ApiResult<Json<AiProvider>> {
    let input: AiProviderInput = body(&b)?;
    Ok(Json(st.engine.ai_save_provider(&id, input)?))
}

async fn delete_provider(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.ai_delete_provider(&id)?;
    Ok(ok())
}

async fn fetch_models(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(st.engine.ai_fetch_models(&id).await?)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TestBody {
    /// A model id, or a `ModelRef` object.
    model: Option<Value>,
}

async fn test_provider(State(st): St, Path(id): Path<String>, b: Bytes) -> ApiResult<Json<Value>> {
    let t: TestBody = body(&b)?;
    let model = match t.model {
        Some(Value::String(s)) => Some(s),
        Some(Value::Object(o)) => o.get("model").and_then(|m| m.as_str()).map(String::from),
        _ => None,
    };
    Ok(Json(st.engine.ai_test_provider(&id, model).await?))
}

async fn cli_status(State(st): St) -> Json<Value> {
    Json(crate::ai::cli::status(&st.engine).await)
}

async fn get_settings(State(st): St) -> Json<AiSettings> {
    Json(st.engine.ai_settings())
}

async fn put_settings(State(st): St, b: Bytes) -> ApiResult<Json<AiSettings>> {
    let s: AiSettings = body(&b)?;
    st.engine.save_ai_settings(&s)?;
    Ok(Json(st.engine.ai_settings()))
}

// ---- chat & writing ---------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct ChatBody {
    model: Option<ModelRef>,
    system: Option<String>,
    messages: Vec<ChatMessage>,
}

async fn chat(State(st): St, b: Bytes) -> ApiResult<Response> {
    let c: ChatBody = body(&b)?;
    if c.messages.iter().all(|m| m.content.trim().is_empty()) {
        return Err(ApiError(Error::Invalid("messages 不能为空".into())));
    }
    let model = st.engine.ai_model_or(c.model, false)?;
    let e = st.engine.clone();
    let (system, messages) = (c.system, c.messages);
    Ok(ndjson(move |tx| async move { e.ai_chat_stream(model, system, messages, Some(tx)).await }))
}

async fn compose(State(st): St, b: Bytes) -> ApiResult<Response> {
    let r: ComposeRequest = body(&b)?;
    let plan = st.engine.ai_compose_plan(&r).await?;
    let e = st.engine.clone();
    Ok(ndjson(move |tx| async move { e.ai_compose_run(plan, Some(tx)).await }))
}

// ---- reading ----------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(default)]
struct SummaryBody {
    thread: bool,
    force: bool,
}

impl Default for SummaryBody {
    fn default() -> Self {
        SummaryBody { thread: true, force: false }
    }
}

async fn summary(State(st): St, Path(id): Path<MessageId>, b: Bytes) -> ApiResult<Response> {
    let sb: SummaryBody = body(&b)?;
    let thread = sb.thread;
    let plan = st.engine.ai_summary_plan(id, thread, sb.force).await?;
    let e = st.engine.clone();
    Ok(ndjson(move |tx| async move { e.ai_summary_run(id, thread, plan, Some(tx)).await }))
}

#[derive(Deserialize)]
struct ThreadQuery {
    thread: Option<bool>,
}

async fn get_summary(State(st): St, Path(id): Path<MessageId>, Query(q): Query<ThreadQuery>) -> ApiResult<Json<Value>> {
    let c = st
        .engine
        .ai_cached_summary(id, q.thread.unwrap_or(true))?
        .ok_or_else(|| ApiError(Error::NotFound("这封邮件还没有摘要".into())))?;
    Ok(Json(json!({ "text": c.text, "model": c.model, "created_at": c.created_at })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TranslateBody {
    target_lang: Option<String>,
}

async fn translate(State(st): St, Path(id): Path<MessageId>, b: Bytes) -> ApiResult<Response> {
    let t: TranslateBody = body(&b)?;
    let plan = st.engine.ai_translate_plan(id, t.target_lang).await?;
    let e = st.engine.clone();
    Ok(ndjson(move |tx| async move { e.ai_translate_run(plan, Some(tx)).await }))
}

async fn replies(State(st): St, Path(id): Path<MessageId>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "suggestions": st.engine.ai_replies(id).await? })))
}

async fn get_insight(State(st): St, Path(id): Path<MessageId>) -> ApiResult<Json<Value>> {
    let ins = st.engine.ai_insight(id).ok_or_else(|| ApiError(Error::NotFound("这封邮件还没有智能分析结果".into())))?;
    Ok(Json(json!(ins)))
}

async fn analyze(State(st): St, Path(id): Path<MessageId>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(st.engine.ai_analyze(id).await?)))
}

async fn reminders(State(st): St) -> ApiResult<Json<Value>> {
    Ok(Json(json!(st.engine.ai_reminders()?)))
}

#[derive(Deserialize)]
#[serde(default)]
struct DoneBody {
    done: bool,
}

impl Default for DoneBody {
    fn default() -> Self {
        DoneBody { done: true }
    }
}

async fn reminder_done(State(st): St, Path(id): Path<MessageId>, b: Bytes) -> ApiResult<Json<Value>> {
    let d: DoneBody = body(&b)?;
    Ok(Json(json!(st.engine.ai_set_reminder_done(id, d.done)?)))
}

// ---- batch ------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct BatchBody {
    ids: Vec<MessageId>,
    op: String,
    instruction: Option<String>,
}

async fn batch(State(st): St, b: Bytes) -> ApiResult<Json<Value>> {
    let req: BatchBody = body(&b)?;
    let job_id = st.engine.ai_start_batch(req.ids, &req.op, req.instruction)?;
    Ok(Json(json!({ "job_id": job_id })))
}

async fn job(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let j = st.engine.ai_job(&id).ok_or_else(|| ApiError(Error::NotFound("任务不存在或已过期".into())))?;
    Ok(Json(json!(j)))
}

async fn cancel_job(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.ai_cancel_job(&id)?;
    Ok(ok())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApplyBody {
    message_ids: Vec<MessageId>,
}

async fn apply_job(State(st): St, Path(id): Path<String>, b: Bytes) -> ApiResult<Json<Value>> {
    let a: ApplyBody = body(&b)?;
    let applied = st.engine.ai_apply_job(&id, &a.message_ids).await?;
    Ok(Json(json!({ "ok": true, "applied": applied })))
}

// ---- loops ------------------------------------------------------------------------------

async fn loops(State(st): St) -> Json<Vec<AiLoop>> {
    Json(st.engine.ai_loops())
}

async fn save_loop(State(st): St, b: Bytes) -> ApiResult<Json<AiLoop>> {
    let l: AiLoop = body(&b)?;
    Ok(Json(st.engine.ai_save_loop(l)?))
}

async fn delete_loop(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.ai_delete_loop(&id)?;
    Ok(ok())
}

async fn run_loop(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "run_id": st.engine.ai_run_loop_now(&id)? })))
}

async fn loop_runs(State(st): St, Path(id): Path<String>) -> Json<Value> {
    Json(json!(st.engine.ai_loop_runs(&id)))
}

async fn loop_templates() -> Json<Vec<AiLoop>> {
    Json(crate::ai::loops::templates())
}

// ---- auto reply -------------------------------------------------------------------------

async fn autoreplies(State(st): St) -> Json<Vec<AutoReplyRule>> {
    Json(st.engine.ai_autoreply_rules())
}

async fn save_autoreply(State(st): St, b: Bytes) -> ApiResult<Json<AutoReplyRule>> {
    let r: AutoReplyRule = body(&b)?;
    Ok(Json(st.engine.ai_save_autoreply(r)?))
}

async fn delete_autoreply(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.ai_delete_autoreply(&id)?;
    Ok(ok())
}

// ---- ask & demo -------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct AskBody {
    question: String,
    mailbox: Option<String>,
}

async fn ask(State(st): St, b: Bytes) -> ApiResult<Response> {
    let a: AskBody = body(&b)?;
    if a.question.trim().is_empty() {
        return Err(ApiError(Error::Invalid("请输入问题".into())));
    }
    if let Some(m) = a.mailbox.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        if crate::model::Mailbox::parse(m).is_none() {
            return Err(ApiError(Error::Invalid(format!("未知邮箱视图 {m}"))));
        }
    }
    // Fail before streaming when no model is configured.
    st.engine.ai_model(false)?;
    let e = st.engine.clone();
    let (question, mailbox) = (a.question, a.mailbox);
    Ok(ndjson(move |tx| async move { e.ai_ask(question, mailbox, Some(tx)).await }))
}

async fn demo(State(st): St) -> ApiResult<Json<Value>> {
    Ok(Json(st.engine.ai_seed_demo()?))
}

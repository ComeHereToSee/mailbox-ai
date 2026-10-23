use super::{ApiError, ApiResult, AppState};
use crate::engine::{Action, KeyImport, NewAccount};
use crate::error::Error;
use crate::model::*;
use crate::store::ListQuery;
use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

type St = State<AppState>;

pub(super) fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/oauth/callback", get(oauth_callback))
        .route("/v1/status", get(status))
        .route("/v1/events", get(events))
        .route("/v1/presets", get(presets))
        .route("/v1/autoconfig", post(autoconfig))
        .route("/v1/accounts", get(accounts).post(add_account))
        .route("/v1/accounts/test", post(test_account))
        .route("/v1/accounts/order", post(order_accounts))
        .route("/v1/accounts/{id}", get(account).put(update_account).delete(delete_account))
        .route("/v1/accounts/{id}/sync", post(sync_account))
        .route("/v1/accounts/{id}/reauth", post(reauth_account))
        .route("/v1/accounts/{id}/sieve", get(sieve_status).post(sieve_upload).delete(sieve_remove))
        .route("/v1/sync", post(sync_all))
        .route("/v1/identities", get(identities).post(save_identity))
        .route("/v1/identities/{id}", delete(delete_identity))
        .route("/v1/folders", get(folders).post(create_folder))
        .route("/v1/folders/{id}", axum::routing::patch(patch_folder).delete(delete_folder))
        .route("/v1/folders/{id}/sync", post(sync_folder))
        .route("/v1/messages", get(list_messages))
        .route("/v1/counts", get(counts))
        .route("/v1/messages/actions", post(actions))
        .route("/v1/messages/mark-all-read", post(mark_all_read))
        .route("/v1/undo", post(undo))
        .route("/v1/messages/{id}", get(message))
        .route("/v1/messages/{id}/thread", get(thread))
        .route("/v1/messages/{id}/raw", get(raw))
        .route("/v1/messages/{id}/attachments/{aid}", get(attachment))
        .route("/v1/messages/{id}/attachments/{aid}/file", post(attachment_file))
        .route("/v1/messages/{id}/cid/{cid}", get(cid))
        .route("/v1/messages/{id}/reply", get(reply))
        .route("/v1/messages/{id}/edit-draft", get(edit_draft))
        .route("/v1/messages/{id}/rsvp", post(rsvp))
        .route("/v1/messages/{id}/unsubscribe", post(unsubscribe))
        .route("/v1/messages/{id}/read-receipt", post(read_receipt))
        .route("/v1/messages/{id}/trust-images", post(trust_images))
        .route("/v1/drafts", get(drafts).post(save_draft))
        .route("/v1/drafts/{id}", get(draft).delete(delete_draft))
        .route("/v1/send", post(send))
        .route("/v1/outbox", get(outbox))
        .route("/v1/outbox/{id}/cancel", post(cancel_send))
        .route("/v1/outbox/{id}/retry", post(retry_send))
        .route("/v1/contacts", get(contacts))
        .route("/v1/contacts/vip", post(set_vip))
        .route("/v1/vips", get(vips))
        .route("/v1/blocked", get(blocked).post(block).delete(unblock))
        .route("/v1/rules", get(rules).post(save_rule))
        .route("/v1/rules/{id}", delete(delete_rule))
        .route("/v1/rules/{id}/run", post(run_rule))
        .route("/v1/templates", get(templates).post(save_template))
        .route("/v1/templates/{id}", delete(delete_template))
        .route("/v1/smart-folders", get(smart_folders).post(save_smart_folder))
        .route("/v1/smart-folders/{id}", delete(delete_smart_folder))
        .route("/v1/forwarders", get(forwarders).post(save_forwarder))
        .route("/v1/forwarders/test", post(test_forwarder))
        .route("/v1/forwarders/{id}", delete(delete_forwarder))
        .route("/v1/settings", get(settings).put(put_settings))
        .route("/v1/notifications/pause", post(pause_notifications))
        .route("/v1/oauth/start", post(oauth_start))
        .route("/v1/oauth/{state}", get(oauth_status))
        .route("/v1/menu", get(menu))
        .route("/v1/search/server", post(server_search))
        .route("/v1/import", post(import))
        .route("/v1/export", post(export))
        .route("/v1/demo", post(demo))
        .route("/v1/storage", get(storage))
        .route("/v1/cleanup", post(cleanup))
        .route("/v1/calendar/invite", post(build_invite))
        .route("/v1/keys", get(keys))
        .route("/v1/keys/import", post(import_key))
        .route("/v1/keys/generate", post(generate_key))
        .route("/v1/keys/check", post(check_keys))
        .route("/v1/keys/{id}", delete(delete_key))
        .route("/v1/keys/{id}/export", get(export_key))
        .route("/v1/keys/{id}/passphrase", post(key_passphrase))
        .merge(super::ai::router())
        .with_state(state)
}

fn ok() -> Json<Value> {
    Json(json!({ "ok": true }))
}

// ---- status & events ----------------------------------------------------------------------

async fn status(State(st): St) -> Json<Value> {
    let statuses = st.engine.statuses();
    let unread: i64 = statuses.iter().map(|s| s.unread).sum();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "accounts": statuses,
        "unread_total": unread,
        "outbox": st.engine.db.outbox().unwrap_or_default().len(),
    }))
}

async fn events(State(st): St, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| pump_events(st, socket))
}

async fn pump_events(st: AppState, mut socket: WebSocket) {
    let mut rx = st.engine.events.subscribe();
    let hello = json!({ "type": "hello", "version": env!("CARGO_PKG_VERSION") }).to_string();
    if socket.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(25));
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(e) => {
                    let Ok(text) = serde_json::to_string(&e) else { continue };
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let resync = json!({ "type": "resync" }).to_string();
                    if socket.send(Message::Text(resync.into())).await.is_err() {
                        return;
                    }
                }
                Err(_) => return,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
        }
    }
}

// ---- accounts -------------------------------------------------------------------------------

async fn presets() -> Json<Value> {
    Json(json!(crate::autoconfig::presets()))
}

#[derive(Deserialize)]
struct EmailBody {
    email: String,
}

async fn autoconfig(Json(b): Json<EmailBody>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(crate::autoconfig::discover(&b.email).await?)))
}

async fn accounts(State(st): St) -> ApiResult<Json<Vec<Account>>> {
    Ok(Json(st.engine.db.accounts()?))
}

async fn account(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Account>> {
    Ok(Json(st.engine.db.account(&id)?))
}

async fn add_account(State(st): St, Json(req): Json<NewAccount>) -> ApiResult<Json<Account>> {
    Ok(Json(st.engine.add_account(req).await?))
}

async fn test_account(State(st): St, Json(req): Json<NewAccount>) -> Json<Value> {
    Json(json!(st.engine.test_account(&req).await))
}

#[derive(Deserialize)]
struct UpdateAccount {
    account: Account,
    password: Option<String>,
    smtp_password: Option<String>,
}

async fn update_account(State(st): St, Path(id): Path<String>, Json(mut b): Json<UpdateAccount>) -> ApiResult<Json<Account>> {
    b.account.id = id;
    Ok(Json(st.engine.update_account(b.account, b.password, b.smtp_password).await?))
}

async fn delete_account(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.delete_account(&id).await?;
    Ok(ok())
}

async fn sync_account(State(st): St, Path(id): Path<String>) -> Json<Value> {
    st.engine.wake(Some(&id));
    ok()
}

async fn sync_all(State(st): St) -> Json<Value> {
    st.engine.wake(None);
    ok()
}

#[derive(Deserialize)]
struct Reauth {
    oauth_state: String,
}

async fn reauth_account(State(st): St, Path(id): Path<String>, Json(b): Json<Reauth>) -> ApiResult<Json<Value>> {
    st.engine.reauthorize(&id, &b.oauth_state)?;
    Ok(ok())
}

async fn sieve_status(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(st.engine.sieve_status(&id).await?)))
}

async fn sieve_upload(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "script": st.engine.sieve_upload(&id).await? })))
}

async fn sieve_remove(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.sieve_remove(&id).await?;
    Ok(ok())
}

#[derive(Deserialize)]
struct Ids<T> {
    ids: Vec<T>,
}

async fn order_accounts(State(st): St, Json(b): Json<Ids<String>>) -> ApiResult<Json<Value>> {
    st.engine.reorder_accounts(&b.ids)?;
    Ok(ok())
}

#[derive(Deserialize)]
struct AccountFilter {
    account: Option<String>,
}

async fn identities(State(st): St, Query(q): Query<AccountFilter>) -> ApiResult<Json<Vec<Identity>>> {
    Ok(Json(st.engine.db.identities(q.account.as_deref())?))
}

async fn save_identity(State(st): St, Json(i): Json<Identity>) -> ApiResult<Json<Identity>> {
    st.engine.db.account(&i.account_id)?;
    Ok(Json(st.engine.db.save_identity(i)?))
}

async fn delete_identity(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.db.delete_identity(&id)?;
    Ok(ok())
}

// ---- folders --------------------------------------------------------------------------------

async fn folders(State(st): St, Query(q): Query<AccountFilter>) -> ApiResult<Json<Vec<Folder>>> {
    Ok(Json(st.engine.db.folders(q.account.as_deref())?))
}

#[derive(Deserialize)]
struct NewFolder {
    account_id: String,
    name: String,
    parent_id: Option<FolderId>,
}

async fn create_folder(State(st): St, Json(b): Json<NewFolder>) -> ApiResult<Json<Folder>> {
    Ok(Json(st.engine.create_folder(&b.account_id, b.name.trim(), b.parent_id)?))
}

#[derive(Deserialize)]
struct FolderPatch {
    name: Option<String>,
    sync_enabled: Option<bool>,
}

async fn patch_folder(State(st): St, Path(id): Path<FolderId>, Json(b): Json<FolderPatch>) -> ApiResult<Json<Folder>> {
    if let Some(name) = b.name.filter(|n| !n.trim().is_empty()) {
        st.engine.rename_folder(id, name.trim())?;
    }
    if let Some(s) = b.sync_enabled {
        st.engine.db.set_folder_sync_enabled(id, s)?;
        if s {
            st.engine.request_folder_sync(id)?;
        }
    }
    Ok(Json(st.engine.db.folder(id)?))
}

async fn delete_folder(State(st): St, Path(id): Path<FolderId>) -> ApiResult<Json<Value>> {
    st.engine.delete_folder(id)?;
    Ok(ok())
}

async fn sync_folder(State(st): St, Path(id): Path<FolderId>) -> ApiResult<Json<Value>> {
    st.engine.request_folder_sync(id)?;
    Ok(ok())
}

// ---- messages -------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListParams {
    mailbox: Option<String>,
    q: Option<String>,
    threads: Option<bool>,
    before_date: Option<i64>,
    before_id: Option<i64>,
    limit: Option<i64>,
    account: Option<String>,
}

fn parse_mailbox(s: Option<&str>) -> ApiResult<Mailbox> {
    match s {
        None | Some("") => Ok(Mailbox::Unified(FolderRole::Inbox)),
        Some(m) => Mailbox::parse(m).ok_or_else(|| ApiError(Error::Invalid(format!("未知邮箱视图 {m}")))),
    }
}

async fn list_messages(State(st): St, Query(p): Query<ListParams>) -> ApiResult<Json<Value>> {
    let mailbox = parse_mailbox(p.mailbox.as_deref())?;
    let limit = p.limit.unwrap_or(100);
    let q = ListQuery {
        mailbox,
        search: p.q,
        threads: p.threads.unwrap_or_else(|| st.engine.settings().conversation_view),
        before: p.before_date.zip(p.before_id),
        limit,
        account_id: p.account,
    };
    let msgs = st.engine.db.list_messages(&q)?;
    let cursor = if msgs.len() as i64 >= limit { msgs.last().map(|m| json!({"before_date": m.sort_date, "before_id": m.id})) } else { None };
    Ok(Json(json!({ "messages": msgs, "cursor": cursor })))
}

#[derive(Deserialize)]
struct CountParams {
    mailboxes: String,
}

async fn counts(State(st): St, Query(p): Query<CountParams>) -> ApiResult<Json<HashMap<String, Value>>> {
    let mut out = HashMap::new();
    for m in p.mailboxes.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(mb) = Mailbox::parse(m) {
            let unread = st.engine.db.count_messages(&mb, true)?;
            let total = st.engine.db.count_messages(&mb, false)?;
            out.insert(m.to_string(), json!({ "unread": unread, "total": total }));
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct OpenParams {
    open: Option<bool>,
}

async fn message(State(st): St, Path(id): Path<MessageId>, Query(p): Query<OpenParams>) -> ApiResult<Json<MessageDetail>> {
    if p.open.unwrap_or(false) {
        Ok(Json(st.engine.open_message(id).await?))
    } else {
        Ok(Json(st.engine.message_detail(id).await?))
    }
}

async fn thread(State(st): St, Path(id): Path<MessageId>) -> ApiResult<Json<Vec<MessageDetail>>> {
    Ok(Json(st.engine.thread_detail(id).await?))
}

async fn raw(State(st): St, Path(id): Path<MessageId>) -> ApiResult<Response> {
    let bytes = st.engine.raw_message(id).await?;
    Ok(([(header::CONTENT_TYPE, "message/rfc822")], bytes).into_response())
}

fn disposition(name: &str) -> String {
    let ascii: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || ".-_ ".contains(c) { c } else { '_' }).collect();
    let enc = percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{enc}")
}

async fn attachment(State(st): St, Path((id, aid)): Path<(MessageId, i64)>) -> ApiResult<Response> {
    let (name, mime, data) = st.engine.attachment(id, aid).await?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_DISPOSITION, disposition(&name))
        .body(Body::from(data))
        .map_err(|e| ApiError(Error::Other(e.to_string())))?)
}

async fn attachment_file(State(st): St, Path((id, aid)): Path<(MessageId, i64)>) -> ApiResult<Json<Value>> {
    let path = st.engine.attachment_file(id, aid).await?;
    Ok(Json(json!({ "path": path })))
}

async fn cid(State(st): St, Path((id, cid)): Path<(MessageId, String)>) -> ApiResult<Response> {
    let (mime, data) = st.engine.cid_part(id, &cid).await?;
    Ok(([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "max-age=86400".into())], data).into_response())
}

#[derive(Deserialize)]
struct ActionBody {
    ids: Vec<MessageId>,
    #[serde(flatten)]
    action: Action,
}

async fn actions(State(st): St, Json(b): Json<ActionBody>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(st.engine.act(&b.ids, b.action).await?)))
}

#[derive(Deserialize)]
struct MailboxBody {
    mailbox: String,
}

async fn mark_all_read(State(st): St, Json(b): Json<MailboxBody>) -> ApiResult<Json<Value>> {
    let mb = parse_mailbox(Some(&b.mailbox))?;
    Ok(Json(json!(st.engine.mark_all_read(&mb).await?)))
}

#[derive(Deserialize)]
struct UndoBody {
    token: String,
}

async fn undo(State(st): St, Json(b): Json<UndoBody>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "ok": st.engine.undo(&b.token)? })))
}

#[derive(Deserialize)]
struct ReplyParams {
    mode: Option<ComposeMode>,
}

async fn reply(State(st): St, Path(id): Path<MessageId>, Query(p): Query<ReplyParams>) -> ApiResult<Json<Draft>> {
    Ok(Json(st.engine.reply_draft(id, p.mode.unwrap_or(ComposeMode::Reply)).await?))
}

async fn edit_draft(State(st): St, Path(id): Path<MessageId>) -> ApiResult<Json<Draft>> {
    Ok(Json(st.engine.draft_from_message(id).await?))
}

#[derive(Deserialize)]
struct Rsvp {
    partstat: String,
}

async fn rsvp(State(st): St, Path(id): Path<MessageId>, Json(b): Json<Rsvp>) -> ApiResult<Json<Value>> {
    st.engine.rsvp(id, &b.partstat).await?;
    Ok(ok())
}

async fn unsubscribe(State(st): St, Path(id): Path<MessageId>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(st.engine.unsubscribe(id).await?)))
}

async fn read_receipt(State(st): St, Path(id): Path<MessageId>) -> ApiResult<Json<Value>> {
    st.engine.send_read_receipt(id).await?;
    Ok(ok())
}

#[derive(Deserialize)]
struct Trust {
    trusted: bool,
}

async fn trust_images(State(st): St, Path(id): Path<MessageId>, Json(b): Json<Trust>) -> ApiResult<Json<Value>> {
    let m = st.engine.db.message(id)?;
    st.engine.db.set_trusted_images(&m.summary.from.email, b.trusted)?;
    Ok(ok())
}

// ---- compose --------------------------------------------------------------------------------

async fn drafts(State(st): St) -> ApiResult<Json<Vec<Draft>>> {
    Ok(Json(st.engine.db.drafts()?))
}

async fn draft(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Draft>> {
    let (d, _) = st.engine.db.draft(&id)?.ok_or_else(|| ApiError(Error::NotFound("草稿不存在".into())))?;
    Ok(Json(d))
}

#[derive(Deserialize)]
struct SyncParam {
    sync: Option<bool>,
}

async fn save_draft(State(st): St, Query(p): Query<SyncParam>, Json(d): Json<Draft>) -> ApiResult<Json<Draft>> {
    let d = st.engine.save_draft(d)?;
    if p.sync.unwrap_or(false) {
        st.engine.sync_draft(&d.id).await?;
    }
    Ok(Json(d))
}

async fn delete_draft(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.discard_draft(&id)?;
    Ok(ok())
}

#[derive(Deserialize)]
struct SendBody {
    draft: Draft,
    send_at: Option<i64>,
}

async fn send(State(st): St, Json(b): Json<SendBody>) -> ApiResult<Json<OutboxItem>> {
    Ok(Json(st.engine.send_draft(b.draft, b.send_at)?))
}

async fn outbox(State(st): St) -> ApiResult<Json<Vec<OutboxItem>>> {
    Ok(Json(st.engine.db.outbox()?))
}

async fn cancel_send(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "draft": st.engine.cancel_send(&id)? })))
}

async fn retry_send(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.retry_send(&id)?;
    Ok(ok())
}

// ---- contacts -------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ContactQuery {
    q: Option<String>,
    limit: Option<i64>,
}

async fn contacts(State(st): St, Query(q): Query<ContactQuery>) -> ApiResult<Json<Vec<Contact>>> {
    Ok(Json(st.engine.db.search_contacts(q.q.as_deref().unwrap_or(""), q.limit.unwrap_or(10))?))
}

#[derive(Deserialize)]
struct VipBody {
    email: String,
    vip: bool,
}

async fn set_vip(State(st): St, Json(b): Json<VipBody>) -> ApiResult<Json<Value>> {
    st.engine.db.set_vip(&b.email, b.vip)?;
    st.engine.changed(None);
    Ok(ok())
}

async fn vips(State(st): St) -> ApiResult<Json<Vec<String>>> {
    Ok(Json(st.engine.db.vips()?))
}

#[derive(Deserialize)]
struct Pattern {
    pattern: String,
}

async fn blocked(State(st): St) -> ApiResult<Json<Vec<String>>> {
    Ok(Json(st.engine.db.blocked()?))
}

async fn block(State(st): St, Json(b): Json<Pattern>) -> ApiResult<Json<Value>> {
    st.engine.db.block(&b.pattern)?;
    Ok(ok())
}

async fn unblock(State(st): St, Json(b): Json<Pattern>) -> ApiResult<Json<Value>> {
    st.engine.db.unblock(&b.pattern)?;
    Ok(ok())
}

// ---- documents (rules, templates, smart folders, forwarders) ------------------------------

fn with_id(id: &mut String) {
    if id.is_empty() {
        *id = crate::store::new_id();
    }
}

async fn rules(State(st): St) -> ApiResult<Json<Vec<Rule>>> {
    Ok(Json(st.engine.db.kv_list("rules")?))
}

async fn save_rule(State(st): St, Json(mut r): Json<Rule>) -> ApiResult<Json<Rule>> {
    with_id(&mut r.id);
    st.engine.db.kv_put("rules", &r.id, &r, r.sort_order)?;
    Ok(Json(r))
}

async fn delete_rule(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.db.kv_delete("rules", &id)?;
    Ok(ok())
}

async fn run_rule(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let rule: Rule = st.engine.db.kv_get("rules", &id)?.ok_or_else(|| ApiError(Error::NotFound("规则不存在".into())))?;
    Ok(Json(json!({ "affected": st.engine.run_rule_on_inbox(&rule).await? })))
}

async fn templates(State(st): St) -> ApiResult<Json<Vec<Template>>> {
    Ok(Json(st.engine.db.kv_list("templates")?))
}

async fn save_template(State(st): St, Json(mut t): Json<Template>) -> ApiResult<Json<Template>> {
    with_id(&mut t.id);
    st.engine.db.kv_put("templates", &t.id, &t, 0)?;
    Ok(Json(t))
}

async fn delete_template(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.db.kv_delete("templates", &id)?;
    Ok(ok())
}

async fn smart_folders(State(st): St) -> ApiResult<Json<Vec<SmartFolder>>> {
    Ok(Json(st.engine.db.kv_list("smart_folders")?))
}

async fn save_smart_folder(State(st): St, Json(mut f): Json<SmartFolder>) -> ApiResult<Json<SmartFolder>> {
    with_id(&mut f.id);
    st.engine.db.kv_put("smart_folders", &f.id, &f, 0)?;
    Ok(Json(f))
}

async fn delete_smart_folder(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.db.kv_delete("smart_folders", &id)?;
    Ok(ok())
}

async fn forwarders(State(st): St) -> ApiResult<Json<Vec<Forwarder>>> {
    Ok(Json(st.engine.db.kv_list("forwarders")?))
}

async fn save_forwarder(State(st): St, Json(mut f): Json<Forwarder>) -> ApiResult<Json<Forwarder>> {
    with_id(&mut f.id);
    st.engine.db.kv_put("forwarders", &f.id, &f, 0)?;
    Ok(Json(f))
}

async fn delete_forwarder(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.db.kv_delete("forwarders", &id)?;
    Ok(ok())
}

async fn test_forwarder(State(st): St, Json(f): Json<Forwarder>) -> ApiResult<Json<Value>> {
    let p = crate::forward::ForwardPayload {
        account_label: "Mailbox".into(),
        from: "测试".into(),
        subject: "这是一条测试通知".into(),
        snippet: "如果你看到这条消息，说明推送配置成功。".into(),
        date: crate::store::now(),
        message_url: None,
    };
    crate::forward::send(&st.engine.http, &f, &p).await?;
    Ok(ok())
}

// ---- settings -------------------------------------------------------------------------------

async fn settings(State(st): St) -> Json<Settings> {
    Json(st.engine.settings())
}

async fn put_settings(State(st): St, Json(s): Json<Settings>) -> ApiResult<Json<Settings>> {
    st.engine.save_settings(&s)?;
    Ok(Json(s))
}

#[derive(Deserialize)]
struct Pause {
    minutes: i64,
}

async fn pause_notifications(State(st): St, Json(p): Json<Pause>) -> ApiResult<Json<Settings>> {
    let mut s = st.engine.settings();
    s.notifications_paused_until = if p.minutes > 0 { Some(crate::store::now() + p.minutes * 60) } else { None };
    st.engine.save_settings(&s)?;
    Ok(Json(s))
}

// ---- oauth ----------------------------------------------------------------------------------

#[derive(Deserialize)]
struct OAuthStart {
    provider: String,
    login_hint: Option<String>,
}

async fn oauth_start(State(st): St, Json(b): Json<OAuthStart>) -> ApiResult<Json<Value>> {
    let (url, state) = st.engine.start_oauth(&b.provider, b.login_hint.as_deref())?;
    Ok(Json(json!({ "url": url, "state": state })))
}

async fn oauth_status(State(st): St, Path(state): Path<String>) -> Json<Value> {
    match st.engine.oauth_status(&state) {
        Some((done, email)) => Json(json!({ "pending": true, "done": done, "email": email })),
        None => Json(json!({ "pending": false, "done": false })),
    }
}

#[derive(Deserialize)]
struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn page(title: &str, body: &str, ok: bool) -> Html<String> {
    let color = if ok { "#30A46C" } else { "#E5484D" };
    Html(format!(
        "<!doctype html><html lang=zh><meta charset=utf-8><title>{title}</title>\
         <body style=\"font-family:-apple-system,system-ui;text-align:center;padding:80px;color:#222\">\
         <div style=\"font-size:56px;color:{color}\">{}</div><h2>{title}</h2><p style=\"color:#666\">{body}</p>\
         <script>setTimeout(()=>window.close(),1500)</script></body></html>",
        if ok { "✓" } else { "✕" }
    ))
}

async fn oauth_callback(State(st): St, Query(c): Query<Callback>) -> Response {
    let Some(state) = c.state else { return (StatusCode::BAD_REQUEST, "missing state").into_response() };
    let err = c.error.map(|e| format!("{e} {}", c.error_description.unwrap_or_default()));
    match st.engine.finish_oauth(&state, c.code.as_deref(), err.as_deref()).await {
        Ok(email) => page("登录成功", &format!("{} 已授权 Mailbox，可以回到应用了。", crate::html::escape_text(&email)), true).into_response(),
        Err(e) => page("登录失败", &crate::html::escape_text(&e.to_string()), false).into_response(),
    }
}

// ---- misc -----------------------------------------------------------------------------------

#[derive(Deserialize)]
struct MenuParams {
    limit: Option<i64>,
}

async fn menu(State(st): St, Query(p): Query<MenuParams>) -> ApiResult<Json<Value>> {
    let accounts: HashMap<String, Account> = st.engine.db.accounts()?.into_iter().map(|a| (a.id.clone(), a)).collect();
    let msgs = st.engine.db.recent_unread(p.limit.unwrap_or(8))?;
    let items: Vec<Value> = msgs
        .into_iter()
        .map(|m| {
            let acc = accounts.get(&m.account_id);
            json!({
                "message": m,
                "account_label": acc.map(|a| a.label.clone()).unwrap_or_default(),
                "account_color": acc.map(|a| a.color.clone()).unwrap_or_default(),
            })
        })
        .collect();
    let unread: i64 = st.engine.db.unread_counts()?.iter().map(|(_, n)| n).sum();
    Ok(Json(json!({ "unread": unread, "items": items, "settings": st.engine.settings() })))
}

#[derive(Deserialize)]
struct ServerSearch {
    account_id: String,
    folder_id: Option<FolderId>,
    q: String,
}

async fn server_search(State(st): St, Json(b): Json<ServerSearch>) -> ApiResult<Json<Vec<MessageSummary>>> {
    Ok(Json(st.engine.server_search(&b.account_id, b.folder_id, &b.q).await?))
}

#[derive(Deserialize)]
struct ImportBody {
    folder_id: FolderId,
    path: String,
}

async fn import(State(st): St, Json(b): Json<ImportBody>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "imported": st.engine.import_messages(b.folder_id, std::path::Path::new(&b.path)).await? })))
}

#[derive(Deserialize)]
struct ExportBody {
    ids: Option<Vec<MessageId>>,
    folder_id: Option<FolderId>,
    path: String,
    format: Option<String>,
}

async fn export(State(st): St, Json(b): Json<ExportBody>) -> ApiResult<Json<Value>> {
    let n = st
        .engine
        .export_messages(b.ids, b.folder_id, std::path::Path::new(&b.path), b.format.as_deref().unwrap_or("mbox"))
        .await?;
    Ok(Json(json!({ "exported": n })))
}

async fn storage(State(st): St) -> ApiResult<Json<Value>> {
    Ok(Json(json!(st.engine.storage_report()?)))
}

async fn cleanup(State(st): St) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "freed": st.engine.cleanup()? })))
}

// ---- PGP / S/MIME keys -----------------------------------------------------------------

async fn keys(State(st): St) -> Json<Value> {
    Json(json!(st.engine.list_keys()))
}

async fn import_key(State(st): St, Json(req): Json<KeyImport>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(st.engine.import_key(req)?)))
}

#[derive(Deserialize)]
struct GenerateKey {
    #[serde(default)]
    name: String,
    email: String,
    passphrase: Option<String>,
}

async fn generate_key(State(st): St, Json(req): Json<GenerateKey>) -> ApiResult<Json<Value>> {
    let engine = st.engine.clone();
    // RSA key generation takes a moment; keep it off the async workers.
    let info = tokio::task::spawn_blocking(move || engine.generate_key(&req.name, &req.email, req.passphrase.as_deref()))
        .await
        .map_err(|e| Error::Other(e.to_string()))??;
    Ok(Json(json!(info)))
}

#[derive(Deserialize)]
struct CheckKeys {
    recipients: Vec<String>,
}

async fn check_keys(State(st): St, Json(req): Json<CheckKeys>) -> Json<Value> {
    let (have, missing) = st.engine.recipient_keys(&req.recipients);
    Json(json!({ "have": have, "missing": missing }))
}

async fn delete_key(State(st): St, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    st.engine.delete_key(&id)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ExportParams {
    #[serde(default)]
    secret: bool,
}

async fn export_key(State(st): St, Path(id): Path<String>, Query(p): Query<ExportParams>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "text": st.engine.export_key(&id, p.secret)? })))
}

#[derive(Deserialize)]
struct Passphrase {
    passphrase: Option<String>,
}

async fn key_passphrase(State(st): St, Path(id): Path<String>, Json(req): Json<Passphrase>) -> ApiResult<Json<Value>> {
    st.engine.set_key_passphrase(&id, req.passphrase.as_deref())?;
    Ok(Json(json!({ "ok": true })))
}

async fn demo(State(st): St) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "account": st.engine.seed_demo()? })))
}

#[derive(Deserialize)]
struct InviteBody {
    summary: String,
    start: i64,
    end: i64,
    #[serde(default)]
    all_day: bool,
    location: Option<String>,
    description: Option<String>,
    organizer: EmailAddress,
    attendees: Vec<EmailAddress>,
}

async fn build_invite(Json(b): Json<InviteBody>) -> Json<Value> {
    let uid = format!("{}@mailbox", uuid::Uuid::new_v4());
    let ics = crate::ics::build_request(
        &uid,
        &b.summary,
        b.start,
        b.end,
        b.all_day,
        b.location.as_deref(),
        b.description.as_deref(),
        &b.organizer,
        &b.attendees,
    );
    Json(json!({ "uid": uid, "ics": ics }))
}

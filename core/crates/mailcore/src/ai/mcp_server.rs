//! Built-in MCP server (`POST /mcp`, Streamable HTTP answered with plain JSON): lets
//! Claude Code, Codex, Cherry Studio … search, read and organise the mailbox. Sending and
//! deleting wait for the user (`ai_confirm` event, `POST /v1/ai/confirm/{id}`), at most
//! 180 seconds (`MAILBOX_AI_CONFIRM_TIMEOUT_SECS` overrides it). The MCP token lives in the
//! secrets file (`ai:mcp_token`).
//!
//! MCP clients count as non-local models for the account policies: "off" and "local_only"
//! accounts are hidden from the lists and cannot be read.

use super::model::Confirmation;
use super::{fmt_time, loops, policy, text_html};
use crate::Engine;
use crate::engine::Action;
use crate::error::{Error, Result};
use crate::events::Event;
use crate::model::{Account, ComposeMode, Draft, EmailAddress, FolderRole, Mailbox, MessageId, MessageSummary};
use crate::store::{ListQuery, new_id, now};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::oneshot;

pub const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
pub const DISABLED: &str = "MCP 未启用：请在 设置 → AI 功能 → MCP 中开启";
const TOKEN_KEY: &str = "ai:mcp_token";
const DEFAULT_CONFIRM_SECS: u64 = 180;
const READ_CHARS: usize = 20_000;

const INSTRUCTIONS: &str = "这是 Mailbox AI 邮件客户端的邮箱工具。可以：list_accounts 列出账户；search_emails 按搜索语法搜索邮件\
（如 from:张三 subject:合同 is:unread newer_than:7d has:attachment）；list_unread 查看统一收件箱的未读邮件；read_email / read_thread \
读取邮件或整个会话的正文；get_pending 查看需要处理的待办和跟进提醒；draft_email / reply_email 生成草稿；mark_emails 标记已读、星标或归档。\
send_email（发送）和 delete_emails（删除）需要用户在 Mailbox AI 中确认，最长等待 180 秒，用户拒绝或超时都不会发送或删除。\
邮件 id 是数字，来自搜索结果。正文是纯文本，空行分段。";

/// The protocol version answered to `initialize`: the client's if supported, else the newest.
pub fn negotiate_version(requested: Option<&str>) -> &'static str {
    let requested = requested.unwrap_or("").trim();
    SUPPORTED_VERSIONS.iter().copied().find(|v| *v == requested).unwrap_or(SUPPORTED_VERSIONS[0])
}

/// How long send / delete wait for the user.
pub fn confirm_timeout() -> Duration {
    let secs = std::env::var("MAILBOX_AI_CONFIRM_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_CONFIRM_SECS);
    Duration::from_secs(secs)
}

/// A confirmation waiting in `AiState.confirms`.
pub(crate) struct PendingConfirm {
    pub info: Confirmation,
    pub tx: oneshot::Sender<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approved,
    Rejected,
    TimedOut,
}

/// Removes the request when the waiting call ends (answered, timed out or disconnected).
struct ConfirmGuard {
    e: Engine,
    id: String,
}

impl Drop for ConfirmGuard {
    fn drop(&mut self) {
        self.e.ai.confirms.lock().remove(&self.id);
    }
}

/// Text answer of a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolText {
    pub text: String,
    pub is_error: bool,
}

fn ok_text(text: impl Into<String>) -> ToolText {
    ToolText { text: text.into(), is_error: false }
}

fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

/// `tools/list`
pub fn tool_definitions() -> Value {
    let addrs = |d: &str| json!({ "type": "array", "items": { "type": "string" }, "description": d });
    let limit = json!({ "type": "integer", "minimum": 1, "maximum": 50, "description": "最多返回几封，默认 20" });
    let tool = |name: &str, description: &str, input: Value| json!({ "name": name, "description": description, "inputSchema": input });

    let search = schema(
        json!({
            "query": { "type": "string", "description": "搜索条件" },
            "mailbox": { "type": "string", "description": "可选：邮箱视图，如 unified:inbox、unified:sent、starred、unread、vip；默认全部邮件" },
            "limit": limit.clone()
        }),
        &["query"],
    );
    let draft = schema(
        json!({
            "to": addrs("收件人，如 \"张三 <zs@example.com>\" 或 \"zs@example.com\""),
            "cc": addrs("抄送（可选）"),
            "subject": { "type": "string", "description": "主题" },
            "body": { "type": "string", "description": "纯文本正文，空行分段" },
            "account": { "type": "string", "description": "可选：发件账户的 id、邮箱地址或标签，默认第一个账户" }
        }),
        &["to", "subject", "body"],
    );
    let reply = schema(
        json!({
            "id": { "type": "integer", "description": "要回复的邮件 id" },
            "body": { "type": "string", "description": "回复正文（纯文本，空行分段）" },
            "reply_all": { "type": "boolean", "description": "是否回复全部，默认 false" }
        }),
        &["id", "body"],
    );
    let send = schema(
        json!({
            "draft_id": { "type": "string", "description": "要发送的草稿 id（与下面的字段二选一）" },
            "to": addrs("收件人"),
            "cc": addrs("抄送（可选）"),
            "bcc": addrs("密送（可选）"),
            "subject": { "type": "string", "description": "主题" },
            "body": { "type": "string", "description": "纯文本正文，空行分段" },
            "account": { "type": "string", "description": "可选：发件账户的 id、邮箱地址或标签" }
        }),
        &[],
    );
    let ids = json!({ "type": "array", "items": { "type": "integer" }, "description": "邮件 id 列表" });
    let mark = schema(
        json!({
            "ids": ids.clone(),
            "action": { "type": "string", "enum": ["read", "unread", "star", "unstar", "archive"], "description": "操作" }
        }),
        &["ids", "action"],
    );
    let one_id = |d: &str| schema(json!({ "id": { "type": "integer", "description": d } }), &["id"]);

    let tools: Vec<Value> = vec![
        tool("list_accounts", "列出所有邮箱账户（id、邮箱地址、标签）。", schema(json!({}), &[])),
        tool(
            "search_emails",
            "用搜索语法搜索邮件，例如 from:张三 subject:合同 is:unread newer_than:7d has:attachment；也可以直接写关键词。返回 id、发件人、主题、时间、摘要、是否未读、账户。",
            search,
        ),
        tool("list_unread", "统一收件箱中的未读邮件（最新的在前）。", schema(json!({ "limit": limit }), &[])),
        tool("read_email", "读取一封邮件：信头和纯文本正文（最多 20000 字）。", one_id("邮件 id")),
        tool("read_thread", "读取一封邮件所在的整个会话（按时间顺序）。", one_id("会话中任意一封邮件的 id")),
        tool(
            "get_pending",
            "需要处理的事项：需要回复、有截止时间或待办的邮件，以及自己发出后一直没收到回复的邮件。",
            schema(json!({}), &[]),
        ),
        tool("draft_email", "写一封新邮件并保存为草稿（同步到服务器的草稿箱），不会发送。返回 draft_id。", draft),
        tool("reply_email", "回复一封邮件，生成带引用的回复草稿，不会发送。返回 draft_id。", reply),
        tool(
            "send_email",
            "发送邮件：发送已有草稿（draft_id），或者直接给出收件人、主题和正文。需要用户在 Mailbox AI 中确认（最长 180 秒）；确认后经发件箱发送，几秒内可以撤销。",
            send,
        ),
        tool("mark_emails", "标记邮件：read（已读）、unread（未读）、star（星标）、unstar（取消星标）、archive（归档）。", mark),
        tool("delete_emails", "把邮件移到废纸篓。需要用户在 Mailbox AI 中确认（最长 180 秒）。", schema(json!({ "ids": ids }), &["ids"])),
    ];
    Value::Array(tools)
}

/// Names of the tools.
pub fn tool_names() -> Vec<String> {
    tool_definitions().as_array().into_iter().flatten().filter_map(|t| t["name"].as_str().map(String::from)).collect()
}

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args[key].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn id_of(v: &Value) -> Option<MessageId> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn id_arg(args: &Value, key: &str) -> Result<MessageId> {
    id_of(&args[key]).ok_or_else(|| Error::Invalid(format!("缺少参数 {key}（邮件 id）")))
}

fn ids_arg(args: &Value, key: &str) -> Result<Vec<MessageId>> {
    let ids: Vec<MessageId> = match &args[key] {
        Value::Array(list) => list.iter().filter_map(id_of).collect(),
        other => id_of(other).into_iter().collect(),
    };
    if ids.is_empty() {
        return Err(Error::Invalid(format!("缺少参数 {key}（邮件 id 列表）")));
    }
    Ok(ids)
}

/// "Name <a@b.c>" or "a@b.c".
pub fn parse_address(s: &str) -> Option<EmailAddress> {
    let s = s.trim();
    let email = crate::mime::extract_email(s)?;
    let name = s.find('<').map(|i| s[..i].trim().trim_matches('"').trim()).filter(|n| !n.is_empty());
    Some(EmailAddress::new(name, &email))
}

/// Addresses of an argument: an array of strings or one comma separated string.
fn addrs_arg(args: &Value, key: &str) -> Result<Vec<EmailAddress>> {
    let items: Vec<String> = match &args[key] {
        Value::Array(list) => list.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        Value::String(s) => s.split([',', ';', '，']).map(String::from).collect(),
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for item in items.iter().map(|x| x.trim()).filter(|x| !x.is_empty()) {
        out.push(parse_address(item).ok_or_else(|| Error::Invalid(format!("邮箱地址无效：{item}")))?);
    }
    Ok(out)
}

fn from_text(a: &EmailAddress) -> String {
    match &a.name {
        Some(n) if !n.is_empty() => format!("{n} <{}>", a.email),
        _ => a.email.clone(),
    }
}

fn subject_or_none(s: &str) -> String {
    if s.trim().is_empty() { "（无主题）".to_string() } else { s.to_string() }
}

/// Body text of a draft (plain text when there is one).
fn draft_text(d: &Draft) -> String {
    match d.text.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(t) => t.to_string(),
        None => crate::html::html_to_text(&d.html),
    }
}

/// What the confirmation dialog shows for a send.
pub fn send_detail(d: &Draft) -> String {
    let list = |v: &[EmailAddress]| v.iter().map(from_text).collect::<Vec<_>>().join(", ");
    let mut s = format!("收件人：{}\n", list(&d.to));
    if !d.cc.is_empty() {
        s.push_str(&format!("抄送：{}\n", list(&d.cc)));
    }
    if !d.bcc.is_empty() {
        s.push_str(&format!("密送：{}\n", list(&d.bcc)));
    }
    s.push_str(&format!("主题：{}\n\n{}", subject_or_none(&d.subject), super::truncate_plain(draft_text(d).trim(), 500)));
    s
}

impl Engine {
    // ---- token & confirmations --------------------------------------------------------

    /// The MCP token (created on first use).
    pub fn ai_mcp_token(&self) -> Result<String> {
        let _guard = self.ai.mcp_token_lock.lock();
        if let Some(t) = self.secrets.get(TOKEN_KEY).filter(|t| !t.trim().is_empty()) {
            return Ok(t);
        }
        let t = random_hex(32);
        self.secrets.set(TOKEN_KEY, &t)?;
        Ok(t)
    }

    /// `POST /v1/ai/mcp/token`: a new token (the old one stops working).
    pub fn ai_regenerate_mcp_token(&self) -> Result<String> {
        let _guard = self.ai.mcp_token_lock.lock();
        let t = random_hex(32);
        self.secrets.set(TOKEN_KEY, &t)?;
        Ok(t)
    }

    /// `GET /v1/ai/confirm`: requests waiting for the user, oldest first.
    pub fn ai_confirmations(&self) -> Vec<Confirmation> {
        let mut list: Vec<Confirmation> = self.ai.confirms.lock().values().map(|p| p.info.clone()).collect();
        list.sort_by_key(|c| c.created_at);
        list
    }

    /// `POST /v1/ai/confirm/{id}`. NotFound when answered already or expired.
    pub fn ai_decide(&self, id: &str, approve: bool) -> Result<()> {
        let pending = self.ai.confirms.lock().remove(id);
        match pending {
            Some(p) => {
                let _ = p.tx.send(approve);
                Ok(())
            }
            None => Err(Error::NotFound("确认请求不存在、已处理或已超时".into())),
        }
    }

    /// Asks the user (`ai_confirm` event) and waits for the decision.
    async fn ai_confirm(&self, kind: &str, title: String, detail: String, draft_id: Option<String>, message_ids: Vec<MessageId>) -> Decision {
        let timeout = confirm_timeout();
        let created = now();
        let info = Confirmation {
            id: new_id(),
            kind: kind.to_string(),
            title,
            detail,
            draft_id,
            message_ids,
            created_at: created,
            expires_at: created + timeout.as_secs() as i64,
        };
        let (tx, rx) = oneshot::channel::<bool>();
        self.ai.confirms.lock().insert(info.id.clone(), PendingConfirm { info: info.clone(), tx });
        let _guard = ConfirmGuard { e: self.clone(), id: info.id.clone() };
        self.events.emit(Event::AiConfirm { id: info.id, kind: info.kind, title: info.title, detail: info.detail });
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(true)) => Decision::Approved,
            Ok(Ok(false)) | Ok(Err(_)) => Decision::Rejected,
            Err(_) => Decision::TimedOut,
        }
    }

    // ---- JSON-RPC -----------------------------------------------------------------------

    /// One JSON-RPC message of `POST /mcp`. None for notifications and responses (HTTP 202).
    pub async fn ai_mcp_handle(&self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned()?;
        let method = msg["method"].as_str()?.to_string();
        let result: std::result::Result<Value, (i64, String)> = match method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": negotiate_version(msg["params"]["protocolVersion"].as_str()),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "mailbox-ai", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("").to_string();
                if !tool_names().contains(&name) {
                    Err((-32602, format!("Unknown tool: {name}")))
                } else {
                    let args = match msg["params"].get("arguments") {
                        Some(a) if a.is_object() => a.clone(),
                        _ => json!({}),
                    };
                    let out = self.ai_mcp_tool(&name, &args).await;
                    Ok(json!({ "content": [{ "type": "text", "text": out.text }], "isError": out.is_error }))
                }
            }
            other => Err((-32601, format!("Method not found: {other}"))),
        };
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        })
    }

    /// Runs a tool; failures become `isError` answers.
    pub async fn ai_mcp_tool(&self, name: &str, args: &Value) -> ToolText {
        match self.ai_mcp_tool_inner(name, args).await {
            Ok(t) => t,
            Err(err) => ToolText { text: err.to_string(), is_error: true },
        }
    }

    async fn ai_mcp_tool_inner(&self, name: &str, args: &Value) -> Result<ToolText> {
        match name {
            "list_accounts" => {
                let list: Vec<Value> =
                    self.db.accounts()?.iter().map(|a| json!({ "id": a.id, "email": a.email, "label": a.label })).collect();
                Ok(ok_text(format!("共 {} 个账户：\n{}", list.len(), serde_json::to_string(&list)?)))
            }
            "search_emails" => {
                let query = str_arg(args, "query").unwrap_or_default();
                let mailbox = match str_arg(args, "mailbox") {
                    Some(m) => Mailbox::parse(&m).ok_or_else(|| Error::Invalid(format!("未知邮箱视图 {m}")))?,
                    None => Mailbox::All,
                };
                let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
                let list = self.mcp_search(&mailbox, &query, limit)?;
                self.mcp_list_text(&list, "没有找到符合条件的邮件。")
            }
            "list_unread" => {
                let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
                let list = self.mcp_search(&Mailbox::Unified(FolderRole::Inbox), "is:unread", limit)?;
                self.mcp_list_text(&list, "统一收件箱里没有未读邮件。")
            }
            "read_email" => {
                let id = id_arg(args, "id")?;
                let m = self.ai_mail_text(id, READ_CHARS, false, false).await?;
                let mut s = format!("邮件 id：{id}\n发件人：{}\n收件人：{}\n", m.from, m.to);
                if !m.cc.is_empty() {
                    s.push_str(&format!("抄送：{}\n", m.cc));
                }
                s.push_str(&format!("时间：{}\n主题：{}\n", fmt_time(m.date), m.subject));
                if !m.attachments.is_empty() {
                    s.push_str(&format!("附件：{}\n", m.attachments.join("、")));
                }
                s.push_str(&format!("\n{}", m.text));
                Ok(ok_text(s))
            }
            "read_thread" => {
                let id = id_arg(args, "id")?;
                let mut ids = self.ai_thread_ids(id)?;
                if ids.len() > 20 {
                    ids = ids[ids.len() - 20..].to_vec();
                }
                let per = (READ_CHARS / ids.len().max(1)).max(2000);
                let mut parts = Vec::new();
                for (i, mid) in ids.iter().enumerate() {
                    match self.ai_mail_text(*mid, per, true, false).await {
                        Ok(m) => parts.push(format!(
                            "【邮件 {}】id：{}\n发件人：{}\n收件人：{}\n时间：{}\n主题：{}\n\n{}",
                            i + 1,
                            mid,
                            m.from,
                            m.to,
                            fmt_time(m.date),
                            m.subject,
                            m.text
                        )),
                        Err(err) if *mid == id => return Err(err),
                        Err(_) => parts.push(format!("【邮件 {}】id：{}（无法读取，已略过）", i + 1, mid)),
                    }
                }
                Ok(ok_text(format!("这个会话共 {} 封邮件：\n\n{}", ids.len(), parts.join("\n\n"))))
            }
            "get_pending" => {
                let s = self.ai_settings();
                let items: Vec<Value> = self
                    .ai_reminders()?
                    .into_iter()
                    .filter(|r| policy::allows(&s, &r.message.account_id, false))
                    .map(|r| {
                        json!({
                            "kind": r.kind,
                            "id": r.message.id,
                            "from": from_text(&r.message.from),
                            "subject": r.message.subject,
                            "date": fmt_time(r.message.date),
                            "account": r.account_label,
                            "summary": r.insight.one_line,
                            "needs_reply": r.insight.needs_reply,
                            "deadline": r.insight.deadline.map(fmt_time),
                            "todos": r.insight.todos,
                            "risk": r.insight.risk,
                        })
                    })
                    .collect();
                if items.is_empty() {
                    return Ok(ok_text("现在没有待处理的事项。"));
                }
                Ok(ok_text(format!("待处理 {} 项：\n{}", items.len(), serde_json::to_string(&items)?)))
            }
            "draft_email" => {
                let to = addrs_arg(args, "to")?;
                if to.is_empty() {
                    return Err(Error::Invalid("请提供收件人 to".into()));
                }
                let account = self.mcp_account(str_arg(args, "account").as_deref())?;
                let body = args["body"].as_str().unwrap_or("").to_string();
                let d = Draft {
                    account_id: account.id,
                    to,
                    cc: addrs_arg(args, "cc")?,
                    subject: args["subject"].as_str().unwrap_or("").trim().to_string(),
                    html: text_html(&body),
                    text: Some(body),
                    ..Default::default()
                };
                let d = self.mcp_save_draft(d).await?;
                Ok(ok_text(format!("已保存为草稿，没有发送：{}", json!({ "draft_id": d.id }))))
            }
            "reply_email" => {
                let id = id_arg(args, "id")?;
                let body = str_arg(args, "body").ok_or_else(|| Error::Invalid("缺少参数 body（回复正文）".into()))?;
                let mode = if args["reply_all"].as_bool().unwrap_or(false) { ComposeMode::ReplyAll } else { ComposeMode::Reply };
                let mut d = self.ai_reply_draft(id, &body, mode, false).await?;
                d.text = None;
                let d = self.mcp_save_draft(d).await?;
                Ok(ok_text(format!("已生成回复草稿，没有发送：{}", json!({ "draft_id": d.id }))))
            }
            "send_email" => self.mcp_send(args).await,
            "mark_emails" => {
                let ids = ids_arg(args, "ids")?;
                let action_name = str_arg(args, "action").unwrap_or_default();
                let (action, label) = match action_name.as_str() {
                    "read" => (Action::Read, "已读"),
                    "unread" => (Action::Unread, "未读"),
                    "star" => (Action::Flag, "星标"),
                    "unstar" => (Action::Unflag, "取消星标"),
                    "archive" => (Action::Archive, "归档"),
                    other => return Err(Error::Invalid(format!("未知操作 {other}：可用 read / unread / star / unstar / archive"))),
                };
                let r = self.act(&ids, action).await?;
                Ok(ok_text(format!("已处理 {} 封邮件（{label}）。", r.affected)))
            }
            "delete_emails" => self.mcp_delete(args).await,
            other => Err(Error::Invalid(format!("未知工具 {other}"))),
        }
    }

    // ---- helpers ------------------------------------------------------------------------

    /// Search with OR groups expanded, newest first; policy-hidden accounts left out.
    fn mcp_search(&self, mailbox: &Mailbox, query: &str, limit: usize) -> Result<Vec<MessageSummary>> {
        let s = self.ai_settings();
        let variants: Vec<String> = if query.trim().is_empty() { vec![String::new()] } else { loops::expand_or(query) };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for v in variants {
            let search = if v.trim().is_empty() { None } else { Some(v) };
            let list = self.db.list_messages(&ListQuery { mailbox: mailbox.clone(), search, limit: (limit * 2) as i64, ..Default::default() })?;
            for m in list {
                if seen.insert(m.id) && policy::allows(&s, &m.account_id, false) {
                    out.push(m);
                }
            }
        }
        out.sort_by(|a, b| b.date.cmp(&a.date));
        out.truncate(limit);
        Ok(out)
    }

    fn mcp_list_text(&self, list: &[MessageSummary], empty: &str) -> Result<ToolText> {
        if list.is_empty() {
            return Ok(ok_text(empty));
        }
        let labels: HashMap<String, String> = self.db.accounts()?.into_iter().map(|a| (a.id, a.label)).collect();
        let items: Vec<Value> = list
            .iter()
            .map(|m| {
                json!({
                    "id": m.id,
                    "from": from_text(&m.from),
                    "subject": m.subject,
                    "date": fmt_time(m.date),
                    "snippet": m.snippet,
                    "unread": !m.flags.seen,
                    "account": labels.get(&m.account_id).cloned().unwrap_or_default(),
                })
            })
            .collect();
        Ok(ok_text(format!("共 {} 封邮件（最新的在前）：\n{}", items.len(), serde_json::to_string(&items)?)))
    }

    /// The sending account: by id, address or label; the first enabled one by default.
    fn mcp_account(&self, sel: Option<&str>) -> Result<Account> {
        let accounts = self.db.accounts()?;
        match sel.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => accounts
                .into_iter()
                .find(|a| a.id == s || a.email.eq_ignore_ascii_case(s) || a.label == s)
                .ok_or_else(|| Error::NotFound(format!("找不到账户 {s}"))),
            None => accounts.into_iter().find(|a| a.enabled).ok_or_else(|| Error::NotFound("还没有添加邮箱账户".into())),
        }
    }

    /// Saves locally and uploads to the server's Drafts folder.
    async fn mcp_save_draft(&self, d: Draft) -> Result<Draft> {
        let d = self.save_draft(d)?;
        if let Err(err) = self.sync_draft(&d.id).await {
            tracing::warn!(error = %err, "could not upload mcp draft");
        }
        Ok(d)
    }

    async fn mcp_send(&self, args: &Value) -> Result<ToolText> {
        let draft = match str_arg(args, "draft_id") {
            Some(did) => self.db.draft(&did)?.map(|(d, _)| d).ok_or_else(|| Error::NotFound(format!("草稿 {did} 不存在")))?,
            None => {
                let to = addrs_arg(args, "to")?;
                if to.is_empty() {
                    return Err(Error::Invalid("请提供 draft_id，或者收件人 to、主题 subject 和正文 body".into()));
                }
                let account = self.mcp_account(str_arg(args, "account").as_deref())?;
                let body = args["body"].as_str().unwrap_or("").to_string();
                let d = Draft {
                    account_id: account.id,
                    to,
                    cc: addrs_arg(args, "cc")?,
                    bcc: addrs_arg(args, "bcc")?,
                    subject: args["subject"].as_str().unwrap_or("").trim().to_string(),
                    html: text_html(&body),
                    text: Some(body),
                    ..Default::default()
                };
                // Saved first, so a refusal keeps the draft.
                self.mcp_save_draft(d).await?
            }
        };
        if draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty() {
            return Err(Error::Invalid("草稿没有收件人".into()));
        }
        let title = format!("AI 请求发送邮件：{}", subject_or_none(&draft.subject));
        let detail = send_detail(&draft);
        let secs = confirm_timeout().as_secs();
        match self.ai_confirm("send", title, detail, Some(draft.id.clone()), Vec::new()).await {
            Decision::Approved => {
                // The user may have edited the draft meanwhile.
                let d = self.db.draft(&draft.id)?.map(|(d, _)| d).unwrap_or(draft);
                self.send_draft(d, None)?;
                let undo = self.settings().undo_send_secs;
                Ok(ok_text(format!("已发送（{undo} 秒内可在 Mailbox AI 中撤销）。")))
            }
            Decision::Rejected => Ok(ok_text(format!("用户拒绝了发送这封邮件（已保留草稿 {}）。", draft.id))),
            Decision::TimedOut => Ok(ok_text(format!("等待确认超时（{secs} 秒内用户没有确认），邮件没有发送（已保留草稿 {}）。", draft.id))),
        }
    }

    async fn mcp_delete(&self, args: &Value) -> Result<ToolText> {
        let ids = ids_arg(args, "ids")?;
        let msgs = self.db.messages_by_ids(&ids)?;
        if msgs.is_empty() {
            return Err(Error::NotFound("这些邮件不存在".into()));
        }
        let found: Vec<MessageId> = msgs.iter().map(|m| m.summary.id).collect();
        let mut lines: Vec<String> = msgs
            .iter()
            .take(20)
            .map(|m| format!("· {} · {} · {}", m.summary.from.display(), subject_or_none(&m.summary.subject), fmt_time(m.summary.date)))
            .collect();
        if msgs.len() > 20 {
            lines.push(format!("…等共 {} 封", msgs.len()));
        }
        let title = format!("AI 请求删除 {} 封邮件", found.len());
        let secs = confirm_timeout().as_secs();
        match self.ai_confirm("delete", title, lines.join("\n"), None, found.clone()).await {
            Decision::Approved => {
                let r = self.act(&found, Action::Trash).await?;
                Ok(ok_text(format!("已把 {} 封邮件移到废纸篓。", r.affected)))
            }
            Decision::Rejected => Ok(ok_text("用户拒绝了删除这些邮件，没有删除。")),
            Decision::TimedOut => Ok(ok_text(format!("等待确认超时（{secs} 秒内用户没有确认），没有删除邮件。"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_tools() {
        assert_eq!(negotiate_version(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate_version(Some("2024-11-05")), "2024-11-05");
        assert_eq!(negotiate_version(Some("1999-01-01")), "2025-06-18");
        assert_eq!(negotiate_version(None), "2025-06-18");
        let names = tool_names();
        assert_eq!(names.len(), 11);
        for t in tool_definitions().as_array().unwrap() {
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
            assert!(!t["description"].as_str().unwrap().is_empty());
        }
        assert!(names.contains(&"send_email".to_string()) && names.contains(&"delete_emails".to_string()));
    }

    #[test]
    fn arguments() {
        let a = parse_address("\"张三\" <zs@example.com>").unwrap();
        assert_eq!((a.name.as_deref(), a.email.as_str()), (Some("张三"), "zs@example.com"));
        assert_eq!(parse_address("ls@example.com").unwrap().name, None);
        assert!(parse_address("不是地址").is_none());
        let args = json!({"to": "a@x.cn, B <b@x.cn>", "ids": [1, "2", "x"], "id": "7"});
        assert_eq!(addrs_arg(&args, "to").unwrap().len(), 2);
        assert!(addrs_arg(&json!({"to": ["bad"]}), "to").is_err());
        assert_eq!(ids_arg(&args, "ids").unwrap(), vec![1, 2]);
        assert_eq!(id_arg(&args, "id").unwrap(), 7);
        assert!(id_arg(&args, "missing").is_err());
        let d = Draft {
            to: vec![EmailAddress::new(Some("张三"), "zs@x.cn")],
            subject: "周报".into(),
            text: Some("第一段\n\n第二段".into()),
            ..Default::default()
        };
        let detail = send_detail(&d);
        assert!(detail.starts_with("收件人：张三 <zs@x.cn>\n主题：周报\n\n第一段"), "{detail}");
        assert_eq!(random_hex(32).len(), 64);
    }
}

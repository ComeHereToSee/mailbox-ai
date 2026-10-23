//! Drafts, the outbox (undo send / send later), reply & forward templates, invitation
//! replies, unsubscribe and read receipts.

use super::{Action, Engine, detail};
use crate::error::{Error, Result};
use crate::events::Event;
use crate::html;
use crate::mime::build::{self, OutAttachment, Outgoing};
use crate::model::*;
use crate::store::{FolderSpec, NewMessage, OpKind, now};
use serde::Serialize;
use std::time::Duration;

fn ensure_prefix(subject: &str, prefix: &str) -> String {
    let lower = subject.trim().to_lowercase();
    let already = match prefix {
        "Re: " => ["re:", "回复:", "回复：", "答复:", "答复："].iter().any(|p| lower.starts_with(p)),
        _ => ["fwd:", "fw:", "转发:", "转发："].iter().any(|p| lower.starts_with(p)),
    };
    if already { subject.trim().to_string() } else { format!("{prefix}{}", subject.trim()) }
}

fn fmt_date(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_opt(ts, 0).single().map(|d| d.format("%Y年%m月%d日 %H:%M").to_string()).unwrap_or_default()
}

fn addr_html(a: &EmailAddress) -> String {
    match &a.name {
        Some(n) => format!("{} &lt;{}&gt;", html::escape_text(n), html::escape_text(&a.email)),
        None => html::escape_text(&a.email),
    }
}

fn dedupe(list: Vec<EmailAddress>, exclude: &[String]) -> Vec<EmailAddress> {
    let mut seen: Vec<String> = exclude.to_vec();
    let mut out = Vec::new();
    for a in list {
        let key = a.email.to_lowercase();
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(a);
    }
    out
}

fn markdown_to_html(md: &str) -> String {
    let mut opts = pulldown_cmark::Options::empty();
    opts.insert(pulldown_cmark::Options::ENABLE_TABLES);
    opts.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    opts.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
    let parser = pulldown_cmark::Parser::new_ext(md, opts);
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, parser);
    out
}

#[derive(Serialize, Debug)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum UnsubscribeResult {
    Done { method: String },
    OpenUrl { url: String },
}

impl Engine {
    // ---- drafts --------------------------------------------------------------------------

    pub fn save_draft(&self, mut d: Draft) -> Result<Draft> {
        if d.id.is_empty() {
            d.id = uuid::Uuid::new_v4().to_string();
        }
        d.updated_at = now();
        self.db.account(&d.account_id)?;
        self.db.save_draft(&d)?;
        Ok(d)
    }

    /// Uploads the draft to the server's Drafts folder (replacing older copies).
    pub async fn sync_draft(&self, draft_id: &str) -> Result<()> {
        let Some((d, _)) = self.db.draft(draft_id)? else { return Err(Error::NotFound("草稿不存在".into())) };
        let acc = self.db.account(&d.account_id)?;
        if acc.kind != AccountKind::Imap {
            return Ok(());
        }
        let (_, o) = self.build_outgoing(&d, true).await?;
        let raw = build::build(&o, true)?;
        let blob = format!("{}.eml", uuid::Uuid::new_v4().simple());
        crate::paths::write_private(&self.paths.uploads().join(&blob), &raw)?;
        self.db.enqueue_op(&acc.id, &OpKind::ReplaceDraft { draft_id: d.id.clone(), blob })?;
        self.wake(Some(&acc.id));
        Ok(())
    }

    pub fn discard_draft(&self, draft_id: &str) -> Result<()> {
        if let Some((d, _)) = self.db.draft(draft_id)? {
            self.db.delete_draft(draft_id)?;
            if let Ok(acc) = self.db.account(&d.account_id) {
                if acc.kind == AccountKind::Imap {
                    self.db.enqueue_op(&acc.id, &OpKind::DeleteDraft { draft_id: draft_id.into() })?;
                    self.wake(Some(&acc.id));
                }
            }
        }
        Ok(())
    }

    // ---- sending -------------------------------------------------------------------------

    /// Queues a message. Without `send_at` it goes out after the undo-send delay.
    pub fn send_draft(&self, mut d: Draft, send_at: Option<i64>) -> Result<OutboxItem> {
        if d.to.is_empty() && d.cc.is_empty() && d.bcc.is_empty() {
            return Err(Error::Invalid("请填写收件人".into()));
        }
        for a in d.to.iter().chain(d.cc.iter()).chain(d.bcc.iter()) {
            if crate::mime::extract_email(&a.email).as_deref() != Some(a.email.trim()) {
                return Err(Error::Invalid(format!("收件人地址无效: {}", a.email)));
            }
        }
        self.check_protection(&d)?;
        if d.id.is_empty() {
            d.id = uuid::Uuid::new_v4().to_string();
        }
        let delay = self.settings().undo_send_secs as i64;
        let at = send_at.unwrap_or_else(|| now() + delay);
        let item = self.db.queue_outgoing(&d, at)?;
        self.events.emit(Event::Outbox { id: item.id.clone(), status: item.status, error: None });
        self.outbox_wake.notify_one();
        Ok(item)
    }

    /// Undo send / cancel a scheduled message; returns the draft to reopen in the composer.
    pub fn cancel_send(&self, outbox_id: &str) -> Result<Option<Draft>> {
        let d = self.db.cancel_outgoing(outbox_id)?;
        if d.is_some() {
            self.events.emit(Event::Outbox { id: outbox_id.into(), status: OutboxStatus::Cancelled, error: None });
        }
        Ok(d)
    }

    pub fn retry_send(&self, outbox_id: &str) -> Result<()> {
        self.db.retry_outgoing(outbox_id, now())?;
        self.outbox_wake.notify_one();
        Ok(())
    }

    pub(super) async fn outbox_loop(&self) {
        loop {
            if let Ok(due) = self.db.due_outgoing(now()) {
                for item in due {
                    if self.db.set_outbox_status(&item.id, OutboxStatus::Sending, None).is_err() {
                        continue;
                    }
                    self.events.emit(Event::Outbox { id: item.id.clone(), status: OutboxStatus::Sending, error: None });
                    let e = self.clone();
                    tokio::spawn(async move { e.send_item(item).await });
                }
            }
            let wait = self
                .db
                .next_outgoing_time()
                .ok()
                .flatten()
                .map(|t| (t - now()).clamp(1, 30) as u64)
                .unwrap_or(30);
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = self.outbox_wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
            }
        }
    }

    async fn send_item(&self, item: OutboxItem) {
        let result = self.deliver(&item).await;
        match result {
            Ok(()) => {
                let _ = self.db.set_outbox_status(&item.id, OutboxStatus::Sent, None);
                self.events.emit(Event::Outbox { id: item.id.clone(), status: OutboxStatus::Sent, error: None });
            }
            Err(err) => {
                let msg = err.to_string();
                let permanent = err.is_auth() || matches!(err, Error::Invalid(_) | Error::NotFound(_) | Error::Smtp(_));
                if !permanent && item.attempts < 5 {
                    let backoff = [30, 120, 300, 900, 1800][item.attempts.clamp(0, 4) as usize];
                    let _ = self.db.set_outbox_status(&item.id, OutboxStatus::Failed, Some(&msg));
                    let _ = self.db.retry_outgoing(&item.id, now() + backoff);
                    self.events.emit(Event::Outbox { id: item.id.clone(), status: OutboxStatus::Queued, error: Some(msg) });
                } else {
                    let _ = self.db.set_outbox_status(&item.id, OutboxStatus::Failed, Some(&msg));
                    self.events.emit(Event::Outbox { id: item.id.clone(), status: OutboxStatus::Failed, error: Some(msg) });
                }
            }
        }
    }

    async fn deliver(&self, item: &OutboxItem) -> Result<()> {
        let d = self.db.outbox_draft(&item.id)?;
        let (acc, o) = self.build_outgoing(&d, false).await?;
        let mut recipients: Vec<String> = Vec::new();
        for a in o.to.iter().chain(o.cc.iter()).chain(o.bcc.iter()) {
            let e = a.email.trim().to_lowercase();
            if !recipients.contains(&e) {
                recipients.push(e);
            }
        }
        let raw = self.protect(&d, &o.from.email, &recipients, build::build(&o, false)?)?;
        let server = acc.config.smtp.clone().ok_or_else(|| Error::Invalid("未配置 SMTP 服务器".into()))?;
        let auth = self.smtp_auth(&acc).await?;
        let user = Self::login_name(&acc, &server);
        crate::smtp::send(&server, &user, &auth, &o.from.email, &recipients, &raw).await?;

        // Bookkeeping after a successful send; failures here must not resend.
        let copy = build::build(&o, true)
            .and_then(|c| self.protect(&d, &o.from.email, &recipients, c))
            .unwrap_or(raw);
        let _ = self.db.record_contacts(&[o.to.clone(), o.cc.clone(), o.bcc.clone()].concat(), 3);
        if let Err(err) = self.store_sent_copy(&acc, &copy).await {
            tracing::warn!(error = %err, "could not store sent copy");
        }
        if let Some(src) = d.source_message_id {
            let flag = match d.mode {
                ComposeMode::Reply | ComposeMode::ReplyAll => Some("answered"),
                ComposeMode::Forward | ComposeMode::ForwardAsAttachment => Some("forwarded"),
                ComposeMode::New => None,
            };
            if let Some(flag) = flag {
                let _ = self.db.set_flag(&[src], flag, true);
                if acc.kind == AccountKind::Imap {
                    let _ = self.db.enqueue_op(&acc.id, &OpKind::Flags { message_ids: vec![src], flag: flag.into(), add: true });
                }
                if flag == "answered" {
                    crate::ai::insight::mark_answered(self, src);
                }
            }
        }
        let _ = self.db.delete_draft(&d.id);
        if acc.kind == AccountKind::Imap {
            let _ = self.db.enqueue_op(&acc.id, &OpKind::DeleteDraft { draft_id: d.id.clone() });
        }
        if let Some(server_draft) = d.draft_message_id {
            let _ = self.act(&[server_draft], Action::Delete).await;
        }
        self.wake(Some(&acc.id));
        self.changed(Some(&acc.id));
        Ok(())
    }

    async fn store_sent_copy(&self, acc: &Account, raw: &[u8]) -> Result<()> {
        match acc.kind {
            AccountKind::Imap if acc.config.save_sent_copy => {
                let folder = match self.db.folder_by_role(&acc.id, FolderRole::Sent)? {
                    Some(f) => f,
                    None => {
                        self.db.enqueue_op(&acc.id, &OpKind::CreateFolder { path: "Sent".into() })?;
                        let id = self.db.upsert_folder(
                            &acc.id,
                            &FolderSpec { path: "Sent".into(), name: "Sent".into(), delimiter: None, role: FolderRole::Sent, selectable: true },
                        )?;
                        self.db.folder(id)?
                    }
                };
                let blob = format!("{}.eml", uuid::Uuid::new_v4().simple());
                crate::paths::write_private(&self.paths.uploads().join(&blob), raw)?;
                self.db.enqueue_op(&acc.id, &OpKind::Append { folder: folder.id, blob, flags: vec!["seen".into()], replace_uid: None })?;
            }
            AccountKind::Imap => {
                if let Some(f) = self.db.folder_by_role(&acc.id, FolderRole::Sent)? {
                    self.folder_requests.lock().entry(acc.id.clone()).or_default().insert(f.id);
                }
            }
            _ => {
                let folder = self.db.ensure_local_folder(&acc.id, FolderRole::Sent, "已发送")?;
                let h = crate::mime::parse_headers(raw);
                let nm = NewMessage {
                    account_id: acc.id.clone(),
                    folder_id: folder,
                    remote_id: Some(format!("sent:{}", uuid::Uuid::new_v4().simple())),
                    message_id: h.message_id.clone(),
                    in_reply_to: h.in_reply_to.clone(),
                    references: h.references.clone(),
                    subject: h.subject.clone(),
                    from: h.from.clone(),
                    to: h.to.clone(),
                    cc: h.cc.clone(),
                    bcc: h.bcc.clone(),
                    date: h.date.unwrap_or_else(now),
                    size: raw.len() as i64,
                    flags: Flags { seen: true, ..Default::default() },
                    ..Default::default()
                };
                let (id, _) = self.db.insert_message(&nm)?;
                detail::store_body(self, id, raw)?;
            }
        }
        Ok(())
    }

    /// Turns a draft into a ready-to-serialize message.
    pub(crate) async fn build_outgoing(&self, d: &Draft, for_draft: bool) -> Result<(Account, Outgoing)> {
        let acc = self.db.account(&d.account_id)?;
        let identity = match &d.identity_id {
            Some(id) => self.db.identity(id).or_else(|_| self.db.default_identity(&acc.id))?,
            None => self.db.default_identity(&acc.id)?,
        };
        let from_name = if identity.name.is_empty() { acc.name.clone() } else { identity.name.clone() };
        let from = EmailAddress::new(Some(&from_name), &identity.email);
        let domain = identity.email.rsplit_once('@').map(|(_, d)| d.to_string()).unwrap_or_else(|| "mailbox.local".into());

        let html_body = if let Some(md) = d.markdown.as_deref().filter(|m| !m.trim().is_empty()) {
            Some(markdown_to_html(md))
        } else if d.plain_text_only {
            None
        } else {
            Some(d.html.clone())
        };
        let mut attachments = Vec::new();
        let html_body = html_body.map(|h| {
            let (h, inline) = build::extract_data_images(&h, &domain);
            attachments.extend(inline);
            // Quoted images of the original message.
            h.replace("mailbox-cid://", "cid-src://")
        });
        let html_body = match html_body {
            Some(h) => Some(self.resolve_quoted_cids(&h, &mut attachments).await),
            None => None,
        };
        let text = d
            .text
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| d.markdown.clone())
            .unwrap_or_else(|| html::html_to_text(html_body.as_deref().unwrap_or("")));

        for a in &d.attachments {
            if let Some(path) = &a.path {
                let data = std::fs::read(path).map_err(|e| Error::NotFound(format!("附件 {path} 无法读取: {e}")))?;
                let filename = if a.filename.is_empty() {
                    std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "附件".into())
                } else {
                    a.filename.clone()
                };
                let mime = a.mime_type.clone().unwrap_or_else(|| build::guess_mime(&filename).to_string());
                attachments.push(OutAttachment { filename, mime, data, content_id: a.content_id.clone() });
            } else if let (Some(mid), Some(aid)) = (a.message_id, a.attachment_id) {
                let (name, mime, data) = self.attachment(mid, aid).await?;
                let filename = if a.filename.is_empty() { name } else { a.filename.clone() };
                attachments.push(OutAttachment { filename, mime, data, content_id: a.content_id.clone() });
            }
        }

        let mut in_reply_to = None;
        let mut references = Vec::new();
        if let Some(src_id) = d.source_message_id {
            if let Ok(src) = self.db.message(src_id) {
                if let Some(mid) = &src.message_id {
                    references = src.references.clone();
                    references.push(mid.clone());
                    if references.len() > 12 {
                        references.drain(..references.len() - 12);
                    }
                    if matches!(d.mode, ComposeMode::Reply | ComposeMode::ReplyAll) {
                        in_reply_to = Some(mid.clone());
                    }
                }
                if d.mode == ComposeMode::ForwardAsAttachment {
                    let raw = self.raw_message(src_id).await?;
                    let name = format!("{}.eml", if src.summary.subject.is_empty() { "邮件" } else { &src.summary.subject });
                    attachments.push(OutAttachment { filename: name, mime: "message/rfc822".into(), data: raw, content_id: None });
                }
            }
        }
        let mut bcc = d.bcc.clone();
        for extra in self.settings().auto_bcc.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()) {
            bcc.push(EmailAddress::new(None, extra));
        }
        let mut extra_headers = Vec::new();
        if for_draft {
            extra_headers.push(("X-Mailbox-Draft-Id".to_string(), d.id.clone()));
        }
        if d.auto_submitted {
            // RFC 3834: keeps other auto responders from answering our auto reply.
            extra_headers.push(("Auto-Submitted".to_string(), "auto-replied".to_string()));
        }
        let o = Outgoing {
            from,
            to: d.to.clone(),
            cc: d.cc.clone(),
            bcc,
            reply_to: identity.reply_to.clone(),
            subject: d.subject.clone(),
            html: html_body,
            text,
            attachments,
            in_reply_to,
            references,
            message_id: build::new_message_id(&identity.email),
            date: now(),
            read_receipt: d.request_read_receipt,
            priority_high: d.priority_high,
            calendar: d.calendar.clone(),
            extra_headers,
        };
        Ok((acc, o))
    }

    /// Re-embeds images of a quoted original (`mailbox-cid://<id>/<cid>`) as inline parts.
    async fn resolve_quoted_cids(&self, html_body: &str, atts: &mut Vec<OutAttachment>) -> String {
        let re = regex::Regex::new(r#"cid-src://(\d+)/([^"'\s>]+)"#).expect("regex");
        let mut out = html_body.to_string();
        let found: Vec<(String, i64, String)> = re
            .captures_iter(html_body)
            .map(|c| (c[0].to_string(), c[1].parse().unwrap_or(0), c[2].to_string()))
            .collect();
        for (whole, mid, enc) in found {
            let cid = percent_encoding::percent_decode_str(&enc).decode_utf8_lossy().to_string();
            if !atts.iter().any(|a| a.content_id.as_deref() == Some(&cid)) {
                if let Ok((mime, data)) = self.cid_part(mid, &cid).await {
                    atts.push(OutAttachment { filename: cid.clone(), mime, data, content_id: Some(cid.clone()) });
                }
            }
            out = out.replace(&whole, &format!("cid:{cid}"));
        }
        out
    }

    // ---- compose helpers -----------------------------------------------------------------

    /// Prefilled draft for reply / reply all / forward of a message.
    pub async fn reply_draft(&self, message_id: MessageId, mode: ComposeMode) -> Result<Draft> {
        let d = self.message_detail(message_id).await?;
        let m = self.db.message(message_id)?;
        let acc = self.db.account(&m.summary.account_id)?;
        let own = self.db.own_addresses()?;
        let identities = self.db.identities(Some(&acc.id))?;
        let identity = identities
            .iter()
            .find(|i| d.summary.to.iter().chain(d.cc.iter()).any(|a| a.email.eq_ignore_ascii_case(&i.email)))
            .cloned()
            .or_else(|| identities.iter().find(|i| i.is_default).cloned())
            .or_else(|| identities.first().cloned());
        let from_me = own.contains(&d.summary.from.email.to_lowercase());
        let primary = if from_me {
            d.summary.to.clone()
        } else if !d.reply_to.is_empty() {
            d.reply_to.clone()
        } else {
            vec![d.summary.from.clone()]
        };
        let (to, cc) = match mode {
            ComposeMode::Reply => (primary, vec![]),
            ComposeMode::ReplyAll => {
                let to = dedupe([primary.clone(), d.summary.to.clone()].concat(), if from_me { &[] } else { &own });
                let to_keys: Vec<String> = to.iter().map(|a| a.email.to_lowercase()).chain(own.iter().cloned()).collect();
                (to, dedupe(d.cc.clone(), &to_keys))
            }
            _ => (vec![], vec![]),
        };
        let original = d.html.clone().unwrap_or_default();
        let quote = match mode {
            ComposeMode::Forward | ComposeMode::ForwardAsAttachment => format!(
                "<div>---------- 转发的邮件 ----------<br>发件人：{}<br>日期：{}<br>主题：{}<br>收件人：{}{}</div><br>{}",
                addr_html(&d.summary.from),
                fmt_date(d.summary.date),
                html::escape_text(&d.summary.subject),
                d.summary.to.iter().map(addr_html).collect::<Vec<_>>().join(", "),
                if d.cc.is_empty() { String::new() } else { format!("<br>抄送：{}", d.cc.iter().map(addr_html).collect::<Vec<_>>().join(", ")) },
                if mode == ComposeMode::Forward { original.clone() } else { String::new() }
            ),
            _ => format!(
                "<div>在 {}，{} 写道：</div><blockquote style=\"margin:0 0 0 .8ex;border-left:2px solid #ccc;padding-left:1ex\">{}</blockquote>",
                fmt_date(d.summary.date),
                addr_html(&d.summary.from),
                original
            ),
        };
        let signature = identity.as_ref().map(|i| i.signature_html.clone()).unwrap_or_default();
        let sig_block = format!("<div id=\"mailbox-signature\">{signature}</div>");
        let quote_block = format!("<div id=\"mailbox-quote\">{quote}</div>");
        let html_body = if self.settings().reply_position == "bottom" {
            format!("{quote_block}<div><br></div>{sig_block}")
        } else {
            format!("<div><br></div><div><br></div>{sig_block}{quote_block}")
        };
        let attachments = if mode == ComposeMode::Forward {
            d.attachments
                .iter()
                .filter(|a| !a.inline)
                .map(|a| DraftAttachment {
                    message_id: Some(message_id),
                    attachment_id: Some(a.id),
                    filename: a.filename.clone(),
                    mime_type: Some(a.mime_type.clone()),
                    size: Some(a.size),
                    ..Default::default()
                })
                .collect()
        } else {
            vec![]
        };
        let subject = match mode {
            ComposeMode::Forward | ComposeMode::ForwardAsAttachment => ensure_prefix(&d.summary.subject, "Fwd: "),
            _ => ensure_prefix(&d.summary.subject, "Re: "),
        };
        Ok(Draft {
            id: uuid::Uuid::new_v4().to_string(),
            account_id: acc.id.clone(),
            identity_id: identity.map(|i| i.id),
            to,
            cc,
            subject,
            html: html_body,
            attachments,
            mode,
            source_message_id: Some(message_id),
            updated_at: now(),
            ..Default::default()
        })
    }

    /// Opens a message from the Drafts folder for editing.
    pub async fn draft_from_message(&self, message_id: MessageId) -> Result<Draft> {
        let raw = self.raw_message(message_id).await?;
        let parsed = crate::mime::parse_message(&raw);
        if let Some(local_id) = parsed.as_ref().and_then(|m| m.header_raw("X-Mailbox-Draft-Id")).map(|s| s.trim().to_string()) {
            if let Some((mut d, _)) = self.db.draft(&local_id)? {
                d.draft_message_id = Some(message_id);
                return Ok(d);
            }
        }
        let (h, b) = crate::mime::parse_full(&raw);
        let m = self.db.message(message_id)?;
        let html_body = b.html.clone().unwrap_or_else(|| html::text_to_html(b.text.as_deref().unwrap_or("")));
        let html_body = html::sanitize(&html_body, message_id, false).html;
        Ok(Draft {
            id: uuid::Uuid::new_v4().to_string(),
            account_id: m.summary.account_id.clone(),
            to: h.to,
            cc: h.cc,
            bcc: h.bcc,
            subject: h.subject,
            html: html_body,
            attachments: b
                .attachments
                .iter()
                .filter(|a| !a.inline)
                .map(|a| DraftAttachment {
                    message_id: Some(message_id),
                    attachment_id: Some(a.id),
                    filename: a.filename.clone(),
                    mime_type: Some(a.mime_type.clone()),
                    size: Some(a.size),
                    ..Default::default()
                })
                .collect(),
            draft_message_id: Some(message_id),
            updated_at: now(),
            ..Default::default()
        })
    }

    /// Rule action: forward a message to other addresses.
    pub(crate) async fn forward_automatically(&self, message_id: MessageId, to: &[String]) -> Result<()> {
        let mut d = self.reply_draft(message_id, ComposeMode::Forward).await?;
        d.to = to.iter().map(|t| EmailAddress::new(None, t)).collect();
        self.send_draft(d, Some(now()))?;
        Ok(())
    }

    /// Accept / tentatively accept / decline an invitation (iMIP REPLY to the organizer).
    pub async fn rsvp(&self, message_id: MessageId, partstat: &str) -> Result<()> {
        let partstat = partstat.to_uppercase();
        let label = match partstat.as_str() {
            "ACCEPTED" => "已接受",
            "TENTATIVE" => "暂定",
            "DECLINED" => "已拒绝",
            _ => return Err(Error::Invalid("partstat 只能是 ACCEPTED / TENTATIVE / DECLINED".into())),
        };
        let d = self.message_detail(message_id).await?;
        let invite = d.invite.ok_or_else(|| Error::NotFound("这封邮件不是会议邀请".into()))?;
        let organizer = invite.organizer.clone().ok_or_else(|| Error::Invalid("邀请没有组织者".into()))?;
        let acc = self.db.account(&d.summary.account_id)?;
        let identities = self.db.identities(Some(&acc.id))?;
        let me = identities
            .iter()
            .find(|i| invite.attendees.iter().any(|a| a.email.eq_ignore_ascii_case(&i.email)))
            .or_else(|| identities.iter().find(|i| i.is_default))
            .cloned()
            .ok_or_else(|| Error::NotFound("没有可用的发件身份".into()))?;
        let ics = crate::ics::build_reply(&invite.raw, &me.email, Some(&me.name).filter(|n| !n.is_empty()).map(|s| s.as_str()), &partstat)?;
        let draft = Draft {
            id: uuid::Uuid::new_v4().to_string(),
            account_id: acc.id.clone(),
            identity_id: Some(me.id.clone()),
            to: vec![organizer],
            subject: format!("{label}: {}", invite.summary),
            html: format!("<p>{} {label}了会议「{}」。</p>", html::escape_text(&me.name), html::escape_text(&invite.summary)),
            mode: ComposeMode::Reply,
            source_message_id: Some(message_id),
            calendar: Some(("REPLY".into(), ics)),
            ..Default::default()
        };
        self.send_draft(draft, Some(now()))?;
        Ok(())
    }

    /// One-click unsubscribe (RFC 8058) or a mailto: unsubscribe; otherwise the URL to open.
    pub async fn unsubscribe(&self, message_id: MessageId) -> Result<UnsubscribeResult> {
        let d = self.message_detail(message_id).await?;
        let u = d.unsubscribe.ok_or_else(|| Error::NotFound("这封邮件没有退订信息".into()))?;
        if let (true, Some(url)) = (u.one_click, u.http.as_ref()) {
            let resp = self
                .http
                .post(url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body("List-Unsubscribe=One-Click")
                .send()
                .await?;
            if resp.status().is_success() {
                return Ok(UnsubscribeResult::Done { method: "one_click".into() });
            }
        }
        if let Some(mailto) = &u.mailto {
            let parsed = url::Url::parse(mailto).map_err(|_| Error::Invalid("无效的退订地址".into()))?;
            let to = percent_encoding::percent_decode_str(parsed.path()).decode_utf8_lossy().to_string();
            let mut subject = "unsubscribe".to_string();
            let mut body = "unsubscribe".to_string();
            for (k, v) in parsed.query_pairs() {
                match k.to_lowercase().as_str() {
                    "subject" => subject = v.to_string(),
                    "body" => body = v.to_string(),
                    _ => {}
                }
            }
            let draft = Draft {
                account_id: d.summary.account_id.clone(),
                to: vec![EmailAddress::new(None, &to)],
                subject,
                html: html::escape_text(&body),
                plain_text_only: true,
                text: Some(body),
                ..Default::default()
            };
            self.send_draft(draft, Some(now()))?;
            return Ok(UnsubscribeResult::Done { method: "mailto".into() });
        }
        match u.http {
            Some(url) => Ok(UnsubscribeResult::OpenUrl { url }),
            None => Err(Error::NotFound("无法自动退订".into())),
        }
    }

    /// Sends a read receipt for a message that requested one.
    pub async fn send_read_receipt(&self, message_id: MessageId) -> Result<()> {
        let d = self.message_detail(message_id).await?;
        let to = d.read_receipt_to.clone().ok_or_else(|| Error::NotFound("这封邮件没有请求回执".into()))?;
        let text = format!(
            "这是一封已读回执。\n\n您于 {} 发送的邮件《{}》已于 {} 被阅读。",
            fmt_date(d.summary.date),
            d.summary.subject,
            fmt_date(now())
        );
        let draft = Draft {
            account_id: d.summary.account_id.clone(),
            to: vec![EmailAddress::new(None, &to)],
            subject: format!("已读: {}", d.summary.subject),
            html: html::text_to_html(&text),
            text: Some(text),
            plain_text_only: true,
            ..Default::default()
        };
        self.send_draft(draft, Some(now()))?;
        Ok(())
    }
}

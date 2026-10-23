//! Batch jobs over selected messages (`POST /v1/ai/batch`): kept in memory, the most
//! recent 20; three messages at a time with the quick model.

use super::model::{BatchItem, BatchJob, ModelRef, SuggestedAction};
use super::{clean_output, prompts};
use crate::Engine;
use crate::engine::{Action, UnsubscribeResult};
use crate::error::{Error, Result};
use crate::events::Event;
use crate::model::{Category, MessageId};
use crate::store::now;
use futures::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub const OPS: &[&str] = &["summarize", "classify", "todos", "draft_replies", "custom", "unsubscribe"];
const MAX_JOBS: usize = 20;
const MAX_ITEMS: usize = 200;
const CONCURRENCY: usize = 3;

#[derive(Debug, Clone, Default)]
struct ItemOutput {
    output: String,
    suggested: Vec<SuggestedAction>,
    draft_id: Option<String>,
}

fn category_zh(c: &str) -> &'static str {
    match c {
        "social" => "社交",
        "promotions" => "推广",
        "updates" => "通知",
        "forums" => "论坛",
        _ => "主要",
    }
}

/// classify JSON → (one line for the table, suggested actions).
pub fn classify_suggestions(v: &Value, current: Category) -> (String, Vec<SuggestedAction>) {
    let label = v["label"].as_str().unwrap_or("").trim().to_string();
    let category = v["category"].as_str().unwrap_or("").trim().to_lowercase();
    let action = v["action"].as_str().unwrap_or("keep").trim().to_lowercase();
    let reason = v["reason"].as_str().unwrap_or("").trim().to_string();
    let valid_category = matches!(category.as_str(), "primary" | "social" | "promotions" | "updates" | "forums");
    let mut out = Vec::new();
    if !label.is_empty() {
        out.push(SuggestedAction { action: "label".into(), value: Some(label.clone()), reason: reason.clone() });
    }
    if valid_category && Category::parse(&category) != current {
        out.push(SuggestedAction { action: "category".into(), value: Some(category.clone()), reason: reason.clone() });
    }
    if action == "archive" || action == "trash" {
        out.push(SuggestedAction { action: action.clone(), value: None, reason: reason.clone() });
    }
    let mut parts = Vec::new();
    if !label.is_empty() {
        parts.push(format!("标签：{label}"));
    }
    if valid_category {
        parts.push(format!("分类：{}", category_zh(&category)));
    }
    let verdict = match action.as_str() {
        "archive" => "归档",
        "trash" => "删除",
        _ => "保留",
    };
    parts.push(format!("建议：{verdict}"));
    let mut line = parts.join(" · ");
    if !reason.is_empty() {
        line.push_str(&format!("（{reason}）"));
    }
    (line, out)
}

/// Overall result of a batch unsubscribe (no AI).
pub fn unsubscribe_summary(items: &[BatchItem]) -> String {
    let done = items.iter().filter(|i| i.error.is_none() && i.output == UNSUBSCRIBED).count();
    let open: Vec<&BatchItem> = items.iter().filter(|i| i.error.is_none() && i.output != UNSUBSCRIBED).collect();
    let failed = items.iter().filter(|i| i.error.is_some()).count();
    let mut s = format!("已退订 {done} 封");
    if !open.is_empty() {
        s.push_str(&format!("，{} 封需要在浏览器中打开退订页面", open.len()));
    }
    if failed > 0 {
        s.push_str(&format!("，{failed} 封没有退订信息或退订失败"));
    }
    s.push('。');
    for i in open {
        s.push_str(&format!("\n- {} · {}：{}", i.from, i.subject, i.output));
    }
    s
}

const UNSUBSCRIBED: &str = "已退订";

impl Engine {
    /// Starts a job and returns its id; progress arrives as `ai_job` events.
    pub fn ai_start_batch(&self, ids: Vec<MessageId>, op: &str, instruction: Option<String>) -> Result<String> {
        let op = op.trim();
        if !OPS.contains(&op) {
            return Err(Error::Invalid(format!("未知的批量操作 {op}")));
        }
        let instruction = instruction.unwrap_or_default().trim().to_string();
        if op == "custom" && instruction.is_empty() {
            return Err(Error::Invalid("请填写要对这些邮件做什么".into()));
        }
        if ids.is_empty() {
            return Err(Error::Invalid("请先选择邮件".into()));
        }
        // Unsubscribing does not use a model.
        let model = if op == "unsubscribe" { ModelRef::default() } else { self.ai_model(true)? };
        let mut items: Vec<BatchItem> = Vec::new();
        for id in ids {
            if items.len() >= MAX_ITEMS {
                break;
            }
            if items.iter().any(|i| i.message_id == id) {
                continue;
            }
            let Ok(m) = self.db.message(id) else { continue };
            items.push(BatchItem {
                message_id: id,
                subject: m.summary.subject.clone(),
                from: m.summary.from.display(),
                ..Default::default()
            });
        }
        if items.is_empty() {
            return Err(Error::NotFound("选中的邮件不存在".into()));
        }
        let job = BatchJob {
            id: crate::store::new_id(),
            op: op.to_string(),
            status: "running".into(),
            total: items.len(),
            done: 0,
            items,
            summary: None,
            created_at: now(),
        };
        let job_id = job.id.clone();
        {
            let mut jobs = self.ai.jobs.lock();
            jobs.insert(job_id.clone(), job);
            while jobs.len() > MAX_JOBS {
                let oldest = jobs
                    .values()
                    .filter(|j| j.status != "running")
                    .min_by_key(|j| j.created_at)
                    .or_else(|| jobs.values().min_by_key(|j| j.created_at))
                    .map(|j| j.id.clone());
                match oldest {
                    Some(o) => {
                        jobs.remove(&o);
                    }
                    None => break,
                }
            }
        }
        let token = CancellationToken::new();
        self.ai.job_cancel.lock().insert(job_id.clone(), token.clone());
        let e = self.clone();
        let id = job_id.clone();
        let op = op.to_string();
        tokio::spawn(async move { e.ai_run_batch(id, op, instruction, model, token).await });
        Ok(job_id)
    }

    pub fn ai_job(&self, id: &str) -> Option<BatchJob> {
        self.ai.jobs.lock().get(id).cloned()
    }

    pub fn ai_cancel_job(&self, id: &str) -> Result<()> {
        if self.ai_job(id).is_none() {
            return Err(Error::NotFound("任务不存在或已过期".into()));
        }
        if let Some(t) = self.ai.job_cancel.lock().get(id) {
            t.cancel();
        }
        Ok(())
    }

    async fn ai_run_batch(&self, job_id: String, op: String, instruction: String, model: ModelRef, cancel: CancellationToken) {
        let ids: Vec<MessageId> = self.ai_job(&job_id).map(|j| j.items.iter().map(|i| i.message_id).collect()).unwrap_or_default();
        let total = ids.len();
        let mut futs = Vec::new();
        for (idx, id) in ids.into_iter().enumerate() {
            let e = self.clone();
            let op = op.clone();
            let instruction = instruction.clone();
            let model = model.clone();
            let cancel = cancel.clone();
            futs.push(async move {
                if cancel.is_cancelled() {
                    return (idx, Err(Error::Ai("已取消".into())));
                }
                let r = tokio::select! {
                    _ = cancel.cancelled() => Err(Error::Ai("已取消".into())),
                    r = e.ai_batch_item(id, &op, &instruction, &model) => r,
                };
                (idx, r)
            });
        }
        let mut results = std::pin::pin!(futures::stream::iter(futs).buffer_unordered(CONCURRENCY));
        let mut done = 0usize;
        while let Some((idx, r)) = results.next().await {
            done += 1;
            {
                let mut jobs = self.ai.jobs.lock();
                if let Some(job) = jobs.get_mut(&job_id) {
                    job.done = done;
                    if let Some(item) = job.items.get_mut(idx) {
                        match r {
                            Ok(out) => {
                                item.output = out.output;
                                item.suggested = out.suggested;
                                item.draft_id = out.draft_id;
                            }
                            Err(err) => item.error = Some(err.to_string()),
                        }
                    }
                }
            }
            self.events.emit(Event::AiJob { job_id: job_id.clone(), done, total, status: "running".into() });
        }
        let cancelled = cancel.is_cancelled();
        let items: Vec<BatchItem> = self.ai_job(&job_id).map(|j| j.items).unwrap_or_default();
        let all_failed = !items.is_empty() && items.iter().all(|i| i.error.is_some());
        let mut summary: Option<String> = None;
        if !cancelled && op == "unsubscribe" {
            summary = Some(unsubscribe_summary(&items));
        }
        if !cancelled && !all_failed {
            match op.as_str() {
                "summarize" | "todos" | "custom" => {
                    summary = Some(match self.ai_batch_summary(&op, &instruction, &model, &items).await {
                        Ok(s) => s,
                        Err(err) => format!("（汇总失败：{err}）"),
                    });
                }
                "draft_replies" => {
                    let n = items.iter().filter(|i| i.draft_id.is_some()).count();
                    summary = Some(format!("已为 {n} 封邮件生成回复草稿，可以在草稿箱中查看、修改后发送。"));
                }
                _ => {}
            }
        }
        let status = if cancelled {
            "cancelled"
        } else if all_failed {
            "failed"
        } else {
            "done"
        };
        {
            let mut jobs = self.ai.jobs.lock();
            if let Some(job) = jobs.get_mut(&job_id) {
                job.status = status.to_string();
                job.summary = summary;
            }
        }
        self.ai.job_cancel.lock().remove(&job_id);
        self.events.emit(Event::AiJob { job_id, done, total, status: status.to_string() });
    }

    async fn ai_batch_item(&self, id: MessageId, op: &str, instruction: &str, model: &ModelRef) -> Result<ItemOutput> {
        if op == "unsubscribe" {
            let output = match self.unsubscribe(id).await? {
                UnsubscribeResult::Done { .. } => UNSUBSCRIBED.to_string(),
                UnsubscribeResult::OpenUrl { url } => format!("需要在浏览器中打开：{url}"),
            };
            return Ok(ItemOutput { output, ..Default::default() });
        }
        let s = self.ai_settings();
        let local = self.ai_model_is_local(model);
        match op {
            "classify" => {
                let mail = self.ai_mail_text(id, s.max_body_chars.min(6000), true, local).await?;
                let current = self.db.message(id)?.summary.category;
                let (system, user) = prompts::classify(&s, &mail);
                let v = self.ai_json(model, "batch", system, user).await?;
                let (output, suggested) = classify_suggestions(&v, current);
                Ok(ItemOutput { output, suggested, draft_id: None })
            }
            "draft_replies" => {
                let text = self.ai_reply_text(id, instruction, model, "batch", None).await?;
                let draft = self.ai_save_reply_draft(id, &text).await?;
                Ok(ItemOutput { output: text, suggested: Vec::new(), draft_id: Some(draft.id) })
            }
            _ => {
                let mail = self.ai_mail_text(id, s.max_body_chars.min(6000), true, local).await?;
                let (system, user) = prompts::batch_item(op, instruction, &s, &mail);
                let text = self.ai_complete(model, "batch", system, user, false, None).await?;
                Ok(ItemOutput { output: clean_output(&text), ..Default::default() })
            }
        }
    }

    async fn ai_batch_summary(&self, op: &str, instruction: &str, model: &ModelRef, items: &[BatchItem]) -> Result<String> {
        let s = self.ai_settings();
        let mut list = String::new();
        for (i, it) in items.iter().enumerate() {
            if it.error.is_some() || it.output.trim().is_empty() {
                continue;
            }
            list.push_str(&format!("[{}] {} · {}\n{}\n\n", i + 1, it.from, it.subject, it.output));
        }
        if list.is_empty() {
            return Ok(String::new());
        }
        let (system, user) = prompts::batch_summary(&s, op, instruction, &list);
        let text = self.ai_complete(model, "batch", system, user, false, None).await?;
        Ok(clean_output(&text))
    }

    /// `POST /v1/ai/jobs/{id}/apply`: runs the suggestions of these items (classify only).
    /// Returns how many messages were changed.
    pub async fn ai_apply_job(&self, job_id: &str, message_ids: &[MessageId]) -> Result<usize> {
        let job = self.ai_job(job_id).ok_or_else(|| Error::NotFound("任务不存在或已过期".into()))?;
        if job.op != "classify" {
            return Err(Error::Invalid("只有「分类整理」任务可以应用建议".into()));
        }
        // Resolved before any await: (message, actions with labels / categories first and
        // moves last, since the message leaves the folder).
        let mut plan: Vec<(MessageId, Vec<Action>)> = Vec::new();
        for item in &job.items {
            if !message_ids.contains(&item.message_id) {
                continue;
            }
            let Ok(m) = self.db.message(item.message_id) else { continue };
            let mut suggested: Vec<SuggestedAction> = item.suggested.clone();
            suggested.sort_by_key(|a| matches!(a.action.as_str(), "archive" | "trash" | "move") as u8);
            let actions: Vec<Action> = suggested.iter().filter_map(|a| self.ai_action(&m.summary.account_id, a)).collect();
            if !actions.is_empty() {
                plan.push((item.message_id, actions));
            }
        }
        let changed = plan.len();
        for (id, actions) in plan {
            for action in actions {
                self.act(&[id], action).await?;
            }
        }
        Ok(changed)
    }

    fn ai_action(&self, account_id: &str, a: &SuggestedAction) -> Option<Action> {
        let value = a.value.clone().unwrap_or_default().trim().to_string();
        let action = match a.action.as_str() {
            "archive" => Action::Archive,
            "trash" => Action::Trash,
            "star" => Action::Flag,
            "read" => Action::Read,
            "label" if !value.is_empty() => Action::Label { add: vec![value], remove: vec![] },
            "category" if !value.is_empty() => Action::Category { category: Category::parse(&value) },
            "move" if !value.is_empty() => Action::Move { folder_id: self.find_folder(account_id, &value)? },
            _ => return None,
        };
        Some(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_json_to_suggestions() {
        let v: Value = serde_json::from_str(r#"{"label":"账单","category":"updates","action":"archive","reason":"信用卡账单"}"#).unwrap();
        let (line, s) = classify_suggestions(&v, Category::Primary);
        assert_eq!(s.iter().map(|a| a.action.as_str()).collect::<Vec<_>>(), vec!["label", "category", "archive"]);
        assert_eq!(s[0].value.as_deref(), Some("账单"));
        assert_eq!(s[1].value.as_deref(), Some("updates"));
        assert!(line.contains("归档") && line.contains("信用卡账单"));
        // Same category and "keep": only the label.
        let v: Value = serde_json::from_str(r#"{"label":"工作","category":"primary","action":"keep"}"#).unwrap();
        let (_, s) = classify_suggestions(&v, Category::Primary);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn unsubscribe_overview() {
        let item = |output: &str, error: Option<&str>| BatchItem {
            subject: "周报".into(),
            from: "店铺".into(),
            output: output.into(),
            error: error.map(String::from),
            ..Default::default()
        };
        let s = unsubscribe_summary(&[item("已退订", None), item("需要在浏览器中打开：https://x.cn/u", None), item("", Some("没有退订信息"))]);
        assert!(s.starts_with("已退订 1 封，1 封需要在浏览器中打开退订页面，1 封没有退订信息或退订失败。"), "{s}");
        assert!(s.contains("https://x.cn/u"));
    }
}

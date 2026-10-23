//! Reading help: summaries (cached in kv space `ai_summary`, id `"{message_id}:{thread}"`),
//! translation and suggested replies.

use super::model::{ModelRef, ReplySuggestion, SummaryCache};
use super::{DeltaTx, StreamDone, clean_output, prompts};
use crate::Engine;
use crate::error::{Error, Result};
use crate::model::MessageId;
use crate::store::now;
use serde_json::json;

const SPACE: &str = "ai_summary";

fn cache_key(id: MessageId, thread: bool) -> String {
    format!("{id}:{thread}")
}

/// What `POST /messages/{id}/summary` will do, decided before the stream starts.
#[derive(Debug, Clone)]
pub enum SummaryPlan {
    Cached(SummaryCache),
    Generate { model: ModelRef, system: String, user: String, thread_count: i64 },
}

#[derive(Debug, Clone)]
pub struct TranslatePlan {
    pub target_lang: String,
    pub model: ModelRef,
    pub system: String,
    pub user: String,
}

impl Engine {
    fn ai_thread_count(&self, id: MessageId, thread: bool) -> Result<i64> {
        if !thread {
            return Ok(1);
        }
        Ok(self.ai_thread_ids(id)?.len() as i64)
    }

    /// The cached summary, unless the conversation got new messages since.
    pub fn ai_cached_summary(&self, id: MessageId, thread: bool) -> Result<Option<SummaryCache>> {
        let Some(c) = self.db.kv_get::<SummaryCache>(SPACE, &cache_key(id, thread))? else { return Ok(None) };
        if thread && c.thread_count != self.ai_thread_count(id, true)? {
            return Ok(None);
        }
        Ok(Some(c))
    }

    pub(crate) fn ai_store_summary(&self, id: MessageId, thread: bool, c: &SummaryCache) -> Result<()> {
        self.db.kv_put(SPACE, &cache_key(id, thread), c, 0)
    }

    pub async fn ai_summary_plan(&self, id: MessageId, thread: bool, force: bool) -> Result<SummaryPlan> {
        self.db.message(id)?;
        if !force {
            if let Some(c) = self.ai_cached_summary(id, thread)? {
                return Ok(SummaryPlan::Cached(c));
            }
        }
        let s = self.ai_settings();
        let model = self.ai_model(true)?;
        let local = self.ai_model_is_local(&model);
        let conversation = self.ai_conversation(id, thread, s.max_body_chars, local).await?;
        let (system, user) = prompts::summary(&s, &conversation, thread);
        let thread_count = self.ai_thread_count(id, thread)?;
        Ok(SummaryPlan::Generate { model, system, user, thread_count })
    }

    /// Streams the summary (a cached one as a single delta); `done` carries `cached`.
    pub async fn ai_summary_run(&self, id: MessageId, thread: bool, plan: SummaryPlan, tx: DeltaTx) -> Result<StreamDone> {
        match plan {
            SummaryPlan::Cached(c) => {
                if let Some(tx) = &tx {
                    let _ = tx.send(c.text.clone()).await;
                }
                Ok(StreamDone::new(c.text, c.model).with("cached", json!(true)))
            }
            SummaryPlan::Generate { model, system, user, thread_count } => {
                let raw = self.ai_complete(&model, "summary", system, user, false, tx).await?;
                let text = clean_output(&raw);
                let c = SummaryCache { text: text.clone(), model: model.label(), created_at: now(), thread_count };
                self.ai_store_summary(id, thread, &c)?;
                Ok(StreamDone::new(text, model.label()).with("cached", json!(false)))
            }
        }
    }

    pub async fn ai_translate_plan(&self, id: MessageId, target_lang: Option<String>) -> Result<TranslatePlan> {
        let s = self.ai_settings();
        let target = target_lang.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| {
            if s.language.trim().is_empty() || s.language == "auto" { "zh-CN".to_string() } else { s.language.clone() }
        });
        let model = self.ai_model(true)?;
        let local = self.ai_model_is_local(&model);
        let mail = self.ai_mail_text(id, s.max_body_chars, false, local).await?;
        let (system, user) = prompts::translate(&target, &mail);
        Ok(TranslatePlan { target_lang: target, model, system, user })
    }

    pub async fn ai_translate_run(&self, plan: TranslatePlan, tx: DeltaTx) -> Result<StreamDone> {
        let label = plan.model.label();
        let raw = self.ai_complete(&plan.model, "summary", plan.system, plan.user, false, tx).await?;
        Ok(StreamDone::new(clean_output(&raw), label).with("target_lang", json!(plan.target_lang)))
    }

    /// Three suggested replies (`label` ≤ 12 characters).
    pub async fn ai_replies(&self, id: MessageId) -> Result<Vec<ReplySuggestion>> {
        let s = self.ai_settings();
        let model = self.ai_model(false)?;
        let local = self.ai_model_is_local(&model);
        let conversation = self.ai_conversation(id, true, s.max_body_chars, local).await?;
        let (system, user) = prompts::replies(&s, &conversation);
        let v = self.ai_json(&model, "compose", system, user).await?;
        let mut out = Vec::new();
        for item in v["suggestions"].as_array().into_iter().flatten() {
            let text = item["text"].as_str().unwrap_or("").trim().to_string();
            if text.is_empty() {
                continue;
            }
            let label = item["label"].as_str().unwrap_or("").trim().to_string();
            let label: String = if label.is_empty() { text.chars().take(12).collect() } else { label.chars().take(12).collect() };
            out.push(ReplySuggestion { label, text });
            if out.len() == 3 {
                break;
            }
        }
        if out.is_empty() {
            return Err(Error::Ai("AI 没有给出可用的回复建议".into()));
        }
        Ok(out)
    }
}

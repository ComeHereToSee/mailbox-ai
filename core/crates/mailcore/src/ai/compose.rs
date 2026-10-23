//! Writing assistant: `POST /v1/ai/compose` (rewrites, write, reply, custom) and the reply
//! drafts used by auto replies and batch jobs.

use super::model::ModelRef;
use super::{DeltaTx, StreamDone, clean_output, prompts, split_subject, text_html};
use crate::Engine;
use crate::error::{Error, Result};
use crate::model::{ComposeMode, Draft, MessageId};
use serde::Deserialize;
use serde_json::json;

pub const OPS: &[&str] = &["polish", "formal", "friendly", "concise", "expand", "proofread", "translate", "write", "reply", "custom"];

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct ComposeRequest {
    pub op: String,
    /// Current body as plain text (may be empty).
    pub text: Option<String>,
    /// Extra wishes of the user (custom / write / reply).
    pub instruction: Option<String>,
    /// translate
    pub target_lang: Option<String>,
    /// reply: the message answered (its whole conversation is used).
    pub message_id: Option<MessageId>,
    /// Optional model override; defaults to `default_model`.
    pub model: Option<ModelRef>,
}

/// Everything resolved before the stream starts (errors become plain HTTP errors).
#[derive(Debug, Clone)]
pub struct ComposePlan {
    pub model: ModelRef,
    pub system: String,
    pub user: String,
}

impl Engine {
    pub async fn ai_compose_plan(&self, r: &ComposeRequest) -> Result<ComposePlan> {
        let op = r.op.trim();
        if !OPS.contains(&op) {
            return Err(Error::Invalid(format!("未知的写作操作 {op}")));
        }
        let text = r.text.clone().unwrap_or_default();
        let instruction = r.instruction.clone().unwrap_or_default();
        let s = self.ai_settings();
        let model = self.ai_model_or(r.model.clone(), false)?;
        let (system, user) = match op {
            "write" => {
                if instruction.trim().is_empty() {
                    return Err(Error::Invalid("请先写下要 AI 帮你写什么".into()));
                }
                prompts::write(&s, instruction.trim(), &text)
            }
            "reply" => {
                let id = r.message_id.ok_or_else(|| Error::Invalid("reply 需要 message_id".into()))?;
                let conversation = self.ai_conversation(id, true, s.max_body_chars).await?;
                prompts::reply(&s, &conversation, &instruction)
            }
            "custom" if text.trim().is_empty() => {
                if instruction.trim().is_empty() {
                    return Err(Error::Invalid("请填写指令".into()));
                }
                prompts::write(&s, instruction.trim(), "")
            }
            _ => {
                if text.trim().is_empty() {
                    return Err(Error::Invalid("请先输入要改写的正文".into()));
                }
                if op == "custom" && instruction.trim().is_empty() {
                    return Err(Error::Invalid("请填写指令".into()));
                }
                let target = r.target_lang.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| {
                    if s.language.starts_with("en") { "zh-CN".to_string() } else { "en".to_string() }
                });
                let text = super::truncate_chars(&text, s.max_body_chars);
                prompts::rewrite(op, &text, instruction.trim(), &target).ok_or_else(|| Error::Invalid(format!("未知的写作操作 {op}")))?
            }
        };
        Ok(ComposePlan { model, system, user })
    }

    /// Runs a plan; the `done` line carries `subject` when the model wrote a `主题：` line.
    pub async fn ai_compose_run(&self, plan: ComposePlan, tx: DeltaTx) -> Result<StreamDone> {
        let label = plan.model.label();
        let raw = self.ai_complete(&plan.model, plan.system, plan.user, false, tx).await?;
        let (text, subject) = split_subject(&clean_output(&raw));
        Ok(StreamDone::new(text, label).with("subject", json!(subject)))
    }

    /// Reply text for the last message of a conversation (no subject line).
    pub(crate) async fn ai_reply_text(&self, id: MessageId, instruction: &str, model: &ModelRef, tx: DeltaTx) -> Result<String> {
        let s = self.ai_settings();
        let conversation = self.ai_conversation(id, true, s.max_body_chars).await?;
        let (system, user) = prompts::reply(&s, &conversation, instruction);
        let raw = self.ai_complete(model, system, user, false, tx).await?;
        Ok(split_subject(&clean_output(&raw)).0)
    }

    /// The normal reply draft (quote, signature) with the AI text in front. Not saved.
    pub(crate) async fn ai_reply_draft(&self, id: MessageId, text: &str, auto_submitted: bool) -> Result<Draft> {
        let mut d = self.reply_draft(id, ComposeMode::Reply).await?;
        let rest = d.html.strip_prefix("<div><br></div><div><br></div>").unwrap_or(&d.html).to_string();
        d.html = format!("{}{}", text_html(text), rest);
        d.auto_submitted = auto_submitted;
        Ok(d)
    }

    /// Saves a reply draft locally and uploads it to the server's Drafts folder.
    pub(crate) async fn ai_save_reply_draft(&self, id: MessageId, text: &str) -> Result<Draft> {
        let d = self.ai_reply_draft(id, text, false).await?;
        let d = self.save_draft(d)?;
        if let Err(err) = self.sync_draft(&d.id).await {
            tracing::warn!(error = %err, "could not upload ai draft");
        }
        Ok(d)
    }
}

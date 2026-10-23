//! The user's prompt library (kv space `ai_prompt`) and custom prompts on a message
//! (`POST /v1/ai/messages/{id}/prompt`).

use super::model::{ModelRef, PromptPreset};
use super::{DeltaTx, StreamDone, clean_output, prompts, truncate_plain};
use crate::Engine;
use crate::error::{Error, Result};
use crate::model::MessageId;
use crate::store::new_id;

const SPACE: &str = "ai_prompt";
/// kv `ai` id: the default prompts were created once (deleting them all keeps them gone).
const SEEDED_FLAG: &str = "prompts_seeded";
pub const SCOPES: [&str; 3] = ["compose", "reader", "batch"];

/// Prompts created on first use.
pub fn defaults() -> Vec<PromptPreset> {
    let p = |id: &str, name: &str, scope: &str, prompt: &str, sort: i64| PromptPreset {
        id: id.to_string(),
        name: name.to_string(),
        scope: scope.to_string(),
        prompt: prompt.to_string(),
        sort,
    };
    vec![
        p("default-client-tone", "客户语气", "compose", "改成适合发给客户的语气", 1),
        p("default-100-chars", "压缩到 100 字", "compose", "压缩到 100 字以内", 2),
        p("default-amounts-dates", "金额和日期", "reader", "提取所有金额和日期", 3),
        p("default-boss-report", "向老板汇报", "reader", "用三句话向老板汇报这封邮件", 4),
        p("default-orders", "订单号和金额", "batch", "提取订单号和金额，输出表格", 5),
    ]
}

/// What `POST /messages/{id}/prompt` will run.
#[derive(Debug, Clone)]
pub struct PromptPlan {
    pub model: ModelRef,
    pub system: String,
    pub user: String,
}

impl Engine {
    /// All prompts (by `sort`); the defaults are created the first time.
    pub fn ai_prompts(&self) -> Vec<PromptPreset> {
        let seeded = self.db.kv_get::<bool>("ai", SEEDED_FLAG).ok().flatten().unwrap_or(false);
        if !seeded {
            for p in defaults() {
                if let Err(err) = self.db.kv_put(SPACE, &p.id, &p, p.sort) {
                    tracing::warn!(error = %err, "could not store default prompt");
                }
            }
            if let Err(err) = self.db.kv_put("ai", SEEDED_FLAG, &true, 0) {
                tracing::warn!(error = %err, "could not store ai flag");
            }
        }
        self.db.kv_list(SPACE).unwrap_or_default()
    }

    /// Saves a prompt; an empty id creates one (at the end of the list unless `sort` is set).
    pub fn ai_save_prompt(&self, mut p: PromptPreset) -> Result<PromptPreset> {
        p.prompt = p.prompt.trim().to_string();
        if p.prompt.is_empty() {
            return Err(Error::Invalid("请填写提示词内容".into()));
        }
        p.name = p.name.trim().to_string();
        if p.name.is_empty() {
            p.name = truncate_plain(&p.prompt, 12);
        }
        p.scope = p.scope.trim().to_string();
        if !SCOPES.contains(&p.scope.as_str()) {
            return Err(Error::Invalid(format!("提示词分类应为 compose / reader / batch，而不是「{}」", p.scope)));
        }
        let existing = self.ai_prompts();
        if p.id.trim().is_empty() {
            p.id = new_id();
            if p.sort == 0 {
                p.sort = existing.iter().map(|x| x.sort).max().unwrap_or(0) + 1;
            }
        }
        self.db.kv_put(SPACE, &p.id, &p, p.sort)?;
        Ok(p)
    }

    pub fn ai_delete_prompt(&self, id: &str) -> Result<()> {
        self.ai_prompts();
        if !self.db.kv_delete(SPACE, id)? {
            return Err(Error::NotFound("提示词不存在".into()));
        }
        Ok(())
    }

    /// A custom prompt on a message (or its conversation), default model.
    pub async fn ai_message_prompt_plan(&self, id: MessageId, prompt: &str, thread: bool) -> Result<PromptPlan> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(Error::Invalid("请填写提示词".into()));
        }
        self.db.message(id)?;
        let s = self.ai_settings();
        let model = self.ai_model(false)?;
        let local = self.ai_model_is_local(&model);
        let conversation = self.ai_conversation(id, thread, s.max_body_chars, local).await?;
        let (system, user) = prompts::reader_prompt(&s, prompt, &conversation, thread);
        Ok(PromptPlan { model, system, user })
    }

    pub async fn ai_message_prompt_run(&self, plan: PromptPlan, tx: DeltaTx) -> Result<StreamDone> {
        let label = plan.model.label();
        let raw = self.ai_complete(&plan.model, "summary", plan.system, plan.user, false, tx).await?;
        Ok(StreamDone::new(clean_output(&raw), label))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prompts() {
        let d = defaults();
        assert_eq!(d.len(), 5);
        assert_eq!(d.iter().filter(|p| p.scope == "compose").count(), 2);
        assert_eq!(d.iter().filter(|p| p.scope == "reader").count(), 2);
        assert_eq!(d.iter().filter(|p| p.scope == "batch").count(), 1);
        assert!(d.iter().all(|p| SCOPES.contains(&p.scope.as_str()) && !p.id.is_empty()));
        assert!(d.iter().any(|p| p.prompt == "用三句话向老板汇报这封邮件"));
    }
}

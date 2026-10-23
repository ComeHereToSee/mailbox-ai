//! Provider presets and the stored provider list (kv space `ai_provider`). API keys live in
//! the encrypted secrets file under `ai:key:<provider_id>` and are never returned.

use super::model::*;
use super::{client, truncate_plain};
use crate::Engine;
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::sync::LazyLock;
use std::time::Duration;

const SPACE: &str = "ai_provider";
/// kv sort value of custom providers (after every preset).
const CUSTOM_SORT: i64 = 1000;

static PRESETS: LazyLock<Vec<ProviderPreset>> = LazyLock::new(|| match serde_json::from_str(include_str!("presets.json")) {
    Ok(p) => p,
    Err(e) => {
        tracing::error!("ai/presets.json 无法解析: {e}");
        Vec::new()
    }
});

pub fn presets() -> &'static [ProviderPreset] {
    PRESETS.as_slice()
}

pub fn preset(id: &str) -> Option<&'static ProviderPreset> {
    presets().iter().find(|p| p.id == id)
}

/// Secrets key of a provider's API key.
pub fn key_name(provider_id: &str) -> String {
    format!("ai:key:{provider_id}")
}

/// Default provider entry of a preset.
pub fn from_preset(p: &ProviderPreset) -> AiProvider {
    AiProvider {
        id: p.id.clone(),
        kind: p.kind,
        name: p.name.clone(),
        base_url: p.base_url.clone(),
        api_version: if p.kind == ProviderKind::AzureOpenai { Some("2024-10-21".into()) } else { None },
        enabled: matches!(p.id.as_str(), "claude_code" | "codex" | "ollama"),
        models: p.models.iter().map(|m| AiModel::new(m)).collect(),
        has_key: false,
        custom: false,
    }
}

fn preset_index(id: &str) -> Option<usize> {
    presets().iter().position(|p| p.id == id)
}

impl Engine {
    /// Every provider: presets in preset order, then custom ones. Missing presets are
    /// created from their defaults (first launch, or a preset added by an update).
    pub fn ai_providers(&self) -> Vec<AiProvider> {
        let stored: Vec<AiProvider> = self.db.kv_list(SPACE).unwrap_or_default();
        let mut out: Vec<AiProvider> = Vec::new();
        for (i, p) in presets().iter().enumerate() {
            match stored.iter().find(|s| s.id == p.id) {
                Some(s) => {
                    let mut s = s.clone();
                    s.kind = p.kind;
                    s.custom = false;
                    out.push(s);
                }
                None => {
                    let d = from_preset(p);
                    if let Err(err) = self.db.kv_put(SPACE, &d.id, &d, i as i64) {
                        tracing::warn!(error = %err, "could not store ai provider");
                    }
                    out.push(d);
                }
            }
        }
        for s in stored {
            if preset_index(&s.id).is_none() {
                let mut s = s;
                s.custom = true;
                out.push(s);
            }
        }
        for p in out.iter_mut() {
            p.has_key = self.secrets.get(&key_name(&p.id)).is_some_and(|k| !k.trim().is_empty());
        }
        out
    }

    pub fn ai_provider(&self, id: &str) -> Option<AiProvider> {
        self.ai_providers().into_iter().find(|p| p.id == id)
    }

    /// Saves a provider (`PUT /v1/ai/providers/{id}`); an unknown id creates a custom provider.
    pub fn ai_save_provider(&self, id: &str, input: AiProviderInput) -> Result<AiProvider> {
        let id = id.trim();
        if id.is_empty() || id.contains(['/', ':']) {
            return Err(Error::Invalid("服务商 id 无效".into()));
        }
        let existing = self.ai_provider(id);
        let mut p = input.provider;
        p.id = id.to_string();
        let sort = match preset_index(id) {
            Some(i) => {
                let pr = &presets()[i];
                p.kind = pr.kind;
                p.custom = false;
                if p.name.trim().is_empty() {
                    p.name = pr.name.clone();
                }
                i as i64
            }
            None => {
                p.custom = true;
                if p.name.trim().is_empty() {
                    p.name = existing.as_ref().map(|e| e.name.clone()).unwrap_or_else(|| "自定义服务商".into());
                }
                CUSTOM_SORT
            }
        };
        p.name = p.name.trim().to_string();
        p.base_url = p.base_url.trim().to_string();
        p.api_version = p.api_version.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let mut models: Vec<AiModel> = Vec::new();
        for mut m in p.models.drain(..) {
            m.id = m.id.trim().to_string();
            if m.id.is_empty() || models.iter().any(|x| x.id == m.id) {
                continue;
            }
            if m.name.trim().is_empty() {
                m.name = m.id.clone();
            }
            if m.group.is_empty() {
                m.group = model_group(&m.id);
            }
            models.push(m);
        }
        p.models = models;
        p.has_key = false;
        match input.api_key.as_deref().map(str::trim) {
            Some("") => self.secrets.remove(&key_name(id))?,
            Some(k) => self.secrets.set(&key_name(id), k)?,
            None => {}
        }
        self.db.kv_put(SPACE, id, &p, sort)?;
        self.ai_provider(id).ok_or_else(|| Error::NotFound("服务商不存在".into()))
    }

    /// Custom providers are deleted; presets are reset to their defaults. The key is removed.
    pub fn ai_delete_provider(&self, id: &str) -> Result<()> {
        match preset_index(id) {
            Some(i) => {
                let d = from_preset(&presets()[i]);
                self.db.kv_put(SPACE, id, &d, i as i64)?;
            }
            None => {
                if !self.db.kv_delete(SPACE, id)? {
                    return Err(Error::NotFound("服务商不存在".into()));
                }
            }
        }
        self.secrets.remove(&key_name(id))?;
        Ok(())
    }

    /// Models offered by the provider (not saved). CLI providers return their presets.
    pub async fn ai_fetch_models(&self, id: &str) -> Result<Vec<AiModel>> {
        let p = self.ai_provider(id).ok_or_else(|| Error::NotFound("服务商不存在".into()))?;
        if p.kind.is_cli() {
            let models = match preset(id) {
                Some(pr) => pr.models.iter().map(|m| AiModel::new(m)).collect(),
                None => vec![AiModel::new("default")],
            };
            return Ok(models);
        }
        let key = self.secrets.get(&key_name(id));
        client::list_models(&self.ai.http, &p, key.as_deref()).await
    }

    /// "Check connection": asks the model to answer "OK". → `{ok, latency_ms, reply?, error?}`
    pub async fn ai_test_provider(&self, id: &str, model: Option<String>) -> Result<Value> {
        let p = self.ai_provider(id).ok_or_else(|| Error::NotFound("服务商不存在".into()))?;
        let model = model
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .or_else(|| p.models.iter().find(|m| m.enabled).map(|m| m.id.clone()))
            .or_else(|| p.models.first().map(|m| m.id.clone()));
        let Some(model) = model else {
            return Ok(json!({ "ok": false, "latency_ms": 0, "error": "请先添加一个模型" }));
        };
        let req = ChatRequest {
            model: ModelRef::new(id, &model),
            system: None,
            messages: vec![ChatMessage::user("这是一次连接测试。请只回复 OK 两个字母，不要输出其他内容。")],
            temperature: None,
            max_tokens: None,
            json: false,
        };
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(120), self.ai_chat(req, None)).await;
        let ms = started.elapsed().as_millis() as u64;
        Ok(match result {
            Err(_) => json!({ "ok": false, "latency_ms": ms, "error": "超过 120 秒没有响应" }),
            Ok(Err(err)) => json!({ "ok": false, "latency_ms": ms, "error": err.to_string() }),
            Ok(Ok(text)) => json!({ "ok": true, "latency_ms": ms, "reply": truncate_plain(text.trim(), 200) }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_parse_and_cover_the_spec() {
        let ids: Vec<&str> = presets().iter().map(|p| p.id.as_str()).collect();
        for id in [
            "claude_code", "codex", "openai", "anthropic", "gemini", "deepseek", "dashscope", "moonshot", "zhipu", "siliconflow",
            "doubao", "hunyuan", "qianfan", "minimax", "stepfun", "openrouter", "groq", "mistral", "xai", "azure_openai", "ollama",
            "lmstudio",
        ] {
            assert!(ids.contains(&id), "missing preset {id}");
        }
        assert_eq!(ids[0], "claude_code");
        let cc = preset("claude_code").unwrap();
        assert_eq!(cc.kind, ProviderKind::ClaudeCode);
        assert_eq!(cc.models, vec!["default", "sonnet", "opus", "haiku"]);
        assert!(!cc.needs_key);
        assert_eq!(preset("azure_openai").unwrap().kind, ProviderKind::AzureOpenai);
        assert!(from_preset(preset("ollama").unwrap()).enabled);
        assert!(!from_preset(preset("openai").unwrap()).enabled);
        assert!(from_preset(preset("codex").unwrap()).models.iter().all(|m| m.enabled));
    }
}

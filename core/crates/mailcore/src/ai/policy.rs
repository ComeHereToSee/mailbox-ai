//! Account AI policies (`AiSettings.account_policies`): "on" (default), "off" or
//! "local_only" (only models running on this machine). Checked where message text is
//! loaded for a model ([`Engine::ai_mail_text`]) and when lists of candidates are built
//! (ask the mailbox, loops, MCP).

use super::model::{AiProvider, AiSettings, ModelRef};
use super::ENCRYPTED_REFUSED;
use crate::Engine;
use crate::error::{Error, Result};
use crate::model::MessageId;

pub const OFF: &str = "这个账户已关闭 AI";
pub const LOCAL_ONLY: &str = "这个账户只允许使用本地模型";

/// Whether a URL points at this machine (localhost, 127.0.0.1, ::1).
pub fn is_local_url(s: &str) -> bool {
    let Ok(u) = url::Url::parse(s.trim()) else { return false };
    match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Whether a provider runs locally: Ollama, LM Studio, or an API on localhost. The
/// subscription CLIs talk to the cloud.
pub fn is_local_provider(p: &AiProvider) -> bool {
    if matches!(p.id.as_str(), "ollama" | "lmstudio") {
        return true;
    }
    if p.kind.is_cli() {
        return false;
    }
    is_local_url(&p.base_url)
}

/// None when the policy lets this model see the account's mail; otherwise why not.
pub fn verdict(policy: &str, local: bool) -> Option<&'static str> {
    match policy {
        "off" => Some(OFF),
        "local_only" if !local => Some(LOCAL_ONLY),
        _ => None,
    }
}

/// The policy of an account ("on" when not set).
pub fn policy_of<'a>(s: &'a AiSettings, account_id: &str) -> &'a str {
    s.account_policies.get(account_id).map(String::as_str).unwrap_or("on")
}

/// Whether a (local or remote) model may see mail of the account.
pub fn allows(s: &AiSettings, account_id: &str, local: bool) -> bool {
    verdict(policy_of(s, account_id), local).is_none()
}

impl Engine {
    /// "on" | "off" | "local_only"
    pub fn ai_account_policy(&self, account_id: &str) -> String {
        policy_of(&self.ai_settings(), account_id).to_string()
    }

    /// Whether the model's provider runs on this machine.
    pub fn ai_model_is_local(&self, m: &ModelRef) -> bool {
        self.ai_provider(&m.provider).is_some_and(|p| is_local_provider(&p))
    }

    /// `AiRefused` when the account's policy keeps its mail from this model.
    pub(crate) fn ai_check_account(&self, account_id: &str, local: bool) -> Result<()> {
        match verdict(policy_of(&self.ai_settings(), account_id), local) {
            Some(msg) => Err(Error::AiRefused(msg.to_string())),
            None => Ok(()),
        }
    }

    /// The account policy and the encrypted-mail rule for a message whose content (or an
    /// attachment) goes to a model.
    pub(crate) async fn ai_check_message(&self, id: MessageId, local: bool) -> Result<()> {
        let account_id = self.db.message(id)?.summary.account_id;
        self.ai_check_account(&account_id, local)?;
        let d = self.message_detail(id).await?;
        if d.encrypted && !self.ai_settings().allow_encrypted {
            return Err(Error::AiRefused(ENCRYPTED_REFUSED.into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::model::ProviderKind;

    #[test]
    fn local_providers() {
        assert!(is_local_url("http://localhost:11434/v1"));
        assert!(is_local_url("http://127.0.0.1:1234/v1"));
        assert!(is_local_url("http://[::1]:8080/v1"));
        assert!(!is_local_url("https://api.openai.com/v1"));
        assert!(!is_local_url(""));
        let p = |id: &str, kind: ProviderKind, base: &str| AiProvider { id: id.into(), kind, base_url: base.into(), ..Default::default() };
        assert!(is_local_provider(&p("ollama", ProviderKind::Openai, "http://192.168.1.5:11434/v1")));
        assert!(is_local_provider(&p("my-vllm", ProviderKind::Openai, "http://localhost:8000/v1")));
        assert!(!is_local_provider(&p("deepseek", ProviderKind::Openai, "https://api.deepseek.com/v1")));
        assert!(!is_local_provider(&p("claude_code", ProviderKind::ClaudeCode, "/usr/local/bin/claude")));
    }

    #[test]
    fn policies() {
        assert_eq!(verdict("on", false), None);
        assert_eq!(verdict("off", true), Some(OFF));
        assert_eq!(verdict("local_only", false), Some(LOCAL_ONLY));
        assert_eq!(verdict("local_only", true), None);
        let mut s = AiSettings::default();
        s.account_policies.insert("a".into(), "off".into());
        s.account_policies.insert("b".into(), "local_only".into());
        assert!(!allows(&s, "a", true));
        assert!(allows(&s, "b", true) && !allows(&s, "b", false));
        assert!(allows(&s, "c", false));
        assert_eq!(policy_of(&s, "c"), "on");
    }
}

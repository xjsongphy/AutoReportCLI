//! Build a concrete provider from a config entry.

use crate::config::schema::ProviderConfig;
use crate::provider::LLMProvider;
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::openai::OpenAICompatProvider;
use crate::provider::openai_responses::OpenAIResponsesProvider;
use anyhow::Result;
use std::sync::Arc;

pub fn build_provider(cfg: &ProviderConfig, model: &str) -> Result<Arc<dyn LLMProvider>> {
    let api_key = crate::config::resolve_api_key(cfg)?;
    let base = cfg.api_base.clone();
    let model = model.to_string();
    let use_responses = cfg.kind == "openai-responses"
        || (cfg.kind == "openai" && is_first_party_openai_base(cfg.api_base.as_deref()));
    Ok(match cfg.kind.as_str() {
        "anthropic" => {
            Arc::new(AnthropicProvider::new(api_key, base, model)) as Arc<dyn LLMProvider>
        }
        _ if use_responses => {
            Arc::new(OpenAIResponsesProvider::new(api_key, base, model)) as Arc<dyn LLMProvider>
        }
        other => {
            Arc::new(OpenAICompatProvider::new(api_key, base, model, other)) as Arc<dyn LLMProvider>
        }
    })
}

/// Codex's first-party OpenAI transport is the Responses API. Keep
/// `openai` compatible with third-party gateways by selecting Responses only
/// when the endpoint is the official OpenAI API (or when the explicit
/// `openai-responses` kind is used).
fn is_first_party_openai_base(base: Option<&str>) -> bool {
    let Some(base) = base else {
        // OpenAICompatProvider's default for `openai` is the first-party
        // `https://api.openai.com/v1` endpoint.
        return true;
    };
    let normalized = base.trim_end_matches('/').to_ascii_lowercase();
    normalized == "https://api.openai.com" || normalized == "https://api.openai.com/v1"
}

#[cfg(test)]
mod tests {
    use super::{build_provider, is_first_party_openai_base};
    use crate::config::schema::ProviderConfig;

    #[test]
    fn recognizes_default_and_official_openai_endpoints() {
        assert!(is_first_party_openai_base(None));
        assert!(is_first_party_openai_base(Some(
            "https://api.openai.com/v1/"
        )));
        assert!(is_first_party_openai_base(Some("HTTPS://API.OPENAI.COM")));
    }

    #[test]
    fn preserves_custom_openai_compatible_endpoints() {
        assert!(!is_first_party_openai_base(Some(
            "https://openrouter.ai/api/v1"
        )));
        assert!(!is_first_party_openai_base(Some(
            "http://localhost:11434/v1"
        )));
    }

    #[test]
    fn explicit_openai_responses_kind_uses_responses_transport() {
        let cfg = ProviderConfig {
            kind: "openai-responses".into(),
            alias: None,
            api_key: Some("test".into()),
            api_base: Some("http://localhost:1234/v1".into()),
            api_key_env: None,
            temperature: 0.1,
            max_tokens: 128,
        };
        let provider = build_provider(&cfg, "gpt-5").expect("provider");
        assert_eq!(provider.id(), "openai-responses/gpt-5");
    }

    #[test]
    fn official_openai_kind_defaults_to_responses_transport() {
        let cfg = ProviderConfig {
            kind: "openai".into(),
            alias: None,
            api_key: Some("test".into()),
            api_base: None,
            api_key_env: None,
            temperature: 0.1,
            max_tokens: 128,
        };
        let provider = build_provider(&cfg, "gpt-5").expect("provider");
        assert_eq!(provider.id(), "openai-responses/gpt-5");
    }
}

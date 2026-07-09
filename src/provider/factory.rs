//! Build a concrete provider from a config entry.

use crate::config::schema::ProviderConfig;
use crate::provider::LLMProvider;
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::openai::OpenAICompatProvider;
use anyhow::Result;
use std::sync::Arc;

pub fn build_provider(cfg: &ProviderConfig) -> Result<Arc<dyn LLMProvider>> {
    let api_key = crate::config::resolve_api_key(cfg)?;
    let base = cfg.api_base.clone();
    let model = cfg.model.clone();
    Ok(match cfg.kind.as_str() {
        "anthropic" => {
            Arc::new(AnthropicProvider::new(api_key, base, model)) as Arc<dyn LLMProvider>
        }
        other => {
            Arc::new(OpenAICompatProvider::new(api_key, base, model, other)) as Arc<dyn LLMProvider>
        }
    })
}

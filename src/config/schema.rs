//! Configuration schema.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Top-level settings, deserialized from `autoreport.config.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Which provider entry (key into `providers`) is active.
    #[serde(default)]
    pub active_provider: Option<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub agents: AgentDefaults,
    /// Context window of the active model, in tokens.
    #[serde(default = "default_context_window")]
    pub context_window: usize,
}

fn default_context_window() -> usize {
    128_000
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            active_provider: None,
            providers: BTreeMap::new(),
            agents: AgentDefaults::default(),
            context_window: default_context_window(),
        }
    }
}

/// A single LLM provider entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// "anthropic" | "openai" | "deepseek" | "openrouter" | "google" | "custom"
    #[serde(default = "default_kind")]
    pub kind: String,
    pub model: String,
    /// Optional, falls back to env var or provider default.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Optional base URL override.
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_kind() -> String {
    "openai".to_string()
}
fn default_temperature() -> f32 {
    0.1
}
fn default_max_tokens() -> u32 {
    8192
}

impl ProviderConfig {
    pub fn env_key(&self) -> Option<&'static str> {
        match self.kind.as_str() {
            "anthropic" => Some("ANTHROPIC_API_KEY"),
            "openai" => Some("OPENAI_API_KEY"),
            "deepseek" => Some("DEEPSEEK_API_KEY"),
            "openrouter" => Some("OPENROUTER_API_KEY"),
            "google" => Some("GEMINI_API_KEY"),
            _ => None,
        }
    }
}

/// Per-agent runtime defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefaults {
    #[serde(default = "default_max_iterations")]
    pub max_tool_iterations: u32,
    /// Trigger auto-compact when history token estimate exceeds this fraction
    /// of `context_window`.
    #[serde(default = "default_compact_threshold")]
    pub compact_threshold: f32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_exec_timeout_secs")]
    pub exec_timeout_secs: u64,
}

fn default_max_iterations() -> u32 {
    12
}
fn default_compact_threshold() -> f32 {
    0.8
}
fn default_exec_timeout_secs() -> u64 {
    120
}

impl Default for AgentDefaults {
    fn default() -> Self {
        Self {
            max_tool_iterations: default_max_iterations(),
            compact_threshold: default_compact_threshold(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            exec_timeout_secs: default_exec_timeout_secs(),
        }
    }
}

//! Configuration schema.

use crate::policy::AskForApproval;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use autoreport_sandboxing::SandboxMode;

/// Top-level settings, deserialized from `autoreport.config.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Kept only to migrate pre-model-page configuration files. It is never
    /// written back to YAML: model selection now lives in [`ModelAssignments`].
    #[serde(default, rename = "active_provider", skip_serializing)]
    pub legacy_active_provider: Option<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    /// The API/model binding used by Main and by the four sub-agents.
    #[serde(default)]
    pub models: ModelAssignments,
    #[serde(default)]
    pub agents: AgentDefaults,
    /// Context window of the active model, in tokens.
    #[serde(default = "default_context_window")]
    pub context_window: usize,
    /// OS-level sandbox applied to `exec` tool commands. Defaults to AutoReport's
    /// `workspace-write` (read all, write only the current agent's assigned
    /// directory + tmp, protect `.git`/`.agents`/`.autoreport`).
    #[serde(default)]
    pub sandbox_mode: SandboxMode,
    /// Whether to allow outbound network access for sandboxed `exec` commands.
    /// Defaults to `false` (network denied), matching AutoReport's default profile.
    #[serde(default)]
    pub sandbox_network: bool,
}

fn default_context_window() -> usize {
    128_000
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            legacy_active_provider: None,
            providers: BTreeMap::new(),
            models: ModelAssignments::default(),
            agents: AgentDefaults::default(),
            context_window: default_context_window(),
            sandbox_mode: SandboxMode::default(),
            sandbox_network: false,
        }
    }
}

/// A single LLM provider entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// "anthropic" | "openai" | "deepseek" | "openrouter" | "google" | "custom"
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Read from legacy `providers.<name>.model` entries so those configs can
    /// migrate cleanly. Models are now selected per agent, never per API.
    #[serde(default, rename = "model", skip_serializing)]
    pub legacy_model: Option<String>,
    /// Optional, falls back to env var or provider default.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Optional base URL override.
    #[serde(default)]
    pub api_base: Option<String>,
    /// Optional explicit env-var name holding the API key (from a synced
    /// cc-switch preset). Falls back to the kind's default when unset.
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

/// A concrete model choice: select an API entry first, then provide its model
/// identifier. The API configuration itself intentionally contains no model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelConfig {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
}

/// Model assignments currently have two choices: one for Main and one shared
/// by all four sub-agents. This keeps the config forward-compatible with
/// future per-sub-agent overrides without exposing five controls today.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelAssignments {
    #[serde(default)]
    pub main: ModelConfig,
    #[serde(default)]
    pub sub: ModelConfig,
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
    /// Context window of the active model, in tokens. Drives auto-compaction
    /// together with `compact_threshold`. Mirrored from the top-level
    /// [`Settings::context_window`] by the loader so runtime code reaches it
    /// via the per-agent defaults it already holds.
    #[serde(default = "default_context_window")]
    pub context_window: usize,
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
    /// When execpolicy and sandbox escalation may consult the user. The product
    /// default is `never`; `on-request`, `untrusted`, and `granular` are
    /// handled by the shared TUI approval queue. AutoReport's enum default is
    /// `OnRequest`, so the config default is pinned via a serde default fn.
    #[serde(default = "default_approval_policy")]
    pub approval_policy: AskForApproval,
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
/// Product default: least human intervention. (Kept as a fn rather than
/// relying on `AskForApproval::default()`, which AutoReport pins to `OnRequest`.)
fn default_approval_policy() -> AskForApproval {
    AskForApproval::Never
}

impl Default for AgentDefaults {
    fn default() -> Self {
        Self {
            max_tool_iterations: default_max_iterations(),
            context_window: default_context_window(),
            compact_threshold: default_compact_threshold(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            exec_timeout_secs: default_exec_timeout_secs(),
            approval_policy: default_approval_policy(),
        }
    }
}

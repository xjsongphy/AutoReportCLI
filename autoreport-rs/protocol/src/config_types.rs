//! `WindowsSandboxLevel`, vendored verbatim from
//! `codex-rs/protocol/src/config_types.rs`. Only this enum from the config
//! crate is needed by the sandbox backends (`manager.rs`, `windows.rs`).

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Controls whether a Windows sandbox launch reconciles persistent proxy
/// firewall settings or preserves settings established by another launch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowsSandboxProxySettingsMode {
    #[default]
    Reconcile,
    Preserve,
}
use strum_macros::Display;
use ts_rs::TS;

#[derive(
    Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Display, JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum WindowsSandboxLevel {
    #[default]
    Disabled,
    RestrictedToken,
    Elevated,
}

//! Approval policy types — verbatim from `codex-rs/protocol/src/protocol.rs`
//! (lines ~895–977). These sit just above the vendored `SandboxPolicy` in the
//! same codex source file; they live here rather than in `sandbox/` to keep
//! ownership clear (the sandbox backend is maintained separately).
//!
//! Only [`AskForApproval::Never`] is wired into the agent loop today — it is
//! also the product default. The other variants parse and serialize for codex
//! fidelity (and so the type matches codex byte-for-byte), but the loader
//! clamps them to `Never` with a warning until an interactive approval path
//! exists. See `config::loader` and `runtime::agent_loop`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use strum_macros::Display;
use ts_rs::TS;

/// Determines the conditions under which the user is consulted to approve
/// running the command proposed by Codex.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    JsonSchema,
    TS,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum AskForApproval {
    /// Under this policy, only "known safe" commands—as determined by
    /// `is_safe_command()`—that **only read files** are auto‑approved.
    /// Everything else will ask the user to approve.
    #[serde(rename = "untrusted")]
    #[strum(serialize = "untrusted")]
    UnlessTrusted,

    /// The model decides when to ask the user for approval.
    #[serde(alias = "on-failure")]
    #[default]
    OnRequest,

    /// Fine-grained controls for individual approval flows.
    ///
    /// When a field is `true`, commands in that category are allowed. When it
    /// is `false`, those requests are automatically rejected instead of shown
    /// to the user.
    #[strum(serialize = "granular")]
    Granular(GranularApprovalConfig),

    /// Never ask the user to approve commands. Failures are immediately returned
    /// to the model, and never escalated to the user for approval.
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
pub struct GranularApprovalConfig {
    /// Whether to allow shell command approval requests, including inline
    /// `with_additional_permissions` and `require_escalated` requests.
    pub sandbox_approval: bool,
    /// Whether to allow prompts triggered by execpolicy `prompt` rules.
    pub rules: bool,
    /// Whether to allow approval prompts triggered by skill script execution.
    #[serde(default)]
    pub skill_approval: bool,
    /// Whether to allow prompts triggered by the `request_permissions` tool.
    #[serde(default)]
    pub request_permissions: bool,
    /// Whether to allow MCP elicitation prompts.
    pub mcp_elicitations: bool,
}

impl GranularApprovalConfig {
    pub const fn allows_sandbox_approval(self) -> bool {
        self.sandbox_approval
    }

    pub const fn allows_rules_approval(self) -> bool {
        self.rules
    }

    pub const fn allows_skill_approval(self) -> bool {
        self.skill_approval
    }

    pub const fn allows_request_permissions(self) -> bool {
        self.request_permissions
    }

    pub const fn allows_mcp_elicitations(self) -> bool {
        self.mcp_elicitations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrips_never() {
        let v = AskForApproval::Never;
        let s = serde_yaml::to_string(&v).unwrap();
        assert_eq!(s.trim(), "never");
        let back: AskForApproval = serde_yaml::from_str("never").unwrap();
        assert_eq!(back, AskForApproval::Never);
    }

    #[test]
    fn on_failure_alias_deserializes_to_onrequest() {
        // codex aliases "on-failure" to OnRequest.
        let back: AskForApproval = serde_yaml::from_str("on-failure").unwrap();
        assert_eq!(back, AskForApproval::OnRequest);
        assert_eq!(
            AskForApproval::OnRequest.to_string(),
            "on-request".to_string()
        );
    }

    #[test]
    fn untrusted_roundtrips() {
        let back: AskForApproval = serde_yaml::from_str("untrusted").unwrap();
        assert_eq!(back, AskForApproval::UnlessTrusted);
        assert_eq!(AskForApproval::UnlessTrusted.to_string(), "untrusted");
    }

    #[test]
    fn granular_roundtrips() {
        // Build the value and let serde produce its canonical YAML, then
        // round-trip — avoids hand-guessing the externally-tagged shape.
        let g = GranularApprovalConfig {
            sandbox_approval: true,
            rules: false,
            skill_approval: true,
            request_permissions: false,
            mcp_elicitations: true,
        };
        let v = AskForApproval::Granular(g);
        let yaml = serde_yaml::to_string(&v).unwrap();
        let back: AskForApproval = serde_yaml::from_str(&yaml).unwrap();
        match back {
            AskForApproval::Granular(g) => {
                assert!(g.allows_sandbox_approval());
                assert!(!g.allows_rules_approval());
                assert!(g.allows_skill_approval());
                assert!(!g.allows_request_permissions());
                assert!(g.allows_mcp_elicitations());
            }
            other => panic!("expected Granular, got {other:?}"),
        }
    }

    #[test]
    fn codex_default_is_onrequest_product_default_is_never() {
        // Codex's #[derive(Default)] keeps OnRequest (verbatim fidelity).
        assert_eq!(AskForApproval::default(), AskForApproval::OnRequest);
        // Our product default is wired separately via a serde default fn in
        // AgentDefaults, so it stays Never regardless of the enum's Default.
    }

    #[test]
    fn review_decision_default_is_denied() {
        assert_eq!(ReviewDecision::default(), ReviewDecision::Denied);
    }

    #[test]
    fn summarize_classifies_common_commands() {
        let read = summarize_command(&["cat".into(), "Data/x.csv".into()]);
        assert!(matches!(read[0], ParsedCommand::Read { .. }));

        let list = summarize_command(&["ls".into(), "Plots".into()]);
        assert!(matches!(list[0], ParsedCommand::ListFiles { .. }));

        let search = summarize_command(&["grep".into(), "foo".into(), "Tex".into()]);
        assert!(matches!(search[0], ParsedCommand::Search { .. }));

        let unknown = summarize_command(&["python".into(), "fit.py".into()]);
        assert!(matches!(unknown[0], ParsedCommand::Unknown { .. }));
    }
}

/// User's decision in response to an approval request. Verbatim subset of
/// codex's `ReviewDecision` (`codex-rs/protocol/src/protocol.rs:4025`): we keep
/// the three decisions the TUI popup offers. The amendment-bearing variants
/// (`ApprovedExecpolicyAmendment`, `NetworkPolicyAmendment`) are deferred —
/// they carry execpolicy/network payloads owned by the sandbox backend and
/// aren't needed until that layer is wired.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ReviewDecision {
    /// User approved this command; the agent should execute it.
    Approved,
    /// User approved and wants matching prompts auto-approved for the session.
    ApprovedForSession,
    /// User denied; the agent should not execute and should try something else.
    #[default]
    Denied,
}

/// Semantic classification of a shell command, verbatim from codex's
/// `ParsedCommand` (`codex-rs/protocol/src/parse_command.rs`). Drives the
/// one-line summary the approval popup shows (e.g. "Read Data/x.csv").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParsedCommand {
    Read {
        cmd: String,
        name: String,
        path: PathBuf,
    },
    ListFiles {
        cmd: String,
        path: Option<String>,
    },
    Search {
        cmd: String,
        query: Option<String>,
        path: Option<String>,
    },
    Unknown {
        cmd: String,
    },
}

impl ParsedCommand {
    /// One-line human summary, mirroring codex's `exec_cell/render.rs`
    /// ("Read <name>", "List <path>", "Search <q> in <p>", "Run <cmd>").
    pub fn summary(&self) -> String {
        match self {
            ParsedCommand::Read { name, .. } => format!("Read {name}"),
            ParsedCommand::ListFiles { path, .. } => {
                format!("List {}", path.clone().unwrap_or_default())
            }
            ParsedCommand::Search { query, path, .. } => match (query, path) {
                (Some(q), Some(p)) => format!("Search {q} in {p}"),
                (Some(q), None) => format!("Search {q}"),
                _ => "Search".into(),
            },
            ParsedCommand::Unknown { cmd } => format!("Run {cmd}"),
        }
    }
}

/// Best-effort classification of a command argv into [`ParsedCommand`]
/// variants. A lightweight stand-in for codex's full shell-classification
/// pipeline (which lives in its `shell-command` crate); same four categories,
/// enough to label the approval popup. Returns one entry per command.
pub fn summarize_command(argv: &[String]) -> Vec<ParsedCommand> {
    let Some(bin) = argv.first().map(|s| s.as_str()).map(str::to_lowercase) else {
        return vec![];
    };
    let stem = bin.rsplit('/').next().unwrap_or(&bin).trim_start_matches(".\\");
    let joined = argv.join(" ");
    vec![match stem {
        "cat" | "head" | "tail" | "less" | "more" | "nl" => {
            let name = argv.iter().nth(1).cloned().unwrap_or_default();
            ParsedCommand::Read {
                cmd: joined.clone(),
                name: name.clone(),
                path: PathBuf::from(name),
            }
        }
        "ls" | "tree" | "du" | "find" | "dir" => ParsedCommand::ListFiles {
            cmd: joined.clone(),
            path: argv.iter().nth(1).cloned(),
        },
        "grep" | "egrep" | "fgrep" | "rg" | "ack" | "ag" => ParsedCommand::Search {
            cmd: joined.clone(),
            query: argv.iter().nth(1).cloned(),
            path: argv.iter().nth(2).cloned(),
        },
        _ => ParsedCommand::Unknown {
            cmd: argv.first().cloned().unwrap_or(joined),
        },
    }]
}

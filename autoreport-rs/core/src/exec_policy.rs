//! Codex-compatible command-prefix policy for `exec`.
//!
//! Rules live in the global workspace state `rules/*.rules` and use Codex's
//! Starlark `prefix_rule(pattern = [...], decision = "...")` shape. Parsing and
//! evaluation delegate to the vendored `autoreport-execpolicy` crate (copied
//! verbatim from `codex-rs/execpolicy`); this module is the thin adapter that
//! keeps the project's `ExecApprovalRequirement` / `ExecPolicyManager` surface,
//! the session-approval cache, the sandbox-escalation guards, and the
//! dangerous/safe-command heuristics layer (`autoreport-shell-command`) that
//! codex's core wrapper adds on top of the starlark policy.

use crate::policy::{AskForApproval, GranularApprovalConfig};
use autoreport_execpolicy::Decision;
use autoreport_execpolicy::MatchOptions;
use autoreport_execpolicy::Policy;
use autoreport_execpolicy::PolicyParser;
use autoreport_execpolicy::RuleMatch;
use autoreport_execpolicy::blocking_append_allow_prefix_rule;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const RULES_DIR: &str = "rules";
const DEFAULT_RULES_FILE: &str = "default.rules";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecApprovalRequirement {
    Skip {
        /// An explicit allow rule or cached approval authorizes the requested
        /// sandbox override for this command.
        allow_escalated_exec: bool,
    },
    NeedsApproval {
        reason: Option<String>,
    },
    Forbidden {
        reason: String,
    },
}

struct PolicyState {
    /// Parsed Starlark policy (codex `execpolicy::Policy`). `Policy::empty()`
    /// when no rules are loaded or a rule file failed to parse.
    policy: Policy,
    /// In-process allow prefixes added by “approve for session”.
    session_allow_prefixes: HashSet<Vec<String>>,
}

impl Default for PolicyState {
    fn default() -> Self {
        Self {
            policy: Policy::empty(),
            session_allow_prefixes: HashSet::new(),
        }
    }
}

impl std::fmt::Debug for PolicyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyState")
            .field("policy", &self.policy)
            .field("session_allow_prefixes", &self.session_allow_prefixes)
            .finish()
    }
}

/// Shared policy manager, inherited by every agent loop in a workspace.
#[derive(Debug, Clone)]
pub struct ExecPolicyManager {
    state_dir: PathBuf,
    state: Arc<Mutex<PolicyState>>,
}

impl ExecPolicyManager {
    pub fn empty(workspace: &Path) -> Self {
        Self {
            state_dir: workspace.to_path_buf(),
            state: Arc::new(Mutex::new(PolicyState::default())),
        }
    }

    pub fn load(workspace: &Path) -> Result<Self, String> {
        let policy = load_policy(workspace)?;
        Ok(Self {
            state_dir: workspace.to_path_buf(),
            state: Arc::new(Mutex::new(PolicyState {
                policy,
                session_allow_prefixes: HashSet::new(),
            })),
        })
    }

    pub fn evaluate(
        &self,
        command: &str,
        approval_policy: AskForApproval,
        requests_escalation: bool,
    ) -> ExecApprovalRequirement {
        let commands = split_commands(command);
        if commands.is_empty() {
            return ExecApprovalRequirement::Forbidden {
                reason: "empty command".to_string(),
            };
        }
        let state = self.state.lock().expect("execpolicy lock poisoned");
        let session_allows = commands.iter().all(|command| {
            state
                .session_allow_prefixes
                .iter()
                .any(|prefix| command.starts_with(prefix))
        });
        if session_allows && !(requests_escalation && contains_untrusted_shell_syntax(command)) {
            return ExecApprovalRequirement::Skip {
                allow_escalated_exec: requests_escalation,
            };
        }

        // Heuristics for commands that match no starlark rule: the project's
        // dangerous/safe-command classification layered with the approval
        // policy + escalation flag (codex core adds the same fallback in
        // `create_exec_approval_requirement_for_command`).
        let fallback =
            |command: &[String]| unmatched_decision(command, approval_policy, requests_escalation);
        let evaluation = state.policy.check_multiple_with_options(
            commands.iter(),
            &fallback,
            &MatchOptions {
                resolve_host_executables: true,
            },
        );
        drop(state);

        let mut decision = evaluation.decision;
        let mut reason = first_justification(&evaluation.matched_rules);

        // Prefix rules intentionally allow additional ordinary arguments, but
        // shell syntax can execute a second command before the approved program
        // runs. Never let a rule or session approval turn such a command into an
        // unrestricted escalation.
        if requests_escalation
            && contains_untrusted_shell_syntax(command)
            && decision == Decision::Allow
        {
            decision = Decision::Prompt;
        }

        match decision {
            Decision::Allow => ExecApprovalRequirement::Skip {
                allow_escalated_exec: requests_escalation,
            },
            Decision::Prompt => match approval_rejects_prompt(approval_policy, reason.is_some()) {
                Some(reason) => ExecApprovalRequirement::Forbidden { reason },
                None => ExecApprovalRequirement::NeedsApproval { reason },
            },
            Decision::Forbidden => ExecApprovalRequirement::Forbidden {
                reason: reason
                    .take()
                    .unwrap_or_else(|| "command forbidden by execpolicy".to_string()),
            },
        }
    }

    /// Cache this prefix for the process lifetime after “approve for session”.
    pub fn approve_for_session(&self, command: &str) {
        for command in split_commands(command) {
            self.state
                .lock()
                .expect("execpolicy lock poisoned")
                .session_allow_prefixes
                .insert(command);
        }
    }

    /// Append a narrow persistent allow rule. Only the TUI approval flow calls
    /// this method; model-generated arguments never reach it directly.
    pub fn persist_allow_prefix(&self, command: &str) -> Result<(), String> {
        let commands = split_commands(command);
        if commands.len() != 1 || commands[0].is_empty() {
            return Err("only one non-empty command can be persisted as an execpolicy rule".into());
        }
        let command = &commands[0];
        if is_interpreter_prefix(command) {
            return Err("refusing to persist an interpreter or shell prefix rule".into());
        }
        let rules_dir = self.state_dir.join(RULES_DIR);
        fs::create_dir_all(&rules_dir)
            .map_err(|err| format!("failed to create execpolicy rules directory: {err}"))?;
        let path = rules_dir.join(DEFAULT_RULES_FILE);
        // Delegate the on-disk append to codex's `blocking_append_allow_prefix_rule`
        // (advisory-locked, serializes the starlark `prefix_rule(...)` call).
        blocking_append_allow_prefix_rule(&path, command)
            .map_err(|err| format!("failed to write {}: {err}", path.display()))?;
        // Mirror the rule into the in-memory policy so the next `evaluate`
        // sees it without a reload.
        let mut state = self.state.lock().expect("execpolicy lock poisoned");
        if let Err(err) = state.policy.add_prefix_rule(command, Decision::Allow) {
            log::warn!("execpolicy in-memory rule add failed: {err}");
        }
        Ok(())
    }
}

/// Extract the first explicit justification from matched rules for surfacing
/// in the approval/forbidden reason.
fn first_justification(matched_rules: &[RuleMatch]) -> Option<String> {
    for rule in matched_rules {
        if let RuleMatch::PrefixRuleMatch {
            justification: Some(j),
            ..
        } = rule
        {
            return Some(j.clone());
        }
    }
    None
}

fn approval_rejects_prompt(policy: AskForApproval, is_rule_prompt: bool) -> Option<String> {
    match policy {
        AskForApproval::Never => {
            Some("approval required by policy, but approval_policy is set to never".to_string())
        }
        AskForApproval::Granular(GranularApprovalConfig { rules, .. })
            if is_rule_prompt && !rules =>
        {
            Some("approval required by execpolicy rule, but granular.rules is false".to_string())
        }
        AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval, ..
        }) if !is_rule_prompt && !sandbox_approval => Some(
            "approval required for sandbox escalation, but granular.sandbox_approval is false"
                .to_string(),
        ),
        _ => None,
    }
}

fn unmatched_decision(
    command: &[String],
    approval_policy: AskForApproval,
    requests_escalation: bool,
) -> Decision {
    if requests_escalation {
        return Decision::Prompt;
    }
    if command_might_be_dangerous(command) {
        return match approval_policy {
            AskForApproval::Never => Decision::Forbidden,
            _ => Decision::Prompt,
        };
    }
    match approval_policy {
        AskForApproval::UnlessTrusted if !known_safe(command) => Decision::Prompt,
        _ => Decision::Allow,
    }
}

fn known_safe(command: &[String]) -> bool {
    // Delegate to Codex's `is_known_safe_command` (vendored
    // `autoreport-shell-command`) instead of a hand-rolled program list, so
    // classification tracks Codex's curated safe-command set.
    autoreport_shell_command::is_safe_command::is_known_safe_command(command)
}

fn command_might_be_dangerous(command: &[String]) -> bool {
    // Codex's `dangerous_command_match` (vendored `autoreport-shell-command`)
    // is the source of truth for the dangerous-command classification.
    if autoreport_shell_command::is_dangerous_command::dangerous_command_match(command).is_some() {
        return true;
    }
    // Project-specific adaptation: a normal report script remains sandboxed and
    // is safe under the `never` default, but inline/eval interpreter modes can
    // conceal arbitrary filesystem or process operations, so they follow Codex's
    // prompt path. This sits on top of Codex's classification, not in place of
    // it.
    let Some(program) = command.first().map(String::as_str) else {
        return false;
    };
    matches!(
        program,
        "python" | "python3" | "bash" | "sh" | "zsh" | "node" | "perl" | "ruby"
    ) && command
        .iter()
        .skip(1)
        .any(|argument| matches!(argument.as_str(), "-c" | "-e" | "-"))
}

fn is_interpreter_prefix(command: &[String]) -> bool {
    matches!(
        command.first().map(String::as_str),
        Some("python" | "python3" | "bash" | "sh" | "zsh" | "node" | "perl" | "ruby")
    )
}

/// Walk `<workspace>/rules/*.rules`, parse each with the starlark
/// `PolicyParser`, and merge into one `Policy`. A parse failure falls back to
/// `Policy::empty()` with a warning (codex never crashes on a bad rule file).
fn load_policy(workspace: &Path) -> Result<Policy, String> {
    let dir = workspace.join(RULES_DIR);
    if !dir.exists() {
        return Ok(Policy::empty());
    }
    let mut paths = fs::read_dir(&dir)
        .map_err(|err| format!("failed to read execpolicy rules {}: {err}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "rules")
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut combined = Policy::empty();
    for path in paths {
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => {
                log::warn!("failed to read execpolicy rule {}: {err}", path.display());
                continue;
            }
        };
        let mut parser = PolicyParser::new();
        if let Err(err) = parser.parse(&path.display().to_string(), &content) {
            // codex falls back to an empty policy + warning rather than crashing.
            log::warn!("execpolicy parse error in {}: {err}", path.display());
            continue;
        }
        let parsed = parser.build();
        combined = combined.merge_overlay(&parsed);
    }
    Ok(combined)
}

fn split_commands(script: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut current = String::new();
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    for character in script.chars() {
        if in_single {
            // Single quotes are literal: no escape handling, and the only
            // character that closes them is a matching `'`. We must not split
            // on `;|&` while inside.
            current.push(character);
            if character == '\'' {
                in_single = false;
            }
            continue;
        }
        if in_double {
            current.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_double = false;
            }
            continue;
        }
        if character == '"' {
            in_double = true;
            current.push(character);
            continue;
        }
        if character == '\'' {
            in_single = true;
            current.push(character);
            continue;
        }
        if matches!(character, ';' | '\n' | '|' | '&') {
            push_command(&mut commands, &mut current);
        } else {
            current.push(character);
        }
    }
    push_command(&mut commands, &mut current);
    commands
}

fn push_command(commands: &mut Vec<Vec<String>>, current: &mut String) {
    let words = current
        .split_whitespace()
        .map(|word| word.trim_matches('"').to_string())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if !words.is_empty() {
        commands.push(words);
    }
    current.clear();
}

/// Return true for shell constructs that are not represented by the simple
/// command-prefix tokenizer. These constructs are safe to run under the
/// normal OS sandbox, but must require a fresh approval before escalation.
fn contains_untrusted_shell_syntax(script: &str) -> bool {
    let chars: Vec<char> = script.chars().collect();
    let mut quote = None;
    let mut escaped = false;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        match quote {
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    quote = None;
                } else if character == '`'
                    || (character == '$' && chars.get(index + 1) == Some(&'('))
                {
                    return true;
                }
            }
            None => match character {
                '\'' | '"' => quote = Some(character),
                '`' | '<' | '>' => return true,
                '$' if chars.get(index + 1) == Some(&'(') => return true,
                _ => {}
            },
            _ => unreachable!(),
        }
        index += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager_with_policy(source: &str) -> ExecPolicyManager {
        let mut parser = PolicyParser::new();
        parser
            .parse("test.rules", source)
            .expect("test policy parses");
        let policy = parser.build();
        ExecPolicyManager {
            state_dir: PathBuf::from("/tmp"),
            state: Arc::new(Mutex::new(PolicyState {
                policy,
                session_allow_prefixes: HashSet::new(),
            })),
        }
    }

    #[test]
    fn parses_prefix_alternatives_and_uses_strictest_match() {
        let manager = manager_with_policy(
            r#"prefix_rule(pattern = ["git", ["status", "log"]], decision = "allow")
prefix_rule(pattern = ["git", "status", "--porcelain"], decision = "prompt", justification = "review status")"#,
        );
        assert!(matches!(
            manager.evaluate("git log", AskForApproval::Never, false),
            ExecApprovalRequirement::Skip { .. }
        ));
        assert!(matches!(
            manager.evaluate("git status --porcelain", AskForApproval::OnRequest, false),
            ExecApprovalRequirement::NeedsApproval { .. }
        ));
    }

    #[test]
    fn never_rejects_rule_prompts_and_escalation() {
        let manager = ExecPolicyManager {
            state_dir: PathBuf::from("/tmp"),
            state: Arc::new(Mutex::new(PolicyState::default())),
        };
        assert!(matches!(
            manager.evaluate("python3 task.py", AskForApproval::Never, true),
            ExecApprovalRequirement::Forbidden { .. }
        ));
    }

    #[test]
    fn never_allows_sandboxed_report_scripts_but_rejects_inline_interpreters() {
        let manager = ExecPolicyManager {
            state_dir: PathBuf::from("/tmp"),
            state: Arc::new(Mutex::new(PolicyState::default())),
        };
        assert!(matches!(
            manager.evaluate("python3 fit.py", AskForApproval::Never, false),
            ExecApprovalRequirement::Skip { .. }
        ));
        assert!(matches!(
            manager.evaluate("python3 -c 'print(1)'", AskForApproval::Never, false),
            ExecApprovalRequirement::Forbidden { .. }
        ));
    }

    #[test]
    fn session_approval_skips_subsequent_prompt() {
        let manager = ExecPolicyManager {
            state_dir: PathBuf::from("/tmp"),
            state: Arc::new(Mutex::new(PolicyState::default())),
        };
        assert!(matches!(
            manager.evaluate("python3 task.py", AskForApproval::OnRequest, true),
            ExecApprovalRequirement::NeedsApproval { .. }
        ));
        manager.approve_for_session("python3 task.py");
        assert!(matches!(
            manager.evaluate("python3 task.py", AskForApproval::OnRequest, true),
            ExecApprovalRequirement::Skip { .. }
        ));
    }

    #[test]
    fn persisted_approval_reloads_from_global_rules() {
        let workspace = tempfile::tempdir().expect("workspace");
        let manager = ExecPolicyManager::empty(workspace.path());
        manager
            .persist_allow_prefix("git status")
            .expect("persist rule");
        let reloaded = ExecPolicyManager::load(workspace.path()).expect("reload rule");
        assert!(matches!(
            reloaded.evaluate("git status --short", AskForApproval::Never, false),
            ExecApprovalRequirement::Skip { .. }
        ));
        assert!(
            workspace
                .path()
                .join(RULES_DIR)
                .join(DEFAULT_RULES_FILE)
                .is_file()
        );
    }

    #[test]
    fn interpreter_prefix_cannot_be_persisted() {
        let workspace = tempfile::tempdir().expect("workspace");
        let manager = ExecPolicyManager::empty(workspace.path());
        assert!(manager.persist_allow_prefix("python3 -c x").is_err());
    }

    #[test]
    fn approved_prefix_cannot_escalate_shell_syntax() {
        let workspace = tempfile::tempdir().expect("workspace");
        let manager = ExecPolicyManager::empty(workspace.path());
        manager
            .persist_allow_prefix("git status")
            .expect("persist rule");

        assert!(matches!(
            manager.evaluate(
                "git status $(touch outside.txt)",
                AskForApproval::OnRequest,
                true
            ),
            ExecApprovalRequirement::NeedsApproval { .. }
        ));
        assert!(matches!(
            manager.evaluate("git status > outside.txt", AskForApproval::Never, true),
            ExecApprovalRequirement::Forbidden { .. }
        ));
    }

    #[test]
    fn split_commands_respects_single_quotes() {
        // Single-quoted pipe must not be treated as a command separator.
        let single = split_commands("echo 'a | b'");
        assert_eq!(
            single.len(),
            1,
            "single-quoted pipe should not split into multiple commands"
        );
        // Double-quoted pipe still works (regression guard).
        let double = split_commands("echo \"a | b\"");
        assert_eq!(
            double.len(),
            1,
            "double-quoted pipe should not split into multiple commands"
        );
        // Unquoted pipe still splits into two commands.
        let unquoted = split_commands("a | b");
        assert_eq!(
            unquoted.len(),
            2,
            "unquoted pipe should split into two commands"
        );
        // Single-quoted semicolons and ampersands also do not split.
        assert_eq!(split_commands("echo 'a ; b'").len(), 1);
        assert_eq!(split_commands("echo 'a & b'").len(), 1);
    }

    #[test]
    fn malformed_rule_file_falls_back_to_empty_policy() {
        let workspace = tempfile::tempdir().expect("workspace");
        let rules_dir = workspace.path().join(RULES_DIR);
        fs::create_dir_all(&rules_dir).expect("rules dir");
        fs::write(
            rules_dir.join(DEFAULT_RULES_FILE),
            "this is not starlark ((((",
        )
        .expect("write rule");
        // A parse failure must not panic; load returns an empty policy.
        let manager = ExecPolicyManager::load(workspace.path()).expect("load fallback");
        assert!(matches!(
            manager.evaluate("ls", AskForApproval::Never, false),
            ExecApprovalRequirement::Skip { .. }
        ));
    }
}

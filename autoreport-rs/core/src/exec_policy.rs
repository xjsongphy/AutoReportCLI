//! Codex-compatible command-prefix policy for `exec`.
//!
//! Rules live in the global workspace state `rules/*.rules` and use Codex's
//! `prefix_rule(pattern = [...], decision = "...")` shape. This module owns
//! loading, evaluation, session approval caching, and safe persistence of the
//! narrow allow-prefix amendments created by the approval UI.

use crate::policy::{AskForApproval, GranularApprovalConfig};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const RULES_DIR: &str = "rules";
const DEFAULT_RULES_FILE: &str = "default.rules";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Decision {
    Allow,
    Prompt,
    Forbidden,
}

impl Decision {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "allow" => Ok(Self::Allow),
            "prompt" => Ok(Self::Prompt),
            "forbidden" => Ok(Self::Forbidden),
            _ => Err(format!("invalid execpolicy decision '{value}'")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PatternToken {
    Single(String),
    Alternatives(Vec<String>),
}

impl PatternToken {
    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Single(expected) => expected == value,
            Self::Alternatives(values) => values.iter().any(|expected| expected == value),
        }
    }
}

#[derive(Debug, Clone)]
struct PrefixRule {
    pattern: Vec<PatternToken>,
    decision: Decision,
    justification: Option<String>,
}

impl PrefixRule {
    fn matches(&self, command: &[String]) -> bool {
        command.len() >= self.pattern.len()
            && self
                .pattern
                .iter()
                .zip(command)
                .all(|(token, command_token)| token.matches(command_token))
    }
}

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

#[derive(Debug, Default)]
struct PolicyState {
    rules: Vec<PrefixRule>,
    session_allow_prefixes: HashSet<Vec<String>>,
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
        let rules = load_rules(workspace)?;
        Ok(Self {
            state_dir: workspace.to_path_buf(),
            state: Arc::new(Mutex::new(PolicyState {
                rules,
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

        let mut decision = Decision::Allow;
        let mut reason = None;
        for command in &commands {
            for rule in state.rules.iter().filter(|rule| rule.matches(command)) {
                if rule.decision > decision {
                    decision = rule.decision;
                    reason = rule.justification.clone();
                }
            }
            if !state.rules.iter().any(|rule| rule.matches(command)) {
                decision = decision.max(unmatched_decision(
                    command,
                    approval_policy,
                    requests_escalation,
                ));
            }
        }
        drop(state);

        // Prefix rules intentionally allow additional ordinary arguments, but
        // shell syntax can execute a second command before the approved
        // program runs. Never let a rule or session approval turn such a
        // command into an unrestricted escalation.
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
                reason: reason.unwrap_or_else(|| "command forbidden by execpolicy".to_string()),
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
        let pattern = command
            .iter()
            .map(|token| serde_json::to_string(token).expect("string serialization"))
            .collect::<Vec<_>>()
            .join(", ");
        let rule = format!("\nprefix_rule(pattern = [{pattern}], decision = \"allow\")\n");
        use std::io::Write;
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(rule.as_bytes()))
            .map_err(|err| format!("failed to write {}: {err}", path.display()))?;
        self.state
            .lock()
            .expect("execpolicy lock poisoned")
            .rules
            .push(PrefixRule {
                pattern: command.iter().cloned().map(PatternToken::Single).collect(),
                decision: Decision::Allow,
                justification: None,
            });
        Ok(())
    }
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
    matches!(
        command.first().map(String::as_str),
        Some(
            "cat"
                | "head"
                | "tail"
                | "ls"
                | "find"
                | "rg"
                | "grep"
                | "sed"
                | "awk"
                | "wc"
                | "pwd"
                | "git"
        )
    ) && !command
        .iter()
        .any(|token| token.starts_with('-') && token.contains('w'))
}

fn command_might_be_dangerous(command: &[String]) -> bool {
    let Some(program) = command.first().map(String::as_str) else {
        return false;
    };
    if matches!(
        program,
        "rm" | "mv" | "cp" | "chmod" | "chown" | "sudo" | "dd" | "mkfs"
    ) {
        return true;
    }
    // A normal report script remains sandboxed and is safe to run under the
    // `never` default. Inline/eval interpreter modes can conceal arbitrary
    // filesystem or process operations, so they follow Codex's prompt path.
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

fn load_rules(workspace: &Path) -> Result<Vec<PrefixRule>, String> {
    let dir = workspace.join(RULES_DIR);
    if !dir.exists() {
        return Ok(Vec::new());
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
    let mut rules = Vec::new();
    for path in paths {
        let content = fs::read_to_string(&path)
            .map_err(|err| format!("failed to read execpolicy rule {}: {err}", path.display()))?;
        rules.extend(parse_rules(&content).map_err(|err| format!("{}: {err}", path.display()))?);
    }
    Ok(rules)
}

fn parse_rules(source: &str) -> Result<Vec<PrefixRule>, String> {
    let mut rules = Vec::new();
    let mut remaining = source;
    while let Some(index) = remaining.find("prefix_rule(") {
        remaining = &remaining[index + "prefix_rule".len()..];
        let end = matching_paren(remaining).ok_or("unterminated prefix_rule")?;
        let body = &remaining[1..end];
        let pattern = field_value(body, "pattern").ok_or("prefix_rule requires pattern")?;
        let pattern = parse_pattern(pattern)?;
        if pattern.is_empty() {
            return Err("prefix_rule pattern cannot be empty".into());
        }
        let decision = field_value(body, "decision")
            .map(parse_string)
            .transpose()?
            .map(|value| Decision::parse(&value))
            .transpose()?
            .unwrap_or(Decision::Allow);
        let justification = field_value(body, "justification")
            .map(parse_string)
            .transpose()?;
        rules.push(PrefixRule {
            pattern,
            decision,
            justification,
        });
        remaining = &remaining[end + 1..];
    }
    Ok(rules)
}

fn matching_paren(value: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        match character {
            '"' => quoted = true,
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn field_value<'a>(body: &'a str, field: &str) -> Option<&'a str> {
    let start = body.find(&format!("{field} ="))? + field.len() + 2;
    let value = body[start..].trim_start();
    let end = matching_value_end(value);
    Some(value[..end].trim())
}

fn matching_value_end(value: &str) -> usize {
    let mut square = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        match character {
            '"' => quoted = true,
            '[' => square += 1,
            ']' => square = square.saturating_sub(1),
            ',' if square == 0 => return index,
            _ => {}
        }
    }
    value.len()
}

fn parse_pattern(value: &str) -> Result<Vec<PatternToken>, String> {
    let value = value.trim();
    if !value.starts_with('[') || !value.ends_with(']') {
        return Err("pattern must be an array".into());
    }
    split_array(&value[1..value.len() - 1])
        .into_iter()
        .map(|entry| {
            let entry = entry.trim();
            if entry.starts_with('[') {
                let alternatives = split_array(&entry[1..entry.len() - 1])
                    .into_iter()
                    .map(parse_string)
                    .collect::<Result<Vec<_>, _>>()?;
                if alternatives.is_empty() {
                    return Err("pattern alternatives cannot be empty".into());
                }
                Ok(PatternToken::Alternatives(alternatives))
            } else {
                Ok(PatternToken::Single(parse_string(entry)?))
            }
        })
        .collect()
}

fn split_array(value: &str) -> Vec<&str> {
    let mut values = Vec::new();
    let mut start = 0;
    let mut square = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        match character {
            '"' => quoted = true,
            '[' => square += 1,
            ']' => square = square.saturating_sub(1),
            ',' if square == 0 => {
                values.push(value[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    if !value[start..].trim().is_empty() {
        values.push(value[start..].trim());
    }
    values
}

fn parse_string(value: &str) -> Result<String, String> {
    serde_json::from_str(value.trim()).map_err(|_| format!("expected a quoted string, got {value}"))
}

fn split_commands(script: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in script.chars() {
        if quoted {
            current.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        if character == '"' {
            quoted = true;
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

    #[test]
    fn parses_prefix_alternatives_and_uses_strictest_match() {
        let rules = parse_rules(r#"prefix_rule(pattern = ["git", ["status", "log"]], decision = "allow")
prefix_rule(pattern = ["git", "status", "--porcelain"], decision = "prompt", justification = "review status")"#).unwrap();
        let manager = ExecPolicyManager {
            state_dir: PathBuf::from("/tmp"),
            state: Arc::new(Mutex::new(PolicyState {
                rules,
                session_allow_prefixes: HashSet::new(),
            })),
        };
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
}

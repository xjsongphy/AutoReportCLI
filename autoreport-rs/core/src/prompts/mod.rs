//! Prompt loading. Each agent's identity + full instructions live in
//! `templates/agents/*.md` (compiled into the binary). Users may override any
//! of them globally under `$AUTOREPORT_HOME/agents/`; explicit project
//! overrides in `References/agents/` are also read, matching Codex's user and
//! project instruction layers.

use crate::skills::SkillLoader;
use crate::types::AgentType;
use std::path::{Path, PathBuf};

// Built-in templates (compile-time embedded).
const COMMON: &str = include_str!("../../../../templates/agents/Common.md");
const MAIN: &str = include_str!("../../../../templates/agents/main_agent.md");
const DATA_ANALYSIS: &str = include_str!("../../../../templates/agents/data_analysis_agent.md");
const PLOTTING: &str = include_str!("../../../../templates/agents/plotting_agent.md");
const THEORY: &str = include_str!("../../../../templates/agents/theory_agent.md");
const REPORT: &str = include_str!("../../../../templates/agents/report_agent.md");

#[derive(Clone)]
pub struct PromptLoader {
    home: PathBuf,
    workspace: PathBuf,
}

impl PromptLoader {
    pub fn new(home: &Path, workspace: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            workspace: workspace.to_path_buf(),
        }
    }

    /// Project override takes precedence over the global user override.
    fn override_path(&self, file: &str) -> Option<PathBuf> {
        let p = self.workspace.join("References").join("agents").join(file);
        if p.exists() {
            return Some(p);
        }
        let p = self.home.join("agents").join(file);
        p.exists().then_some(p)
    }

    fn read(&self, file: &str, default: &str) -> String {
        match self.override_path(file) {
            Some(p) => std::fs::read_to_string(&p).unwrap_or_else(|_| default.to_string()),
            None => default.to_string(),
        }
    }

    pub fn common(&self) -> String {
        self.read("Common.md", COMMON)
    }

    pub fn agent_prompt(&self, agent: AgentType) -> String {
        match agent {
            AgentType::Main => self.read("main_agent.md", MAIN),
            AgentType::DataAnalysis => self.read("data_analysis_agent.md", DATA_ANALYSIS),
            AgentType::Plotting => self.read("plotting_agent.md", PLOTTING),
            AgentType::Theory => self.read("theory_agent.md", THEORY),
            AgentType::Report => self.read("report_agent.md", REPORT),
        }
    }

    /// Assemble the full system prompt: shared context + agent instructions +
    /// Codex-style skills context + workspace layout.
    pub fn build_system_prompt(
        &self,
        agent: AgentType,
        skills: &SkillLoader,
        workspace: &Path,
    ) -> String {
        let mut parts = Vec::new();
        parts.push(self.common());
        parts.push(self.agent_prompt(agent));
        let write_scope = match agent.write_dir() {
            Some(dir) => format!("You may write only under `{dir}/`."),
            None => "You may write only under `Outline/`.".to_string(),
        };
        parts.push(format!(
            "\n\n## Workspace\nYou are operating in: `{}`. The project has fixed directories: \
             `Data/` (raw + `Data/Processed/` analysis output), `References/` (reference PDFs, \
             images, custom templates/skills), `Theory/`, `Plots/` (`Plots/Fig/` figures + \
             `Plots/Scripts/` code), `Tex/` (LaTeX sources + compiled PDF), `Outline/` (Main's \
             report outline). {} \
             Read files and directory trees with `exec` (`cat`, `sed -n`, `rg`, `find`). \
             Edit files with `apply_patch`. Use `exec` for running programs, but any writes inside \
             the workspace must stay within your write directory.",
            workspace.display(),
            write_scope
        ));
        let skills_context = skills.render_context();
        if !skills_context.is_empty() {
            parts.push(skills_context);
        }
        parts.join("\n\n")
    }
}

/// codex `current_time_reminder` context fragment (verbatim body format):
/// a per-turn `developer`-role note telling the model the current UTC time,
/// so date-sensitive report writing has an accurate "today". Kept OUT of the
/// (otherwise static) system prompt so the system base stays byte-stable
/// across turns — a prerequisite for prompt-prefix caching. See codex
/// `core/src/context/current_time_reminder.rs`.
pub fn current_time_reminder() -> String {
    format!(
        "It is {}.",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_system_prompt_reads_agent_override_each_time() {
        let workspace = std::env::temp_dir().join(format!("prompts-{}", stamp()));
        let agents = workspace.join("References").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let prompt_path = agents.join("main_agent.md");
        std::fs::write(&prompt_path, "first prompt").unwrap();

        let loader = PromptLoader::new(&workspace, &workspace);
        let skills = SkillLoader::new(&workspace, &workspace);

        let first = loader.build_system_prompt(AgentType::Main, &skills, &workspace);
        std::fs::write(&prompt_path, "second prompt").unwrap();
        let second = loader.build_system_prompt(AgentType::Main, &skills, &workspace);

        assert!(first.contains("first prompt"));
        assert!(second.contains("second prompt"));
        assert!(!second.contains("first prompt"));

        std::fs::remove_dir_all(&workspace).ok();
    }

    fn stamp() -> String {
        use std::time::SystemTime;
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string()
    }
}

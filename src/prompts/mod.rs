//! Prompt loading. Each agent's identity + full instructions live in
//! `templates/agents/*.md` (compiled into the binary). Users may override any
//! of them by placing a file of the same name in `references/agents/`.

use crate::skills::SkillLoader;
use crate::types::AgentType;
use std::path::{Path, PathBuf};

// Built-in templates (compile-time embedded).
const COMMON: &str = include_str!("../../templates/agents/Common.md");
const MAIN: &str = include_str!("../../templates/agents/main_agent.md");
const DATA_ANALYSIS: &str = include_str!("../../templates/agents/data_analysis_agent.md");
const PLOTTING: &str = include_str!("../../templates/agents/plotting_agent.md");
const THEORY: &str = include_str!("../../templates/agents/theory_agent.md");
const REPORT: &str = include_str!("../../templates/agents/report_agent.md");

#[derive(Clone)]
pub struct PromptLoader {
    workspace: PathBuf,
}

impl PromptLoader {
    pub fn new(workspace: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
        }
    }

    /// User override path for an agent file, if present.
    fn override_path(&self, file: &str) -> Option<PathBuf> {
        let p = self.workspace.join("references").join("agents").join(file);
        if p.exists() {
            Some(p)
        } else {
            None
        }
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
    /// skills summary + workspace layout.
    pub fn build_system_prompt(
        &self,
        agent: AgentType,
        skills: &SkillLoader,
        workspace: &Path,
    ) -> String {
        let mut parts = Vec::new();
        parts.push(self.common());
        parts.push(self.agent_prompt(agent));
        parts.push(format!(
            "\n\n## Workspace\nYou are operating in: `{}`. The project has fixed directories: \
             `data/` (raw + `data/processed/` analysis output), `references/` (reference PDFs, \
             images, custom templates/skills), `theory/`, `code/` (plots + scripts), `tex/` \
             (LaTeX sources + compiled PDF), `outline/` (Main's report outline). You may only \
             write to your assigned directory.",
            workspace.display()
        ));
        let summary = skills.summary();
        if !summary.is_empty() {
            parts.push(format!("\n\n## Skills\n{summary}"));
        }
        parts.join("\n\n")
    }
}

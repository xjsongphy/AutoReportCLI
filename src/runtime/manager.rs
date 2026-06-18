//! [`LoopManager`] owns one [`AgentLoop`] per agent type, each with an isolated
//! tool registry matching its write permissions. All loops are persistent for
//! the life of the process.

use crate::bus::Bus;
use crate::config::AgentDefaults;
use crate::prompts::PromptLoader;
use crate::provider::LLMProvider;
use crate::runtime::AgentLoop;
use crate::skills::SkillLoader;
use crate::taskboard::TaskBoard;
use crate::tools::file_tools::{self, FsCtx};
use crate::tools::manifest::{ManifestStore, ManifestTool};
use crate::tools::registry::ToolRegistry;
use crate::tools::skill_tool::{ListSkillsTool, LoadSkillTool};
use crate::tools::task_tools;
use crate::types::{AgentType, MessageSource};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct LoopManager {
    workspace: PathBuf,
    bus: Bus,
    task_board: TaskBoard,
    manifest: ManifestStore,
    skills: SkillLoader,
    prompts: PromptLoader,
    defaults: AgentDefaults,
    loops: HashMap<AgentType, Arc<AgentLoop>>,
    provider_storage: Arc<dyn LLMProvider>,
}

impl LoopManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workspace: &Path,
        provider: Arc<dyn LLMProvider>,
        bus: Bus,
        defaults: AgentDefaults,
    ) -> Self {
        let task_board = TaskBoard::new();
        let manifest = ManifestStore::new(workspace);
        let skills = SkillLoader::new(workspace);
        let prompts = PromptLoader::new(workspace);
        Self {
            workspace: workspace.to_path_buf(),
            bus,
            task_board,
            manifest,
            skills,
            prompts,
            defaults,
            loops: HashMap::new(),
            provider_storage: provider,
        }
    }

    /// Build each agent loop and start it. Agents persist until the process
    /// exits (they are never shut down); only their context can be cleared.
    pub fn start(&mut self) -> Result<()> {
        let provider = self.provider_storage.clone();
        for agent in AgentType::ALL {
            let tools = self.build_tools(agent);
            let loop_ = Arc::new(AgentLoop::new(
                agent,
                self.workspace.clone(),
                tools,
                provider.clone(),
                self.prompts.clone(),
                self.skills.clone(),
                self.manifest.clone(),
                self.bus.clone(),
                self.task_board.clone(),
                self.defaults.clone(),
            ));
            loop_.clone().start();
            self.loops.insert(agent, loop_);
        }
        Ok(())
    }

    pub fn get(&self, agent: AgentType) -> Option<Arc<AgentLoop>> {
        self.loops.get(&agent).cloned()
    }

    /// Snapshot of the produced-file manifests (all agents, or one if given).
    pub fn manifest_snapshot(&self, agent: Option<AgentType>) -> serde_json::Value {
        self.manifest.snapshot(agent)
    }

    /// Address a user message to a specific agent.
    pub fn submit(&self, agent: AgentType, content: String, source: MessageSource) {
        if let Some(l) = self.loops.get(&agent) {
            l.submit(content, source);
        }
    }

    pub fn clear_context(&self, agent: AgentType) {
        if let Some(l) = self.loops.get(&agent) {
            let l = l.clone();
            tokio::spawn(async move {
                l.clear_context().await;
            });
        }
    }

    pub fn compact(&self, agent: AgentType) {
        if let Some(l) = self.loops.get(&agent) {
            let l = l.clone();
            tokio::spawn(async move {
                l.compact().await;
            });
        }
    }

    /// Absolute write directory for an agent.
    fn write_dir(&self, agent: AgentType) -> PathBuf {
        match agent {
            AgentType::Main => self.workspace.join("outline"),
            other => match other.write_dir() {
                Some(d) => self.workspace.join(d),
                None => self.workspace.join("outline"),
            },
        }
    }

    fn build_tools(&self, agent: AgentType) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        let ctx = FsCtx::new(self.workspace.clone(), Some(self.write_dir(agent)));

        // File tools (read anywhere, write confined).
        for t in file_tools::bundle(ctx.clone()) {
            registry.register(t);
        }

        // codex-compatible multi-edit patch tool (same write isolation).
        registry.register(crate::tools::apply_patch::make(ctx));

        // Exec for compute/build agents.
        if matches!(agent, AgentType::Main | AgentType::DataAnalysis | AgentType::Plotting | AgentType::Report) {
            registry.register(crate::tools::exec_tool::make(
                self.workspace.clone(),
                self.defaults.exec_timeout_secs,
            ));
        }

        // Manifest.
        registry.register(Arc::new(ManifestTool::new(self.manifest.clone(), agent)));

        // Skills.
        registry.register(Arc::new(LoadSkillTool::new(self.skills.clone())));
        registry.register(Arc::new(ListSkillsTool::new(self.skills.clone())));

        // Coordination: Main delegates, sub-agents report back.
        if agent == AgentType::Main {
            for t in task_tools::main_tools(self.task_board.clone(), self.bus.clone()) {
                registry.register(t);
            }
        } else {
            for t in task_tools::sub_tools(self.task_board.clone(), self.bus.clone(), agent) {
                registry.register(t);
            }
        }

        registry
    }
}

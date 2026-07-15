//! [`LoopManager`] owns one [`AgentLoop`] per agent type, each with an isolated
//! tool registry matching its write permissions. All loops are persistent for
//! the life of the process.

use crate::AgentLoop;
use anyhow::Result;
use autoreport_core::bus::Bus;
use autoreport_core::config::AgentDefaults;
use autoreport_core::prompts::PromptLoader;
use autoreport_core::provider::LLMProvider;
use autoreport_core::skills::SkillLoader;
use autoreport_core::taskboard::TaskBoard;
use autoreport_core::types::{AgentType, MessageSource};
use autoreport_tools::file_tools::FsCtx;
use autoreport_tools::manifest::{ManifestStore, ManifestTool};
use autoreport_tools::registry::ToolRegistry;
use autoreport_tools::task_tools;
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
    sandbox: autoreport_sandboxing::SandboxSpec,
    loops: HashMap<AgentType, Arc<AgentLoop>>,
    main_provider: Arc<dyn LLMProvider>,
    sub_provider: Arc<dyn LLMProvider>,
}

impl LoopManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workspace: &Path,
        main_provider: Arc<dyn LLMProvider>,
        sub_provider: Arc<dyn LLMProvider>,
        bus: Bus,
        defaults: AgentDefaults,
        sandbox: autoreport_sandboxing::SandboxSpec,
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
            sandbox,
            loops: HashMap::new(),
            main_provider,
            sub_provider,
        }
    }

    /// Build each agent loop and start it. Agents persist until the process
    /// exits (they are never shut down); only their context can be cleared.
    pub fn start(&mut self) -> Result<()> {
        for agent in AgentType::ALL {
            let provider = if agent == AgentType::Main {
                self.main_provider.clone()
            } else {
                self.sub_provider.clone()
            };
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

    /// Interrupt the active turn of one agent (codex ESC semantics).
    pub fn interrupt(&self, agent: AgentType) {
        if let Some(l) = self.loops.get(&agent) {
            let l = l.clone();
            tokio::spawn(async move {
                l.interrupt().await;
            });
        }
    }

    /// Interrupt every agent (e.g. on Ctrl+C cleanup).
    pub fn interrupt_all(&self) {
        for agent in AgentType::ALL {
            self.interrupt(agent);
        }
    }

    /// Absolute write directory for an agent.
    fn write_dir(&self, agent: AgentType) -> PathBuf {
        match agent {
            AgentType::Main => self.workspace.join("Outline"),
            other => match other.write_dir() {
                Some(d) => self.workspace.join(d),
                None => self.workspace.join("Outline"),
            },
        }
    }

    fn build_tools(&self, agent: AgentType) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        let ctx = FsCtx::new(self.workspace.clone(), Some(self.write_dir(agent)));

        // codex-compatible multi-edit patch tool (same write isolation).
        registry.register(autoreport_tools::apply_patch::make(ctx.clone()));

        // Unified shell entrypoint for reading files and running commands.
        registry.register(autoreport_tools::exec_tool::make(
            ctx,
            self.defaults.exec_timeout_secs,
            self.sandbox.clone(),
        ));

        // Manifest.
        registry.register(Arc::new(ManifestTool::new(self.manifest.clone(), agent)));

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

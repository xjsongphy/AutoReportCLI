//! [`LoopManager`] owns one [`AgentLoop`] per agent type, each with an isolated
//! tool registry matching its write permissions. All loops are persistent for
//! the life of the process.

use crate::AgentLoop;
use anyhow::Result;
use autoreport_core::bus::Bus;
use autoreport_core::config::AgentDefaults;
use autoreport_core::exec_policy::ExecPolicyManager;
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
    autoreport_home: PathBuf,
    project_home: PathBuf,
    bus: Bus,
    task_board: TaskBoard,
    manifest: ManifestStore,
    skills: SkillLoader,
    prompts: PromptLoader,
    defaults: AgentDefaults,
    sandbox: autoreport_sandboxing::SandboxSpec,
    exec_policy: Arc<ExecPolicyManager>,
    loops: HashMap<AgentType, Arc<AgentLoop>>,
    main_provider: Arc<dyn LLMProvider>,
    sub_provider: Arc<dyn LLMProvider>,
}

impl LoopManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workspace: &Path,
        autoreport_home: &Path,
        main_provider: Arc<dyn LLMProvider>,
        sub_provider: Arc<dyn LLMProvider>,
        bus: Bus,
        defaults: AgentDefaults,
        sandbox: autoreport_sandboxing::SandboxSpec,
    ) -> Self {
        let task_board = TaskBoard::new();
        let state_dir = autoreport_core::config::workspace_state_dir(autoreport_home, workspace);
        let _ = std::fs::create_dir_all(&state_dir);
        let manifest = ManifestStore::new(workspace, &state_dir);
        let skills = SkillLoader::new(autoreport_home, workspace);
        let prompts = PromptLoader::new(autoreport_home, workspace);
        let exec_policy = ExecPolicyManager::load(&state_dir).unwrap_or_else(|err| {
            log::warn!("failed to load execpolicy rules; starting with an empty policy: {err}");
            ExecPolicyManager::empty(&state_dir)
        });
        Self {
            workspace: workspace.to_path_buf(),
            autoreport_home: autoreport_home.to_path_buf(),
            project_home: state_dir,
            bus,
            task_board,
            manifest,
            skills,
            prompts,
            defaults,
            sandbox,
            exec_policy: Arc::new(exec_policy),
            loops: HashMap::new(),
            main_provider,
            sub_provider,
        }
    }

    /// Build each agent loop and start it.
    pub async fn start(&mut self) -> Result<()> {
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
                self.project_home.clone(),
                tools,
                provider.clone(),
                self.prompts.clone(),
                self.skills.clone(),
                self.manifest.clone(),
                self.bus.clone(),
                self.task_board.clone(),
                self.defaults.clone(),
                self.exec_policy.clone(),
            ));
            loop_.clone().start().await;
            self.loops.insert(agent, loop_);
        }
        Ok(())
    }

    pub fn get(&self, agent: AgentType) -> Option<Arc<AgentLoop>> {
        self.loops.get(&agent).cloned()
    }

    /// Snapshot every project-scoped conversation after startup recovery.
    pub async fn history_snapshot(
        &self,
    ) -> Vec<(AgentType, Vec<autoreport_rollout::ResponseItem>)> {
        let mut snapshot = Vec::new();
        for agent in AgentType::ALL {
            if let Some(loop_) = self.loops.get(&agent) {
                snapshot.push((agent, loop_.history_snapshot().await));
            }
        }
        snapshot
    }

    /// List only this project's persisted sessions, suitable for a TUI
    /// `/sessions` view. The project state directory is selected before lookup.
    pub fn session_summaries(&self) -> Vec<(AgentType, String, String)> {
        let mut out = Vec::new();
        for agent in AgentType::ALL {
            for (_, meta) in autoreport_rollout::list_for_agent(
                &self.project_home,
                agent.as_str(),
                &self.workspace,
            ) {
                out.push((agent, meta.conversation_id, meta.timestamp));
            }
        }
        out
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

    /// Cancel and retract a focused turn that has not reached its first tool
    /// call. The TUI uses this Codex-style boundary to restore the submitted
    /// text and remove the optimistic user row.
    pub fn interrupt_and_retract(&self, agent: AgentType) {
        if let Some(l) = self.loops.get(&agent) {
            let l = l.clone();
            tokio::spawn(async move {
                l.interrupt_and_retract().await;
            });
        }
    }

    /// Interrupt every agent (e.g. on Ctrl+C cleanup).
    pub fn interrupt_all(&self) {
        for agent in AgentType::ALL {
            self.interrupt(agent);
        }
    }

    /// Stop active turns and flush every rollout before the UI/process exits.
    pub async fn shutdown(&self) {
        for agent in AgentType::ALL {
            if let Some(loop_) = self.loops.get(&agent) {
                loop_.interrupt().await;
            }
        }
        for agent in AgentType::ALL {
            if let Some(loop_) = self.loops.get(&agent) {
                // `compact`/`clear_context` already use the same bounded idle
                // wait. A final interrupt plus a short polling window here
                // keeps shutdown deterministic without hanging on a provider.
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
                while loop_.is_busy().await && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                if loop_.is_busy().await {
                    log::warn!("{}: shutdown timed out with an active turn", agent);
                }
                loop_.flush_rollout().await;
            }
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
        registry.register(autoreport_tools::exec_tool::make_with_environment(
            ctx,
            self.defaults.exec_timeout_secs,
            self.sandbox.clone(),
            self.autoreport_home.clone(),
        ));

        // Manifest.
        registry.register(Arc::new(ManifestTool::new(self.manifest.clone(), agent)));

        // Codex exposes request_user_input to the active thread, including
        // delegated threads. The shared broker keeps the TUI prompt queue
        // independent of which agent is focused.
        registry.register(autoreport_tools::request_user_input::make(
            self.bus.clone(),
            agent,
        ));

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

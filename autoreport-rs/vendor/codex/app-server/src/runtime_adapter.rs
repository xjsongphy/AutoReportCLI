//! Session registry for the AutoReport runtime used by the app server.
//!
//! This module deliberately contains no Codex account, cloud, MCP, image, or
//! realtime integration. A registered thread is only a handle to the
//! project-local [`autoreport_runtime::LoopManager`] and its `Main` agent.

use anyhow::{Result, anyhow, bail};
use autoreport_core::types::{AgentType, MessageSource};
use autoreport_rollout::ResponseItem;
use autoreport_runtime::LoopManager;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Immutable app-server view of one AutoReport thread.
///
/// Every app-server thread is deliberately bound to the project's main agent.
/// The other AutoReport agents remain an implementation detail of that main
/// agent's local runtime and are not independently addressable through this
/// adapter.
#[derive(Clone)]
pub struct RuntimeSession {
    thread_id: String,
    workspace: PathBuf,
    manager: Arc<LoopManager>,
}

impl RuntimeSession {
    fn new(thread_id: String, workspace: PathBuf, manager: Arc<LoopManager>) -> Self {
        Self {
            thread_id,
            workspace,
            manager,
        }
    }

    /// AutoReport's stable identifier for this app-server thread.
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Workspace used when the project-local runtime was constructed.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// The only agent exposed through app-server threads.
    pub const fn agent(&self) -> AgentType {
        AgentType::Main
    }

    /// Enqueue ordinary user input for the main AutoReport agent.
    pub fn submit(&self, content: impl Into<String>) -> Result<()> {
        let content = non_empty_input(content.into(), "turn input")?;
        self.manager
            .submit(AgentType::Main, content, MessageSource::User);
        Ok(())
    }

    /// Append input to the active main-agent turn.
    ///
    /// The runtime itself decides whether a turn is active; a missing or idle
    /// loop is returned as an ordinary local error rather than being emulated.
    pub async fn steer(&self, content: impl Into<String>) -> Result<()> {
        let content = non_empty_input(content.into(), "steer input")?;
        let loop_ = self.main_loop()?;
        loop_
            .steer_input(content, MessageSource::User)
            .await
            .map_err(|error| anyhow!(error))
    }

    /// Request interruption of the current main-agent turn.
    pub fn interrupt(&self) -> Result<()> {
        self.main_loop()?;
        self.manager.interrupt(AgentType::Main);
        Ok(())
    }

    /// Return a stable snapshot of this thread's main-agent transcript.
    pub async fn history(&self) -> Result<Vec<ResponseItem>> {
        Ok(self.main_loop()?.history_snapshot().await)
    }

    fn main_loop(&self) -> Result<Arc<autoreport_runtime::AgentLoop>> {
        self.manager.get(AgentType::Main).ok_or_else(|| {
            anyhow!(
                "AutoReport runtime for thread '{}' has not been started",
                self.thread_id
            )
        })
    }
}

/// Small, thread-safe registry owned by the app-server process.
///
/// The registry does not create runtimes: construction and startup are owned
/// by the AutoReport application, so provider configuration and credentials
/// never enter this layer.
#[derive(Default)]
pub struct RuntimeSessionRegistry {
    sessions: RwLock<HashMap<String, RuntimeSession>>,
}

impl RuntimeSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one already-created project runtime.
    ///
    /// Duplicate thread IDs are rejected to prevent silently directing an
    /// existing client thread to a different workspace or provider runtime.
    pub fn register(
        &self,
        thread_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        manager: Arc<LoopManager>,
    ) -> Result<RuntimeSession> {
        let thread_id = validate_thread_id(thread_id.into())?;
        let session = RuntimeSession::new(thread_id.clone(), workspace.into(), manager);
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| anyhow!("runtime session registry lock is poisoned"))?;
        if sessions.contains_key(&thread_id) {
            bail!("AutoReport thread '{thread_id}' is already registered");
        }
        sessions.insert(thread_id, session.clone());
        Ok(session)
    }

    /// Find a registered thread without exposing the registry lock.
    pub fn get(&self, thread_id: &str) -> Result<RuntimeSession> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| anyhow!("runtime session registry lock is poisoned"))?;
        sessions
            .get(thread_id)
            .cloned()
            .ok_or_else(|| anyhow!("AutoReport thread '{thread_id}' was not found"))
    }

    /// Remove a thread registration. This does not shut down the runtime,
    /// whose lifetime remains owned by the application that created it.
    pub fn remove(&self, thread_id: &str) -> Result<RuntimeSession> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| anyhow!("runtime session registry lock is poisoned"))?;
        sessions
            .remove(thread_id)
            .ok_or_else(|| anyhow!("AutoReport thread '{thread_id}' was not found"))
    }

    /// List registered thread metadata. Runtime/provider internals stay hidden.
    pub fn list(&self) -> Result<Vec<RuntimeSessionInfo>> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| anyhow!("runtime session registry lock is poisoned"))?;
        let mut list = sessions
            .values()
            .map(|session| RuntimeSessionInfo {
                thread_id: session.thread_id.clone(),
                workspace: session.workspace.clone(),
                agent: AgentType::Main,
            })
            .collect::<Vec<_>>();
        list.sort_by(|left, right| left.thread_id.cmp(&right.thread_id));
        Ok(list)
    }

    pub fn submit(&self, thread_id: &str, content: impl Into<String>) -> Result<()> {
        self.get(thread_id)?.submit(content)
    }

    pub async fn steer(&self, thread_id: &str, content: impl Into<String>) -> Result<()> {
        self.get(thread_id)?.steer(content).await
    }

    pub fn interrupt(&self, thread_id: &str) -> Result<()> {
        self.get(thread_id)?.interrupt()
    }

    pub async fn history(&self, thread_id: &str) -> Result<Vec<ResponseItem>> {
        self.get(thread_id)?.history().await
    }
}

/// Safe metadata for thread-list responses. No provider configuration or
/// credentials are retained or exposed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSessionInfo {
    pub thread_id: String,
    pub workspace: PathBuf,
    pub agent: AgentType,
}

fn validate_thread_id(thread_id: String) -> Result<String> {
    if thread_id.trim().is_empty() {
        bail!("AutoReport thread id cannot be empty");
    }
    Ok(thread_id)
}

fn non_empty_input(content: String, name: &str) -> Result<String> {
    if content.trim().is_empty() {
        bail!("{name} cannot be empty");
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::{non_empty_input, validate_thread_id};

    #[test]
    fn thread_id_validation_rejects_blank_ids_without_normalizing_valid_ids() {
        assert!(validate_thread_id(" \t\n".into()).is_err());
        assert_eq!(validate_thread_id("thread-1".into()).unwrap(), "thread-1");
    }

    #[test]
    fn input_validation_keeps_content_but_rejects_blank_input() {
        assert!(non_empty_input("  \n".into(), "turn input").is_err());
        assert_eq!(
            non_empty_input(" keep spaces ".into(), "turn input").unwrap(),
            " keep spaces "
        );
    }
}

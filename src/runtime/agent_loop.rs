//! A single agent's event loop.
//!
//! Subscribes to the bus, processes messages addressed to it one at a time,
//! streams model output, and iterates tool calls. Conversation history is
//! owned by the loop and can be cleared (`/clear`) without stopping the agent.

use crate::bus::Bus;
use crate::config::AgentDefaults;
use crate::prompts::PromptLoader;
use crate::provider::types::{Message, ToolCall};
use crate::provider::LLMProvider;
use crate::runtime::agent_loop::internal::AgentStatusLock;
use crate::skills::SkillLoader;
use crate::taskboard::TaskBoard;
use crate::tools::manifest::ManifestStore;
use crate::tools::ToolRegistry;
use crate::types::{AgentStatus, AgentType, BusMessage, MessageSource};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// One queued inbound message for an agent to process.
struct Inbound {
    content: String,
    source: MessageSource,
}

pub struct AgentLoop {
    pub agent: AgentType,
    workspace: PathBuf,
    tools: ToolRegistry,
    provider: Arc<dyn LLMProvider>,
    prompts: PromptLoader,
    skills: SkillLoader,
    manifest: ManifestStore,
    bus: Bus,
    task_board: TaskBoard,
    defaults: AgentDefaults,
    history: Arc<Mutex<Vec<Message>>>,
    system_prompt: Arc<Mutex<String>>,
    status: AgentStatusLock,
    inbox_tx: mpsc::Sender<Inbound>,
    inbox_rx: Arc<Mutex<mpsc::Receiver<Inbound>>>,
}

impl AgentLoop {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent: AgentType,
        workspace: PathBuf,
        tools: ToolRegistry,
        provider: Arc<dyn LLMProvider>,
        prompts: PromptLoader,
        skills: SkillLoader,
        manifest: ManifestStore,
        bus: Bus,
        task_board: TaskBoard,
        defaults: AgentDefaults,
    ) -> Self {
        let system_prompt = prompts.build_system_prompt(agent, &skills, &workspace);
        let (inbox_tx, inbox_rx) = mpsc::channel(64);
        Self {
            agent,
            workspace,
            tools,
            provider,
            prompts,
            skills,
            manifest,
            bus,
            task_board,
            defaults,
            history: Arc::new(Mutex::new(Vec::new())),
            system_prompt: Arc::new(Mutex::new(system_prompt)),
            status: AgentStatusLock::new(AgentStatus::Idle),
            inbox_tx,
            inbox_rx: Arc::new(Mutex::new(inbox_rx)),
        }
    }

    /// Spawn the bus listener and the single-threaded processor. Returns a
    /// handle that survives for the life of the program (sub-agents persist and
    /// are never shut down).
    pub fn start(self: Arc<Self>) {
        self.spawn_listener();
        self.clone().spawn_processor();
    }

    fn spawn_listener(&self) {
        let agent = self.agent;
        let bus = self.bus.clone();
        let inbox = self.inbox_tx.clone();
        tokio::spawn(async move {
            let mut rx = bus.subscribe();
            loop {
                match rx.recv().await {
                    Ok(msg) => forward_message(agent, &msg, &inbox).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }

    fn spawn_processor(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                let inbound = {
                    let mut g = self.inbox_rx.lock().await;
                    match g.recv().await {
                        Some(m) => m,
                        None => return,
                    }
                };
                // Never let one bad message kill the agent.
                if let Err(e) = self.process(inbound).await {
                    self.bus.publish(BusMessage::Error {
                        agent_type: Some(self.agent),
                        message: format!("{e:?}"),
                    });
                    self.set_status(AgentStatus::Idle);
                }
            }
        });
    }

    /// Direct submission (used by the TUI when the user addresses this agent).
    pub fn submit(&self, content: String, source: MessageSource) {
        let _ = self.inbox_tx.try_send(Inbound { content, source });
    }

    /// Clear conversation history but keep the agent running.
    pub async fn clear_context(&self) {
        self.history.lock().await.clear();
        self.bus.publish(BusMessage::StatusChange {
            agent_type: self.agent,
            status: AgentStatus::Idle,
        });
    }

    /// Reset to a fresh state (equivalent to clear for now).
    pub async fn reset(&self) {
        self.clear_context().await;
    }

    pub async fn status(&self) -> AgentStatus {
        self.status.get()
    }

    fn set_status(&self, s: AgentStatus) {
        self.status.set(s);
        self.bus.publish(BusMessage::StatusChange {
            agent_type: self.agent,
            status: s,
        });
    }

    async fn process(&self, inbound: Inbound) -> anyhow::Result<()> {
        self.set_status(AgentStatus::Thinking);
        {
            let mut h = self.history.lock().await;
            h.push(Message::user(&inbound.content));
        }

        let mut iterations: u32 = 0;
        loop {
            // Auto-compact if we are over budget.
            self.maybe_compact().await;

            let messages = self.build_request().await;
            let defs = self.tools.definitions();

            let (content, tool_calls, _usage) = self
                .stream_completion(&messages, &defs)
                .await?;

            // Append the assistant turn (text + any tool calls) to history.
            {
                let assistant = Message {
                    role: "assistant".into(),
                    content: content.clone().unwrap_or_default(),
                    tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls.clone()) },
                    tool_call_id: None,
                    thinking: None,
                };
                self.history.lock().await.push(assistant);
            }

            if tool_calls.is_empty() {
                break;
            }

            self.set_status(AgentStatus::RunningTool);
            for call in &tool_calls {
                self.execute_tool_call(call).await;
            }
            self.set_status(AgentStatus::Thinking);

            iterations += 1;
            if iterations >= self.defaults.max_tool_iterations {
                self.history.lock().await.push(Message::user(
                    "You have reached the maximum number of tool iterations for this turn. \
                     Stop calling tools and summarize what you have so far.",
                ));
            }
        }

        self.set_status(AgentStatus::Idle);
        Ok(())
    }

    async fn build_request(&self) -> Vec<Message> {
        let sys = self.system_prompt.lock().await.clone();
        let mut out = Vec::with_capacity(self.history.lock().await.len() + 1);
        out.push(Message::system(sys));
        out.extend(self.history.lock().await.iter().cloned());
        out
    }

    /// Stream one completion, publishing deltas, returning final text + tool calls.
    async fn stream_completion(
        &self,
        messages: &[Message],
        defs: &[crate::provider::types::ToolDef],
    ) -> anyhow::Result<(Option<String>, Vec<ToolCall>, Option<crate::provider::types::Usage>)> {
        let mut rx = self
            .provider
            .chat_stream(messages, defs, self.defaults.temperature, self.defaults.max_tokens)
            .await?;

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = None;
        while let Some(chunk) = rx.recv().await {
            let chunk = chunk?;
            if let Some(delta) = chunk.delta {
                content.push_str(&delta);
                self.bus.publish(BusMessage::AgentResponse {
                    agent_type: self.agent,
                    content: delta,
                    streaming: true,
                });
            }
            if let Some(tc) = chunk.tool_calls {
                tool_calls = tc;
            }
            if let Some(u) = chunk.usage {
                usage = Some(u);
            }
            if chunk.done {
                break;
            }
        }
        // Final marker so the UI knows the turn is complete.
        self.bus.publish(BusMessage::AgentResponse {
            agent_type: self.agent,
            content: String::new(),
            streaming: false,
        });
        Ok((if content.is_empty() { None } else { Some(content) }, tool_calls, usage))
    }

    async fn execute_tool_call(&self, call: &ToolCall) {
        self.bus.publish(BusMessage::ToolCall {
            agent_type: self.agent,
            tool_name: call.name.clone(),
            arguments: call.arguments.clone(),
        });

        let out = self.tools.call(&call.name, &call.arguments).await;
        let result_value = match (&out.result, &out.error) {
            (_, Some(e)) => serde_json::json!({ "error": e }),
            (r, None) => r.clone(),
        };
        // Serialize the result compactly for the model.
        let result_text = serde_json::to_string(&result_value).unwrap_or_else(|_| "{}".into());

        self.bus.publish(BusMessage::ToolResult {
            agent_type: self.agent,
            tool_name: call.name.clone(),
            result: result_value,
            error: out.error.clone(),
        });

        // Track produced files in the manifest for write-style tools.
        if let Some(path) = extract_path(&call.name, &call.arguments) {
            self.manifest.record(self.agent, path);
        }

        self.history.lock().await.push(Message::tool_result(&call.id, result_text));
    }

    /// Rough token estimate (chars / 4). Good enough for compaction decisions.
    async fn estimated_tokens(&self) -> usize {
        let h = self.history.lock().await;
        let chars: usize = h
            .iter()
            .map(|m| m.content.chars().count() + m.content.len())
            .sum::<usize>()
            / 2;
        chars / 3
    }

    async fn maybe_compact(&self) {
        let est = self.estimated_tokens().await;
        let budget = (self.defaults.compact_threshold * 128_000.0) as usize;
        if est > budget {
            self.compact_internal().await;
        }
    }

    /// Replace history with a model-generated summary of itself, keeping the
    /// most recent turn verbatim. Falls back to trimming if summarization fails.
    async fn compact_internal(&self) {
        self.bus.publish(BusMessage::StatusChange {
            agent_type: self.agent,
            status: AgentStatus::Thinking,
        });

        let snapshot: Vec<Message> = self.history.lock().await.clone();
        if snapshot.len() < 4 {
            return;
        }

        let transcript = transcript_text(&snapshot);
        let keep = snapshot.len().saturating_sub(2);
        let recent: Vec<Message> = snapshot[keep..].to_vec();

        let summary_prompt = format!(
            "Summarize the conversation below into a concise context note: key decisions, \
             facts, file paths produced, and open tasks. Be terse.\n\n---\n{transcript}"
        );

        let req = vec![
            Message::system("You are a context-summarization assistant. Reply with only the summary."),
            Message::user(summary_prompt),
        ];
        let resp = self
            .provider
            .chat(&req, &[], 0.0, 1024)
            .await;

        let summary = match resp {
            Ok(r) => r.content.unwrap_or_else(|| "(no summary)".into()),
            Err(e) => {
                log::warn!("compact summarization failed, trimming instead: {e}");
                // Fallback: keep only the recent turns.
                *self.history.lock().await = recent;
                self.bus.publish(BusMessage::AgentResponse {
                    agent_type: self.agent,
                    content: "[context trimmed]".into(),
                    streaming: false,
                });
                return;
            }
        };

        let mut new_history = Vec::new();
        new_history.push(Message::user(format!(
            "## Prior context (compacted)\n{summary}"
        )));
        new_history.extend(recent);
        *self.history.lock().await = new_history;

        self.bus.publish(BusMessage::AgentResponse {
            agent_type: self.agent,
            content: "[context compacted]".into(),
            streaming: false,
        });
    }

    /// Public `/compact` command.
    pub async fn compact(&self) {
        self.compact_internal().await;
        self.set_status(AgentStatus::Idle);
    }

    // Allow the manager to rebuild the system prompt (e.g. after skill changes).
    pub async fn refresh_system_prompt(&self) {
        let p = self.prompts.build_system_prompt(self.agent, &self.skills, &self.workspace);
        *self.system_prompt.lock().await = p;
    }
}

/// Forward a bus message into this agent's inbox if it is addressed to us.
async fn forward_message(agent: AgentType, msg: &BusMessage, inbox: &mpsc::Sender<Inbound>) {
    match msg {
        BusMessage::UserMessage {
            agent_type,
            content,
            source,
            ..
        } if *agent_type == agent => {
            let _ = inbox
                .send(Inbound {
                    content: content.clone(),
                    source: *source,
                })
                .await;
        }
        BusMessage::AgentFeedback { agent_type, content, feedback_type }
            if agent == AgentType::Main =>
        {
            let _ = inbox
                .send(Inbound {
                    content: format!(
                        "⚠️ {agent_type} 报告问题（{feedback_type}）：{content}\n请决定如何处理。"
                    ),
                    source: MessageSource::Agent(*agent_type),
                })
                .await;
        }
        BusMessage::TaskUpdate {
            action,
            source_agent,
            brief,
            ..
        } if agent == AgentType::Main =>
        {
            let _ = inbox
                .send(Inbound {
                    content: format!(
                        "✅ {source_agent} 任务「{brief}」状态变为 {action}。"
                    ),
                    source: MessageSource::Agent(*source_agent),
                })
                .await;
        }
        _ => {}
    }
}

fn transcript_text(msgs: &[Message]) -> String {
    let mut s = String::new();
    for m in msgs {
        let role = &m.role;
        s.push_str(&format!("[{role}] {}\n", m.content));
    }
    s
}

/// Best-effort extraction of the written file path from a tool call, for
/// manifest tracking.
fn extract_path(tool_name: &str, args: &serde_json::Value) -> Option<String> {
    if !matches!(tool_name, "write_file" | "edit_file" | "delete_file") {
        return None;
    }
    let p = args.get("path")?.as_str()?;
    let resolved = crate::tools::file_tools::resolve_within(p, std::path::Path::new(""))
        .ok()?;
    Some(resolved.to_string_lossy().into_owned())
}

mod internal {
    use crate::types::AgentStatus;
    use std::sync::{Arc, Mutex};

    /// Tracks an agent's current status behind an Arc so the loop handle stays
    /// cheaply cloneable.
    pub struct AgentStatusLock {
        inner: Arc<Mutex<AgentStatus>>,
    }

    impl Clone for AgentStatusLock {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }

    impl AgentStatusLock {
        pub fn new(s: AgentStatus) -> Self {
            Self {
                inner: Arc::new(Mutex::new(s)),
            }
        }
        pub fn get(&self) -> AgentStatus {
            *self.inner.lock().unwrap()
        }
        pub fn set(&self, s: AgentStatus) {
            *self.inner.lock().unwrap() = s;
        }
    }
}

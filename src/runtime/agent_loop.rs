//! A single agent's event loop, modelled on codex's session/turn design.
//!
//! - **Conversation unit** is codex's `ResponseItem` (`rollout::ResponseItem`),
//!   not a raw provider message. History is `Vec<ResponseItem>`.
//! - **Prompt assembly** is codex's `instructions + items` shape: fresh
//!   instructions plus the history items, converted to the provider's wire
//!   format at call time (`items_to_messages`).
//! - **Message queue** is codex's session discipline: an `Op` channel consumed
//!   by a single processor task (one turn at a time); a new user input
//!   interrupts the active turn and is then processed.
//! - **Persistence**: every produced item is appended to a codex-format rollout
//!   file (`.autoreport/sessions/rollout-<ts>-<id>.jsonl`); on startup the
//!   latest rollout for the agent is resumed.

use crate::bus::Bus;
use crate::config::AgentDefaults;
use crate::prompts::PromptLoader;
use crate::provider::LLMProvider;
use crate::provider::types::{Message, ToolCall as ProviderToolCall};
use crate::rollout::{self, ResponseItem, RolloutRecorder};
use crate::runtime::agent_loop::internal::AgentStatusLock;
use crate::skills::SkillLoader;
use crate::taskboard::TaskBoard;
use crate::tools::ToolRegistry;
use crate::tools::manifest::ManifestStore;
use crate::types::{AgentStatus, AgentType, BusMessage, MessageSource};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, mpsc};

/// Result of a turn-end guard check.
enum GuardOutcome {
    /// No guard triggered — the turn may end.
    Allow,
    /// Guard gave up (retries exhausted) — surface the notice and end anyway.
    Exhausted,
    /// Guard wants another completion cycle with this reminder injected.
    Retry(String),
}

/// Operations delivered to an agent's session task (codex `Op` analogue).
/// Interrupts go directly through `cancel_current_turn()` (not queued) so they
/// take effect immediately even while a turn is mid-stream.
enum Op {
    UserInput {
        content: String,
        source: MessageSource,
    },
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
    /// codex-style history of conversation items.
    history: Arc<Mutex<Vec<ResponseItem>>>,
    system_prompt: Arc<Mutex<String>>,
    status: AgentStatusLock,
    op_tx: mpsc::Sender<Op>,
    op_rx: Arc<Mutex<mpsc::Receiver<Op>>>,
    /// Cancellation token for the currently-running turn (codex interrupt).
    current_turn: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// Set when this agent calls `respond` during the current turn — the
    /// sub-report guard requires it before a Main-dispatched turn may end.
    turn_reported: Arc<AtomicBool>,
    /// Append-only rollout recorder (`None` until the first item is written).
    recorder: Arc<Mutex<Option<RolloutRecorder>>>,
    /// Logical conversation id; `/clear` rolls a new one (new rollout file).
    conversation_id: Arc<Mutex<String>>,
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
        let (op_tx, op_rx) = mpsc::channel(64);
        let conversation_id = format!("{}-{}", agent.as_str(), uuid::Uuid::new_v4());
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
            op_tx,
            op_rx: Arc::new(Mutex::new(op_rx)),
            current_turn: Arc::new(Mutex::new(None)),
            turn_reported: Arc::new(AtomicBool::new(false)),
            recorder: Arc::new(Mutex::new(None)),
            conversation_id: Arc::new(Mutex::new(conversation_id)),
        }
    }

    /// Spawn the bus listener and the single-threaded session processor, and
    /// resume the most recent rollout for this agent if one exists.
    pub fn start(self: Arc<Self>) {
        self.spawn_listener();
        self.clone().spawn_processor();
    }

    async fn resume_from_rollout(&self) {
        let cid = self.conversation_id.lock().await.clone();
        let Some(path) = rollout::latest_for(&self.workspace, &cid) else {
            return;
        };
        match rollout::read(&path) {
            Ok(entries) => {
                let items = rollout::items(&entries);
                if !items.is_empty() {
                    log::info!(
                        "{}: resumed {} items from {}",
                        self.agent,
                        items.len(),
                        path.display()
                    );
                    *self.history.lock().await = items;
                    // Reopen the same file so subsequent appends CONTINUE it
                    // (otherwise the first new record would fork into a fresh
                    // file and fragment the on-disk history across restarts).
                    match RolloutRecorder::open(&path) {
                        Ok(rec) => *self.recorder.lock().await = Some(rec),
                        Err(e) => log::warn!("reopen rollout {}: {e}", path.display()),
                    }
                }
            }
            Err(e) => log::warn!("resume {}: {e}", path.display()),
        }
    }

    fn spawn_listener(&self) {
        let agent = self.agent;
        let bus = self.bus.clone();
        let submit = self.op_tx.clone();
        let cancel = self.current_turn.clone();
        tokio::spawn(async move {
            let mut rx = bus.subscribe();
            loop {
                match rx.recv().await {
                    Ok(msg) => forward_message(agent, &msg, &submit, &cancel).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }

    fn spawn_processor(self: Arc<Self>) {
        tokio::spawn(async move {
            // Resume the most recent rollout before processing new input.
            self.resume_from_rollout().await;
            loop {
                let op = {
                    let mut g = self.op_rx.lock().await;
                    match g.recv().await {
                        Some(op) => op,
                        None => return,
                    }
                };
                match op {
                    Op::UserInput { content, source } => {
                        if let Err(e) = self.process_turn(&content, source).await {
                            // The turn bailed mid-stream: ensure no stale token
                            // remains (otherwise is_busy() lies and the invariant
                            // "set token ⇔ live turn" breaks).
                            self.cancel_current_turn().await;
                            self.bus.publish(BusMessage::Error {
                                agent_type: Some(self.agent),
                                message: format!("{e:?}"),
                            });
                            self.set_status(AgentStatus::Idle);
                        }
                    }
                }
            }
        });
    }

    /// Address a user message to this agent (codex session: a new user input
    /// interrupts any active turn, then is processed).
    pub fn submit(&self, content: String, source: MessageSource) {
        let op_tx = self.op_tx.clone();
        let cancel = self.current_turn.clone();
        tokio::spawn(async move {
            // New user input interrupts the active turn (codex semantics).
            if let Some(token) = cancel.lock().await.take() {
                token.cancel();
            }
            let _ = op_tx.send(Op::UserInput { content, source }).await;
        });
    }

    /// Interrupt the currently-running turn.
    pub async fn interrupt(&self) {
        self.cancel_current_turn().await;
    }

    async fn cancel_current_turn(&self) {
        if let Some(token) = self.current_turn.lock().await.take() {
            token.cancel();
        }
    }

    pub async fn is_busy(&self) -> bool {
        self.current_turn.lock().await.is_some()
    }

    /// Clear context and start a fresh conversation (new rollout file), keeping
    /// the agent running.
    pub async fn clear_context(&self) {
        self.history.lock().await.clear();
        *self.recorder.lock().await = None;
        *self.conversation_id.lock().await =
            format!("{}-{}", self.agent.as_str(), uuid::Uuid::new_v4());
        self.bus.publish(BusMessage::StatusChange {
            agent_type: self.agent,
            status: AgentStatus::Idle,
        });
    }

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

    /// Persist an item. Rollout is appended FIRST (source of truth), then the
    /// in-memory history — so a failed append leaves history unchanged rather
    /// than silently diverging from disk.
    async fn record(&self, item: ResponseItem) {
        if let Err(e) = self.append_to_rollout(&item).await {
            log::warn!("rollout append: {e}");
        }
        self.history.lock().await.push(item);
    }

    async fn append_to_rollout(&self, item: &ResponseItem) -> anyhow::Result<()> {
        let mut rec = self.recorder.lock().await;
        if rec.is_none() {
            let cid = self.conversation_id.lock().await.clone();
            let ts = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();
            *rec = Some(RolloutRecorder::create(&self.workspace, &cid, &ts)?);
        }
        rec.as_ref().unwrap().append(item)?;
        Ok(())
    }

    async fn process_turn(&self, content: &str, source: MessageSource) -> anyhow::Result<()> {
        self.set_status(AgentStatus::Thinking);
        self.record(ResponseItem::user_message(content)).await;

        let turn_token = tokio_util::sync::CancellationToken::new();
        *self.current_turn.lock().await = Some(turn_token.clone());
        self.turn_reported.store(false, Ordering::Relaxed);

        const MAX_GUARD_RETRIES: u32 = 2;
        let mut guard_retries: u32 = 0;
        let mut next_prompt: Option<String> = None;

        loop {
            // Guard re-prompt round: feed the reminder as a user item so the
            // model sees why it must continue.
            if let Some(prompt) = next_prompt.take() {
                self.set_status(AgentStatus::Thinking);
                self.record(ResponseItem::user_message(&prompt)).await;
            }

            let interrupted = self.run_completion_cycle(&turn_token).await;
            if interrupted {
                self.record(ResponseItem::user_message(
                    "[interrupted by user — stop what you are doing and wait for the next instruction.]",
                ))
                .await;
                self.bus.publish(BusMessage::AgentResponse {
                    agent_type: self.agent,
                    content: String::new(),
                    streaming: false,
                });
                break;
            }

            // Loop guards (AutoReport report protocol): sub must `respond`
            // before ending a Main-dispatched turn; Main may not go IDLE while
            // it has BLOCKED tasks.
            match self
                .apply_turn_guards(source, &mut guard_retries, MAX_GUARD_RETRIES)
                .await
            {
                GuardOutcome::Allow => break,
                GuardOutcome::Exhausted => break,
                GuardOutcome::Retry(reminder) => {
                    next_prompt = Some(reminder);
                    continue;
                }
            }
        }

        self.current_turn.lock().await.take();
        self.status.set(AgentStatus::Idle);
        self.bus.publish(BusMessage::StatusChange {
            agent_type: self.agent,
            status: AgentStatus::Idle,
        });
        Ok(())
    }

    /// One completion cycle: stream a response, run any tool calls, repeat
    /// until the model stops emitting tools or the turn is interrupted.
    /// Returns `true` if interrupted by the cancellation token.
    async fn run_completion_cycle(&self, turn_token: &tokio_util::sync::CancellationToken) -> bool {
        let mut iterations: u32 = 0;
        loop {
            self.maybe_compact().await;
            if turn_token.is_cancelled() {
                return true;
            }

            let messages = self.build_request().await;
            let defs = self.tools.definitions();
            let (content, reasoning, tool_calls, _usage, intr) =
                match self.stream_completion(&messages, &defs, turn_token).await {
                    Ok(v) => v,
                    Err(e) => {
                        log::warn!("{} stream_completion: {e}", self.agent);
                        return false;
                    }
                };

            if intr {
                if let Some(r) = reasoning {
                    if !r.is_empty() {
                        self.record(ResponseItem::reasoning(r)).await;
                    }
                }
                if let Some(c) = content {
                    if !c.is_empty() {
                        self.record(ResponseItem::assistant_message(c)).await;
                    }
                }
                return true;
            }

            if let Some(text) = reasoning {
                if !text.is_empty() {
                    self.record(ResponseItem::reasoning(text)).await;
                }
            }
            if let Some(text) = content {
                if !text.is_empty() {
                    self.record(ResponseItem::assistant_message(text)).await;
                }
            }
            if tool_calls.is_empty() {
                return false;
            }
            for call in &tool_calls {
                let args_json = serde_json::to_string(&call.arguments).unwrap_or_default();
                self.record(ResponseItem::function_call(&call.id, &call.name, args_json))
                    .await;
            }

            self.set_status(AgentStatus::RunningTool);
            let mut interrupted = false;
            for call in &tool_calls {
                if turn_token.is_cancelled() {
                    interrupted = true;
                    break;
                }
                self.execute_tool_call(call).await;
            }
            if interrupted {
                return true;
            }
            self.set_status(AgentStatus::Thinking);

            iterations += 1;
            if iterations >= self.defaults.max_tool_iterations {
                self.record(ResponseItem::user_message(
                    "You have reached the maximum number of tool iterations for this turn. \
                     Stop calling tools and summarize what you have so far.",
                ))
                .await;
            }
        }
    }

    /// Enforce the report-protocol turn guards. See `GuardOutcome`.
    async fn apply_turn_guards(
        &self,
        source: MessageSource,
        retries: &mut u32,
        max_retries: u32,
    ) -> GuardOutcome {
        // Sub-agent report guard: a Main-dispatched turn must end with respond.
        if self.agent != AgentType::Main && source == MessageSource::MainAgent {
            if self.turn_reported.load(Ordering::Relaxed) {
                return GuardOutcome::Allow;
            }
            *retries += 1;
            let active: Vec<crate::types::TaskItem> = self
                .task_board
                .todolist(self.agent)
                .into_iter()
                .filter(|t| t.source_agent == AgentType::Main)
                .collect();
            if *retries > max_retries {
                // Exhausted: mark the active task blocked and emit a report on
                // the sub's behalf so Main's blocking wait always resolves.
                let notice = if active.is_empty() {
                    "多次未调用 respond，本轮结束。".to_string()
                } else {
                    let ids: Vec<String> = active.iter().map(|t| t.task_id.clone()).collect();
                    for t in &active {
                        self.task_board.block(&t.task_id);
                        self.bus.publish(BusMessage::Report {
                            agent_type: self.agent,
                            task_id: t.task_id.clone(),
                            report_type: "missing_data".into(),
                            summary: "Sub-agent did not call respond".into(),
                            content: "Sub-agent did not call respond within the turn budget."
                                .into(),
                        });
                    }
                    format!(
                        "多次未调用 respond，任务 {} 已标记为 blocked，交回 Main 处理。",
                        ids.join(", ")
                    )
                };
                self.bus.publish(BusMessage::SystemNotice {
                    agent_type: Some(self.agent),
                    content: notice,
                });
                return GuardOutcome::Exhausted;
            }
            let ids: Vec<String> = active.iter().map(|t| t.task_id.clone()).collect();
            self.bus.publish(BusMessage::SystemNotice {
                agent_type: Some(self.agent),
                content: format!(
                    "本轮需要调用 respond 向 Main 回复 ({}/{max_retries})。",
                    *retries
                ),
            });
            return GuardOutcome::Retry(format!(
                "本轮需要调用 Respond 向 Main 回复。请调用 respond(task_id, type, summary, content)：\
                 type='reply' 表示完成，'missing_data'/'quality' 表示卡住。\
                 当前未回复的任务：[{}]。",
                ids.join(", ")
            ));
        }

        // Main blocked guard: Main may not go IDLE while BLOCKED tasks remain.
        if self.agent == AgentType::Main {
            let blocked = self.task_board.blocked_waitlist(AgentType::Main);
            if blocked.is_empty() {
                return GuardOutcome::Allow;
            }
            *retries += 1;
            let names: Vec<String> = blocked
                .iter()
                .map(|t| format!("{}:{}", t.target_agent.as_str(), t.brief))
                .collect();
            if *retries > max_retries {
                self.bus.publish(BusMessage::SystemNotice {
                    agent_type: Some(AgentType::Main),
                    content: format!(
                        "Main 多次未解决被阻塞任务，暂停以便用户介入：{}",
                        names.join(", ")
                    ),
                });
                return GuardOutcome::Exhausted;
            }
            self.bus.publish(BusMessage::SystemNotice {
                agent_type: Some(AgentType::Main),
                content: format!(
                    "Main 还有被阻塞的任务：{}，请先解决 ({}/{max_retries})。",
                    names.join(", "),
                    *retries
                ),
            });
            return GuardOutcome::Retry(
                "还有被阻塞的任务未处理。请用 send_to_agent 重派、转交其他 agent，或自行补齐所需输入后再结束。"
                    .into(),
            );
        }

        GuardOutcome::Allow
    }

    /// codex prompt assembly: fresh `instructions` + `items` (history),
    /// converted to provider messages.
    async fn build_request(&self) -> Vec<Message> {
        let instructions =
            self.prompts
                .build_system_prompt(self.agent, &self.skills, &self.workspace);
        *self.system_prompt.lock().await = instructions.clone();
        let items = self.history.lock().await.clone();
        let mut out = Vec::with_capacity(items.len() + 1);
        out.push(Message::system(instructions));
        out.extend(items_to_messages(&items));
        out
    }

    /// Stream one completion, selecting on the turn's cancellation token.
    async fn stream_completion(
        &self,
        messages: &[Message],
        defs: &[crate::provider::types::ToolDef],
        cancel: &tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<(
        Option<String>,
        Option<String>,
        Vec<ProviderToolCall>,
        Option<crate::provider::types::Usage>,
        bool,
    )> {
        let mut rx = self
            .provider
            .chat_stream(
                messages,
                defs,
                self.defaults.temperature,
                self.defaults.max_tokens,
            )
            .await?;

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = None;
        let mut interrupted = false;
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => { interrupted = true; break; }
                chunk = rx.recv() => {
                    let Some(chunk) = chunk else { break };
                    let chunk = chunk?;
                    if let Some(delta) = chunk.delta {
                        content.push_str(&delta);
                        self.bus.publish(BusMessage::AgentResponse {
                            agent_type: self.agent,
                            content: delta,
                            streaming: true,
                        });
                    }
                    if let Some(delta) = chunk.thinking_delta {
                        reasoning.push_str(&delta);
                        self.bus.publish(BusMessage::AgentReasoning {
                            agent_type: self.agent,
                            content: delta,
                            streaming: true,
                        });
                    }
                    if let Some(tc) = chunk.tool_calls { tool_calls = tc; }
                    if let Some(u) = chunk.usage { usage = Some(u); }
                    if chunk.done { break; }
                }
            }
        }
        self.bus.publish(BusMessage::AgentResponse {
            agent_type: self.agent,
            content: String::new(),
            streaming: false,
        });
        self.bus.publish(BusMessage::AgentReasoning {
            agent_type: self.agent,
            content: String::new(),
            streaming: false,
        });
        Ok((
            if content.is_empty() {
                None
            } else {
                Some(content)
            },
            if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            tool_calls,
            usage,
            interrupted,
        ))
    }

    async fn execute_tool_call(&self, call: &ProviderToolCall) {
        self.bus.publish(BusMessage::ToolCall {
            agent_type: self.agent,
            tool_name: call.name.clone(),
            arguments: call.arguments.clone(),
        });
        let out = self.tools.call(&call.name, &call.arguments).await;
        let result_value = match (&out.result, &out.error) {
            (_, Some(e)) => Value::String(e.clone()),
            (r, None) => r.clone(),
        };
        let result_text = serde_json::to_string(&result_value).unwrap_or_else(|_| "{}".into());
        self.bus.publish(BusMessage::ToolResult {
            agent_type: self.agent,
            tool_name: call.name.clone(),
            result: result_value,
            error: out.error.clone(),
        });
        for path in extract_paths(&call.name, &call.arguments, &out.result) {
            self.manifest.record(self.agent, path);
        }
        // The sub-agent's `respond` satisfies the report guard for this turn.
        if call.name == "respond" && out.error.is_none() {
            self.turn_reported.store(true, Ordering::Relaxed);
        }
        self.record(ResponseItem::function_call_output(&call.id, result_text))
            .await;
    }

    async fn maybe_compact(&self) {
        let est = self.estimated_tokens().await;
        let budget = (self.defaults.compact_threshold * 128_000.0) as usize;
        if est > budget {
            self.compact_internal().await;
        }
    }

    async fn estimated_tokens(&self) -> usize {
        let h = self.history.lock().await;
        let chars: usize = h
            .iter()
            .map(|i| i.text().map(|t| t.len()).unwrap_or(0))
            .sum();
        chars / 3
    }

    async fn compact_internal(&self) {
        self.bus.publish(BusMessage::StatusChange {
            agent_type: self.agent,
            status: AgentStatus::Thinking,
        });
        let snapshot: Vec<ResponseItem> = self.history.lock().await.clone();
        if snapshot.len() < 4 {
            return;
        }
        let transcript = transcript_text(&snapshot);
        let keep = snapshot.len().saturating_sub(2);
        let recent: Vec<ResponseItem> = snapshot[keep..].to_vec();

        let summary_prompt = format!(
            "You are summarizing the conversation so far to compact its context.\n\
             Write a concise context note preserving: the user's goal/constraints, key decisions, \
             file paths produced and their purpose, the current sub-task and what remains, and any \
             blockers. Be terse and factual.\n\n---\n{transcript}"
        );
        let req = vec![
            Message::system(
                "You are a context-compaction assistant. Reply with only the compacted summary.",
            ),
            Message::user(summary_prompt),
        ];
        let summary = match self.provider.chat(&req, &[], 0.0, 1024).await {
            Ok(r) => r.content.unwrap_or_else(|| "(no summary)".into()),
            Err(e) => {
                log::warn!("compact failed, trimming instead: {e}");
                *self.history.lock().await = recent;
                self.record(ResponseItem::Compaction {
                    encrypted_content: "[context trimmed]".into(),
                })
                .await;
                return;
            }
        };

        *self.history.lock().await = Vec::new();
        self.record(ResponseItem::Compaction {
            encrypted_content: summary,
        })
        .await;
        for item in recent {
            self.record(item).await;
        }
        self.bus.publish(BusMessage::AgentResponse {
            agent_type: self.agent,
            content: "[context compacted]".into(),
            streaming: false,
        });
    }

    pub async fn compact(&self) {
        self.compact_internal().await;
        self.set_status(AgentStatus::Idle);
    }

    pub async fn refresh_system_prompt(&self) {
        let p = self
            .prompts
            .build_system_prompt(self.agent, &self.skills, &self.workspace);
        *self.system_prompt.lock().await = p;
    }
}

/// codex `items → messages` conversion: turn `ResponseItem`s into the provider's
/// chat-message wire format. Consecutive `FunctionCall`s fold into one
/// assistant message's `tool_calls` (matching how the model emitted them).
fn items_to_messages(items: &[ResponseItem]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    let mut pending: Vec<ProviderToolCall> = Vec::new();
    let flush = |pending: &mut Vec<ProviderToolCall>, out: &mut Vec<Message>| {
        if !pending.is_empty() {
            let calls = std::mem::take(pending);
            out.push(Message {
                role: "assistant".into(),
                content: String::new(),
                tool_calls: Some(calls),
                tool_call_id: None,
                thinking: None,
            });
        }
    };
    for item in items {
        match item {
            ResponseItem::Message { role, content, .. } => {
                flush(&mut pending, &mut out);
                let text: String = content.iter().map(|c| c.text().to_string()).collect();
                out.push(Message {
                    role: role.clone(),
                    content: text,
                    tool_calls: None,
                    tool_call_id: None,
                    thinking: None,
                });
            }
            ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                pending.push(ProviderToolCall {
                    id: call_id.clone(),
                    name: name.clone(),
                    arguments: args,
                });
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                flush(&mut pending, &mut out);
                out.push(Message::tool_result(call_id, output));
            }
            ResponseItem::Reasoning { .. } => {
                // codex round-trips thinking to the API; our providers don't
                // accept a bare thinking block, so drop it from the wire view
                // (it remains in history + rollout for display/resume).
                flush(&mut pending, &mut out);
            }
            ResponseItem::Compaction { encrypted_content } => {
                // Re-feed the compaction summary to the model as a context note
                // (otherwise compact() would trim history and tell nobody).
                flush(&mut pending, &mut out);
                let text = format!(
                    "[This conversation was compacted. Summary of prior context:]\n\n{}",
                    encrypted_content
                );
                out.push(Message {
                    role: "user".into(),
                    content: text,
                    tool_calls: None,
                    tool_call_id: None,
                    thinking: None,
                });
            }
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Forward a bus message into the session Op queue (codex session input).
async fn forward_message(
    agent: AgentType,
    msg: &BusMessage,
    op_tx: &mpsc::Sender<Op>,
    cancel: &Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
) {
    let op = match msg {
        BusMessage::UserMessage {
            agent_type,
            content,
            source,
            ..
        } if *agent_type == agent => Op::UserInput {
            content: content.clone(),
            source: *source,
        },
        // Task lifecycle notices for Main are surfaced as gentle user-visible
        // notes (they don't drive a coordination turn — Main's `send_to_agent`
        // resolves directly via the Report channel).
        BusMessage::TaskUpdate {
            action,
            source_agent,
            brief,
            ..
        } if agent == AgentType::Main => Op::UserInput {
            content: format!("ℹ️ {source_agent} 任务「{brief}」状态变为 {action}。"),
            source: MessageSource::Agent(*source_agent),
        },
        _ => return,
    };
    // New input interrupts the active turn (codex semantics), then queues.
    if let Some(token) = cancel.lock().await.take() {
        token.cancel();
    }
    let _ = op_tx.send(op).await;
}

fn transcript_text(items: &[ResponseItem]) -> String {
    let mut s = String::new();
    for item in items {
        let label = match item {
            ResponseItem::Message { role, .. } => role.as_str(),
            ResponseItem::FunctionCall { name, .. } => name.as_str(),
            ResponseItem::FunctionCallOutput { .. } => "tool",
            ResponseItem::Reasoning { .. } => "reasoning",
            ResponseItem::Compaction { .. } => "compaction",
        };
        if let Some(t) = item.text() {
            s.push_str(&format!("[{label}] {t}\n"));
        }
    }
    s
}

/// Best-effort extraction of the written file path from a tool call, for
/// manifest tracking.
fn extract_paths(tool_name: &str, args: &Value, result: &Value) -> Vec<String> {
    match tool_name {
        "write_file" | "edit_file" | "delete_file" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|path| vec![path.to_string()])
            .unwrap_or_default(),
        "apply_patch" => {
            let mut out = Vec::new();
            if let Some(applied) = result.get("applied").and_then(|v| v.as_array()) {
                for item in applied {
                    if let Some(path) = item.get("path").and_then(|v| v.as_str()) {
                        out.push(path.to_string());
                    }
                }
            }
            out
        }
        "exec" => result
            .get("written_paths")
            .and_then(|v| v.as_array())
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|path| path.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

mod internal {
    use crate::types::AgentStatus;
    use std::sync::{Arc, Mutex};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_to_messages_folds_tool_calls() {
        let items = vec![
            ResponseItem::user_message("please patch a.txt"),
            ResponseItem::function_call(
                "c1",
                "apply_patch",
                "{\"patch\":\"*** Begin Patch\"}".into(),
            ),
            ResponseItem::function_call("c2", "exec", "{\"command\":\"cat a.txt\"}".into()),
            ResponseItem::function_call_output("c1", "ok"),
            ResponseItem::assistant_message("done"),
        ];
        let msgs = items_to_messages(&items);
        // system is added by build_request, not here; expect: user, assistant(tool_calls x2), tool, assistant
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].tool_calls.as_ref().unwrap().len(), 2);
        assert_eq!(msgs[2].role, "tool");
        assert_eq!(msgs[3].role, "assistant");
    }

    #[test]
    fn compaction_summary_is_refed_to_model() {
        // Regression: a Compaction item must reach the provider as a context
        // note (otherwise /compact trims history and discards the summary).
        let items = vec![
            ResponseItem::user_message("original question"),
            ResponseItem::Compaction {
                encrypted_content: "decided to use latex".into(),
            },
            ResponseItem::user_message("continue"),
        ];
        let msgs = items_to_messages(&items);
        // user, user(compaction note), user(continue)
        let compaction_msg = msgs
            .iter()
            .find(|m| m.content.contains("decided to use latex"))
            .expect("compaction summary must appear in the wire messages");
        assert_eq!(compaction_msg.role, "user");
    }
}

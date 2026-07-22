//! A single agent's event loop, modelled on codex's session/turn design.
//!
//! - **Conversation unit** is codex's `ResponseItem` (`rollout::ResponseItem`),
//!   not a raw provider message. History is `Vec<ResponseItem>`.
//! - **Prompt assembly** is codex's `instructions + items` shape: fresh
//!   instructions plus the history items, converted to the provider's wire
//!   format at call time (`items_to_messages`).
//! - **Message queue** is codex's session discipline: an `Op` channel consumed
//!   by a single processor task (one turn at a time); a new user input
//!   is queued behind the active turn; explicit interrupt requests use the
//!   cancellation token directly.
//! - **Persistence**: every produced item is appended to a project-scoped
//!   codex-format rollout file (`$AUTOREPORT_HOME/workspaces/<workspace-id>/sessions/YYYY/MM/DD/...`);
//!   on startup the latest matching rollout for the agent is resumed.

use crate::codex_thread::internal::AgentStatusLock;
use crate::history::{
    extract_paths, inject_before_last_user, items_to_messages, make_reasoning, transcript_text,
    truncate_for_history,
};
use autoreport_core::bus::Bus;
use autoreport_core::config::AgentDefaults;
use autoreport_core::exec_policy::ExecPolicyManager;
use autoreport_core::prompts::PromptLoader;
use autoreport_core::provider::LLMProvider;
use autoreport_core::provider::types::{Message, ToolCall as ProviderToolCall};
use autoreport_core::skills::SkillLoader;
use autoreport_core::taskboard::TaskBoard;
use autoreport_core::types::{AgentStatus, AgentType, BusMessage, MessageSource};
use autoreport_rollout as rollout;
use autoreport_rollout::{ResponseItem, RolloutRecorder};
use autoreport_tools::manifest::ManifestStore;
use autoreport_tools::{ToolExecutionContext, ToolRegistry};
use serde_json::Value;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, mpsc};

const RETRACT_LAST_USER_MARKER: &str = "__autoreport_retract_last_user__";

/// Maximum approximate tokens of prior user messages to retain across a
/// compaction. Mirrors codex `compact::COMPACT_USER_MESSAGE_MAX_TOKENS`
/// (`codex-rs/core/src/compact.rs`).
const COMPACT_USER_MESSAGE_MAX_TOKENS: usize = 20_000;

/// codex `compact::build_compacted_history_with_limit`: walk the prior user
/// messages newest-first, keeping each that fits in the remaining token budget
/// and truncating the middle of the boundary message to the remainder; older
/// user messages are dropped. The selected messages are restored to original
/// order, then the compaction summary is appended as the final item so it is
/// the last thing the model reads.
fn build_compacted_history(user_messages: &[ResponseItem], summary: &str) -> Vec<ResponseItem> {
    let max_tokens = COMPACT_USER_MESSAGE_MAX_TOKENS;
    let mut selected: Vec<String> = Vec::new();
    if max_tokens > 0 {
        let mut remaining = max_tokens;
        for msg in user_messages.iter().rev() {
            if remaining == 0 {
                break;
            }
            let text = msg.text().unwrap_or_default();
            let tokens = autoreport_utils_string::approx_token_count(&text);
            if tokens <= remaining {
                selected.push(text);
                remaining = remaining.saturating_sub(tokens);
            } else {
                // codex `truncate_text(_, TruncationPolicy::Tokens(remaining))`
                // == `truncate_middle_with_token_budget(_, remaining).0`.
                let (truncated, _) =
                    autoreport_utils_string::truncate_middle_with_token_budget(&text, remaining);
                selected.push(truncated);
                break;
            }
        }
        selected.reverse();
    }

    let mut history: Vec<ResponseItem> = Vec::with_capacity(selected.len() + 1);
    for text in &selected {
        history.push(ResponseItem::user_message(text));
    }
    // The project renders `Compaction` as a trailing `role:"user"` context note
    // in `items_to_messages` (history.rs), matching codex's summary-as-last-
    // user-message ordering.
    history.push(ResponseItem::Compaction {
        encrypted_content: summary.to_string(),
    });
    history
}

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
    /// Project-scoped state directory used for Codex-compatible conversation
    /// persistence, history JSONL, and session discovery.
    project_home: PathBuf,
    tools: ToolRegistry,
    provider: Arc<dyn LLMProvider>,
    prompts: PromptLoader,
    skills: SkillLoader,
    manifest: ManifestStore,
    bus: Bus,
    task_board: TaskBoard,
    defaults: AgentDefaults,
    exec_policy: Arc<ExecPolicyManager>,
    /// codex-style history of conversation items.
    history: Arc<Mutex<Vec<ResponseItem>>>,
    status: AgentStatusLock,
    op_tx: mpsc::Sender<Op>,
    op_rx: Arc<Mutex<mpsc::Receiver<Op>>>,
    /// Cancellation token for the currently-running turn (codex interrupt).
    current_turn: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// Turn-local steer input, adapted from Codex's `InputQueue::Steer`.
    /// Ordinary TUI follow-ups remain in the TUI queue; this path is for
    /// callers that explicitly target the active turn.
    pending_steers: Arc<Mutex<VecDeque<(String, MessageSource)>>>,
    /// Pre-tool Ctrl-C intent. Separate from the cancellation token because
    /// the processor may observe cancellation after recording the user item.
    retract_requested: Arc<Mutex<bool>>,
    /// Set when this agent calls `respond` during the current turn — the
    /// sub-report guard requires it before a Main-dispatched turn may end.
    turn_reported: Arc<AtomicBool>,
    /// Append-only rollout recorder (`None` until the first item is written).
    recorder: Arc<Mutex<Option<RolloutRecorder>>>,
    /// Logical conversation id (recorded in the rollout meta payload);
    /// `/clear` rolls a new one (new rollout file).
    conversation_id: Arc<Mutex<String>>,
    /// Bare UUID identifying the rollout file on disk (codex layout:
    /// `rollout-<ts>-<uuid>.jsonl`). Stable per session so resume can find it.
    session_uuid: Arc<Mutex<String>>,
}

impl AgentLoop {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent: AgentType,
        workspace: PathBuf,
        project_home: PathBuf,
        tools: ToolRegistry,
        provider: Arc<dyn LLMProvider>,
        prompts: PromptLoader,
        skills: SkillLoader,
        manifest: ManifestStore,
        bus: Bus,
        task_board: TaskBoard,
        defaults: AgentDefaults,
        exec_policy: Arc<ExecPolicyManager>,
    ) -> Self {
        let (op_tx, op_rx) = mpsc::channel(64);
        let session_uuid = uuid::Uuid::new_v4().to_string();
        let conversation_id = format!("{}-{}", agent.as_str(), session_uuid);
        Self {
            agent,
            workspace,
            project_home,
            tools,
            provider,
            prompts,
            skills,
            manifest,
            bus,
            task_board,
            defaults,
            exec_policy,
            history: Arc::new(Mutex::new(Vec::new())),
            status: AgentStatusLock::new(AgentStatus::Idle),
            op_tx,
            op_rx: Arc::new(Mutex::new(op_rx)),
            current_turn: Arc::new(Mutex::new(None)),
            pending_steers: Arc::new(Mutex::new(VecDeque::new())),
            retract_requested: Arc::new(Mutex::new(false)),
            turn_reported: Arc::new(AtomicBool::new(false)),
            recorder: Arc::new(Mutex::new(None)),
            conversation_id: Arc::new(Mutex::new(conversation_id)),
            session_uuid: Arc::new(Mutex::new(session_uuid)),
        }
    }

    /// Spawn the bus listener and the single-threaded session processor, and
    /// resume the most recent rollout for this agent if one exists.
    pub async fn start(self: Arc<Self>) {
        // Complete discovery before the UI is built. This mirrors Codex's
        // InitialHistory contract: callers see either New or Resumed history,
        // never an empty transcript that is populated later by a race.
        self.resume_from_rollout().await;
        self.spawn_listener();
        self.clone().spawn_processor();
    }

    async fn resume_from_rollout(&self) {
        let cwd = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());
        let Some((path, meta)) =
            rollout::latest_for_agent(&self.project_home, self.agent.as_str(), &cwd)
        else {
            return;
        };
        match rollout::read(&path) {
            Ok(entries) => {
                let items = normalize_retracted_items(rollout::items(&entries));
                if !items.is_empty() {
                    log::info!(
                        "{}: resumed {} items from {}",
                        self.agent,
                        items.len(),
                        path.display()
                    );
                    *self.history.lock().await = items;
                    *self.session_uuid.lock().await = meta.session_id.clone();
                    *self.conversation_id.lock().await = meta.conversation_id.clone();
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
        tokio::spawn(async move {
            let mut rx = bus.subscribe();
            loop {
                match rx.recv().await {
                    Ok(msg) => forward_message(agent, &msg, &submit).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }

    fn spawn_processor(self: Arc<Self>) {
        tokio::spawn(async move {
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
                            self.clear_turn_token().await;
                            if self.take_retract_request().await {
                                self.remove_last_user_and_record_retraction().await;
                            }
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

    /// Address a user message to this agent. The single session processor
    /// naturally queues it behind an active turn; explicit interrupt is a
    /// separate operation, matching Codex's input queue boundary.
    pub fn submit(&self, content: String, source: MessageSource) {
        let op_tx = self.op_tx.clone();
        tokio::spawn(async move {
            let _ = op_tx.send(Op::UserInput { content, source }).await;
        });
    }

    /// Interrupt the currently-running turn.
    pub async fn interrupt(&self) {
        self.cancel_current_turn().await;
        self.pending_steers.lock().await.clear();
    }

    pub async fn interrupt_and_retract(&self) {
        // Follow-ups are kept in the TUI queue, so never leave a retract
        // request behind that could consume a later unrelated runtime op.
        if self.current_turn.lock().await.is_some() {
            *self.retract_requested.lock().await = true;
            self.cancel_current_turn().await;
        }
        self.pending_steers.lock().await.clear();
    }

    /// Inject additional input into the active regular turn. This is the
    /// local equivalent of Codex `Session::steer_input`: it is accepted only
    /// while a turn is active and is consumed before the next Responses/
    /// Messages request. The provider stream itself is not interrupted.
    pub async fn steer_input(&self, content: String, source: MessageSource) -> Result<(), String> {
        if content.trim().is_empty() {
            return Err("steer input cannot be empty".into());
        }
        if self.current_turn.lock().await.is_none() {
            return Err("no active turn".into());
        }
        self.pending_steers
            .lock()
            .await
            .push_back((content, source));
        Ok(())
    }

    async fn cancel_current_turn(&self) {
        if let Some(token) = self.current_turn.lock().await.as_ref() {
            token.cancel();
        }
    }

    async fn clear_turn_token(&self) {
        self.current_turn.lock().await.take();
    }

    /// Wait until the processor has observed cancellation and cleared the
    /// turn token. Context mutations must not race an in-flight turn: a late
    /// response/tool result would otherwise be appended to the newly-cleared
    /// conversation.
    async fn wait_until_idle(&self) -> bool {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if !self.is_busy().await {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    pub async fn is_busy(&self) -> bool {
        self.current_turn.lock().await.is_some()
    }

    pub async fn flush_rollout(&self) {
        if let Some(recorder) = self.recorder.lock().await.as_ref()
            && let Err(e) = recorder.flush().await
        {
            log::warn!("{}: flushing rollout failed: {e}", self.agent);
        }
    }

    /// Clear context and start a fresh conversation (new rollout file), keeping
    /// the agent running.
    pub async fn clear_context(&self) {
        self.cancel_current_turn().await;
        if !self.wait_until_idle().await {
            log::warn!(
                "{}: clear context timed out while turn is still active",
                self.agent
            );
            return;
        }
        self.history.lock().await.clear();
        if let Some(recorder) = self.recorder.lock().await.as_ref() {
            if let Err(e) = recorder.flush().await {
                log::warn!("{}: flushing rollout before clear failed: {e}", self.agent);
            }
        }
        *self.recorder.lock().await = None;
        let new_uuid = uuid::Uuid::new_v4().to_string();
        *self.session_uuid.lock().await = new_uuid.clone();
        *self.conversation_id.lock().await = format!("{}-{}", self.agent.as_str(), new_uuid);
        self.bus.publish(BusMessage::StatusChange {
            agent_type: self.agent,
            status: AgentStatus::Idle,
        });
    }

    pub async fn status(&self) -> AgentStatus {
        self.status.get()
    }

    /// Return a stable snapshot for transcript consumers such as the TUI.
    pub async fn history_snapshot(&self) -> Vec<ResponseItem> {
        self.history.lock().await.clone()
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
        if let ResponseItem::Message { role, content, .. } = &item
            && role == "user"
        {
            let text = content
                .iter()
                .map(|part| part.text())
                .collect::<Vec<_>>()
                .join("");
            let sid = self.session_uuid.lock().await.clone();
            if let Err(e) = rollout::history::append(&self.project_home, &sid, &text).await {
                log::warn!("history append: {e}");
            }
        }
        self.history.lock().await.push(item);
    }

    async fn append_to_rollout(&self, item: &ResponseItem) -> anyhow::Result<()> {
        // Lazily create the rollout file on first append, then hand the encoded
        // line to the recorder's dedicated writer task over an mpsc channel.
        // `append` is a non-blocking channel send, so this hot-path call never
        // stalls the async agent-loop worker on file I/O (codex
        // `rollout::recorder` writer-task design).
        let mut rec = self.recorder.lock().await;
        if rec.is_none() {
            let cid = self.conversation_id.lock().await.clone();
            let sid = self.session_uuid.lock().await.clone();
            let ts = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();
            *rec = Some(RolloutRecorder::create(
                &self.project_home,
                &cid,
                &sid,
                &ts,
                &self.workspace,
                self.agent.as_str(),
            )?);
        }
        rec.as_ref().unwrap().append(item)
    }

    async fn process_turn(&self, content: &str, source: MessageSource) -> anyhow::Result<()> {
        if self.take_retract_request().await {
            return Ok(());
        }
        // Install the cancellation handle before publishing Thinking. The
        // UI/test may interrupt immediately after observing that status.
        let turn_token = tokio_util::sync::CancellationToken::new();
        *self.current_turn.lock().await = Some(turn_token.clone());
        self.turn_reported.store(false, Ordering::Relaxed);

        self.set_status(AgentStatus::Thinking);
        self.record(ResponseItem::user_message(content)).await;

        // Progressive disclosure: if the user named a skill (`$skill-name`),
        // inject its SKILL.md body for this turn so the model has the full
        // instructions without `cat`-ing the file (codex skill injection).
        let skill_injection = self
            .skills
            .render_injections(&autoreport_core::skills::extract_skill_mentions(content));

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

            let interrupted = self
                .run_completion_cycle(&turn_token, &skill_injection)
                .await?;
            if interrupted {
                if self.take_retract_request().await {
                    self.remove_last_user_and_record_retraction().await;
                    break;
                }
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

            // A steer can arrive in the small boundary window after the
            // provider returned its final item. Keep the turn open and feed it
            // through the next completion cycle instead of stranding it for a
            // later turn.
            if self.has_pending_steers().await {
                continue;
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

        // A non-streaming AgentResponse is the turn boundary consumed by the
        // TUI (and by callers waiting for a completed answer).  Emit it only
        // after the whole completion cycle, including every tool call and
        // guard retry, has finished.  Emitting this marker from
        // `stream_completion` would make a tool-using turn look complete
        // between the model response and the tool result, allowing queued
        // input to overtake the active turn.
        if self.take_retract_request().await {
            self.remove_last_user_and_record_retraction().await;
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

        self.current_turn.lock().await.take();
        self.status.set(AgentStatus::Idle);
        self.bus.publish(BusMessage::StatusChange {
            agent_type: self.agent,
            status: AgentStatus::Idle,
        });
        Ok(())
    }

    async fn take_retract_request(&self) -> bool {
        let mut requested = self.retract_requested.lock().await;
        std::mem::take(&mut *requested)
    }

    async fn remove_last_user_and_record_retraction(&self) {
        let removed = {
            let mut history = self.history.lock().await;
            history
                .iter()
                .rposition(|item| {
                    matches!(
                        item,
                        ResponseItem::Message { role, .. } if role == "user"
                    )
                })
                .map(|index| history.remove(index))
        };
        if removed.is_some() {
            // Rollouts are append-only. The marker lets resume normalization
            // remove the optimistic user item without rewriting a live file.
            let marker = ResponseItem::Compaction {
                encrypted_content: RETRACT_LAST_USER_MARKER.to_string(),
            };
            if let Err(error) = self.append_to_rollout(&marker).await {
                log::warn!("{}: recording retraction: {error}", self.agent);
            }
        }
    }

    /// One completion cycle: stream a response, run any tool calls, repeat
    /// until the model stops emitting tools or the turn is interrupted.
    /// Returns `Ok(true)` if interrupted by the cancellation token; `Err` if the
    /// provider call failed (propagated so the user sees it via the bus).
    async fn run_completion_cycle(
        &self,
        turn_token: &tokio_util::sync::CancellationToken,
        skill_injection: &str,
    ) -> anyhow::Result<bool> {
        let mut iterations: u32 = 0;
        // A few OpenAI-compatible gateways can terminate a reasoning turn
        // before emitting the visible assistant message. Give the model one
        // Codex-style continuation opportunity; reasoning remains protocol
        // state and is never inserted into the visible transcript.
        let mut reasoning_only_retries: u8 = 0;
        loop {
            self.maybe_compact().await;
            if turn_token.is_cancelled() {
                return Ok(true);
            }

            self.record_pending_steers().await;

            let messages = self.build_request(skill_injection).await;
            let defs = self.tools.definitions();
            let (content, reasoning, reasoning_sig, tool_calls, _usage, intr) =
                match self.stream_completion(&messages, &defs, turn_token).await {
                    Ok(v) => v,
                    Err(e) => {
                        // Surface the failure instead of silently treating it
                        // as an empty completed turn — the user must learn that
                        // the model call failed (auth/rate-limit/network).
                        log::warn!("{} stream_completion: {e}", self.agent);
                        return Err(e);
                    }
                };

            log::debug!(
                "{} provider turn result: content_chars={}, reasoning_chars={}, tool_calls={}, interrupted={}",
                self.agent,
                content.as_deref().map_or(0, str::len),
                reasoning.as_deref().map_or(0, str::len),
                tool_calls.len(),
                intr,
            );
            if !intr && content.is_none() && tool_calls.is_empty() && reasoning.is_some() {
                if reasoning_only_retries == 0 {
                    reasoning_only_retries = 1;
                    self.record(ResponseItem::user_message(
                        "Continue with the final answer now. Do not stop at internal reasoning; provide the visible response or call the appropriate tool.",
                    ))
                    .await;
                    continue;
                }
                self.bus.publish(BusMessage::SystemNotice {
                    agent_type: Some(self.agent),
                    content: "provider returned reasoning but no final text".into(),
                });
                log::warn!(
                    "{} provider returned reasoning without assistant text or tool calls",
                    self.agent
                );
            }

            if intr {
                if let Some(r) = reasoning {
                    if !r.is_empty() {
                        self.record(make_reasoning(r, reasoning_sig)).await;
                    }
                }
                if let Some(c) = content {
                    if !c.is_empty() {
                        self.record(ResponseItem::assistant_message(c)).await;
                    }
                }
                return Ok(true);
            }

            if let Some(text) = reasoning {
                if !text.is_empty() {
                    self.record(make_reasoning(text, reasoning_sig)).await;
                }
            }
            if let Some(text) = content {
                if !text.is_empty() {
                    self.record(ResponseItem::assistant_message(text)).await;
                }
            }
            if tool_calls.is_empty() {
                if self.has_pending_steers().await {
                    continue;
                }
                return Ok(false);
            }
            for call in &tool_calls {
                let args_json = serde_json::to_string(&call.arguments).unwrap_or_default();
                self.record(ResponseItem::function_call(&call.id, &call.name, args_json))
                    .await;
            }

            self.set_status(AgentStatus::RunningTool);
            if turn_token.is_cancelled() {
                // None should run. Record synthetic outputs for every call so
                // the next request doesn't carry a FunctionCall without a
                // matching FunctionCallOutput (both Anthropic and OpenAI reject
                // that with a 400). Mirrors codex's
                // `normalize::ensure_call_outputs_present`.
                for call in &tool_calls {
                    self.record(ResponseItem::function_call_output(
                        &call.id,
                        "[aborted: turn interrupted before this tool ran]",
                    ))
                    .await;
                }
                return Ok(true);
            }
            // File edits, manifests, approvals, and task-board tools all have
            // externally visible side effects. Run the batch in model order;
            // this matches codex's default for tools that do not explicitly
            // advertise parallel safety and avoids races between calls such as
            // apply_patch followed by exec/read.
            for call in &tool_calls {
                self.execute_tool_call(call, &turn_token).await;
            }
            self.set_status(AgentStatus::Thinking);

            iterations += 1;
            if iterations >= self.defaults.max_tool_iterations {
                self.record(ResponseItem::user_message(
                    "You have reached the maximum number of tool iterations for this turn. \
                     Stop calling tools and summarize what you have so far.",
                ))
                .await;
                // HARD STOP: end the cycle so the turn terminates regardless of
                // what the model emits next. Without this, a model that keeps
                // emitting tool calls loops forever — re-appending this same
                // warning each iteration, growing history/rollout/cost without
                // bound (the compacter cannot keep up). The warning above is
                // recorded so it is visible on resume and as an audit marker.
                return Ok(false);
            }
        }
    }

    async fn has_pending_steers(&self) -> bool {
        !self.pending_steers.lock().await.is_empty()
    }

    async fn record_pending_steers(&self) {
        let pending = self
            .pending_steers
            .lock()
            .await
            .drain(..)
            .collect::<Vec<_>>();
        for (content, _source) in pending {
            self.record(ResponseItem::user_message(content)).await;
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
            let active: Vec<autoreport_core::types::TaskItem> = self
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
    /// converted to provider messages. When the user mentioned one or more
    /// skills this turn (`$skill-name`), the bodies are injected as a user
    /// message immediately before the last user message (codex
    /// `BeforeLastUserMessage` placement) so the model sees the full SKILL.md
    /// context adjacent to the request that triggered it.
    async fn build_request(&self, skill_injection: &str) -> Vec<Message> {
        let instructions = self.prompts.build_system_prompt_with_environment(
            self.agent,
            &self.skills,
            &self.workspace,
        );
        let items = self.history.lock().await.clone();
        let mut out = Vec::with_capacity(items.len() + 2);
        out.push(Message::system(instructions));
        // Per-turn `developer` fragment: current time (codex
        // `current_time_reminder`). Kept out of the static system base so the
        // system prompt stays byte-stable across turns (prefix-caching ready).
        out.push(Message::developer(
            autoreport_core::prompts::current_time_reminder(),
        ));
        out.extend(items_to_messages(&items));
        if !skill_injection.is_empty() {
            inject_before_last_user(&mut out, Message::user(skill_injection.to_string()));
        }
        out
    }

    /// Stream one completion, selecting on the turn's cancellation token.
    async fn stream_completion(
        &self,
        messages: &[Message],
        defs: &[autoreport_core::provider::types::ToolDef],
        cancel: &tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<(
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<ProviderToolCall>,
        Option<autoreport_core::provider::types::Usage>,
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
        let mut reasoning_signature: Option<String> = None;
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
                    if let Some(sig) = chunk.thinking_signature {
                        reasoning_signature = Some(sig);
                    }
                    if chunk.done { break; }
                }
            }
        }
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
            reasoning_signature,
            tool_calls,
            usage,
            interrupted,
        ))
    }

    async fn execute_tool_call(
        &self,
        call: &ProviderToolCall,
        turn_token: &tokio_util::sync::CancellationToken,
    ) {
        // Execpolicy determines whether this exact command is allowed,
        // forbidden, or needs an approval. The model-visible escalation flag
        // is only a request: `ToolExecutionContext` below is the unforgeable
        // runtime authority granted after the user approves it.
        use autoreport_core::exec_policy::ExecApprovalRequirement;
        use autoreport_core::policy::{ReviewDecision, summarize_command};
        let mut execution_context = ToolExecutionContext::default();
        if call.name == "exec" {
            let command = call
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let requests_escalation = call
                .arguments
                .get("sandbox_permissions")
                .and_then(Value::as_str)
                .is_some_and(|value| value == "require_escalated");
            let requirement = self.exec_policy.evaluate(
                &command,
                self.defaults.approval_policy,
                requests_escalation,
            );
            match requirement {
                ExecApprovalRequirement::Forbidden { reason } => {
                    self.publish_tool_denial(call, reason).await;
                    return;
                }
                ExecApprovalRequirement::Skip {
                    allow_escalated_exec,
                } => {
                    execution_context.allow_escalated_exec = allow_escalated_exec;
                }
                ExecApprovalRequirement::NeedsApproval { reason } => {
                    let argv: Vec<String> =
                        command.split_whitespace().map(str::to_string).collect();
                    let summary = summarize_command(&argv);
                    let payload = autoreport_core::types::ApprovalRequestPayload {
                        agent_type: self.agent,
                        call_id: call.id.clone(),
                        command: command.clone(),
                        cwd: None,
                        summary,
                        reason,
                    };
                    // Register first so the payload is the non-lossy source of
                    // truth even if the broadcast below lags or has no receiver
                    // yet; then publish for instant TUI delivery.
                    let rx = self.bus.register_approval(payload.clone()).await;
                    self.bus
                        .publish(BusMessage::ApprovalRequest { payload });
                    match tokio::select! {
                        biased;
                        _ = turn_token.cancelled() => Err(()),
                        decision = rx => decision.map_err(|_| ()),
                    } {
                        Ok(ReviewDecision::Denied) => {
                            self.publish_tool_denial(
                                call,
                                "command denied by user; try a different approach".to_string(),
                            )
                            .await;
                            return;
                        }
                        Ok(ReviewDecision::ApprovedForSession) => {
                            self.exec_policy.approve_for_session(&command);
                            execution_context.allow_escalated_exec = requests_escalation;
                        }
                        Ok(ReviewDecision::ApprovedAndPersisted) => {
                            // A compound command may be safe to run once but
                            // too broad to persist as a prefix rule. Preserve
                            // the user's explicit one-shot approval and only
                            // downgrade persistence, rather than denying the
                            // already-approved command.
                            if let Err(err) = self.exec_policy.persist_allow_prefix(&command) {
                                self.bus.publish(BusMessage::SystemNotice {
                                    agent_type: Some(self.agent),
                                    content: format!("execpolicy rule not persisted: {err}"),
                                });
                            }
                            execution_context.allow_escalated_exec = requests_escalation;
                        }
                        Ok(ReviewDecision::Approved) => {
                            execution_context.allow_escalated_exec = requests_escalation;
                        }
                        // Resolved without a decision (e.g. process tearing down):
                        // treat as denied so we never run an unapproved command.
                        Err(_) => {
                            self.publish_tool_denial(
                                call,
                                "approval request cancelled".to_string(),
                            )
                            .await;
                            return;
                        }
                    }
                }
            }
        }
        self.bus.publish(BusMessage::ToolCall {
            agent_type: self.agent,
            tool_name: call.name.clone(),
            arguments: call.arguments.clone(),
            call_id: call.id.clone(),
        });
        let out = tokio::select! {
            biased;
            _ = turn_token.cancelled() => {
                self.publish_tool_denial(call, "[aborted: turn interrupted before this tool completed]".into()).await;
                return;
            }
            out = self.tools.call_with_context(&call.name, &call.arguments, execution_context) => out,
        };
        let result_value = match (&out.result, &out.error) {
            (_, Some(e)) => Value::String(e.clone()),
            (r, None) => r.clone(),
        };
        let result_text = truncate_for_history(
            serde_json::to_string(&result_value).unwrap_or_else(|_| "{}".into()),
        );
        self.bus.publish(BusMessage::ToolResult {
            agent_type: self.agent,
            tool_name: call.name.clone(),
            result: result_value,
            error: out.error.clone(),
            call_id: call.id.clone(),
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

    async fn publish_tool_denial(&self, call: &ProviderToolCall, denial: String) {
        self.bus.publish(BusMessage::ToolResult {
            agent_type: self.agent,
            tool_name: call.name.clone(),
            result: Value::String(denial.clone()),
            error: Some(denial.clone()),
            call_id: call.id.clone(),
        });
        self.record(ResponseItem::function_call_output(&call.id, denial))
            .await;
    }

    async fn maybe_compact(&self) {
        let est = self.estimated_tokens().await;
        let budget =
            (self.defaults.compact_threshold * self.defaults.context_window as f32) as usize;
        if est > budget {
            self.compact_internal().await;
        }
    }

    async fn estimated_tokens(&self) -> usize {
        // Use the vendored codex token estimate (`bytes / APPROX_BYTES_PER_TOKEN`,
        // matching codex's `approx_token_count`) rather than a `chars / 3` rule
        // of thumb, so the compaction threshold tracks the same budget codex
        // uses to decide when to compact.
        let h = self.history.lock().await;
        let bytes: usize = h
            .iter()
            .map(|i| i.text().map(|t| t.len()).unwrap_or(0))
            .sum();
        autoreport_utils_string::approx_tokens_from_byte_count(bytes) as usize
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

        // codex `compact::collect_user_messages`: only prior user messages
        // survive compaction. No `FunctionCall`/`FunctionCallOutput`/assistant
        // turns are retained — a partial tail can orphan a tool result (dropping
        // its matching `FunctionCall`) and emit a `role:"tool"` message with no
        // preceding tool_use, which providers reject with HTTP 400. The summary
        // captures the working context instead.
        let user_msgs: Vec<ResponseItem> = snapshot
            .iter()
            .filter(|i| matches!(i, ResponseItem::Message { role, .. } if role.as_str() == "user"))
            .cloned()
            .collect();

        let summary = self.compact_summary(&snapshot).await;

        // codex `compact::build_compacted_history`: keep the most recent user
        // messages that fit in the budget (truncating the boundary message),
        // then append the compaction summary as the final item.
        let new_history = build_compacted_history(&user_msgs, &summary);

        *self.history.lock().await = Vec::new();
        for item in new_history {
            self.record(item).await;
        }
        // Compaction is an internal history transition, not the end of the
        // active turn.  A final AgentResponse marker here would make the TUI
        // drain queued input while the same turn is still about to call the
        // provider again.  Keep the notice visible without crossing the turn
        // boundary.
        self.bus.publish(BusMessage::SystemNotice {
            agent_type: Some(self.agent),
            content: "context compacted".into(),
        });
    }

    /// Summarize the conversation for compaction. On provider failure, drop the
    /// oldest items and retry (codex `compact.rs:284-298`) instead of throwing
    /// away the whole history; give up with a trimmed marker only when almost
    /// nothing remains.
    async fn compact_summary(&self, items: &[ResponseItem]) -> String {
        const SYS: &str =
            "You are a context-compaction assistant. Reply with only the compacted summary.";
        const PREFIX: &str = "You are summarizing the conversation so far to compact its context.\n\
             Write a concise context note preserving: the user's goal/constraints, key decisions, \
             file paths produced and their purpose, the current sub-task and what remains, and any \
             blockers. Be terse and factual.\n\n---\n";
        let mut cur: Vec<ResponseItem> = items.to_vec();
        loop {
            let transcript = transcript_text(&cur);
            match self
                .provider
                .chat(
                    &[
                        Message::system(SYS),
                        Message::user(format!("{PREFIX}{transcript}")),
                    ],
                    &[],
                    0.0,
                    1024,
                )
                .await
            {
                Ok(r) => return r.content.unwrap_or_else(|| "(no summary)".into()),
                Err(e) => {
                    log::warn!("compact summarizer failed ({} items): {e}", cur.len());
                    if cur.len() <= 4 {
                        return "[context trimmed: summarizer unavailable]".into();
                    }
                    let drop_n = (cur.len() / 4).max(1);
                    cur.drain(0..drop_n);
                }
            }
        }
    }

    pub async fn compact(&self) {
        self.cancel_current_turn().await;
        if !self.wait_until_idle().await {
            log::warn!(
                "{}: compact timed out while turn is still active",
                self.agent
            );
            return;
        }
        self.compact_internal().await;
        self.set_status(AgentStatus::Idle);
    }
}

fn normalize_retracted_items(items: Vec<ResponseItem>) -> Vec<ResponseItem> {
    let mut normalized = Vec::with_capacity(items.len());
    for item in items {
        if matches!(
            &item,
            ResponseItem::Compaction { encrypted_content }
                if encrypted_content == RETRACT_LAST_USER_MARKER
        ) {
            if let Some(index) = normalized.iter().rposition(
                |item| matches!(item, ResponseItem::Message { role, .. } if role == "user"),
            ) {
                normalized.remove(index);
            }
        } else {
            normalized.push(item);
        }
    }
    normalized
}

async fn forward_message(agent: AgentType, msg: &BusMessage, op_tx: &mpsc::Sender<Op>) {
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
        _ => return,
    };
    // The processor receives one operation at a time, so a message received
    // during a turn remains queued until the current turn reaches its normal
    // boundary. Interrupt remains an explicit user action.
    let _ = op_tx.send(op).await;
}

mod internal {
    use autoreport_core::types::AgentStatus;
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
            *self.inner.lock().unwrap_or_else(|e| e.into_inner())
        }
        pub fn set(&self, s: AgentStatus) {
            *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_compacted_history_appends_summary_last_and_preserves_recent_user_msgs() {
        let user_msgs = vec![
            ResponseItem::user_message("first task brief"),
            ResponseItem::user_message("second user message"),
            ResponseItem::user_message("latest user message"),
        ];
        let out = build_compacted_history(&user_msgs, "the summary");
        // All fit within the 20k budget → all retained in original order, then
        // the compaction summary appended LAST (codex ordering).
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].text().unwrap(), "first task brief");
        assert_eq!(out[1].text().unwrap(), "second user message");
        assert_eq!(out[2].text().unwrap(), "latest user message");
        assert!(matches!(
            out.last(),
            Some(ResponseItem::Compaction { encrypted_content })
                if encrypted_content == "the summary"
        ));
    }

    #[test]
    fn build_compacted_history_drops_oldest_when_budget_exceeded() {
        // Each char is ~1/4 token, so make messages large enough to exceed the
        // 20k-token budget and force truncation of the newest + drop of older.
        let big = "x".repeat(80_000); // ~20_000 tokens each
        let user_msgs = vec![
            ResponseItem::user_message(big.clone()), // oldest -> dropped
            ResponseItem::user_message(big.clone()), // middle -> dropped
            ResponseItem::user_message("small recent".to_string()), // newest -> kept
        ];
        let out = build_compacted_history(&user_msgs, "sum");
        // Summary is always last.
        assert!(matches!(out.last(), Some(ResponseItem::Compaction { .. })));
        // The small recent message survives; the two oversized ones do not fit
        // alongside it (budget exhausted by the newest large message gets
        // truncated, leaving no room for older ones).
        let texts: Vec<_> = out
            .iter()
            .filter_map(|i| match i {
                ResponseItem::Message { role, .. } if role == "user" => i.text(),
                _ => None,
            })
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("small recent")),
            "newest small message must survive: {texts:?}"
        );
    }

    #[test]
    fn retraction_tombstone_removes_only_the_latest_user_item() {
        let items = normalize_retracted_items(vec![
            ResponseItem::user_message("keep"),
            ResponseItem::assistant_message("answer"),
            ResponseItem::user_message("cancel me"),
            ResponseItem::Compaction {
                encrypted_content: RETRACT_LAST_USER_MARKER.into(),
            },
        ]);
        assert_eq!(items.len(), 2);
        assert!(matches!(
            &items[0],
            ResponseItem::Message { role, .. } if role == "user"
        ));
        assert!(matches!(
            &items[1],
            ResponseItem::Message { role, .. } if role == "assistant"
        ));
    }

    #[test]
    fn retraction_marker_is_not_sent_as_compaction_context() {
        let messages = items_to_messages(&[
            ResponseItem::user_message("keep"),
            ResponseItem::Compaction {
                encrypted_content: RETRACT_LAST_USER_MARKER.into(),
            },
        ]);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "keep");
    }

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
    fn items_to_messages_injects_output_for_orphan_tool_call() {
        // Regression: an interrupted turn can leave a FunctionCall with no
        // matching output. The wire view must synthesize an "[aborted]" tool
        // result so the next request isn't rejected with a 400.
        let items = vec![
            ResponseItem::user_message("do a thing"),
            ResponseItem::function_call("c1", "exec", "{\"command\":\"sleep 99\"}".into()),
            // c1 has NO FunctionCallOutput — simulates a mid-loop interrupt.
        ];
        let msgs = items_to_messages(&items);
        let assistant = msgs
            .iter()
            .find(|m| m.role == "assistant" && m.tool_calls.is_some())
            .expect("assistant tool_call message present");
        let call_id = assistant.tool_calls.as_ref().unwrap()[0].id.clone();
        let answered = msgs
            .iter()
            .filter(|m| m.role == "tool")
            .any(|m| m.tool_call_id.as_deref() == Some(call_id.as_str()));
        assert!(
            answered,
            "orphan tool_call must get a synthetic tool result"
        );
    }

    #[test]
    fn items_to_messages_pairs_reasoning_with_assistant_message() {
        // Regression: a signed Reasoning item preceding an assistant message
        // must round-trip as the assistant message's thinking + signature, so
        // Anthropic extended thinking continues across turns.
        let items = vec![
            ResponseItem::user_message("think then answer"),
            ResponseItem::reasoning_signed("pondering", "SIG-1234"),
            ResponseItem::assistant_message("the answer is 42"),
        ];
        let msgs = items_to_messages(&items);
        let assistant = msgs
            .iter()
            .find(|m| m.role == "assistant" && m.thinking.is_some())
            .expect("assistant message should carry the paired thinking");
        assert_eq!(assistant.thinking.as_deref(), Some("pondering"));
        assert_eq!(assistant.thinking_signature.as_deref(), Some("SIG-1234"));
    }

    #[test]
    fn items_to_messages_preserves_empty_signed_reasoning() {
        let items = vec![
            ResponseItem::user_message("continue"),
            ResponseItem::reasoning_signed("", "SIG-EMPTY-SUMMARY"),
            ResponseItem::assistant_message("the answer"),
        ];
        let msgs = items_to_messages(&items);
        let assistant = msgs
            .iter()
            .find(|m| m.role == "assistant" && m.thinking_signature.is_some())
            .expect("assistant message should carry the encrypted reasoning");
        assert_eq!(assistant.thinking.as_deref(), Some(""));
        assert_eq!(
            assistant.thinking_signature.as_deref(),
            Some("SIG-EMPTY-SUMMARY")
        );
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

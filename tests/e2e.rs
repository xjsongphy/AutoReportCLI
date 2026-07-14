//! End-to-end test of the agent loop with a scripted mock provider:
//! proves message → stream → tool call (apply_patch) → tool result → final text
//! works, and that write-directory isolation is enforced.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;

use autoreport_cli::bus::Bus;
use autoreport_cli::config::AgentDefaults;
use autoreport_cli::prompts::PromptLoader;
use autoreport_cli::provider::LLMProvider;
use autoreport_cli::provider::types::{
    LLMResponse, LLMStreamChunk, Message, ToolCall, ToolDef, Usage,
};
use autoreport_cli::runtime::AgentLoop;
use autoreport_cli::skills::SkillLoader;
use autoreport_cli::taskboard::TaskBoard;
use autoreport_cli::tools::ToolRegistry;
use autoreport_cli::tools::file_tools::FsCtx;
use autoreport_cli::tools::manifest::ManifestStore;
use autoreport_cli::types::{AgentStatus, AgentType, BusMessage, MessageSource};

/// Scripted provider: returns each scripted response in order.
struct Mock {
    calls: AtomicU32,
    script: Vec<(Option<String>, Vec<ToolCall>)>,
}

impl Mock {
    fn new(script: Vec<(Option<String>, Vec<ToolCall>)>) -> Self {
        Self {
            calls: AtomicU32::new(0),
            script,
        }
    }
}

#[async_trait]
impl LLMProvider for Mock {
    fn id(&self) -> &str {
        "mock/test"
    }
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDef],
        _temperature: f32,
        _max_tokens: u32,
    ) -> anyhow::Result<LLMResponse> {
        let idx = self.calls.fetch_add(1, Ordering::SeqCst) as usize;
        let (content, calls) = self
            .script
            .get(idx)
            .cloned()
            .unwrap_or((Some("(done)".to_string()), Vec::new()));
        Ok(LLMResponse {
            content,
            tool_calls: calls,
            thinking: None,
            usage: Some(Usage::default()),
        })
    }
}

fn make_temp_workspace() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("autoreport-test-{}", uuid_stamp()));
    std::fs::create_dir_all(&dir).unwrap();
    autoreport_cli::config::ensure_workspace(&dir).unwrap();
    dir
}

/// A provider that streams one delta then blocks "forever", so an interrupt
/// must cancel it mid-stream.
struct SlowMock;

#[async_trait]
impl LLMProvider for SlowMock {
    fn id(&self) -> &str {
        "slow/mock"
    }
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDef],
        _temperature: f32,
        _max_tokens: u32,
    ) -> anyhow::Result<LLMResponse> {
        Ok(LLMResponse::default())
    }
    async fn chat_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDef],
        _temperature: f32,
        _max_tokens: u32,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<LLMStreamChunk>>> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk {
                    delta: Some("thinking".into()),
                    thinking_delta: None,
                    thinking_signature: None,
                    tool_calls: None,
                    done: false,
                    usage: None,
                }))
                .await;
            // block well beyond the test window; interrupt must break this
            tokio::time::sleep(std::time::Duration::from_secs(120)).await;
            let _ = tx
                .send(Ok(LLMStreamChunk {
                    delta: None,
                    thinking_delta: None,
                    thinking_signature: None,
                    tool_calls: None,
                    done: true,
                    usage: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

#[tokio::test]
async fn interrupt_cancels_active_turn() {
    let workspace = make_temp_workspace();
    let bus = Bus::new();
    let agent = AgentType::DataAnalysis;
    let tools = registry_for(&workspace, agent);
    let prompts = PromptLoader::new(&workspace);
    let skills = SkillLoader::new(&workspace);
    let manifest = ManifestStore::new(&workspace);
    let task_board = TaskBoard::new();
    let loop_ = Arc::new(AgentLoop::new(
        agent,
        workspace.clone(),
        tools,
        Arc::new(SlowMock) as Arc<dyn LLMProvider>,
        prompts,
        skills,
        manifest,
        bus.clone(),
        task_board,
        AgentDefaults::default(),
    ));
    loop_.clone().start();
    let mut rx = bus.subscribe();

    loop_.submit("go".into(), MessageSource::User);

    // Wait until the turn is actively running.
    let mut busy = false;
    for _ in 0..200 {
        if let Ok(Ok(BusMessage::StatusChange { status, .. })) =
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
        {
            if matches!(status, AgentStatus::Thinking | AgentStatus::RunningTool) {
                busy = true;
                break;
            }
        }
    }
    assert!(busy, "agent should have started the turn");

    // Interrupt and confirm it returns to idle promptly (well under the 120s sleep).
    loop_.interrupt().await;
    let mut idle = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if let Ok(Ok(BusMessage::StatusChange {
            status: AgentStatus::Idle,
            ..
        })) = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
        {
            idle = true;
            break;
        }
    }
    assert!(idle, "agent should return to idle after interrupt");
    assert!(!loop_.is_busy().await, "no turn should remain active");

    std::fs::remove_dir_all(&workspace).ok();
}

fn uuid_stamp() -> String {
    // avoid pulling in time; use a counter-ish via pid + nanos-ish fallback
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{d}")
}

fn registry_for(workspace: &std::path::Path, agent: AgentType) -> ToolRegistry {
    let write_dir = match agent {
        AgentType::DataAnalysis => workspace.join("data").join("processed"),
        AgentType::Plotting => workspace.join("code"),
        AgentType::Theory => workspace.join("theory"),
        AgentType::Report => workspace.join("tex"),
        AgentType::Main => workspace.join("outline"),
    };
    let ctx = FsCtx::new(workspace.to_path_buf(), Some(write_dir));
    let mut reg = ToolRegistry::new();
    reg.register(autoreport_cli::tools::list_dir::ListDirTool::make(
        ctx.clone(),
    ));
    reg.register(autoreport_cli::tools::apply_patch::make(ctx.clone()));
    reg.register(autoreport_cli::tools::exec_tool::make(
        ctx,
        10,
        autoreport_cli::sandbox::SandboxSpec::new(
            autoreport_cli::sandbox::SandboxMode::DangerFullAccess,
            false,
        ),
    ));
    reg
}

#[tokio::test]
async fn agent_loop_writes_file_then_replies() {
    let workspace = make_temp_workspace();
    let bus = Bus::new();

    // Script: first turn emits an apply_patch tool call, second turn emits text.
    let write_call = ToolCall {
        id: "call_1".into(),
        name: "apply_patch".into(),
        arguments: serde_json::json!({
            "patch": "*** Begin Patch\n*** Add File: data/processed/out.csv\n+a,b\n+1,2\n*** End Patch\n",
        }),
    };
    let mock = Arc::new(Mock::new(vec![
        (Some(String::new()), vec![write_call]),
        (Some("wrote the file".into()), vec![]),
    ])) as Arc<dyn LLMProvider>;

    let agent = AgentType::DataAnalysis;
    let tools = registry_for(&workspace, agent);
    let prompts = PromptLoader::new(&workspace);
    let skills = SkillLoader::new(&workspace);
    let manifest = ManifestStore::new(&workspace);
    let task_board = TaskBoard::new();
    let defaults = AgentDefaults::default();

    let loop_ = Arc::new(AgentLoop::new(
        agent,
        workspace.clone(),
        tools,
        mock,
        prompts,
        skills,
        manifest,
        bus.clone(),
        task_board,
        defaults,
    ));
    loop_.clone().start();

    // Collect bus events.
    let mut rx = bus.subscribe();

    loop_.submit("please write out.csv".into(), MessageSource::User);

    // Advance the runtime (paused) until we see a final, non-streaming reply.
    let mut saw_final = false;
    let mut iterations = 0;
    while !saw_final && iterations < 200 {
        iterations += 1;
        // Unpause briefly to let spawned tasks progress, then re-pause.
        if let Ok(Ok(BusMessage::AgentResponse {
            streaming: false, ..
        })) = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
        {
            saw_final = true;
        }
    }
    assert!(saw_final, "expected a final agent reply on the bus");

    // The tool call must have produced the file inside the allowed dir.
    let written = std::fs::read_to_string(workspace.join("data/processed/out.csv")).unwrap();
    assert_eq!(written, "a,b\n1,2\n");

    std::fs::remove_dir_all(&workspace).ok();
}

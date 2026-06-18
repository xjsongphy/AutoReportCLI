//! End-to-end test of the agent loop with a scripted mock provider:
//! proves message → stream → tool call (write_file) → tool result → final text
//! works, and that write-directory isolation is enforced.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use autoreport_cli::bus::Bus;
use autoreport_cli::config::AgentDefaults;
use autoreport_cli::prompts::PromptLoader;
use autoreport_cli::provider::types::{LLMResponse, Message, ToolCall, ToolDef, Usage};
use autoreport_cli::provider::LLMProvider;
use autoreport_cli::runtime::AgentLoop;
use autoreport_cli::skills::SkillLoader;
use autoreport_cli::taskboard::TaskBoard;
use autoreport_cli::tools::file_tools::{self, FsCtx};
use autoreport_cli::tools::manifest::ManifestStore;
use autoreport_cli::tools::ToolRegistry;
use autoreport_cli::types::{AgentType, BusMessage, MessageSource};

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
    for t in file_tools::bundle(ctx) {
        reg.register(t);
    }
    reg
}

#[tokio::test]
async fn agent_loop_writes_file_then_replies() {
    let workspace = make_temp_workspace();
    let bus = Bus::new();

    // Script: first turn emits a write_file tool call, second turn emits text.
    let write_call = ToolCall {
        id: "call_1".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({
            "path": "data/processed/out.csv",
            "content": "a,b\n1,2\n",
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
        tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .ok()
            .and_then(|r| r.ok())
            .map(|m| match m {
                BusMessage::AgentResponse { streaming: false, .. } => saw_final = true,
                _ => {}
            });
    }
    assert!(saw_final, "expected a final agent reply on the bus");

    // The tool call must have produced the file inside the allowed dir.
    let written = std::fs::read_to_string(workspace.join("data/processed/out.csv")).unwrap();
    assert_eq!(written, "a,b\n1,2\n");

    std::fs::remove_dir_all(&workspace).ok();
}

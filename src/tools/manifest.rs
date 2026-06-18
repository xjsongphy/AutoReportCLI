//! Manifest store: per-agent record of the files each agent has produced, used
//! for cross-agent visibility (who owns what). Stored as JSON under
//! `.autoreport/manifests/`.

use crate::tools::registry::{arg_opt_str, Tool, ToolOutput};
use crate::types::AgentType;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    pub bytes: u64,
    pub mtime: Option<String>,
}

#[derive(Clone)]
pub struct ManifestStore {
    dir: PathBuf,
    /// In-memory cache keyed by agent, kept in sync with disk.
    cache: Arc<Mutex<BTreeMap<String, Vec<ManifestEntry>>>>,
}

impl ManifestStore {
    pub fn new(workspace: &std::path::Path) -> Self {
        let dir = workspace.join(".autoreport").join("manifests");
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            cache: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn path_for(&self, agent: AgentType) -> PathBuf {
        self.dir.join(format!("{}.json", agent.as_str()))
    }

    fn load(&self, agent: AgentType) -> Vec<ManifestEntry> {
        {
            let g = self.cache.lock().unwrap();
            if let Some(v) = g.get(agent.as_str()) {
                return v.clone();
            }
        }
        let p = self.path_for(agent);
        let entries: Vec<ManifestEntry> = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        self.cache
            .lock()
            .unwrap()
            .insert(agent.as_str().to_string(), entries.clone());
        entries
    }

    /// Record (or refresh) a file under an agent's manifest and persist.
    pub fn record(&self, agent: AgentType, path: String) {
        let meta = std::fs::metadata(&path).ok();
        let entry = ManifestEntry {
            path: path.clone(),
            bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            mtime: meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| format!("{}", d.as_secs())),
        };
        let mut g = self.cache.lock().unwrap();
        let list = g.entry(agent.as_str().to_string()).or_default();
        if let Some(existing) = list.iter_mut().find(|e| e.path == path) {
            *existing = entry;
        } else {
            list.push(entry);
        }
        let snapshot = list.clone();
        drop(g);
        let _ = std::fs::write(
            self.path_for(agent),
            serde_json::to_string_pretty(&snapshot).unwrap_or_default(),
        );
    }

    pub fn snapshot(&self, agent: Option<AgentType>) -> Value {
        match agent {
            Some(a) => {
                let entries = self.load(a);
                json!({ a.as_str(): entries })
            }
            None => {
                let mut map = serde_json::Map::new();
                for a in AgentType::ALL {
                    let entries = self.load(a);
                    map.insert(a.as_str().to_string(), serde_json::to_value(entries).unwrap());
                }
                Value::Object(map)
            }
        }
    }
}

/// `manifest` tool — view the file manifest for an agent (self by default,
/// all agents if requested). Main can view any; sub-agents view their own.
pub struct ManifestTool {
    store: ManifestStore,
    agent: AgentType,
}

impl ManifestTool {
    pub fn new(store: ManifestStore, agent: AgentType) -> Self {
        Self { store, agent }
    }
}

#[async_trait]
impl Tool for ManifestTool {
    fn name(&self) -> &str {
        "manifest"
    }
    fn description(&self) -> &str {
        "View produced-file manifests. Pass `agent` to inspect a specific agent, or `all: true` for everyone (Main only)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent": {"type": "string", "enum": ["main","data_analysis","plotting","theory","report"]},
                "all": {"type": "boolean"}
            }
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        if arg_opt_str(args, "all").is_some() && args.get("all").and_then(|v| v.as_bool()).unwrap_or(false) {
            if self.agent != AgentType::Main {
                return ToolOutput::err("only Main can view all manifests");
            }
            return ToolOutput::ok(self.store.snapshot(None));
        }
        let target = match arg_opt_str(args, "agent") {
            Some(s) => match s.parse::<AgentType>() {
                Ok(a) => a,
                Err(e) => return ToolOutput::err(e),
            },
            None => self.agent,
        };
        if self.agent != AgentType::Main && target != self.agent {
            return ToolOutput::err("sub-agents can only view their own manifest");
        }
        ToolOutput::ok(self.store.snapshot(Some(target)))
    }
}

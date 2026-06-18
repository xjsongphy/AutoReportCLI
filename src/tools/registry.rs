//! Tool trait and registry.

use crate::provider::types::ToolDef;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// The outcome of executing a tool.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// JSON-serializable result, returned to the model.
    pub result: Value,
    /// Human/model-readable error string, set instead of `result` on failure.
    pub error: Option<String>,
}

impl ToolOutput {
    pub fn ok(result: Value) -> Self {
        Self { result, error: None }
    }
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            result: Value::Null,
            error: Some(message.into()),
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    async fn call(&self, args: &Value) -> ToolOutput;
}

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn definitions(&self) -> Vec<ToolDef> {
        self.tools
            .values()
            .map(|t| ToolDef {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    pub async fn call(&self, name: &str, args: &Value) -> ToolOutput {
        match self.get(name) {
            Some(tool) => tool.call(args).await,
            None => ToolOutput::err(format!("unknown tool '{name}'")),
        }
    }
}

/// Helper to pull a typed argument out of a JSON object, or return an error.
pub fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

pub fn arg_opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

pub fn arg_opt_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

pub fn arg_opt_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

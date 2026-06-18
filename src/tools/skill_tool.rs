//! `load_skill` tool — returns a skill's full body so the agent can follow it.

use crate::skills::SkillLoader;
use crate::tools::registry::{arg_str, Tool, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct LoadSkillTool {
    loader: SkillLoader,
}

impl LoadSkillTool {
    pub fn new(loader: SkillLoader) -> Self {
        Self { loader }
    }
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> &str {
        "load_skill"
    }
    fn description(&self) -> &str {
        "Load and follow a skill by name. Returns the skill's full instructions. Call `list_skills` first if unsure of available names."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let name = match arg_str(args, "name") {
            Ok(n) => n,
            Err(e) => return ToolOutput::err(e),
        };
        match self.loader.load(&name) {
            Some(skill) => ToolOutput::ok(json!({
                "name": skill.name,
                "description": skill.description,
                "content": skill.body,
            })),
            None => {
                let available: Vec<String> = self.loader.list().into_iter().map(|s| s.name).collect();
                ToolOutput::err(format!(
                    "skill '{name}' not found. Available: [{}]",
                    available.join(", ")
                ))
            }
        }
    }
}

/// `list_skills` — enumerate available skills.
pub struct ListSkillsTool {
    loader: SkillLoader,
}

impl ListSkillsTool {
    pub fn new(loader: SkillLoader) -> Self {
        Self { loader }
    }
}

#[async_trait]
impl Tool for ListSkillsTool {
    fn name(&self) -> &str {
        "list_skills"
    }
    fn description(&self) -> &str {
        "List the names and descriptions of all available skills."
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn call(&self, _args: &Value) -> ToolOutput {
        let skills: Vec<Value> = self
            .loader
            .list()
            .into_iter()
            .map(|s| json!({"name": s.name, "description": s.description}))
            .collect();
        ToolOutput::ok(json!({"skills": skills}))
    }
}

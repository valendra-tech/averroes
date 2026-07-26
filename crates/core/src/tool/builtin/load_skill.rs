use crate::skill::SkillIndex;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use std::sync::Arc;

pub struct LoadSkillTool {
    pub index: Arc<SkillIndex>,
}

impl LoadSkillTool {
    pub fn new(index: Arc<SkillIndex>) -> Self {
        Self { index }
    }
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> &str {
        "load_skill"
    }
    fn description(&self) -> &str {
        "Load the full content of a skill by name. Use list_skills first to see available skills."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string", "description": "The name of the skill to load" } },
            "required": ["name"]
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "name parameter is required".into(),
            })?;
        match self.index.load(name) {
            Ok(content) => {
                Ok(ToolResult::ok(content).with_metadata(serde_json::json!({"skill": name})))
            }
            Err(e) => Ok(ToolResult::error(format!(
                "Failed to load skill '{}': {}",
                name, e
            ))),
        }
    }
}

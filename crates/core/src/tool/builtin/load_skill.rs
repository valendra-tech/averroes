use crate::observability::diagnostics::{self, DiagnosticLevel};
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
        "Load the full content of a workspace skill by exact name. Skill names are already present in the system context; use list_skills with a focused query only when the name is unclear."
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
        diagnostics::record(
            DiagnosticLevel::Info,
            "skills.tool",
            format!("load_skill requested for '{name}'."),
        );
        match self.index.load(name) {
            Ok(content) => {
                diagnostics::record(
                    DiagnosticLevel::Success,
                    "skills.tool",
                    format!("load_skill returned '{name}' ({} bytes).", content.len()),
                );
                Ok(ToolResult::ok(content).with_metadata(serde_json::json!({"skill": name})))
            }
            Err(e) => {
                diagnostics::record(
                    DiagnosticLevel::Error,
                    "skills.tool",
                    format!("load_skill failed for '{name}': {e}."),
                );
                Ok(ToolResult::error(format!(
                    "Failed to load skill '{}': {}",
                    name, e
                )))
            }
        }
    }
}

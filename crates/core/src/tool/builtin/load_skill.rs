use crate::observability::diagnostics::{self, DiagnosticLevel};
use crate::skill::SkillIndex;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

pub struct LoadSkillTool {
    pub index: Arc<SkillIndex>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadSkillParams {
    name: String,
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
            "required": ["name"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: LoadSkillParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let name = params.name.trim();
        if name.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "name parameter is required".into(),
            });
        }
        diagnostics::record(
            DiagnosticLevel::Info,
            "skills.tool",
            format!("load_skill requested for '{name}'."),
        );
        match self.index.load(name) {
            Ok(content) => {
                let truncated =
                    content.ends_with(crate::skill::loader::SKILL_CONTENT_TRUNCATION_NOTICE);
                let bytes = content.len();
                diagnostics::record(
                    DiagnosticLevel::Success,
                    "skills.tool",
                    format!(
                        "load_skill returned '{name}' ({} bytes{}).",
                        bytes,
                        if truncated { ", truncated" } else { "" }
                    ),
                );
                Ok(ToolResult::ok(content).with_metadata(serde_json::json!({
                    "skill": name,
                    "bytes": bytes,
                    "truncated": truncated
                })))
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

use crate::observability::diagnostics::{self, DiagnosticLevel};
use crate::provider::types::{ContentPart, MessageContent, Role};
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
        "Load the full content of a workspace skill by exact name after the user explicitly selects it with $skill-name. Skill names are already present in the system context; use list_skills with a focused query when the name is unclear."
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
        if !latest_user_selected_skill(_ctx, &self.index, name) {
            let message = format!(
                "Skill '{name}' was not explicitly selected. Mention it as `${name}` in the latest user request before loading it."
            );
            diagnostics::record(DiagnosticLevel::Warning, "skills.tool", message.clone());
            return Ok(ToolResult::error(message));
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

fn latest_user_selected_skill(
    ctx: &crate::tool::ToolContext,
    index: &SkillIndex,
    requested_name: &str,
) -> bool {
    let Ok(requested) = index.resolve(requested_name) else {
        return false;
    };
    let Some(message) = ctx
        .conversation_context
        .iter()
        .rev()
        .find(|message| message.role == Role::User)
    else {
        return false;
    };
    let selected = |text: &str| {
        index
            .explicit_skill_mentions(text)
            .into_iter()
            .filter_map(|mention| index.resolve(&mention).ok())
            .any(|skill| skill.path == requested.path)
    };
    match &message.content {
        MessageContent::Text(text) => selected(text),
        MessageContent::Parts(parts) => parts.iter().any(|part| match part {
            ContentPart::Text { text } => selected(text),
            _ => false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::LoadSkillTool;
    use crate::agent::ContextController;
    use crate::provider::types::{ChatMessage, MessageContent, Role};
    use crate::skill::{SkillIndex, SkillLoader};
    use crate::tool::{Tool, ToolActivation, ToolContext};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn context(user_input: &str) -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from("/tmp"),
            workspace_root: PathBuf::from("/tmp"),
            session_id: "session".into(),
            agent_id: "agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(ToolActivation::default()),
            context_controller: Arc::new(ContextController::for_test(100_000, 16_384)),
            conversation_context: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text(user_input.into()),
                tool_call_id: None,
                tool_calls: None,
            }],
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    fn tool() -> (tempfile::TempDir, LoadSkillTool) {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("pdf.md"),
            "# PDF\n\nCreate documents safely.\n",
        )
        .unwrap();
        let index =
            SkillIndex::build(SkillLoader::new(vec![workspace.path().to_path_buf()])).unwrap();
        (workspace, LoadSkillTool::new(Arc::new(index)))
    }

    #[tokio::test]
    async fn refuses_to_load_a_skill_without_an_explicit_mention() {
        let (_workspace, tool) = tool();

        let result = tool
            .execute(
                &context("Please help me with documents."),
                &json!({"name": "pdf"}),
            )
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .is_some_and(|error| error.contains("$pdf")));
    }

    #[tokio::test]
    async fn loads_a_skill_selected_in_the_latest_user_message() {
        let (_workspace, tool) = tool();

        let result = tool
            .execute(&context("Use $pdf for this task."), &json!({"name": "pdf"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.content.contains("Create documents safely"));
    }
}

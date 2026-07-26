use crate::skill::SkillIndex;
use crate::tool::{Result, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use std::sync::Arc;

pub struct ListSkillsTool {
    pub index: Arc<SkillIndex>,
}

impl ListSkillsTool {
    pub fn new(index: Arc<SkillIndex>) -> Self {
        Self { index }
    }
}

#[async_trait]
impl Tool for ListSkillsTool {
    fn name(&self) -> &str {
        "list_skills"
    }
    fn description(&self) -> &str {
        "List all available skills with their names and descriptions"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, _params: &serde_json::Value) -> Result<ToolResult> {
        let skills = self.index.list();
        let output: Vec<String> = skills
            .iter()
            .map(|s| format!("- **{}**: {}", s.name, s.description))
            .collect();
        if output.is_empty() {
            Ok(ToolResult::ok("No skills available"))
        } else {
            Ok(ToolResult::ok(output.join("\n"))
                .with_metadata(serde_json::json!({"count": output.len()})))
        }
    }
}

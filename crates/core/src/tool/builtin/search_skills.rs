use crate::tool::{Result, SkillMarketplaceBackend, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 20;
const MAX_QUERY_CHARS: usize = 1_000;

pub struct SearchSkillsTool {
    marketplace: Arc<dyn SkillMarketplaceBackend>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchSkillsParams {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

impl SearchSkillsTool {
    pub fn new(marketplace: Arc<dyn SkillMarketplaceBackend>) -> Self {
        Self { marketplace }
    }
}

#[async_trait]
impl Tool for SearchSkillsTool {
    fn name(&self) -> &str {
        "search_skills"
    }

    fn description(&self) -> &str {
        "Search the public skills marketplace. Omit query to see trending skills, then use install_skill with a returned skill_id."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Optional skill name, capability, or keyword. Omit it to list trending skills."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LIMIT,
                    "description": "Maximum number of marketplace results to return; defaults to 10."
                }
            },
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: SearchSkillsParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let query = params.query.unwrap_or_default().trim().to_owned();
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!("query cannot exceed {MAX_QUERY_CHARS} characters"),
            });
        }
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let skills = self
            .marketplace
            .search(&query, limit)
            .await
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error,
            })?;

        if skills.is_empty() {
            return Ok(ToolResult::ok(if query.is_empty() {
                "The skills marketplace returned no trending skills."
            } else {
                "No marketplace skills matched that query."
            })
            .with_metadata(json!({ "query": query, "skills": [] })));
        }

        let content = skills
            .iter()
            .map(|skill| {
                let description = skill
                    .description
                    .as_deref()
                    .filter(|description| !description.is_empty())
                    .unwrap_or("No description available.");
                format!(
                    "- **{}** (`{}`): {}\n  Source: `{}` · installs: {}\n  Install with `install_skill` using skill_id `{}`.",
                    skill.name,
                    skill.id,
                    description,
                    skill.source,
                    skill.installs,
                    skill.id,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolResult::ok(content).with_metadata(json!({
            "query": query,
            "count": skills.len(),
            "skills": skills,
        })))
    }
}

use crate::observability::diagnostics::{self, DiagnosticLevel};
use crate::skill::SkillIndex;
use crate::tool::{Result, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

const MAX_QUERY_CHARS: usize = 1_000;

pub struct ListSkillsTool {
    pub index: Arc<SkillIndex>,
}

impl ListSkillsTool {
    pub fn new(index: Arc<SkillIndex>) -> Self {
        Self { index }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListSkillsParams {
    query: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[async_trait]
impl Tool for ListSkillsTool {
    fn name(&self) -> &str {
        "list_skills"
    }
    fn description(&self) -> &str {
        "List a small filtered page of workspace skills with names and concise descriptions. Prefer a focused query."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "maxLength": MAX_QUERY_CHARS,
                    "description": "Optional text matched against skill names and descriptions."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum results to return (default 12, maximum 25).",
                    "minimum": 1,
                    "maximum": 25
                },
                "offset": {
                    "type": "integer",
                    "description": "Zero-based offset for the next page.",
                    "minimum": 0
                }
            },
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: ListSkillsParams = serde_json::from_value(params.clone()).map_err(|error| {
            crate::tool::ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            }
        })?;
        let query = params.query.as_deref().map(str::trim).unwrap_or_default();
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(crate::tool::ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!("query cannot exceed {MAX_QUERY_CHARS} characters"),
            });
        }
        let normalized_query = normalize_search_text(query);
        let query_terms = normalized_query.split_whitespace().collect::<Vec<_>>();
        let limit = params.limit.unwrap_or(12);
        if !(1..=25).contains(&limit) {
            return Err(crate::tool::ToolError::InvalidParams {
                tool: self.name().into(),
                message: "limit must be between 1 and 25".into(),
            });
        }
        let offset = params.offset.unwrap_or(0);
        let matching = self
            .index
            .list()
            .into_iter()
            .filter(|skill| {
                let searchable =
                    normalize_search_text(&format!("{} {}", skill.name, skill.description));
                query_terms.iter().all(|term| {
                    searchable
                        .split_whitespace()
                        .any(|word| word.contains(term))
                })
            })
            .collect::<Vec<_>>();
        let total = matching.len();
        let skills = matching
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        diagnostics::record(
            DiagnosticLevel::Info,
            "skills.tool",
            format!(
                "list_skills requested with query '{query}'; returned {} of {total} matching skill(s).",
                skills.len()
            ),
        );
        let output: Vec<String> = skills
            .iter()
            .map(|s| format!("- **{}**: {}", s.name, concise_description(&s.description)))
            .collect();
        if output.is_empty() {
            Ok(ToolResult::ok(if query.is_empty() {
                "No workspace skills available".to_string()
            } else {
                format!("No workspace skills matched '{query}'")
            }))
        } else {
            let returned = output.len();
            let next_offset = offset + returned;
            let mut content = output.join("\n");
            if next_offset < total {
                content.push_str(&format!(
                    "\n\nShowing {returned} of {total} matches. Request offset {next_offset} for the next page."
                ));
            }
            Ok(ToolResult::ok(content).with_metadata(serde_json::json!({
                "count": returned,
                "total": total,
                "offset": offset,
                "next_offset": (next_offset < total).then_some(next_offset),
                "query": query,
            })))
        }
    }
}

fn concise_description(description: &str) -> String {
    const MAX_CHARS: usize = 240;
    let normalized = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_CHARS {
        return normalized;
    }
    let mut concise = normalized.chars().take(MAX_CHARS - 1).collect::<String>();
    concise.push('…');
    concise
}

fn normalize_search_text(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{concise_description, normalize_search_text, ListSkillsTool, MAX_QUERY_CHARS};
    use crate::skill::{SkillIndex, SkillLoader};
    use crate::tool::{Tool, ToolActivation, ToolContext, ToolError};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn context() -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: "session".into(),
            agent_id: "agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    fn tool() -> ListSkillsTool {
        ListSkillsTool::new(Arc::new(
            SkillIndex::build(SkillLoader::new(Vec::new())).unwrap(),
        ))
    }

    #[test]
    fn descriptions_are_single_line_and_bounded() {
        let description = format!("first\n\n{}", "word ".repeat(80));
        let concise = concise_description(&description);

        assert!(!concise.contains('\n'));
        assert_eq!(concise.chars().count(), 240);
        assert!(concise.ends_with('…'));
    }

    #[test]
    fn search_normalization_matches_hyphenated_skill_names() {
        let skill = normalize_search_text("daily-work-briefing");
        let query = normalize_search_text("daily work");

        assert!(query
            .split_whitespace()
            .all(|term| skill.split_whitespace().any(|word| word.contains(term))));
    }

    #[tokio::test]
    async fn rejects_invalid_parameters_at_execution_boundary() {
        let tool = tool();
        let invalid = [
            json!({"query": "x".repeat(MAX_QUERY_CHARS + 1)}),
            json!({"limit": 0}),
            json!({"limit": 26}),
            json!({"offset": -1}),
            json!({"unexpected": true}),
            json!({"query": 42}),
        ];

        for params in invalid {
            assert!(matches!(
                tool.execute(&context(), &params).await,
                Err(ToolError::InvalidParams { .. })
            ));
        }
    }
}

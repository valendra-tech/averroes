//! Multi-engine web search powered by OxiBrowser's search module.

use async_trait::async_trait;
use oxibrowser::{search::dispatch, SearchOutput};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

const DEFAULT_ENGINE: &str = "ddg,bing";
const DEFAULT_MAX_RESULTS: usize = 8;
const DEFAULT_TIMEOUT_SECONDS: u64 = 20;

pub struct WebSearchTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebSearchParams {
    query: String,
    source: Option<String>,
    engine: Option<String>,
    repo: Option<String>,
    max_results: Option<usize>,
    timeout_seconds: Option<u64>,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search_intrernal"
    }

    fn description(&self) -> &str {
        "Search the public web, GitHub repositories, or GitHub issues with OxiBrowser"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "maxLength": 1000,
                    "description": "What to search for"
                },
                "source": {
                    "type": "string",
                    "enum": ["web", "github", "github-issues"],
                    "description": "Search target; defaults to the public web"
                },
                "engine": {
                    "type": "string",
                    "description": "Comma-separated web engines (ddg, bing, wiki); defaults to ddg,bing"
                },
                "repo": {
                    "type": "string",
                    "description": "owner/repository, required when source is github-issues"
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 30,
                    "description": "Maximum number of results to return"
                },
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": 5,
                    "maximum": 60,
                    "description": "Per-engine timeout in seconds"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: WebSearchParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let query = params.query.trim();
        if query.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "Missing required parameter: query".into(),
            });
        }
        if query.chars().count() > 1_000 {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "query cannot exceed 1,000 characters".into(),
            });
        }

        let source = params.source.as_deref().unwrap_or("web");
        if !matches!(source, "web" | "github" | "github-issues") {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "source must be web, github, or github-issues".into(),
            });
        }

        let repo = params.repo.as_deref().map(str::trim);
        if source == "github-issues" && repo.is_none_or(str::is_empty) {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "repo is required when source is github-issues".into(),
            });
        }

        let engine = params.engine.as_deref().unwrap_or(DEFAULT_ENGINE);
        let max_results = params.max_results.unwrap_or(DEFAULT_MAX_RESULTS);
        let timeout_seconds = params.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        validate_search_limits(self.name(), max_results, timeout_seconds)?;

        tracing::info!(
            tool = self.name(),
            source,
            engine,
            max_results,
            "searching the internet with OxiBrowser"
        );
        let output = dispatch(
            query,
            source,
            engine,
            repo,
            None,
            max_results,
            timeout_seconds,
        )
        .await
        .map_err(|error| ToolError::Execution {
            tool: self.name().into(),
            message: format!("OxiBrowser search failed: {error}"),
        })?;

        Ok(format_search_result(output))
    }
}

fn validate_search_limits(tool: &str, max_results: usize, timeout_seconds: u64) -> Result<()> {
    if !(1..=30).contains(&max_results) {
        return Err(ToolError::InvalidParams {
            tool: tool.into(),
            message: "max_results must be between 1 and 30".into(),
        });
    }
    if !(5..=60).contains(&timeout_seconds) {
        return Err(ToolError::InvalidParams {
            tool: tool.into(),
            message: "timeout_seconds must be between 5 and 60".into(),
        });
    }
    Ok(())
}

fn format_search_result(output: SearchOutput) -> ToolResult {
    let mut content = format!(
        "# Search results for: {}\n\nFound {} result(s) using {}.\n",
        output.query, output.total_results, output.engine
    );

    for (index, result) in output.results.iter().enumerate() {
        content.push_str(&format!(
            "\n{}. {}\n   URL: {}\n   Source: {}",
            index + 1,
            result.title,
            result.url,
            result.source
        ));
        if !result.snippet.trim().is_empty() {
            content.push_str(&format!("\n   {}", result.snippet.trim()));
        }
        if let Some(extra) = &result.extra {
            content.push_str(&format!(
                "\n   GitHub: {} stars, {} forks",
                extra.stars, extra.forks
            ));
        }
        content.push('\n');
    }

    let metadata = json!({
        "browser": "oxibrowser",
        "source": output.source.as_str(),
        "engine": output.engine.as_str(),
        "query": output.query.as_str(),
        "result_count": output.total_results,
        "results": output.results.iter().map(|result| json!({
            "title": result.title.as_str(),
            "url": result.url.as_str(),
            "source": result.source.as_str()
        })).collect::<Vec<_>>()
    });
    ToolResult::ok(content).with_metadata(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolActivation;
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

    #[test]
    fn rejects_search_limits_outside_the_advertised_range() {
        assert!(validate_search_limits("web_search_intrernal", 0, 20).is_err());
        assert!(validate_search_limits("web_search_intrernal", 31, 20).is_err());
        assert!(validate_search_limits("web_search_intrernal", 8, 4).is_err());
        assert!(validate_search_limits("web_search_intrernal", 8, 61).is_err());
        assert!(validate_search_limits("web_search_intrernal", 8, 20).is_ok());
    }

    #[test]
    fn formats_search_results_for_agent_context() {
        let output = SearchOutput {
            query: "rust async".into(),
            source: "web".into(),
            engine: "DuckDuckGo,Bing".into(),
            total_results: 1,
            results: vec![oxibrowser::SearchResult {
                title: "Rust".into(),
                url: "https://www.rust-lang.org/".into(),
                snippet: "A language empowering everyone.".into(),
                source: "DuckDuckGo".into(),
                extra: None,
            }],
        };

        let result = format_search_result(output);
        assert!(result.success);
        assert!(result.content.contains("https://www.rust-lang.org/"));
        assert_eq!(result.metadata.as_ref().unwrap()["result_count"], 1);
    }

    #[tokio::test]
    async fn rejects_invalid_parameters_before_network_request() {
        let tool = WebSearchTool;
        let invalid = [
            json!({"query": "x".repeat(1_001)}),
            json!({"query": "rust", "max_results": 0}),
            json!({"query": "rust", "max_results": 31}),
            json!({"query": "rust", "timeout_seconds": 4}),
            json!({"query": "rust", "timeout_seconds": 61}),
            json!({"query": "rust", "unexpected": true}),
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

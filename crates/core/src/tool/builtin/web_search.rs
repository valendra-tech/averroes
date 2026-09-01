//! Multi-engine web search powered by OxiBrowser's search module.

use async_trait::async_trait;
use oxibrowser::{search::dispatch, SearchOutput};
use serde_json::{json, Value};

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

const DEFAULT_ENGINE: &str = "ddg,bing";
const DEFAULT_MAX_RESULTS: usize = 8;
const DEFAULT_TIMEOUT_SECONDS: u64 = 20;

pub struct WebSearchTool;

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
                    "description": "Maximum number of results to return"
                },
                "timeout_seconds": {
                    "type": "integer",
                    "description": "Per-engine timeout in seconds"
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let query = params["query"]
            .as_str()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "Missing required parameter: query".into(),
            })?;
        if query.chars().count() > 1_000 {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "query cannot exceed 1,000 characters".into(),
            });
        }

        let source = params
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("web");
        if !matches!(source, "web" | "github" | "github-issues") {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "source must be web, github, or github-issues".into(),
            });
        }

        let repo = params.get("repo").and_then(Value::as_str).map(str::trim);
        if source == "github-issues" && repo.is_none_or(str::is_empty) {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "repo is required when source is github-issues".into(),
            });
        }

        let engine = params
            .get("engine")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_ENGINE);
        let max_results = params
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_MAX_RESULTS as u64)
            .clamp(1, 30) as usize;
        let timeout_seconds = params
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
            .clamp(5, 60);

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
}

use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch content from a URL and return it as text"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch content from"
                }
            },
            "required": ["url"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let url_str = params["url"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "Missing required parameter: url".into(),
            })?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ToolError::Execution {
                tool: self.name().into(),
                message: format!("Failed to create HTTP client: {e}"),
            })?;

        let response = client
            .get(url_str)
            .send()
            .await
            .map_err(|e| ToolError::Execution {
                tool: self.name().into(),
                message: format!("Failed to fetch URL: {e}"),
            })?;

        let status_code = response.status().as_u16();

        let body = response.text().await.map_err(|e| ToolError::Execution {
            tool: self.name().into(),
            message: format!("Failed to read response body: {e}"),
        })?;

        let metadata = json!({
            "status_code": status_code
        });

        if status_code < 200 || status_code >= 300 {
            return Ok(ToolResult::error(body).with_metadata(metadata));
        }

        let truncated = if body.len() > 50_000 {
            &body[..50_000]
        } else {
            &body
        };

        Ok(ToolResult::ok(truncated).with_metadata(metadata))
    }
}

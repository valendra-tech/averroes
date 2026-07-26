pub mod builtin;
pub mod dynamic;
pub mod registry;

pub use registry::ToolRegistry;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub working_dir: PathBuf,
    pub session_id: String,
    pub agent_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            success: true,
            content: content.into(),
            error: None,
            metadata: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            content: String::new(),
            error: Some(message.into()),
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;
    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult>;

    fn is_read_only(&self) -> bool {
        false
    }
    fn requires_confirmation(&self) -> bool {
        false
    }
}

pub type ToolRef = Arc<dyn Tool>;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Tool '{tool}' not found")]
    NotFound { tool: String },
    #[error("Tool '{tool}' execution failed: {message}")]
    Execution { tool: String, message: String },
    #[error("Invalid parameters for tool '{tool}': {message}")]
    InvalidParams { tool: String, message: String },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ToolError>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_tool_result_ok() {
        let result = ToolResult::ok("success");
        assert!(result.success);
        assert_eq!(result.content, "success");
        assert!(result.error.is_none());
        assert!(result.metadata.is_none());
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult::error("something went wrong");
        assert!(!result.success);
        assert_eq!(result.content, "");
        assert_eq!(result.error, Some("something went wrong".into()));
        assert!(result.metadata.is_none());
    }

    #[test]
    fn test_tool_result_with_metadata() {
        let result = ToolResult::ok("done").with_metadata(json!({"tokens": 42}));
        assert!(result.success);
        assert_eq!(result.content, "done");
        assert_eq!(result.metadata, Some(json!({"tokens": 42})));
    }

    #[test]
    fn test_tool_result_error_with_metadata() {
        let result = ToolResult::error("fail").with_metadata(json!({"code": 500}));
        assert!(!result.success);
        assert_eq!(result.error, Some("fail".into()));
        assert_eq!(result.metadata, Some(json!({"code": 500})));
    }
}

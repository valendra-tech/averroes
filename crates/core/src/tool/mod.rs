pub mod registry;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::path::PathBuf;

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
        Self { success: true, content: content.into(), error: None, metadata: None }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self { success: false, content: String::new(), error: Some(message.into()), metadata: None }
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

    fn is_read_only(&self) -> bool { false }
    fn requires_confirmation(&self) -> bool { false }
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

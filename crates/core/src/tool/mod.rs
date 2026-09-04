pub mod builtin;
pub mod dynamic;
pub mod registry;

pub use registry::ToolRegistry;

use crate::agent::orchestration::AgentRunner;
use crate::work::ConversationSearchResult;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

#[async_trait]
pub trait MemorySearchBackend: Send + Sync {
    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> std::result::Result<Vec<ConversationSearchResult>, String>;
}

/// A marketplace entry that can be presented to an agent and passed back to
/// the installer without exposing UI-specific runtime types in core.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMarketplaceEntry {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub source: String,
    pub slug: String,
    pub installs: u64,
    pub url: Option<String>,
}

/// Application-provided access to the public skills marketplace. The core
/// tools own the agent-facing contract while the desktop runtime supplies the
/// network and workspace implementation.
#[async_trait]
pub trait SkillMarketplaceBackend: Send + Sync {
    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> std::result::Result<Vec<SkillMarketplaceEntry>, String>;

    async fn install(
        &self,
        workspace_root: &Path,
        skill: &SkillMarketplaceEntry,
    ) -> std::result::Result<String, String>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalPolicy {
    #[default]
    Ask,
    AllowAll,
}

impl ToolApprovalPolicy {
    pub fn allows_all(self) -> bool {
        matches!(self, Self::AllowAll)
    }
}

#[derive(Clone)]
pub struct ToolContext {
    pub working_dir: PathBuf,
    pub session_id: String,
    pub agent_id: String,
    /// The tool schemas advertised to the provider for the current request.
    pub enabled_tools: Vec<EnabledTool>,
    /// Compact descriptors for every tool registered in this agent's scoped
    /// registry.
    pub available_tools: Vec<EnabledTool>,
    /// Per-agent execution state shared by tools that need a current directory.
    pub tool_activation: Arc<ToolActivation>,
    /// A deliberately reduced, read-only snapshot for delegated agents.
    pub conversation_context: Vec<crate::provider::ChatMessage>,
    pub agent_runner: Option<Arc<dyn AgentRunner>>,
    pub memory_search_backend: Option<Arc<dyn MemorySearchBackend>>,
    /// Receives live events from a tool's delegated child agent and forwards
    /// them to the current conversation stream.
    pub agent_event_sink:
        Option<tokio::sync::mpsc::UnboundedSender<crate::agent::AgentStreamEvent>>,
}

impl ToolContext {
    /// Current base directory for relative tool paths. It starts at the
    /// workspace root and can change without changing the workspace itself.
    pub fn current_dir(&self) -> PathBuf {
        self.tool_activation.current_directory(&self.working_dir)
    }

    pub fn set_current_dir(&self, path: PathBuf) {
        self.tool_activation.set_current_directory(path);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnabledTool {
    pub name: String,
    pub description: String,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolContext")
            .field("working_dir", &self.working_dir)
            .field("current_dir", &self.current_dir())
            .field("session_id", &self.session_id)
            .field("agent_id", &self.agent_id)
            .field("enabled_tools", &self.enabled_tools.len())
            .field("available_tools", &self.available_tools.len())
            .field("conversation_context", &self.conversation_context.len())
            .field("agent_runner", &self.agent_runner.is_some())
            .field(
                "memory_search_backend",
                &self.memory_search_backend.is_some(),
            )
            .field("agent_event_sink", &self.agent_event_sink.is_some())
            .finish()
    }
}

/// The selected subset of a scoped registry whose full schemas are sent to a
/// provider. It is intentionally independent from persisted bindings so a
/// registry addition never requires a hard-coded allow-list migration.
#[derive(Default)]
pub struct ToolActivation {
    names: RwLock<BTreeSet<String>>,
    current_directory: RwLock<Option<PathBuf>>,
    approval_policy: RwLock<ToolApprovalPolicy>,
}

impl ToolActivation {
    pub fn new(names: impl IntoIterator<Item = String>) -> Self {
        let activation = Self::default();
        activation.names.write().unwrap().extend(
            names
                .into_iter()
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty()),
        );
        activation
    }

    pub fn names(&self) -> Vec<String> {
        self.names.read().unwrap().iter().cloned().collect()
    }

    pub fn approval_policy(&self) -> ToolApprovalPolicy {
        *self.approval_policy.read().unwrap()
    }

    pub fn set_approval_policy(&self, policy: ToolApprovalPolicy) {
        *self.approval_policy.write().unwrap() = policy;
    }

    pub(crate) fn current_directory(&self, fallback: &Path) -> PathBuf {
        self.current_directory
            .read()
            .unwrap()
            .clone()
            .unwrap_or_else(|| fallback.to_path_buf())
    }

    fn set_current_directory(&self, path: PathBuf) {
        *self.current_directory.write().unwrap() = Some(path);
    }

    pub fn enabled(&self, catalog: &[EnabledTool]) -> Vec<EnabledTool> {
        let catalog = catalog
            .iter()
            .map(|tool| (tool.name.as_str(), tool))
            .collect::<BTreeMap<_, _>>();
        self.names
            .read()
            .unwrap()
            .iter()
            .filter_map(|name| catalog.get(name.as_str()).cloned().cloned())
            .collect()
    }

    pub fn enable(
        &self,
        catalog: &[EnabledTool],
        requested: impl IntoIterator<Item = String>,
    ) -> Result<Vec<EnabledTool>> {
        let available = catalog
            .iter()
            .map(|tool| (tool.name.as_str(), tool))
            .collect::<BTreeMap<_, _>>();
        let requested = requested
            .into_iter()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect::<BTreeSet<_>>();
        let missing = requested
            .iter()
            .filter(|name| !available.contains_key(name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: "enable_tools".into(),
                message: format!("unknown tool name(s): {}", missing.join(", ")),
            });
        }
        self.names.write().unwrap().extend(requested);
        Ok(self.enabled(catalog))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub content: String,
    /// Provider-ready images returned by a tool. The textual projection stays
    /// in `content` so existing tools and non-vision models keep working.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::provider::types::ImageSource>,
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
            images: Vec::new(),
            error: None,
            metadata: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            content: String::new(),
            images: Vec::new(),
            error: Some(message.into()),
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn with_image(
        mut self,
        media_type: impl Into<String>,
        base64_data: impl Into<String>,
    ) -> Self {
        self.images.push(crate::provider::types::ImageSource {
            media_type: media_type.into(),
            data: base64_data.into(),
        });
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

    /// Whether this particular invocation needs approval. Mixed tools can
    /// keep read-only actions available while gating only state-changing ones.
    fn requires_confirmation_for(&self, params: &Value) -> bool {
        let _ = params;
        self.requires_confirmation()
    }

    /// Legacy marker retained for integrations that used to define a bootstrap
    /// catalogue. The application exposes every registered tool immediately.
    fn is_bootstrap(&self) -> bool {
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
        assert!(result.images.is_empty());
        assert!(result.error.is_none());
        assert!(result.metadata.is_none());
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult::error("something went wrong");
        assert!(!result.success);
        assert_eq!(result.content, "");
        assert_eq!(result.error, Some("something went wrong".into()));
        assert!(result.images.is_empty());
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

    #[test]
    fn test_tool_result_with_image() {
        let result = ToolResult::ok("screenshot").with_image("image/png", "aW1hZ2U=");

        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].media_type, "image/png");
        assert_eq!(result.images[0].data, "aW1hZ2U=");
    }
}

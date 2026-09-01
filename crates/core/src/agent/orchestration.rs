use crate::agent::AgentStreamEvent;
use crate::provider::ChatMessage;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
}

impl Default for AgentDescriptor {
    fn default() -> Self {
        Self {
            id: "default".into(),
            name: "Default agent".into(),
            description: "A focused delegated worker for the current task.".into(),
            connection_id: None,
            model_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentCallRequest {
    pub parent_session_id: String,
    pub parent_agent_id: String,
    /// Stable identifier for the bilateral delegated-agent thread.
    pub thread_id: String,
    /// The objective inherited from the parent conversation.
    pub parent_objective: String,
    /// The configured delegated agent to run.
    pub agent_id: String,
    /// The parent's currently enabled tools, copied into the child agent.
    pub tools: Vec<String>,
    pub prompt: String,
    pub model_id: Option<String>,
    pub working_dir: PathBuf,
    pub context: Vec<ChatMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentThreadSnapshot {
    pub id: String,
    /// Explicit alias for `id` used by bilateral agent communication.
    #[serde(default)]
    pub thread_id: String,
    #[serde(default)]
    pub agent_id: String,
    pub parent_session_id: String,
    pub title: String,
    pub model_id: String,
    pub status: AgentThreadStatus,
    pub prompt: String,
    pub output: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentThreadStatus {
    Running,
    Completed,
    Failed,
}

#[async_trait]
pub trait AgentRunner: Send + Sync {
    async fn list_agents(&self, parent_session_id: &str) -> Result<Vec<AgentDescriptor>, String>;
    async fn call_agent(&self, request: AgentCallRequest) -> Result<AgentThreadSnapshot, String>;

    /// Runs a delegated agent while forwarding its live stream to the parent.
    /// Implementations that do not support streaming retain the old behavior,
    /// which keeps lightweight test runners and external integrations working.
    async fn call_agent_streaming(
        &self,
        request: AgentCallRequest,
        _events: tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>,
    ) -> Result<AgentThreadSnapshot, String> {
        self.call_agent(request).await
    }
}

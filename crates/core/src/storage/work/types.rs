use crate::agent::orchestration::AgentThreadSnapshot;
use crate::agent::ContextUsage;
use crate::connection::{ConnectionId, SessionBinding};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkProject {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub created_at: i64,
    pub last_opened_at: i64,
}

/// A persisted item in the first-run welcome checklist.
///
/// Some items are completed explicitly by the user, while others are kept in
/// sync with real application state by the UI. Keeping both kinds in SQLite
/// makes an interrupted setup resumable across launches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkOnboardingStep {
    pub id: String,
    pub completed: bool,
    pub completed_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkWindowMode {
    Windowed,
    Maximized,
    Fullscreen,
}

/// Restorable state for one native Averroes window.
///
/// Conversation contents remain normalized in their existing tables; this
/// record only remembers which conversations were open in each window and
/// the native window geometry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkWindowState {
    pub id: String,
    pub session_ids: Vec<String>,
    pub active_session_id: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub mode: WorkWindowMode,
}

/// A virtual project/folder that groups conversations inside one workspace.
/// It is intentionally not a filesystem path: the workspace remains the
/// execution context, while this entity is only a navigation organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkConversationFolder {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkMessageRole {
    User,
    Assistant,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkToolActivityState {
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkToolActivity {
    pub call_id: Option<String>,
    pub name: String,
    pub text_offset: usize,
    pub group_id: Option<usize>,
    pub input: String,
    pub summary: String,
    pub output: String,
    pub state: WorkToolActivityState,
    pub duration_ms: Option<u64>,
    pub expanded: bool,
    pub inside_reasoning: bool,
}

fn default_reasoning_complete() -> bool {
    true
}

impl WorkMessageRole {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Error => "error",
        }
    }

    pub(super) fn parse(value: &str) -> Self {
        match value {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            _ => Self::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkMessage {
    pub role: WorkMessageRole,
    pub text: String,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default = "default_reasoning_complete")]
    pub reasoning_complete: bool,
    #[serde(default)]
    pub reasoning_expanded: bool,
    #[serde(default)]
    pub tool_activities: Vec<WorkToolActivity>,
    #[serde(default)]
    pub expanded_tool_groups: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
}

impl CheckpointStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
        }
    }

    pub(super) fn parse(value: &str) -> Self {
        match value {
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "blocked" => Self::Blocked,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkCheckpoint {
    pub id: String,
    pub title: String,
    pub status: CheckpointStatus,
    pub detail: Option<String>,
    /// Position of the conversation message that generated this checkpoint.
    /// Older databases may not have this value and use `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_position: Option<usize>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Done,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
        }
    }

    pub(super) fn parse(value: &str) -> Self {
        match value {
            "done" => Self::Done,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkTask {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkSource {
    pub key: String,
    pub kind: String,
    pub label: String,
    /// Canonical page URL for web-backed sources. Tool sources leave this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Page title returned by the browser or search provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub detail: Option<String>,
    pub count: u32,
    pub last_used_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkConversation {
    pub id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub pinned: bool,
    #[serde(default)]
    pub unread: bool,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub binding: SessionBinding,
    /// The latest compact model-generated understanding of this conversation.
    /// It is indexed separately from the visible transcript and injected when
    /// an agent resumes the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_summary: Option<String>,
    /// Provider-reported usage for the latest request in this conversation.
    /// Unknown values stay unknown across restarts instead of being estimated.
    #[serde(default)]
    pub context_usage: ContextUsage,
    #[serde(default)]
    pub messages: Vec<WorkMessage>,
    #[serde(default)]
    pub checkpoints: Vec<WorkCheckpoint>,
    #[serde(default)]
    pub tasks: Vec<WorkTask>,
    #[serde(default)]
    pub sources: Vec<WorkSource>,
    #[serde(default)]
    pub agent_threads: Vec<AgentThreadSnapshot>,
    #[serde(default)]
    pub agent_thread_transcripts: HashMap<String, Vec<WorkMessage>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub pinned: bool,
    pub unread: bool,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub connection_id: ConnectionId,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedConversationFragment {
    pub conversation_id: String,
    pub message_position: usize,
    pub chunk_index: usize,
    pub title: String,
    pub project_id: Option<String>,
    pub updated_at: i64,
    pub text: String,
    pub content_hash: String,
    pub connection_id: ConnectionId,
    pub embedding: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VectorSearchHit {
    pub conversation_id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub updated_at: i64,
    pub text: String,
    /// sqlite-vector-rs returns cosine distance, where zero is an exact match.
    pub distance: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSearchResult {
    pub conversation_id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub snippet: String,
    pub updated_at: i64,
    pub score: u32,
}

/// A deliberately bounded, user-visible slice of a past conversation.
/// Reasoning, tool arguments and tool output are excluded from deep memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepMemoryExcerpt {
    pub conversation_id: String,
    pub title: String,
    pub context_summary: Option<String>,
    pub messages: Vec<DeepMemoryMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepMemoryMessage {
    pub position: usize,
    pub role: WorkMessageRole,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingIndexStatus {
    pub config: Option<EmbeddingConfig>,
    pub total_conversations: usize,
    pub indexed_conversations: usize,
    pub indexed_fragments: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationDocument {
    pub id: String,
    pub context_summary: Option<String>,
    pub messages: Vec<WorkMessage>,
}

#[derive(Debug, Clone)]
pub struct CheckpointEvent {
    pub session_id: String,
    pub checkpoint: WorkCheckpoint,
}

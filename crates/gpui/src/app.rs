use crate::i18n;
use crate::runtime::AppRuntime;
use crate::session::SessionId;
use crate::shortcuts::{CloseSession, FocusInput, NewSession, Quit, SendMessage, ToggleSettings};
use crate::tool_groups::{summarize_tool_names, ToolGroupEvent, ToolGroupTracker};
use crate::ui::{
    markdown::{normalize_reasoning_for_display, render_markdown, render_streaming_markdown},
    provider_logo, tool_icon, UiTheme,
};
use crate::update::{open_installer, UpdateClient, UpdateInfo, UpdateState};
use crate::version::APP_VERSION;
use averroes_core::agent::orchestration::{AgentThreadSnapshot, AgentThreadStatus};
use averroes_core::agent::{Agent, AgentStreamEvent, ContextUsage};
use averroes_core::codex::CodexAccount;
use averroes_core::config::AgentProfile;
use averroes_core::connection::{ConnectionId, ConnectionKind, ConnectionProfile, SessionBinding};
use averroes_core::diagnostics::{self, DiagnosticLevel};
use averroes_core::models::ManualModel;
use averroes_core::provider::types::{ChatMessage, MessageContent, Role};
use averroes_core::provider::{ModelInfo, ModelSource};
use averroes_core::work::{
    now, CheckpointStatus, ConversationSearchResult, ConversationSummary, EmbeddingConfig,
    EmbeddingIndexStatus, TaskStatus, WorkCheckpoint, WorkConversation, WorkMessage,
    WorkMessageRole, WorkProject, WorkSource, WorkTask,
};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, img, list, px, Anchor, Animation, AnimationExt as _, AnyElement, App, AppContext,
    ClipboardItem, Context, Entity, FollowMode, FontWeight, FutureExt as _, InteractiveElement,
    IntoElement, ListAlignment, ListOffset, ListState, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Subscription, Task, Transformation, Window,
};
use gpui_component::button::{Button, ButtonRounded, ButtonVariant, ButtonVariants};
use gpui_component::dialog::DialogButtonProps;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::link::Link;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::select::{
    SearchableVec, Select, SelectEvent, SelectGroup, SelectItem, SelectState,
};
use gpui_component::text::TextView;
use gpui_component::{Disableable, Icon, IconName, Root as ComponentRoot, Sizable, WindowExt as _};
use semver::Version;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

// Keep the main window responsive while the provider emits many small deltas.
// A completed batch is still applied immediately when the stream goes idle.
// Thirty frames per second is enough for text streaming and avoids forcing a
// full virtual-list remeasure for every small provider delta. Tool lifecycle
// events still bypass this window and are painted as soon as they arrive.
const STREAM_UI_BATCH_WINDOW: Duration = Duration::from_millis(32);
const STREAM_UI_MAX_EVENTS: usize = 64;
const STREAM_MESSAGE_FADE_DURATION: Duration = Duration::from_millis(260);

fn stream_event_requires_immediate_flush(event: &AgentStreamEvent) -> bool {
    matches!(
        event,
        AgentStreamEvent::ToolPreparing { .. }
            | AgentStreamEvent::ToolStarted { .. }
            | AgentStreamEvent::ToolFinished { .. }
            | AgentStreamEvent::ReasoningFinished
            | AgentStreamEvent::ContextUpdated { .. }
            | AgentStreamEvent::CompactionStarted { .. }
            | AgentStreamEvent::CompactionFinished { .. }
            | AgentStreamEvent::DelegatedAgentStarted { .. }
    ) || matches!(
        event,
        AgentStreamEvent::DelegatedAgentEvent { event, .. }
            if stream_event_requires_immediate_flush(event)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Chat,
    Connections,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Connections,
    Models,
    Agents,
    Diagnostics,
    Storage,
    About,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageRole {
    User,
    Assistant,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolActivityState {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone)]
struct ToolActivity {
    /// Provider tool-call ID, when the provider exposes one. This lets the
    /// streaming preview turn into the real execution row without creating a
    /// duplicate entry for the same call.
    call_id: Option<String>,
    name: String,
    /// Byte position in the assistant text at which the tool was invoked.
    /// Stream deltas only append, so this preserves the provider's temporal
    /// ordering without retaining every individual delta as a UI node.
    text_offset: usize,
    /// Visible non-reasoning activities share a group until assistant text
    /// arrives. Reasoning activities intentionally have no group.
    group_id: Option<usize>,
    input: String,
    summary: String,
    output: String,
    state: ToolActivityState,
    started_at: Instant,
    duration_ms: Option<u64>,
    expanded: bool,
    inside_reasoning: bool,
}

#[derive(Clone)]
struct ShellMessage {
    role: MessageRole,
    text: String,
    reasoning: String,
    reasoning_complete: bool,
    reasoning_expanded: bool,
    animate_in: bool,
    tool_activities: Vec<ToolActivity>,
    tool_groups: ToolGroupTracker,
    expanded_tool_groups: HashSet<usize>,
}

#[derive(Clone, Default)]
struct AgentThreadTranscript {
    messages: Vec<ShellMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposerAttachment {
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueuedMessage {
    text: String,
    attachments: Vec<ComposerAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelChoice {
    connection_id: ConnectionId,
    connection_name: SharedString,
    info: ModelInfo,
}

impl SelectItem for ModelChoice {
    type Value = Self;

    fn title(&self) -> SharedString {
        self.info.display_name.clone().into()
    }

    fn value(&self) -> &Self::Value {
        self
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.to_ascii_lowercase();
        self.info.display_name.to_ascii_lowercase().contains(&query)
            || self.info.id.to_ascii_lowercase().contains(&query)
            || self.connection_name.to_ascii_lowercase().contains(&query)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceChoice {
    id: Option<String>,
    name: SharedString,
    root: PathBuf,
}

impl SelectItem for WorkspaceChoice {
    type Value = Self;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn value(&self) -> &Self::Value {
        self
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.to_ascii_lowercase();
        self.name.to_ascii_lowercase().contains(&query)
            || self
                .root
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains(&query)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConnectionKindChoice {
    kind: ConnectionKind,
    label: SharedString,
}

impl ConnectionKindChoice {
    fn new(kind: ConnectionKind, label: SharedString) -> Self {
        Self { kind, label }
    }
}

impl SelectItem for ConnectionKindChoice {
    type Value = ConnectionKind;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.kind
    }
}

impl ShellMessage {
    fn user(text: String) -> Self {
        Self {
            role: MessageRole::User,
            text,
            reasoning: String::new(),
            reasoning_complete: true,
            reasoning_expanded: false,
            animate_in: false,
            tool_activities: Vec::new(),
            tool_groups: ToolGroupTracker::default(),
            expanded_tool_groups: HashSet::new(),
        }
    }

    fn assistant() -> Self {
        Self {
            role: MessageRole::Assistant,
            text: String::new(),
            reasoning: String::new(),
            reasoning_complete: false,
            reasoning_expanded: false,
            animate_in: true,
            tool_activities: Vec::new(),
            tool_groups: ToolGroupTracker::default(),
            expanded_tool_groups: HashSet::new(),
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Error,
            text: text.into(),
            reasoning: String::new(),
            reasoning_complete: true,
            reasoning_expanded: false,
            animate_in: false,
            tool_activities: Vec::new(),
            tool_groups: ToolGroupTracker::default(),
            expanded_tool_groups: HashSet::new(),
        }
    }

    fn assign_tool_group(&mut self, inside_reasoning: bool) -> Option<usize> {
        self.tool_groups
            .apply(ToolGroupEvent::Tool { inside_reasoning })
    }

    fn assistant_text_arrived(&mut self) {
        self.tool_groups.close_on_assistant_text();
    }

    fn reasoning_arrived(&mut self) {
        self.tool_groups.apply(ToolGroupEvent::Reasoning);
    }

    fn is_tool_group_expanded(&self, group_id: usize) -> bool {
        self.expanded_tool_groups.contains(&group_id)
    }

    fn toggle_tool_group(&mut self, group_id: usize) {
        if !self.expanded_tool_groups.insert(group_id) {
            self.expanded_tool_groups.remove(&group_id);
        }
    }

    fn active_tool_group(&self) -> Option<usize> {
        self.tool_groups.active_group_id()
    }
}

struct ShellSession {
    id: SessionId,
    title: String,
    binding: SessionBinding,
    messages: Vec<ShellMessage>,
    agent: Option<Arc<Agent>>,
    processing: bool,
    task: Option<Task<()>>,
    project_id: Option<String>,
    workspace_root: Option<PathBuf>,
    pinned: bool,
    unread: bool,
    persisted: bool,
    context_summary: Option<String>,
    checkpoints: Vec<WorkCheckpoint>,
    tasks: Vec<WorkTask>,
    sources: Vec<WorkSource>,
    queued_messages: Vec<QueuedMessage>,
    queue_autostart: bool,
    pending_user_question: Option<averroes_core::tool::builtin::ask_user::UserQuestion>,
    context_usage: ContextUsage,
    agent_threads: Vec<AgentThreadSnapshot>,
    agent_thread_transcripts: HashMap<String, AgentThreadTranscript>,
    context_busy: bool,
    created_at: i64,
}

impl ShellSession {
    fn new(project: Option<&WorkProject>, binding: SessionBinding) -> Self {
        Self {
            id: SessionId(uuid::Uuid::new_v4().to_string()),
            title: "New conversation".into(),
            binding,
            messages: Vec::new(),
            agent: None,
            processing: false,
            task: None,
            project_id: project.map(|project| project.id.clone()),
            workspace_root: project.map(|project| project.root.clone()),
            pinned: false,
            unread: false,
            persisted: false,
            context_summary: None,
            checkpoints: Vec::new(),
            tasks: Vec::new(),
            sources: Vec::new(),
            queued_messages: Vec::new(),
            queue_autostart: false,
            pending_user_question: None,
            context_usage: ContextUsage::unknown(0),
            agent_threads: Vec::new(),
            agent_thread_transcripts: HashMap::new(),
            context_busy: false,
            created_at: now(),
        }
    }

    fn from_work(conversation: WorkConversation, projects: &[WorkProject]) -> Self {
        let workspace_root = conversation.project_id.as_ref().and_then(|project_id| {
            projects
                .iter()
                .find(|project| &project.id == project_id)
                .map(|project| project.root.clone())
        });
        Self {
            id: SessionId(conversation.id),
            title: conversation.title,
            binding: conversation.binding,
            messages: conversation
                .messages
                .into_iter()
                .map(|message| ShellMessage {
                    role: match message.role {
                        WorkMessageRole::User => MessageRole::User,
                        WorkMessageRole::Assistant => MessageRole::Assistant,
                        WorkMessageRole::Error => MessageRole::Error,
                    },
                    text: message.text,
                    reasoning: message.reasoning,
                    reasoning_complete: true,
                    reasoning_expanded: false,
                    animate_in: false,
                    tool_activities: Vec::new(),
                    tool_groups: ToolGroupTracker::default(),
                    expanded_tool_groups: HashSet::new(),
                })
                .collect(),
            agent: None,
            processing: false,
            task: None,
            project_id: conversation.project_id,
            workspace_root,
            pinned: conversation.pinned,
            unread: conversation.unread,
            persisted: true,
            context_summary: conversation.context_summary,
            checkpoints: conversation.checkpoints,
            tasks: conversation.tasks,
            sources: normalize_tool_sources(conversation.sources),
            queued_messages: Vec::new(),
            queue_autostart: false,
            pending_user_question: None,
            context_usage: conversation.context_usage,
            agent_threads: Vec::new(),
            agent_thread_transcripts: HashMap::new(),
            context_busy: false,
            created_at: conversation.created_at,
        }
    }

    fn snapshot(&self) -> WorkConversation {
        let timestamp = now();
        WorkConversation {
            id: self.id.to_string(),
            title: self.title.clone(),
            project_id: self.project_id.clone(),
            pinned: self.pinned,
            unread: self.unread,
            created_at: self.created_at,
            updated_at: timestamp,
            binding: self.binding.clone(),
            context_summary: self.context_summary.clone(),
            context_usage: self.context_usage,
            messages: self
                .messages
                .iter()
                .map(|message| WorkMessage {
                    role: match message.role {
                        MessageRole::User => WorkMessageRole::User,
                        MessageRole::Assistant => WorkMessageRole::Assistant,
                        MessageRole::Error => WorkMessageRole::Error,
                    },
                    text: message.text.clone(),
                    reasoning: message.reasoning.clone(),
                })
                .collect(),
            checkpoints: self.checkpoints.clone(),
            tasks: self.tasks.clone(),
            sources: self.sources.clone(),
        }
    }
}

#[derive(Clone)]
struct Notice {
    success: bool,
    text: String,
}

pub struct AverroesApp {
    runtime: Arc<AppRuntime>,
    route: Route,
    settings_tab: SettingsTab,
    sessions: Vec<ShellSession>,
    active_session: usize,
    remembered_binding: SessionBinding,
    composer: Entity<TextareaState>,
    connection_select: Entity<SelectState<Vec<SharedString>>>,
    workspace_select: Entity<SelectState<Vec<WorkspaceChoice>>>,
    model_select: Entity<SelectState<SearchableVec<SelectGroup<ModelChoice>>>>,
    reasoning_select: Entity<SelectState<Vec<SharedString>>>,
    kind_select: Entity<SelectState<Vec<ConnectionKindChoice>>>,
    connection_labels: Vec<(SharedString, ConnectionId)>,
    model_choices: Vec<ModelChoice>,
    selected_kind: Option<ConnectionKind>,
    name_input: Entity<InputState>,
    url_input: Entity<InputState>,
    key_input: Entity<InputState>,
    diagnostics_search: Entity<InputState>,
    conversation_search: Entity<InputState>,
    ask_user_input: Entity<InputState>,
    conversation_search_open: bool,
    conversation_search_results: Vec<ConversationSearchResult>,
    conversation_search_generation: u64,
    embedding_connection_select: Entity<SelectState<Vec<SharedString>>>,
    embedding_model_select: Entity<SelectState<Vec<SharedString>>>,
    embedding_connection_labels: Vec<(SharedString, ConnectionId)>,
    embedding_model_labels: Vec<(SharedString, String)>,
    embedding_connection_id: Option<ConnectionId>,
    embedding_model_id: Option<String>,
    agent_connection_select: Entity<SelectState<Vec<SharedString>>>,
    agent_model_select: Entity<SelectState<Vec<SharedString>>>,
    agent_connection_labels: Vec<(SharedString, ConnectionId)>,
    agent_model_labels: Vec<(SharedString, String)>,
    agent_form_connection_id: Option<ConnectionId>,
    agent_form_model_id: Option<String>,
    agent_id_input: Entity<InputState>,
    agent_name_input: Entity<InputState>,
    agent_description_input: Entity<InputState>,
    editing_agent_id: Option<String>,
    embedding_index_busy: bool,
    background_indexing: bool,
    background_index_scheduled: bool,
    embedding_status: Option<EmbeddingIndexStatus>,
    manual_model_connection: Option<ConnectionId>,
    manual_model_id_input: Entity<InputState>,
    manual_model_name_input: Entity<InputState>,
    manual_model_reasoning_input: Entity<InputState>,
    show_manual_copilot_token: bool,
    notice: Option<Notice>,
    update_state: UpdateState,
    update_prompt_shown: bool,
    update_open_error: Option<String>,
    update_open_in_flight: bool,
    attachments: Vec<ComposerAttachment>,
    codex_account: Option<CodexAccount>,
    codex_busy: bool,
    copilot_busy: bool,
    projects: Vec<WorkProject>,
    conversations: Vec<ConversationSummary>,
    projects_expanded: bool,
    show_sources: bool,
    show_tool_activity: bool,
    show_context: bool,
    conversation_list: ListState,
    conversation_list_session: Option<SessionId>,
    selected_agent_thread: Option<String>,
    agent_thread_view: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl AverroesApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, runtime: Arc<AppRuntime>) -> Self {
        diagnostics::record(
            DiagnosticLevel::Info,
            "app",
            "Averroes UI started; refreshing connected provider catalogs.",
        );
        let connections = runtime.connections();
        let connection_labels: Vec<(SharedString, ConnectionId)> = connections
            .iter()
            .map(|profile| (profile.name.clone().into(), profile.id.clone()))
            .collect::<Vec<_>>();
        let connection_items = connection_labels
            .iter()
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>();
        let projects = runtime.database.projects().unwrap_or_default();
        let workspace_items = workspace_choices(&projects);
        let stored_binding = runtime.database.last_binding().ok().flatten();
        let mut remembered_binding = stored_binding
            .filter(|binding| {
                binding.is_ready()
                    && binding
                        .connection_id
                        .as_ref()
                        .is_some_and(|id| connections.iter().any(|connection| &connection.id == id))
            })
            .unwrap_or_default();
        let _ = ensure_binding_tools(&mut remembered_binding, &runtime.default_agent_tools());

        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 7)
                .submit_on_enter(true)
                .placeholder(i18n::text(cx, "composer.placeholder"))
        });
        let connection_select =
            cx.new(|cx| SelectState::new(connection_items, None, window, cx).searchable(true));
        let workspace_select =
            cx.new(|cx| SelectState::new(workspace_items, None, window, cx).searchable(true));
        let model_choices = initial_model_choices(&runtime);
        let model_select = cx.new(|cx| {
            SelectState::new(grouped_model_items(&model_choices), None, window, cx).searchable(true)
        });
        let reasoning_select =
            cx.new(|cx| SelectState::new(vec![i18n::text(cx, "reasoning.auto")], None, window, cx));
        let kind_select = cx.new(|cx| {
            SelectState::new(
                vec![
                    ConnectionKindChoice::new(
                        ConnectionKind::QDivZero,
                        i18n::text(cx, "connection.qdivzero"),
                    ),
                    ConnectionKindChoice::new(
                        ConnectionKind::Codex,
                        i18n::text(cx, "connection.codex"),
                    ),
                    ConnectionKindChoice::new(
                        ConnectionKind::Copilot,
                        i18n::text(cx, "connection.copilot"),
                    ),
                    ConnectionKindChoice::new(
                        ConnectionKind::OpenAi,
                        i18n::text(cx, "connection.openai"),
                    ),
                    ConnectionKindChoice::new(
                        ConnectionKind::Anthropic,
                        i18n::text(cx, "connection.anthropic"),
                    ),
                    ConnectionKindChoice::new(
                        ConnectionKind::DeepSeek,
                        i18n::text(cx, "connection.deepseek"),
                    ),
                    ConnectionKindChoice::new(
                        ConnectionKind::Groq,
                        i18n::text(cx, "connection.groq"),
                    ),
                    ConnectionKindChoice::new(
                        ConnectionKind::Ollama,
                        i18n::text(cx, "connection.ollama_local"),
                    ),
                    ConnectionKindChoice::new(
                        ConnectionKind::OllamaCloud,
                        i18n::text(cx, "connection.ollama_cloud"),
                    ),
                    ConnectionKindChoice::new(
                        ConnectionKind::Compatible,
                        i18n::text(cx, "connection.compatible"),
                    ),
                ],
                None,
                window,
                cx,
            )
        });
        let name_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::text(cx, "placeholder.connection_name"))
        });
        let url_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::text(cx, "placeholder.base_url"))
        });
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n::text(cx, "placeholder.api_key"))
                .masked(true)
        });
        let diagnostics_search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n::text(cx, "placeholder.search_diagnostics"))
        });
        let conversation_search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n::text(cx, "placeholder.search_conversations"))
        });
        let ask_user_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::text(cx, "placeholder.answer"))
        });
        let embedding_profiles = runtime.embedding_connections();
        let embedding_connection_labels: Vec<(SharedString, ConnectionId)> = embedding_profiles
            .iter()
            .map(|profile| (profile.name.clone().into(), profile.id.clone()))
            .collect::<Vec<_>>();
        let stored_embedding_config = runtime.database.embedding_config().ok().flatten();
        let embedding_connection_id = stored_embedding_config
            .as_ref()
            .map(|config| config.connection_id.clone())
            .filter(|id| {
                embedding_connection_labels
                    .iter()
                    .any(|(_, candidate)| candidate == id)
            });
        let embedding_model_id = stored_embedding_config.map(|config| config.model_id);
        let embedding_model_labels =
            embedding_model_choices(&runtime, embedding_connection_id.as_ref());
        let embedding_connection_select = cx.new(|cx| {
            SelectState::new(
                embedding_connection_labels
                    .iter()
                    .map(|(label, _)| label.clone())
                    .collect(),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let embedding_model_select = cx.new(|cx| {
            SelectState::new(
                embedding_model_labels
                    .iter()
                    .map(|(label, _)| label.clone())
                    .collect(),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let agent_connection_labels = connection_labels.clone();
        let agent_connection_select = cx.new(|cx| {
            SelectState::new(
                agent_connection_labels
                    .iter()
                    .map(|(label, _)| label.clone())
                    .collect(),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let agent_model_labels = Vec::new();
        let agent_model_select = cx.new(|cx| {
            SelectState::new(Vec::<SharedString>::new(), None, window, cx).searchable(true)
        });
        let agent_id_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::text(cx, "placeholder.agent_id"))
        });
        let agent_name_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::text(cx, "placeholder.agent_name"))
        });
        let agent_description_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::text(cx, "placeholder.agent_description"))
        });
        let manual_model_id_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::text(cx, "placeholder.model_id"))
        });
        let manual_model_name_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::text(cx, "placeholder.display_name"))
        });
        let manual_model_reasoning_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::text(cx, "placeholder.reasoning_levels"))
        });

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &composer,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    this.submit_message(window, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &connection_select,
            window,
            |this, _, event: &SelectEvent<Vec<SharedString>>, window, cx| {
                let SelectEvent::Confirm(value) = event;
                this.select_connection(value.as_ref(), window, cx);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &workspace_select,
            window,
            |this, _, event: &SelectEvent<Vec<WorkspaceChoice>>, _, cx| {
                let SelectEvent::Confirm(value) = event;
                this.select_workspace(value.as_ref(), cx);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &model_select,
            window,
            |this, _, event: &SelectEvent<SearchableVec<SelectGroup<ModelChoice>>>, window, cx| {
                let SelectEvent::Confirm(value) = event;
                this.select_model(value.as_ref(), window, cx);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &reasoning_select,
            window,
            |this, _, event: &SelectEvent<Vec<SharedString>>, window, cx| {
                let SelectEvent::Confirm(value) = event;
                this.select_reasoning_effort(value.as_ref(), window, cx);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &kind_select,
            window,
            |this, _, event: &SelectEvent<Vec<ConnectionKindChoice>>, window, cx| {
                let SelectEvent::Confirm(value) = event;
                this.selected_kind = value.as_ref().copied();
                diagnostics::record(
                    DiagnosticLevel::Info,
                    "settings.provider",
                    format!(
                        "Provider form selection confirmed: {:?}.",
                        this.selected_kind
                    ),
                );
                this.show_manual_copilot_token = false;
                if this.selected_kind == Some(ConnectionKind::Copilot) {
                    this.key_input
                        .update(cx, |state, cx| state.set_value("", window, cx));
                }
                this.notice = None;
                cx.notify();
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &agent_connection_select,
            window,
            |this, _, event: &SelectEvent<Vec<SharedString>>, window, cx| {
                let SelectEvent::Confirm(value) = event;
                this.select_agent_connection(value.as_ref(), window, cx);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &agent_model_select,
            window,
            |this, _, event: &SelectEvent<Vec<SharedString>>, _, cx| {
                let SelectEvent::Confirm(value) = event;
                this.select_agent_model(value.as_ref(), cx);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &diagnostics_search,
            window,
            |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &conversation_search,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.refresh_conversation_search(cx);
                }
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    this.search_conversations_semantically(window, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &ask_user_input,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    let session_id = this.active().id.clone();
                    this.submit_user_question_answer(&session_id, window, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &embedding_connection_select,
            window,
            |this, _, event: &SelectEvent<Vec<SharedString>>, window, cx| {
                let SelectEvent::Confirm(value) = event;
                this.select_embedding_connection(value.as_ref(), window, cx);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &embedding_model_select,
            window,
            |this, _, event: &SelectEvent<Vec<SharedString>>, _, cx| {
                let SelectEvent::Confirm(value) = event;
                this.select_embedding_model(value.as_ref(), cx);
            },
        ));
        subscriptions.push(cx.observe_window_appearance(window, |_, window, cx| {
            UiTheme::sync_component_theme(window, cx);
            cx.notify();
        }));

        let should_probe_codex = connections
            .iter()
            .any(|connection| connection.kind == ConnectionKind::Codex);
        let conversations = runtime
            .database
            .conversation_summaries(80)
            .unwrap_or_default();
        let embedding_status = runtime.database.embedding_index_status().ok();
        // Conversation entries have highly variable height (Markdown, tool
        // output, source cards). GPUI's variable-height list only lays out
        // nearby entries instead of rebuilding the complete history whenever
        // the user scrolls or a streamed delta arrives.
        let conversation_list = ListState::new(0, ListAlignment::Top, px(768.0));
        conversation_list.set_follow_mode(FollowMode::Tail);
        let mut app = Self {
            runtime,
            route: Route::Chat,
            settings_tab: SettingsTab::Models,
            sessions: vec![ShellSession::new(None, remembered_binding.clone())],
            active_session: 0,
            remembered_binding,
            composer,
            connection_select,
            workspace_select,
            model_select,
            reasoning_select,
            kind_select,
            connection_labels,
            model_choices,
            selected_kind: None,
            name_input,
            url_input,
            key_input,
            diagnostics_search,
            conversation_search,
            ask_user_input,
            conversation_search_open: false,
            conversation_search_results: Vec::new(),
            conversation_search_generation: 0,
            embedding_connection_select,
            embedding_model_select,
            embedding_connection_labels,
            embedding_model_labels,
            embedding_connection_id,
            embedding_model_id,
            agent_connection_select,
            agent_model_select,
            agent_connection_labels,
            agent_model_labels,
            agent_form_connection_id: None,
            agent_form_model_id: None,
            agent_id_input,
            agent_name_input,
            agent_description_input,
            editing_agent_id: None,
            embedding_index_busy: false,
            background_indexing: false,
            background_index_scheduled: false,
            embedding_status,
            manual_model_connection: None,
            manual_model_id_input,
            manual_model_name_input,
            manual_model_reasoning_input,
            show_manual_copilot_token: false,
            notice: connections.is_empty().then(|| Notice {
                success: true,
                text: i18n::text(cx, "notice.first_connection").to_string(),
            }),
            update_state: UpdateState::Idle,
            update_prompt_shown: false,
            update_open_error: None,
            update_open_in_flight: false,
            attachments: Vec::new(),
            codex_account: None,
            codex_busy: false,
            copilot_busy: false,
            projects,
            conversations,
            projects_expanded: true,
            show_sources: true,
            show_tool_activity: true,
            show_context: false,
            conversation_list,
            conversation_list_session: None,
            selected_agent_thread: None,
            agent_thread_view: None,
            _subscriptions: subscriptions,
        };
        app.sync_embedding_selectors(window, cx);
        if should_probe_codex {
            app.refresh_codex_account(cx);
        }
        app.sync_selectors_to_active(window, cx);
        // Refresh configured remote catalogs concurrently. Each Copilot
        // request re-discovers GitHub's current per-account API endpoint, so
        // model availability and routing never depend on a stale startup URL.
        app.refresh_model_catalogs(cx);

        app.schedule_background_indexing(cx);
        app.start_update_check(window, cx);
        app
    }

    fn start_update_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cfg!(target_os = "macos") {
            diagnostics::record(
                DiagnosticLevel::Info,
                "update.check",
                "Skipping update check on an unsupported platform.",
            );
            return;
        }

        if !update_check_can_start(&self.update_state) {
            diagnostics::record(
                DiagnosticLevel::Info,
                "update.check",
                "Skipping update check because one is already active or resolved.",
            );
            return;
        }

        let current_version = match Version::parse(APP_VERSION) {
            Ok(version) => version,
            Err(error) => {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "update.check",
                    format!("Could not parse app version {APP_VERSION:?}: {error}"),
                );
                return;
            }
        };

        self.update_state = UpdateState::Checking;
        cx.notify();

        let runtime = self.runtime.clone();
        let request = runtime.spawn_background(async move {
            let client = UpdateClient::new()?;
            client.check(&current_version).await
        });

        cx.spawn_in(window, async move |this, cx| {
            match request.await {
                Ok(Ok(Some(info))) => {
                    this.update_in(cx, |app, window, cx| {
                        diagnostics::record(
                            DiagnosticLevel::Info,
                            "update.check",
                            format!("Stable update available: {}.", info.version),
                        );
                        app.update_state = UpdateState::Available(info.clone());
                        app.show_update_dialog(info, window, cx);
                        cx.notify();
                    })?;
                }
                Ok(Ok(None)) => {
                    diagnostics::record(
                        DiagnosticLevel::Info,
                        "update.check",
                        "No stable update is available.",
                    );
                    this.update_in(cx, |app, _, cx| {
                        app.update_state = UpdateState::Idle;
                        cx.notify();
                    })?;
                }
                Ok(Err(error)) => {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "update.check",
                        format!("Update check failed: {error}"),
                    );
                    this.update_in(cx, |app, _, cx| {
                        app.update_state = UpdateState::Idle;
                        cx.notify();
                    })?;
                }
                Err(error) => {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "update.check",
                        format!("Update check task failed: {error}"),
                    );
                    this.update_in(cx, |app, _, cx| {
                        app.update_state = UpdateState::Idle;
                        cx.notify();
                    })?;
                }
            }

            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    fn show_update_dialog(
        &mut self,
        info: UpdateInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.update_prompt_shown {
            diagnostics::record(
                DiagnosticLevel::Info,
                "update.dialog",
                "Skipping duplicate update dialog.",
            );
            return;
        }

        self.update_prompt_shown = true;
        let fallback_info = info;
        let view = cx.entity();
        let localization = cx.global::<i18n::Localization>().clone();

        window.open_dialog(cx, move |dialog, _window, cx| {
            let (state, open_error, open_in_flight) = {
                let app = view.read(cx);
                (
                    app.update_state.clone(),
                    app.update_open_error.clone(),
                    app.update_open_in_flight,
                )
            };

            let info = match &state {
                UpdateState::Available(info)
                | UpdateState::Downloading(info)
                | UpdateState::ReadyToOpen { info, .. }
                | UpdateState::Failed {
                    info: Some(info), ..
                } => info.clone(),
                UpdateState::Idle
                | UpdateState::Checking
                | UpdateState::Failed { info: None, .. } => fallback_info.clone(),
            };

            let downloading = update_dialog_is_downloading(&state);
            let failed_download = matches!(state, UpdateState::Failed { .. });
            let ready_to_open = matches!(state, UpdateState::ReadyToOpen { .. });
            let retryable_open = update_dialog_can_retry_open(&state, open_error.as_deref());
            let opening = open_in_flight && ready_to_open && !retryable_open;
            let current_version = format!(
                "{}: {APP_VERSION}",
                localization.text("update.current_version")
            );
            let new_version = format!(
                "{}: {}",
                localization.text("update.new_version"),
                info.version
            );

            let mut body = div().flex().flex_col().gap(px(10.0));
            if downloading || opening {
                body = body.child(localization.text(if opening {
                    "update.opening"
                } else {
                    "update.downloading"
                }));
            } else {
                body = body
                    .child(localization.text("update.available_description"))
                    .child(
                        div()
                            .text_color(UiTheme::current(cx).muted)
                            .child(current_version),
                    )
                    .child(
                        div()
                            .text_color(UiTheme::current(cx).muted)
                            .child(new_version),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(localization.text("update.release_notes"))
                            .child(
                                div()
                                    .max_h(px(180.0))
                                    .overflow_y_scrollbar()
                                    .text_size(px(12.0))
                                    .text_color(UiTheme::current(cx).muted)
                                    .whitespace_normal()
                                    .child(info.release_notes.clone()),
                            )
                            .child(
                                Link::new("update-release-link")
                                    .href(info.release_url.clone())
                                    .child(info.release_url.clone()),
                            ),
                    );
            }

            if failed_download {
                body = body.child(
                    div()
                        .text_color(UiTheme::current(cx).destructive)
                        .child(localization.text("update.download_failed")),
                );
            }
            if open_error.is_some() {
                body = body.child(
                    div()
                        .text_color(UiTheme::current(cx).destructive)
                        .child(localization.text("update.open_failed")),
                );
            }

            let mut footer = div().flex().justify_end().gap(px(8.0));
            if downloading || opening {
                // Keep the dialog open while the request runs so the user gets
                // immediate state feedback and can retry if opening fails.
            } else if retryable_open {
                let retry_view = view.clone();
                let retry_info = info.clone();
                let retry_path = match &state {
                    UpdateState::ReadyToOpen { path, .. } => Some(path.clone()),
                    _ => None,
                };
                if let Some(path) = retry_path {
                    footer = footer.child(
                        Button::new("update-open-retry")
                            .primary()
                            .label(localization.text("update.retry"))
                            .on_click(move |_, window, cx| {
                                let path = path.clone();
                                let info = retry_info.clone();
                                let _ = retry_view.update(cx, |app, cx| {
                                    app.start_installer_open(info, path, window, cx);
                                });
                            }),
                    );
                }
            } else {
                let update_view = view.clone();
                let update_info = info.clone();
                footer = footer.child(
                    Button::new("update-dismiss")
                        .secondary()
                        .label(localization.text("update.not_now"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                );
                if matches!(state, UpdateState::Available(_)) {
                    footer = footer.child(
                        Button::new("update-start")
                            .primary()
                            .label(localization.text("update.update"))
                            .on_click(move |_, window, cx| {
                                let info = update_info.clone();
                                let _ = update_view.update(cx, |app, cx| {
                                    app.start_update_download(info, window, cx);
                                });
                            }),
                    );
                } else if matches!(state, UpdateState::Failed { info: Some(_), .. }) {
                    footer = footer.child(
                        Button::new("update-retry")
                            .primary()
                            .label(localization.text("update.retry"))
                            .on_click(move |_, window, cx| {
                                let info = update_info.clone();
                                let _ = update_view.update(cx, |app, cx| {
                                    app.start_update_download(info, window, cx);
                                });
                            }),
                    );
                }
            }

            dialog
                .title(localization.text("update.available_title"))
                .w(px(500.0))
                .close_button(!(downloading || opening))
                .overlay_closable(!(downloading || opening))
                .keyboard(!(downloading || opening))
                .child(body)
                .footer(footer)
        });
    }

    fn start_update_download(
        &mut self,
        info: UpdateInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let requested_version = info.version.clone();
        let Some((state, info)) =
            std::mem::replace(&mut self.update_state, UpdateState::Idle).begin_download()
        else {
            diagnostics::record(
                DiagnosticLevel::Info,
                "update.download",
                "Skipping download because another update operation is active or unavailable.",
            );
            return;
        };

        self.update_state = state;
        self.update_open_error = None;
        diagnostics::record(
            DiagnosticLevel::Info,
            "update.download",
            format!("Starting download for update {}.", requested_version),
        );
        cx.notify();

        let runtime = self.runtime.clone();
        let request = runtime.spawn_background(async move {
            let client = UpdateClient::new()?;
            client.download(&info).await
        });

        cx.spawn_in(window, async move |this, cx| {
            match request.await {
                Ok(Ok(path)) => {
                    this.update_in(cx, |app, window, cx| {
                        let state = std::mem::replace(&mut app.update_state, UpdateState::Idle)
                            .download_ready(path);
                        let UpdateState::ReadyToOpen { info, path } = state else {
                            diagnostics::record(
                                DiagnosticLevel::Warning,
                                "update.download",
                                "Download completed but update state was no longer downloading.",
                            );
                            app.update_state = state;
                            cx.notify();
                            return;
                        };

                        diagnostics::record(
                            DiagnosticLevel::Info,
                            "update.download",
                            format!("Update download completed for {}.", info.version),
                        );
                        app.update_state = UpdateState::ReadyToOpen {
                            info: info.clone(),
                            path: path.clone(),
                        };
                        app.start_installer_open(info, path, window, cx);
                        cx.notify();
                    })?;
                }
                Ok(Err(error)) => {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "update.download",
                        format!("Update download failed: {error}"),
                    );
                    this.update_in(cx, |app, _, cx| {
                        app.update_state =
                            std::mem::replace(&mut app.update_state, UpdateState::Idle)
                                .download_failed(error.to_string());
                        cx.notify();
                    })?;
                }
                Err(error) => {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "update.download",
                        format!("Update download task failed: {error}"),
                    );
                    this.update_in(cx, |app, _, cx| {
                        app.update_state =
                            std::mem::replace(&mut app.update_state, UpdateState::Idle)
                                .download_failed(error.to_string());
                        cx.notify();
                    })?;
                }
            }

            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    fn start_installer_open(
        &mut self,
        info: UpdateInfo,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.update_state, UpdateState::ReadyToOpen { .. }) {
            return;
        }
        if self.update_open_in_flight {
            diagnostics::record(
                DiagnosticLevel::Info,
                "update.open",
                "Skipping duplicate installer open while one is already in flight.",
            );
            return;
        }

        self.update_open_in_flight = true;
        self.update_open_error = None;
        cx.notify();

        diagnostics::record(
            DiagnosticLevel::Info,
            "update.open",
            format!("Opening installer for update {}.", info.version),
        );
        let request = self
            .runtime
            .runtime
            .spawn_blocking(move || open_installer(&path));

        cx.spawn_in(window, async move |this, cx| {
            match request.await {
                Ok(Ok(())) => {
                    diagnostics::record(
                        DiagnosticLevel::Info,
                        "update.open",
                        format!("Installer accepted for update {}.", info.version),
                    );
                    this.update_in(cx, |app, window, cx| {
                        app.update_open_in_flight = false;
                        window.close_dialog(cx);
                        cx.quit();
                    })?;
                }
                Ok(Err(error)) => {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "update.open",
                        format!("Could not open installer: {error}"),
                    );
                    this.update_in(cx, |app, _, cx| {
                        app.update_open_in_flight = false;
                        app.update_open_error = Some(error.to_string());
                        cx.notify();
                    })?;
                }
                Err(error) => {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "update.open",
                        format!("Installer open task failed: {error}"),
                    );
                    this.update_in(cx, |app, _, cx| {
                        app.update_open_in_flight = false;
                        app.update_open_error = Some(error.to_string());
                        cx.notify();
                    })?;
                }
            }

            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    fn active(&self) -> &ShellSession {
        &self.sessions[self.active_session]
    }

    fn active_mut(&mut self) -> &mut ShellSession {
        &mut self.sessions[self.active_session]
    }

    /// Keeps the virtual list in step with the active conversation without
    /// invalidating entries whose content has not changed. Appending a reply
    /// is the common path, so it is a cheap splice; replacing a conversation
    /// or regenerating one intentionally resets the measured layout.
    fn sync_conversation_list_state(&mut self) {
        let session_id = self.active().id.clone();
        let message_count = self.active().messages.len();
        let list_count = self.conversation_list.item_count();

        if self.conversation_list_session.as_ref() != Some(&session_id) {
            self.conversation_list.reset(message_count);
            self.conversation_list_session = Some(session_id);
        } else if message_count > list_count {
            self.conversation_list
                .splice(list_count..list_count, message_count - list_count);
        } else if message_count < list_count {
            self.conversation_list.reset(message_count);
        }
    }

    fn remeasure_active_conversation_tail(&mut self, session_id: &SessionId) {
        if self.active().id != *session_id {
            return;
        }
        self.sync_conversation_list_state();
        let message_count = self.active().messages.len();
        if message_count > 0 {
            self.conversation_list
                .remeasure_items(message_count - 1..message_count);
        }
    }

    fn reset_conversation_scroll(&mut self) {
        self.sync_conversation_list_state();
        self.conversation_list.set_follow_mode(FollowMode::Tail);
        self.conversation_list.scroll_to_end();
    }

    fn scroll_to_checkpoint(&mut self, message_position: Option<usize>, cx: &mut Context<Self>) {
        let Some(last_message) = self.active().messages.len().checked_sub(1) else {
            return;
        };
        // Checkpoints created before message positions were persisted still
        // get a useful fallback target instead of producing a dead click.
        let target = message_position.unwrap_or(last_message).min(last_message);
        self.sync_conversation_list_state();
        self.conversation_list.scroll_to(ListOffset {
            item_ix: target,
            offset_in_item: px(0.0),
        });
        cx.notify();
    }

    fn refresh_model_catalogs(&mut self, cx: &mut Context<Self>) {
        let runtime = self.runtime.clone();
        let task_runtime = runtime.clone();
        let request = runtime
            .spawn_background(async move { task_runtime.refresh_model_catalogs_parallel().await });
        cx.spawn(async move |this, cx| {
            let results = match request.await {
                Ok(results) => results,
                Err(error) => {
                    diagnostics::record(
                        DiagnosticLevel::Error,
                        "models.refresh",
                        format!("Parallel catalog refresh task failed: {error}"),
                    );
                    return;
                }
            };
            _ = this.update_in(cx, |app, window, cx| {
                let mut changed = false;
                for (connection_id, result) in results {
                    match result {
                        Ok(models) => {
                            app.replace_model_choices(&connection_id, models);
                            changed = true;
                        }
                        Err(error) => {
                            diagnostics::record(
                                DiagnosticLevel::Warning,
                                "models.refresh",
                                format!("Could not refresh {connection_id}: {error}"),
                            );
                        }
                    }
                }
                if changed {
                    app.refresh_model_picker(window, cx);
                    app.refresh_embedding_connections(window, cx);
                    app.refresh_agent_model_picker(window, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn remember_active_setup(&mut self, cx: &mut Context<Self>) {
        let binding = self.active().binding.clone();
        if !binding.is_ready() {
            return;
        }
        match self.runtime.database.remember_binding(&binding) {
            Ok(()) => self.remembered_binding = binding,
            Err(error) => self.show_error(error.to_string(), cx),
        }
    }

    fn mark_active_read(&mut self, cx: &mut Context<Self>) {
        if !self.active().unread {
            return;
        }
        let conversation_id = self.active().id.to_string();
        let persisted = self.active().persisted;
        self.active_mut().unread = false;
        if let Some(summary) = self
            .conversations
            .iter_mut()
            .find(|conversation| conversation.id == conversation_id)
        {
            summary.unread = false;
        }
        if persisted {
            if let Err(error) = self
                .runtime
                .database
                .set_conversation_unread(&conversation_id, false)
            {
                self.show_error(error.to_string(), cx);
            }
        }
    }

    fn refresh_navigation(&mut self) {
        // Keep the current navigation visible if a transient SQLite read
        // fails. Clearing the lists here made a successful action look like
        // it had deleted everything, and hid the actual storage error.
        if let Ok(projects) = self.runtime.database.projects() {
            self.projects = projects;
        }
        if let Ok(conversations) = self.runtime.database.conversation_summaries(80) {
            self.conversations = conversations;
        }
        if let Ok(status) = self.runtime.database.embedding_index_status() {
            self.embedding_status = Some(status);
        }
    }

    fn refresh_embedding_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.embedding_connection_labels = self
            .runtime
            .embedding_connections()
            .into_iter()
            .map(|profile| (profile.name.into(), profile.id))
            .collect();
        if self.embedding_connection_id.as_ref().is_some_and(|id| {
            !self
                .embedding_connection_labels
                .iter()
                .any(|(_, candidate)| candidate == id)
        }) {
            self.embedding_connection_id = None;
            self.embedding_model_id = None;
        }
        self.sync_embedding_selectors(window, cx);
    }

    fn refresh_embedding_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.embedding_model_labels =
            embedding_model_choices(&self.runtime, self.embedding_connection_id.as_ref());
        if let Some(model_id) = self.embedding_model_id.clone() {
            if !self
                .embedding_model_labels
                .iter()
                .any(|(_, candidate)| candidate == &model_id)
            {
                // Preserve a manually configured model while a remote catalog
                // is loading, or when an endpoint does not advertise it.
                self.embedding_model_labels
                    .push((format!("{model_id} · saved").into(), model_id));
            }
        }
        let items = self
            .embedding_model_labels
            .iter()
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>();
        let selected = self.embedding_model_id.as_ref().and_then(|id| {
            self.embedding_model_labels
                .iter()
                .find(|(_, candidate)| candidate == id)
                .map(|(label, _)| label.clone())
        });
        self.embedding_model_select.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            match selected.as_ref() {
                Some(label) => select.set_selected_value(label, window, cx),
                None => select.set_selected_index(None, window, cx),
            }
        });
    }

    fn sync_embedding_selectors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = self
            .embedding_connection_labels
            .iter()
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>();
        let selected = self.embedding_connection_id.as_ref().and_then(|id| {
            self.embedding_connection_labels
                .iter()
                .find(|(_, candidate)| candidate == id)
                .map(|(label, _)| label.clone())
        });
        self.embedding_connection_select.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            match selected.as_ref() {
                Some(label) => select.set_selected_value(label, window, cx),
                None => select.set_selected_index(None, window, cx),
            }
        });
        self.refresh_embedding_models(window, cx);
    }

    fn select_embedding_connection(
        &mut self,
        value: Option<&SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.embedding_connection_id = value.and_then(|value| {
            self.embedding_connection_labels
                .iter()
                .find(|(label, _)| label == value)
                .map(|(_, id)| id.clone())
        });
        self.embedding_model_id = None;
        self.refresh_embedding_models(window, cx);
        cx.notify();
    }

    fn select_embedding_model(&mut self, value: Option<&SharedString>, cx: &mut Context<Self>) {
        self.embedding_model_id = value.and_then(|value| {
            self.embedding_model_labels
                .iter()
                .find(|(label, _)| label == value)
                .map(|(_, id)| id.clone())
        });
        cx.notify();
    }

    fn refresh_agent_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent_model_labels =
            agent_model_choices(&self.model_choices, self.agent_form_connection_id.as_ref());
        let items = self
            .agent_model_labels
            .iter()
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>();
        let selected = self.agent_form_model_id.as_ref().and_then(|id| {
            self.agent_model_labels
                .iter()
                .find(|(_, candidate)| candidate == id)
                .map(|(label, _)| label.clone())
        });
        self.agent_model_select.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            match selected.as_ref() {
                Some(label) => select.set_selected_value(label, window, cx),
                None => select.set_selected_index(None, window, cx),
            }
        });
    }

    fn sync_agent_selectors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent_connection_labels = self
            .runtime
            .connections()
            .into_iter()
            .map(|profile| (profile.name.into(), profile.id))
            .collect();
        let items = self
            .agent_connection_labels
            .iter()
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>();
        let selected = self.agent_form_connection_id.as_ref().and_then(|id| {
            self.agent_connection_labels
                .iter()
                .find(|(_, candidate)| candidate == id)
                .map(|(label, _)| label.clone())
        });
        self.agent_connection_select.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            match selected.as_ref() {
                Some(label) => select.set_selected_value(label, window, cx),
                None => select.set_selected_index(None, window, cx),
            }
        });
        self.refresh_agent_model_picker(window, cx);
    }

    fn select_agent_connection(
        &mut self,
        value: Option<&SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.agent_form_connection_id = value.and_then(|value| {
            self.agent_connection_labels
                .iter()
                .find(|(label, _)| label == value)
                .map(|(_, id)| id.clone())
        });
        self.agent_form_model_id = None;
        self.refresh_agent_model_picker(window, cx);
        cx.notify();
    }

    fn select_agent_model(&mut self, value: Option<&SharedString>, cx: &mut Context<Self>) {
        self.agent_form_model_id = value.and_then(|value| {
            self.agent_model_labels
                .iter()
                .find(|(label, _)| label == value)
                .map(|(_, id)| id.clone())
        });
        cx.notify();
    }

    fn edit_agent_profile(
        &mut self,
        agent: AgentProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing_agent_id = Some(agent.id.clone());
        self.agent_form_connection_id = Some(ConnectionId(agent.connection_id.clone()));
        self.agent_form_model_id = Some(agent.model_id.clone());
        self.agent_id_input
            .update(cx, |input, cx| input.set_value(&agent.id, window, cx));
        self.agent_name_input
            .update(cx, |input, cx| input.set_value(&agent.name, window, cx));
        self.agent_description_input.update(cx, |input, cx| {
            input.set_value(&agent.description, window, cx)
        });
        self.sync_agent_selectors(window, cx);
        cx.notify();
    }

    fn clear_agent_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_agent_id = None;
        self.agent_form_connection_id = None;
        self.agent_form_model_id = None;
        for input in [
            &self.agent_id_input,
            &self.agent_name_input,
            &self.agent_description_input,
        ] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.sync_agent_selectors(window, cx);
        cx.notify();
    }

    fn save_agent_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self
            .editing_agent_id
            .clone()
            .unwrap_or_else(|| self.agent_id_input.read(cx).value().trim().to_owned());
        let name = self.agent_name_input.read(cx).value().trim().to_owned();
        let description = self
            .agent_description_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        let Some(connection_id) = self.agent_form_connection_id.clone() else {
            self.show_error("Choose a connection for this agent", cx);
            return;
        };
        let Some(model_id) = self.agent_form_model_id.clone() else {
            self.show_error("Choose a model for this agent", cx);
            return;
        };
        if id.is_empty() || name.is_empty() {
            self.show_error("Give the agent an id and a name", cx);
            return;
        }
        match self.runtime.save_agent(AgentProfile {
            id,
            name,
            description,
            connection_id: connection_id.to_string(),
            model_id,
        }) {
            Ok(()) => {
                self.notice = Some(Notice {
                    success: true,
                    text: "Agent saved.".into(),
                });
                self.clear_agent_form(window, cx);
            }
            Err(error) => self.show_error(error.to_string(), cx),
        }
        cx.notify();
    }

    fn delete_agent_profile(&mut self, id: String, cx: &mut Context<Self>) {
        match self.runtime.delete_agent(&id) {
            Ok(true) => {
                if self.editing_agent_id.as_deref() == Some(id.as_str()) {
                    self.editing_agent_id = None;
                }
                self.notice = Some(Notice {
                    success: true,
                    text: "Agent removed.".into(),
                });
            }
            Ok(false) => {}
            Err(error) => self.show_error(error.to_string(), cx),
        }
        cx.notify();
    }

    fn refresh_conversation_search(&mut self, cx: &mut Context<Self>) {
        let query = self.conversation_search.read(cx).value().trim().to_owned();
        self.conversation_search_generation = self.conversation_search_generation.wrapping_add(1);
        self.conversation_search_results = if query.is_empty() {
            Vec::new()
        } else {
            self.runtime
                .database
                .search_conversations_text(&query, 24)
                .unwrap_or_default()
        };
        cx.notify();
    }

    fn search_conversations_semantically(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let query = self.conversation_search.read(cx).value().trim().to_owned();
        if query.is_empty() {
            return;
        }
        let generation = self.conversation_search_generation;
        let runtime = self.runtime.clone();
        let task_runtime = runtime.clone();
        let request = runtime
            .spawn_background(async move { task_runtime.search_conversations(&query, 24).await });
        cx.spawn(async move |this, cx| {
            let result = request.await.ok().and_then(Result::ok);
            _ = this.update(cx, |app, cx| {
                if app.conversation_search_generation != generation {
                    return;
                }
                if let Some(results) = result {
                    app.conversation_search_results = results;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn compile_embedding_index(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.embedding_index_busy {
            return;
        }
        let (Some(connection_id), Some(model_id)) = (
            self.embedding_connection_id.clone(),
            self.embedding_model_id.clone(),
        ) else {
            self.show_error(i18n::text(cx, "notice.choose_embedding"), cx);
            return;
        };
        let config = EmbeddingConfig {
            connection_id,
            model_id,
        };
        if let Err(error) = self.runtime.database.save_embedding_config(&config) {
            self.show_error(error.to_string(), cx);
            return;
        }
        self.embedding_index_busy = true;
        self.notice = Some(Notice {
            success: true,
            text: i18n::text(cx, "notice.indexing").to_string(),
        });
        let runtime = self.runtime.clone();
        let task_runtime = runtime.clone();
        let request = runtime
            .spawn_background(async move { task_runtime.rebuild_conversation_index(config).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update(cx, |app, cx| {
                app.embedding_index_busy = false;
                match result {
                    Ok(Ok((conversations, fragments))) => {
                        app.embedding_status = app.runtime.database.embedding_index_status().ok();
                        app.notice = Some(Notice {
                            success: true,
                            text: i18n::format(
                                cx,
                                "notice.index_ready",
                                &[
                                    ("conversations", conversations.to_string()),
                                    ("fragments", fragments.to_string()),
                                ],
                            ),
                        });
                    }
                    Ok(Err(error)) => {
                        app.notice = Some(Notice {
                            success: false,
                            text: i18n::format(
                                cx,
                                "notice.index_error",
                                &[("error", error.to_string())],
                            ),
                        });
                    }
                    Err(error) => {
                        app.notice = Some(Notice {
                            success: false,
                            text: i18n::format(
                                cx,
                                "notice.index_task_error",
                                &[("error", error.to_string())],
                            ),
                        });
                    }
                }
                cx.notify();
            })?;
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    fn persist_active(&mut self, cx: &mut Context<Self>) -> bool {
        let snapshot = self.active().snapshot();
        match self.runtime.database.save_conversation(&snapshot) {
            Ok(()) => {
                self.active_mut().persisted = true;
                self.refresh_navigation();
                self.schedule_background_indexing(cx);
                true
            }
            Err(error) => {
                self.notice = Some(Notice {
                    success: false,
                    text: error.to_string(),
                });
                cx.notify();
                false
            }
        }
    }

    fn persist_session(&mut self, id: &SessionId, cx: &mut Context<Self>) {
        let Some(index) = self.sessions.iter().position(|session| &session.id == id) else {
            return;
        };
        let snapshot = self.sessions[index].snapshot();
        match self.runtime.database.save_conversation(&snapshot) {
            Ok(()) => {
                self.sessions[index].persisted = true;
                self.refresh_navigation();
                self.schedule_background_indexing(cx);
            }
            Err(error) => {
                self.notice = Some(Notice {
                    success: false,
                    text: error.to_string(),
                });
            }
        }
        cx.notify();
    }

    fn persist_session_binding(&mut self, id: &SessionId, cx: &mut Context<Self>) {
        self.persist_session(id, cx);
        if self.active().id == *id {
            self.remember_active_setup(cx);
        }
    }

    fn schedule_background_indexing(&mut self, cx: &mut Context<Self>) {
        if self.background_indexing || self.background_index_scheduled {
            return;
        }
        self.background_index_scheduled = true;
        cx.spawn(async move |this, cx| {
            // Debounce writes and wait for the UI/provider stream to become
            // quiet before spending network capacity on pending fragments.
            cx.background_executor().timer(Duration::from_secs(4)).await;
            _ = this.update(cx, |app, cx| {
                app.background_index_scheduled = false;
                app.start_background_indexing(cx);
            });
        })
        .detach();
    }

    fn start_background_indexing(&mut self, cx: &mut Context<Self>) {
        if self.background_indexing || self.embedding_index_busy {
            return;
        }
        if self.active().processing {
            self.schedule_background_indexing(cx);
            return;
        }
        let Some(config) = self.runtime.database.embedding_config().ok().flatten() else {
            return;
        };
        let pending = match self.runtime.database.pending_embedding_count(&config) {
            Ok(pending) => pending,
            Err(error) => {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "memory.index",
                    format!("Could not inspect pending conversation fragments: {error}"),
                );
                return;
            }
        };
        if pending == 0 {
            return;
        }

        diagnostics::record(
            DiagnosticLevel::Info,
            "memory.index",
            format!("Idle indexing started for {pending} pending conversation(s)."),
        );
        self.background_indexing = true;
        self.embedding_index_busy = true;
        cx.notify();

        let runtime = self.runtime.clone();
        let task_runtime = runtime.clone();
        let request = runtime.spawn_background(async move {
            task_runtime.index_pending_conversations(config).await
        });
        cx.spawn(async move |this, cx| {
            let result = request.await;
            _ = this.update(cx, |app, cx| {
                app.background_indexing = false;
                app.embedding_index_busy = false;
                app.embedding_status = app.runtime.database.embedding_index_status().ok();
                let should_retry = matches!(&result, Ok(Ok(_)));
                match result {
                    Ok(Ok((conversations, fragments))) => diagnostics::record(
                        DiagnosticLevel::Success,
                        "memory.index",
                        format!(
                            "Idle indexing completed: {conversations} conversation(s), {fragments} fragment(s)."
                        ),
                    ),
                    Ok(Err(error)) => diagnostics::record(
                        DiagnosticLevel::Warning,
                        "memory.index",
                        format!("Idle indexing stopped: {error}"),
                    ),
                    Err(error) => diagnostics::record(
                        DiagnosticLevel::Warning,
                        "memory.index",
                        format!("Idle indexing task stopped: {error}"),
                    ),
                }
                // A successful pass may have raced with a newly persisted
                // conversation, so check again after the debounce. Provider
                // errors must not create a four-second retry loop while idle.
                if should_retry {
                    app.schedule_background_indexing(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(i18n::text(cx, "notice.open_workspace")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut roots))) = receiver.await else {
                return Ok::<(), anyhow::Error>(());
            };
            let Some(root) = roots.pop() else {
                return Ok(());
            };
            this.update_in(cx, |app, window, cx| {
                match app.runtime.database.open_project(&root) {
                    Ok(project) => {
                        app.refresh_navigation();
                        app.new_session_for_project(Some(project), window, cx);
                    }
                    Err(error) => app.show_error(error.to_string(), cx),
                }
            })?;
            Ok(())
        })
        .detach();
    }

    fn open_attachment_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(i18n::text(cx, "notice.attach_files")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return Ok::<(), anyhow::Error>(());
            };
            this.update_in(cx, |app, _, cx| {
                for path in paths {
                    if !app
                        .attachments
                        .iter()
                        .any(|attachment| attachment.path == path)
                    {
                        app.attachments.push(ComposerAttachment { path });
                    }
                }
                cx.notify();
            })?;
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    fn remove_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.attachments.len() {
            self.attachments.remove(index);
            cx.notify();
        }
    }

    fn queue_composer_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }

        let attachments = std::mem::take(&mut self.attachments);
        let session = self.active_mut();
        session
            .queued_messages
            .push(QueuedMessage { text, attachments });
        session.queue_autostart = true;
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        diagnostics::record(
            DiagnosticLevel::Info,
            "agent.request",
            format!(
                "Queued message {} for conversation {} while the provider is streaming.",
                self.active().queued_messages.len(),
                self.active().id
            ),
        );
        cx.notify();
    }

    fn stop_active_stream(&mut self, cx: &mut Context<Self>) {
        if !self.active().processing {
            return;
        }

        let session_id = self.active().id.clone();
        // Dropping the GPUI task also drops AgentStreamHandle, which aborts
        // the provider task and closes the stream. Keep the partial assistant
        // bubble visible so a forced follow-up has an honest transcript.
        let _stream_task = self.active_mut().task.take();
        let session = self.active_mut();
        session.processing = false;
        session.queue_autostart = false;
        session.context_busy = false;
        session.pending_user_question = None;
        if let Some(message) = session.messages.last_mut() {
            message.reasoning_complete = true;
            for activity in &mut message.tool_activities {
                if activity.state == ToolActivityState::Running {
                    activity.state = ToolActivityState::Failed;
                    activity.summary = "Stopped by user".into();
                    activity.duration_ms = Some(activity.started_at.elapsed().as_millis() as u64);
                }
            }
        }
        diagnostics::record(
            DiagnosticLevel::Info,
            "agent.request",
            format!("Streaming stopped by user for conversation {session_id}."),
        );
        self.remeasure_active_conversation_tail(&session_id);
        self.persist_session(&session_id, cx);
        cx.notify();
    }

    fn remove_queued_message(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.active().queued_messages.len() {
            let session = self.active_mut();
            session.queued_messages.remove(index);
            if session.queued_messages.is_empty() {
                session.queue_autostart = false;
            }
            cx.notify();
        }
    }

    fn start_next_queued_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active().processing || !self.active().queue_autostart {
            return;
        }
        let Some(message) = self.active_mut().queued_messages.first().cloned() else {
            self.active_mut().queue_autostart = false;
            return;
        };
        self.active_mut().queued_messages.remove(0);
        if self.active().queued_messages.is_empty() {
            self.active_mut().queue_autostart = false;
        }
        self.start_message_request(message.text, message.attachments, false, window, cx);
    }

    fn force_send_queued_message(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.active_mut().queued_messages.get(index).cloned() else {
            return;
        };
        self.active_mut().queued_messages.remove(index);
        if self.active().processing {
            self.stop_active_stream(cx);
        }
        self.active_mut().queue_autostart = !self.active().queued_messages.is_empty();
        self.start_message_request(message.text, message.attachments, false, window, cx);
    }

    fn new_session_for_project(
        &mut self,
        project: Option<WorkProject>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = project.as_ref() {
            self.runtime.load_workspace_tools(&project.root);
        }
        let binding = inherited_session_binding(
            &self.active().binding,
            &self.remembered_binding,
            &self.runtime.default_agent_tools(),
        );
        self.sessions
            .push(ShellSession::new(project.as_ref(), binding));
        self.active_session = self.sessions.len() - 1;
        self.route = Route::Chat;
        self.show_sources = true;
        self.show_context = false;
        self.selected_agent_thread = None;
        self.agent_thread_view = None;
        self.attachments.clear();
        self.reset_conversation_scroll();
        self.sync_selectors_to_active(window, cx);
        self.composer
            .update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn select_project(&mut self, project_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let project = self
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .cloned();
        if let Some(project) = project {
            self.new_session_for_project(Some(project), window, cx);
        }
    }

    fn select_conversation(
        &mut self,
        conversation_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.conversation_search_open = false;
        if let Some(index) = self
            .sessions
            .iter()
            .position(|session| session.id.as_str() == conversation_id)
        {
            self.select_session(index, window, cx);
            return;
        }
        match self.runtime.database.conversation(conversation_id) {
            Ok(Some(conversation)) => {
                let mut session = ShellSession::from_work(conversation, &self.projects);
                let binding_changed =
                    ensure_binding_tools(&mut session.binding, &self.runtime.default_agent_tools());
                if let Some(root) = session.workspace_root.as_deref() {
                    self.runtime.load_workspace_tools(root);
                }
                self.sessions.push(session);
                self.active_session = self.sessions.len() - 1;
                self.route = Route::Chat;
                self.show_sources = true;
                self.show_context = false;
                self.selected_agent_thread = None;
                self.agent_thread_view = None;
                self.attachments.clear();
                self.reset_conversation_scroll();
                self.mark_active_read(cx);
                if binding_changed {
                    self.persist_active(cx);
                }
                self.remember_active_setup(cx);
                self.sync_selectors_to_active(window, cx);
                cx.notify();
            }
            Ok(None) => {
                self.refresh_navigation();
                self.show_error(i18n::text(cx, "notice.conversation_missing"), cx);
            }
            Err(error) => self.show_error(error.to_string(), cx),
        }
    }

    fn conversation_title(&self, conversation_id: &str) -> Option<String> {
        self.sessions
            .iter()
            .find(|session| session.id.as_str() == conversation_id)
            .map(|session| session.title.clone())
            .or_else(|| {
                self.conversations
                    .iter()
                    .find(|conversation| conversation.id == conversation_id)
                    .map(|conversation| conversation.title.clone())
            })
    }

    fn open_rename_conversation(
        &mut self,
        conversation_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        diagnostics::record(
            DiagnosticLevel::Info,
            "conversation.action",
            format!("Opening rename dialog for conversation {conversation_id}."),
        );
        let Some(title) = self.conversation_title(conversation_id) else {
            self.show_error(i18n::text(cx, "notice.conversation_missing"), cx);
            return;
        };
        let rename_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n::text(cx, "dialog.conversation_title"))
                .default_value(title)
        });
        let view = cx.entity();
        let conversation_id = conversation_id.to_string();
        window.open_dialog(cx, move |dialog, window, cx| {
            rename_input.update(cx, |input, cx| input.focus(window, cx));
            let submit_input = rename_input.clone();
            let submit_view = view.clone();
            let submit_id = conversation_id.clone();
            let cancel_button = Button::new("rename-cancel")
                .secondary()
                .label(i18n::text(cx, "dialog.cancel"))
                .on_click(|_, window, cx| window.close_dialog(cx));
            let confirm_input = rename_input.clone();
            let confirm_view = view.clone();
            let confirm_id = conversation_id.clone();
            let confirm_button = Button::new("rename-confirm")
                .primary()
                .label(i18n::text(cx, "dialog.rename"))
                .on_click(move |_, window, cx| {
                    diagnostics::record(
                        DiagnosticLevel::Info,
                        "conversation.action",
                        format!(
                            "Rename confirmation click received for conversation {confirm_id}."
                        ),
                    );
                    let title = confirm_input.read(cx).value().trim().to_string();
                    if confirm_view.update(cx, |app, cx| {
                        app.rename_conversation(&confirm_id, &title, cx)
                    }) {
                        window.close_dialog(cx);
                    }
                });
            dialog
                .title(i18n::text(cx, "dialog.rename_title"))
                .w(px(420.0))
                .child(div().py(px(8.0)).child(Input::new(&rename_input).w_full()))
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.0))
                        .child(cancel_button)
                        .child(confirm_button),
                )
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(i18n::text(cx, "dialog.rename"))
                        .cancel_text(i18n::text(cx, "dialog.cancel"))
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    let title = submit_input.read(cx).value().trim().to_string();
                    submit_view.update(cx, |app, cx| {
                        app.rename_conversation(&submit_id, &title, cx)
                    })
                })
        });
    }

    fn rename_conversation(
        &mut self,
        conversation_id: &str,
        title: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = title.trim();
        if title.is_empty() {
            self.show_error(i18n::text(cx, "dialog.title_empty"), cx);
            return false;
        }

        let result = if let Some(index) = self
            .sessions
            .iter()
            .position(|session| session.id.as_str() == conversation_id)
        {
            if self.sessions[index].persisted {
                match self
                    .runtime
                    .database
                    .rename_conversation(conversation_id, title)
                {
                    Ok(changed) if changed => {
                        self.sessions[index].title = title.into();
                        Ok(true)
                    }
                    Ok(changed) => Ok(changed),
                    Err(error) => Err(error),
                }
            } else {
                let previous_title =
                    std::mem::replace(&mut self.sessions[index].title, title.into());
                let snapshot = self.sessions[index].snapshot();
                match self.runtime.database.save_conversation(&snapshot) {
                    Ok(()) => {
                        self.sessions[index].persisted = true;
                        Ok(true)
                    }
                    Err(error) => {
                        self.sessions[index].title = previous_title;
                        Err(error)
                    }
                }
            }
        } else {
            self.runtime
                .database
                .rename_conversation(conversation_id, title)
        };

        match result {
            Ok(true) => {
                self.notice = None;
                if let Some(summary) = self
                    .conversations
                    .iter_mut()
                    .find(|conversation| conversation.id == conversation_id)
                {
                    summary.title = title.to_string();
                }
                self.refresh_navigation();
                cx.notify();
                true
            }
            Ok(false) => {
                self.show_error(i18n::text(cx, "notice.conversation_missing"), cx);
                false
            }
            Err(error) => {
                self.show_error(error.to_string(), cx);
                false
            }
        }
    }

    fn open_delete_conversation(
        &mut self,
        conversation_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        diagnostics::record(
            DiagnosticLevel::Info,
            "conversation.action",
            format!("Opening delete dialog for conversation {conversation_id}."),
        );
        let title = self
            .conversation_title(conversation_id)
            .unwrap_or_else(|| i18n::text(cx, "dialog.this_conversation").to_string());
        let localization = cx.global::<i18n::Localization>().clone();
        let view = cx.entity();
        let conversation_id = conversation_id.to_string();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let confirm_view = view.clone();
            let confirm_id = conversation_id.clone();
            let delete_label = localization.text("dialog.delete");
            let cancel_label = localization.text("dialog.cancel");
            let confirm_button = Button::new("delete-confirm")
                .danger()
                .label(delete_label.clone())
                .on_click(move |_, window, cx| {
                    diagnostics::record(
                        DiagnosticLevel::Info,
                        "conversation.action",
                        format!(
                            "Delete confirmation click received for conversation {confirm_id}."
                        ),
                    );
                    if confirm_view.update(cx, |app, cx| {
                        app.delete_conversation(&confirm_id, window, cx)
                    }) {
                        window.close_dialog(cx);
                    }
                });
            let cancel_button = Button::new("delete-cancel")
                .secondary()
                .label(cancel_label.clone())
                .on_click(|_, window, cx| window.close_dialog(cx));
            alert
                .title(localization.text("dialog.delete_title"))
                .description(
                    localization.format("dialog.delete_description", &[("title", title.clone())]),
                )
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.0))
                        .child(cancel_button)
                        .child(confirm_button),
                )
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(delete_label)
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text(cancel_label)
                        .show_cancel(true),
                )
        });
    }

    fn delete_conversation(
        &mut self,
        conversation_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let session_index = self
            .sessions
            .iter()
            .position(|session| session.id.as_str() == conversation_id);
        let is_ephemeral = session_index
            .and_then(|index| self.sessions.get(index))
            .is_some_and(|session| !session.persisted);

        if !is_ephemeral {
            match self.runtime.database.delete_conversation(conversation_id) {
                Ok(true) => {}
                Ok(false) => {
                    self.show_error(i18n::text(cx, "notice.conversation_missing"), cx);
                    return false;
                }
                Err(error) => {
                    self.show_error(error.to_string(), cx);
                    return false;
                }
            }
        }

        let mut sync_selectors = false;
        if let Some(index) = session_index {
            self.sessions.remove(index);
            if self.sessions.is_empty() {
                self.sessions
                    .push(ShellSession::new(None, self.remembered_binding.clone()));
                self.active_session = 0;
                sync_selectors = true;
            } else if index < self.active_session {
                self.active_session -= 1;
            } else if index == self.active_session {
                self.active_session = index.min(self.sessions.len() - 1);
                sync_selectors = true;
            }
        }
        self.notice = None;
        self.conversations
            .retain(|conversation| conversation.id != conversation_id);
        self.refresh_navigation();
        if sync_selectors {
            self.route = Route::Chat;
            self.mark_active_read(cx);
            self.sync_selectors_to_active(window, cx);
        }
        cx.notify();
        true
    }

    fn toggle_pin(&mut self, cx: &mut Context<Self>) {
        let conversation_id = self.active().id.to_string();
        let pinned = !self.active().pinned;
        self.set_conversation_pinned(&conversation_id, pinned, cx);
    }

    fn set_conversation_pinned(
        &mut self,
        conversation_id: &str,
        pinned: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let session_index = self
            .sessions
            .iter()
            .position(|session| session.id.as_str() == conversation_id);
        let is_ephemeral = session_index
            .and_then(|index| self.sessions.get(index))
            .is_some_and(|session| !session.persisted);

        if is_ephemeral {
            let Some(index) = session_index else {
                return false;
            };
            self.sessions[index].pinned = pinned;
            let snapshot = self.sessions[index].snapshot();
            return match self.runtime.database.save_conversation(&snapshot) {
                Ok(()) => {
                    self.sessions[index].persisted = true;
                    self.notice = None;
                    self.refresh_navigation();
                    cx.notify();
                    true
                }
                Err(error) => {
                    self.sessions[index].pinned = !pinned;
                    self.show_error(error.to_string(), cx);
                    false
                }
            };
        }

        match self.runtime.database.set_pinned(conversation_id, pinned) {
            Ok(true) => {
                if let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| session.id.as_str() == conversation_id)
                {
                    session.pinned = pinned;
                }
                if let Some(summary) = self
                    .conversations
                    .iter_mut()
                    .find(|conversation| conversation.id == conversation_id)
                {
                    summary.pinned = pinned;
                }
                self.notice = None;
                self.refresh_navigation();
                cx.notify();
                true
            }
            Ok(false) => {
                self.show_error(i18n::text(cx, "notice.conversation_missing"), cx);
                false
            }
            Err(error) => {
                self.show_error(error.to_string(), cx);
                false
            }
        }
    }

    fn record_tool_source(&mut self, session_id: &SessionId, name: &str) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
        else {
            return;
        };
        if let Some(source) = session
            .sources
            .iter_mut()
            .find(|source| source.kind == name)
        {
            source.count = source.count.saturating_add(1);
            source.last_used_at = now();
            return;
        }
        session.sources.push(WorkSource {
            key: name.to_string(),
            kind: name.to_string(),
            label: tool_display_name(name),
            url: None,
            title: None,
            detail: None,
            count: 1,
            last_used_at: now(),
        });
    }

    fn start_compaction_activity(&mut self, session_id: &SessionId, reason: String) {
        let input = format_tool_input(&serde_json::json!({ "reason": reason }));
        let mut record_source = false;
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
        {
            if let Some(message) = session
                .messages
                .iter_mut()
                .rev()
                .find(|message| message.role == MessageRole::Assistant)
            {
                // An explicit compact_conversation tool call already created
                // an activity. Reuse it for the actual compaction pass so the
                // conversation shows one coherent tool, not two duplicates.
                if let Some(activity) = message.tool_activities.iter_mut().rev().find(|activity| {
                    activity.name == "compact_conversation"
                        && activity.state == ToolActivityState::Completed
                        && activity.summary.starts_with("Compaction requested")
                }) {
                    activity.input = input;
                    activity.summary.clear();
                    activity.output.clear();
                    activity.state = ToolActivityState::Running;
                    activity.started_at = Instant::now();
                    activity.duration_ms = None;
                } else {
                    let text_offset = message.text.len();
                    let group_id = message.assign_tool_group(false);
                    message.tool_activities.push(ToolActivity {
                        call_id: None,
                        name: "compact_conversation".into(),
                        text_offset,
                        group_id,
                        input,
                        summary: String::new(),
                        output: String::new(),
                        state: ToolActivityState::Running,
                        started_at: Instant::now(),
                        duration_ms: None,
                        expanded: false,
                        inside_reasoning: false,
                    });
                    record_source = true;
                }
            }
        }
        if record_source {
            self.record_tool_source(session_id, "compact_conversation");
        }
    }

    fn finish_compaction_activity(
        &mut self,
        session_id: &SessionId,
        original_messages: usize,
        retained_messages: usize,
        understood_context: Option<&str>,
    ) {
        let summary = format!("Compacted {original_messages} messages to {retained_messages}.");
        let output = if let Some(context) = understood_context
            .map(str::trim)
            .filter(|context| !context.is_empty())
        {
            format!("{summary}\n\nUnderstood context:\n{context}")
        } else {
            format!(
                "{summary}\n\nNo new understood context was generated; the retained conversation remains active."
            )
        };
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
        {
            if let Some(message) = session
                .messages
                .iter_mut()
                .rev()
                .find(|message| message.role == MessageRole::Assistant)
            {
                if let Some(activity) = message.tool_activities.iter_mut().rev().find(|activity| {
                    activity.name == "compact_conversation"
                        && activity.state == ToolActivityState::Running
                }) {
                    activity.state = ToolActivityState::Completed;
                    activity.summary = summary;
                    activity.output = output;
                    activity.duration_ms = Some(activity.started_at.elapsed().as_millis() as u64);
                    return;
                }
                // Keep the event visible even if the UI missed the start
                // event while switching sessions.
                let text_offset = message.text.len();
                let group_id = message.assign_tool_group(false);
                message.tool_activities.push(ToolActivity {
                    call_id: None,
                    name: "compact_conversation".into(),
                    text_offset,
                    group_id,
                    input: "{}".into(),
                    summary,
                    output,
                    state: ToolActivityState::Completed,
                    started_at: Instant::now(),
                    duration_ms: Some(0),
                    expanded: false,
                    inside_reasoning: false,
                });
            }
        }
    }

    fn fail_compaction_activity(&mut self, session_id: &SessionId, error: String) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
        {
            if let Some(message) = session
                .messages
                .iter_mut()
                .rev()
                .find(|message| message.role == MessageRole::Assistant)
            {
                if let Some(activity) = message.tool_activities.iter_mut().rev().find(|activity| {
                    activity.name == "compact_conversation"
                        && activity.state == ToolActivityState::Running
                }) {
                    activity.state = ToolActivityState::Failed;
                    activity.summary = "Compaction failed".into();
                    activity.output = error;
                    activity.duration_ms = Some(activity.started_at.elapsed().as_millis() as u64);
                }
            }
        }
    }

    fn record_web_sources(
        &mut self,
        session_id: &SessionId,
        tool_name: &str,
        metadata: Option<&serde_json::Value>,
    ) {
        let Some(metadata) = metadata else {
            return;
        };
        let mut pages = Vec::new();
        match tool_name {
            "web_search_intrernal" => {
                if let Some(results) = metadata.get("results").and_then(|value| value.as_array()) {
                    for result in results {
                        let Some(url) = result.get("url").and_then(|value| value.as_str()) else {
                            continue;
                        };
                        let Some(url) = normalize_web_source_url(url) else {
                            continue;
                        };
                        let title = result
                            .get("title")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        pages.push((url, title, None));
                    }
                }
            }
            "web_fetch" => {
                let Some(url) = metadata.get("url").and_then(|value| value.as_str()) else {
                    return;
                };
                let Some(url) = normalize_web_source_url(url) else {
                    return;
                };
                let title = metadata
                    .get("title")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let favicon_url = metadata
                    .get("favicon_url")
                    .and_then(|value| value.as_str())
                    .and_then(normalize_web_source_url);
                pages.push((url, title, favicon_url));
            }
            _ => return,
        }

        for (url, title, favicon_url) in pages {
            self.record_web_source(session_id, url, title, favicon_url);
        }
    }

    fn record_web_source(
        &mut self,
        session_id: &SessionId,
        url: String,
        title: String,
        favicon_url: Option<String>,
    ) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
        else {
            return;
        };
        let key = format!("web:{url}");
        let label = if title.is_empty() {
            source_host(&url).unwrap_or_else(|| "Web page".into())
        } else {
            title.clone()
        };
        if let Some(source) = session
            .sources
            .iter_mut()
            .find(|source| source.key == key || source.url.as_deref() == Some(url.as_str()))
        {
            source.count = source.count.saturating_add(1);
            source.last_used_at = now();
            if source.title.is_none() && !title.is_empty() {
                source.title = Some(title);
                source.label = label;
            }
            if source.detail.is_none() {
                source.detail = favicon_url;
            }
            return;
        }
        session.sources.push(WorkSource {
            key,
            kind: "web_page".into(),
            label,
            url: Some(url),
            title: (!title.is_empty()).then_some(title),
            detail: favicon_url,
            count: 1,
            last_used_at: now(),
        });
    }

    fn toggle_tool_activity(
        &mut self,
        session_id: &SessionId,
        message_index: usize,
        activity_index: usize,
        cx: &mut Context<Self>,
    ) {
        let toggled = if let Some(activity) = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
            .and_then(|session| session.messages.get_mut(message_index))
            .and_then(|message| message.tool_activities.get_mut(activity_index))
        {
            activity.expanded = !activity.expanded;
            true
        } else {
            false
        };
        if toggled {
            if self.active().id == *session_id {
                self.sync_conversation_list_state();
                self.conversation_list
                    .remeasure_items(message_index..message_index + 1);
            }
            cx.notify();
        }
    }

    fn toggle_tool_group(
        &mut self,
        session_id: &SessionId,
        message_index: usize,
        group_id: usize,
        cx: &mut Context<Self>,
    ) {
        let toggled = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
            .and_then(|session| session.messages.get_mut(message_index))
            .map(|message| message.toggle_tool_group(group_id))
            .is_some();
        if toggled {
            if self.active().id == *session_id {
                self.sync_conversation_list_state();
                self.conversation_list
                    .remeasure_items(message_index..message_index + 1);
            }
            cx.notify();
        }
    }

    fn toggle_reasoning(
        &mut self,
        session_id: &SessionId,
        message_index: usize,
        cx: &mut Context<Self>,
    ) {
        let toggled = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
            .and_then(|session| session.messages.get_mut(message_index))
            .map(|message| {
                message.reasoning_expanded = !message.reasoning_expanded;
            })
            .is_some();

        if toggled {
            if self.active().id == *session_id {
                self.sync_conversation_list_state();
                self.conversation_list
                    .remeasure_items(message_index..message_index + 1);
            }
            cx.notify();
        }
    }

    fn toggle_tool_activity_visibility(&mut self, cx: &mut Context<Self>) {
        self.show_tool_activity = !self.show_tool_activity;
        self.conversation_list.remeasure();
        cx.notify();
    }

    fn upsert_agent_thread_snapshot(
        &mut self,
        session_id: &SessionId,
        thread: AgentThreadSnapshot,
    ) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
        else {
            return;
        };
        if let Some(existing) = session
            .agent_threads
            .iter_mut()
            .find(|existing| existing.id == thread.id)
        {
            *existing = thread;
        } else {
            session.agent_threads.push(thread);
        }
    }

    fn start_agent_thread_transcript(
        &mut self,
        session_id: &SessionId,
        thread: &AgentThreadSnapshot,
    ) {
        self.upsert_agent_thread_snapshot(session_id, thread.clone());
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
        else {
            return;
        };
        let transcript = session
            .agent_thread_transcripts
            .entry(thread.id.clone())
            .or_default();
        let prompt_is_current = transcript.messages.last().is_some_and(|message| {
            message.role == MessageRole::User && message.text == thread.prompt
        });
        if !prompt_is_current {
            transcript
                .messages
                .push(ShellMessage::user(thread.prompt.clone()));
            transcript.messages.push(ShellMessage::assistant());
        }
    }

    fn apply_delegated_agent_event(
        &mut self,
        session_id: &SessionId,
        thread_id: &str,
        event: AgentStreamEvent,
    ) {
        match event {
            AgentStreamEvent::DelegatedAgentStarted { thread } => {
                self.start_agent_thread_transcript(session_id, &thread);
            }
            AgentStreamEvent::DelegatedAgentEvent { thread_id, event } => {
                self.apply_delegated_agent_event(session_id, &thread_id, *event);
            }
            event => {
                let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| &session.id == session_id)
                else {
                    return;
                };
                let transcript = session
                    .agent_thread_transcripts
                    .entry(thread_id.to_owned())
                    .or_default();
                if !matches!(
                    transcript.messages.last().map(|message| message.role),
                    Some(MessageRole::Assistant)
                ) {
                    transcript.messages.push(ShellMessage::assistant());
                }
                let message = transcript.messages.last_mut().expect("assistant added");
                match event {
                    AgentStreamEvent::TextDelta { text } => {
                        if !text.is_empty() {
                            message.text.push_str(&text);
                            message.assistant_text_arrived();
                        }
                    }
                    AgentStreamEvent::ReasoningDelta { text } => {
                        message.reasoning.push_str(&text);
                        message.reasoning_arrived();
                        message.reasoning_complete = false;
                    }
                    AgentStreamEvent::ReasoningFinished => {
                        message.reasoning_complete = true;
                    }
                    AgentStreamEvent::ToolPreparing {
                        call_id,
                        name,
                        input,
                        inside_reasoning,
                    } => {
                        if let Some(activity) =
                            message.tool_activities.iter_mut().rev().find(|activity| {
                                activity.call_id.as_deref() == Some(call_id.as_str())
                                    && activity.state == ToolActivityState::Running
                            })
                        {
                            activity.name = name;
                            activity.input = format_tool_input(&input);
                            activity.inside_reasoning = inside_reasoning;
                        } else {
                            let text_offset = message.text.len();
                            let group_id = message.assign_tool_group(inside_reasoning);
                            message.tool_activities.push(ToolActivity {
                                call_id: Some(call_id),
                                name,
                                text_offset,
                                group_id,
                                input: format_tool_input(&input),
                                summary: String::new(),
                                output: String::new(),
                                state: ToolActivityState::Running,
                                started_at: Instant::now(),
                                duration_ms: None,
                                expanded: false,
                                inside_reasoning,
                            });
                        }
                    }
                    AgentStreamEvent::ToolStarted {
                        call_id,
                        name,
                        input,
                    } => {
                        let existing = call_id.as_deref().and_then(|call_id| {
                            message
                                .tool_activities
                                .iter_mut()
                                .rev()
                                .find(|activity| activity.call_id.as_deref() == Some(call_id))
                        });
                        if let Some(activity) = existing {
                            activity.name = name;
                            activity.input = format_tool_input(&input);
                            activity.state = ToolActivityState::Running;
                            activity.summary.clear();
                            activity.output.clear();
                            activity.started_at = Instant::now();
                            activity.duration_ms = None;
                        } else {
                            let text_offset = message.text.len();
                            let group_id = message.assign_tool_group(false);
                            message.tool_activities.push(ToolActivity {
                                call_id,
                                name,
                                text_offset,
                                group_id,
                                input: format_tool_input(&input),
                                summary: String::new(),
                                output: String::new(),
                                state: ToolActivityState::Running,
                                started_at: Instant::now(),
                                duration_ms: None,
                                expanded: false,
                                inside_reasoning: false,
                            });
                        }
                    }
                    AgentStreamEvent::ToolFinished {
                        call_id,
                        name,
                        success,
                        summary,
                        output,
                        ..
                    } => {
                        let existing = call_id.as_deref().and_then(|call_id| {
                            message.tool_activities.iter_mut().rev().find(|activity| {
                                activity.call_id.as_deref() == Some(call_id)
                                    && activity.name == name
                            })
                        });
                        if let Some(activity) = existing {
                            activity.state = if success {
                                ToolActivityState::Completed
                            } else {
                                ToolActivityState::Failed
                            };
                            activity.summary = summary;
                            activity.output = output;
                            activity.duration_ms =
                                Some(activity.started_at.elapsed().as_millis() as u64);
                        } else {
                            let text_offset = message.text.len();
                            let group_id = message.assign_tool_group(false);
                            message.tool_activities.push(ToolActivity {
                                call_id,
                                name,
                                text_offset,
                                group_id,
                                input: String::new(),
                                summary,
                                output,
                                state: if success {
                                    ToolActivityState::Completed
                                } else {
                                    ToolActivityState::Failed
                                },
                                started_at: Instant::now(),
                                duration_ms: Some(0),
                                expanded: false,
                                inside_reasoning: false,
                            });
                        }
                    }
                    AgentStreamEvent::CompactionStarted { reason } => {
                        let text_offset = message.text.len();
                        let group_id = message.assign_tool_group(false);
                        message.tool_activities.push(ToolActivity {
                            call_id: None,
                            name: "compact_conversation".into(),
                            text_offset,
                            group_id,
                            input: reason,
                            summary: String::new(),
                            output: String::new(),
                            state: ToolActivityState::Running,
                            started_at: Instant::now(),
                            duration_ms: None,
                            expanded: false,
                            inside_reasoning: false,
                        });
                    }
                    AgentStreamEvent::CompactionFinished {
                        original_messages,
                        retained_messages,
                        understood_context,
                        ..
                    } => {
                        if let Some(activity) =
                            message.tool_activities.iter_mut().rev().find(|activity| {
                                activity.name == "compact_conversation"
                                    && activity.state == ToolActivityState::Running
                            })
                        {
                            activity.state = ToolActivityState::Completed;
                            activity.summary = format!(
                                "Compacted {original_messages} messages to {retained_messages}."
                            );
                            activity.output = understood_context.unwrap_or_default();
                            activity.duration_ms =
                                Some(activity.started_at.elapsed().as_millis() as u64);
                        }
                    }
                    AgentStreamEvent::ContextUpdated { .. } => {}
                    AgentStreamEvent::DelegatedAgentStarted { .. }
                    | AgentStreamEvent::DelegatedAgentEvent { .. } => unreachable!(),
                }
            }
        }
    }

    fn apply_agent_stream_event(
        &mut self,
        session_id: &SessionId,
        event: AgentStreamEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            AgentStreamEvent::DelegatedAgentStarted { thread } => {
                self.start_agent_thread_transcript(session_id, &thread);
            }
            AgentStreamEvent::DelegatedAgentEvent { thread_id, event } => {
                self.apply_delegated_agent_event(session_id, &thread_id, *event);
            }
            AgentStreamEvent::TextDelta { text } => {
                let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| &session.id == session_id)
                else {
                    return;
                };
                if let Some(message) = session.messages.last_mut() {
                    if !text.is_empty() {
                        message.text.push_str(&text);
                        message.assistant_text_arrived();
                    }
                }
            }
            AgentStreamEvent::ReasoningDelta { text } => {
                let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| &session.id == session_id)
                else {
                    return;
                };
                if let Some(message) = session.messages.last_mut() {
                    message.reasoning.push_str(&text);
                    message.reasoning_arrived();
                    // A tool loop can start a second reasoning phase in the
                    // same assistant bubble. Re-open its live state until
                    // the provider signals the next completed phase.
                    message.reasoning_complete = false;
                }
            }
            AgentStreamEvent::ReasoningFinished => {
                if let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| &session.id == session_id)
                {
                    if let Some(message) = session.messages.last_mut() {
                        message.reasoning_complete = true;
                    }
                }
            }
            AgentStreamEvent::ToolPreparing {
                call_id,
                name,
                input,
                inside_reasoning,
            } => {
                if let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| &session.id == session_id)
                {
                    if let Some(message) = session.messages.last_mut() {
                        if let Some(activity) =
                            message.tool_activities.iter_mut().rev().find(|activity| {
                                activity.call_id.as_deref() == Some(call_id.as_str())
                                    && activity.state == ToolActivityState::Running
                            })
                        {
                            activity.name = name;
                            activity.input = format_tool_input(&input);
                            activity.inside_reasoning = inside_reasoning;
                        } else {
                            let text_offset = message.text.len();
                            let group_id = message.assign_tool_group(inside_reasoning);
                            message.tool_activities.push(ToolActivity {
                                call_id: Some(call_id),
                                name,
                                text_offset,
                                group_id,
                                input: format_tool_input(&input),
                                summary: String::new(),
                                output: String::new(),
                                state: ToolActivityState::Running,
                                started_at: Instant::now(),
                                duration_ms: None,
                                expanded: false,
                                inside_reasoning,
                            });
                        }
                    }
                }
            }
            AgentStreamEvent::ToolStarted {
                call_id,
                name,
                input,
            } => {
                let user_question = if name == "ask_user" {
                    match self.runtime.prepare_user_question(session_id, &input) {
                        Ok(question) => Some(question),
                        Err(error) => {
                            diagnostics::record(
                                DiagnosticLevel::Warning,
                                "ask_user",
                                format!("Could not present the agent question: {error}"),
                            );
                            None
                        }
                    }
                } else {
                    None
                };
                self.record_tool_source(session_id, &name);
                if let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| &session.id == session_id)
                {
                    if user_question.is_some() {
                        session.pending_user_question = user_question;
                    }
                    if let Some(message) = session.messages.last_mut() {
                        if let Some(activity) =
                            message.tool_activities.iter_mut().rev().find(|activity| {
                                activity.state == ToolActivityState::Running
                                    && call_id
                                        .as_deref()
                                        .is_some_and(|id| activity.call_id.as_deref() == Some(id))
                            })
                        {
                            activity.name = name;
                            activity.input = format_tool_input(&input);
                            activity.summary.clear();
                        } else {
                            let text_offset = message.text.len();
                            let group_id = message.assign_tool_group(false);
                            message.tool_activities.push(ToolActivity {
                                call_id,
                                name,
                                text_offset,
                                group_id,
                                input: format_tool_input(&input),
                                summary: String::new(),
                                output: String::new(),
                                state: ToolActivityState::Running,
                                started_at: Instant::now(),
                                duration_ms: None,
                                expanded: false,
                                inside_reasoning: false,
                            });
                        }
                    }
                }
            }
            AgentStreamEvent::ToolFinished {
                call_id,
                name,
                success,
                summary,
                output,
                metadata,
            } => {
                self.record_web_sources(session_id, &name, metadata.as_ref());
                let enabled_tools = (name == "enable_tools" && success)
                    .then(|| {
                        metadata
                            .as_ref()
                            .and_then(|metadata| metadata.get("enabled_tools"))
                            .and_then(|tools| {
                                serde_json::from_value::<Vec<String>>(tools.clone()).ok()
                            })
                    })
                    .flatten();
                let mut binding_changed = false;
                if let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| &session.id == session_id)
                {
                    if let Some(activity) = session.messages.last_mut().and_then(|message| {
                        message.tool_activities.iter_mut().rev().find(|activity| {
                            activity.state == ToolActivityState::Running
                                && call_id
                                    .as_deref()
                                    .is_some_and(|id| activity.call_id.as_deref() == Some(id))
                        })
                    }) {
                        activity.state = if success {
                            ToolActivityState::Completed
                        } else {
                            ToolActivityState::Failed
                        };
                        activity.summary = summary;
                        activity.output = output;
                        activity.duration_ms =
                            Some(activity.started_at.elapsed().as_millis() as u64);
                    }
                    if name == "ask_user" {
                        session.pending_user_question = None;
                    }
                    if let Some(enabled_tools) = enabled_tools {
                        binding_changed = apply_enabled_tools(&mut session.binding, enabled_tools);
                        if binding_changed {
                            diagnostics::record(
                                DiagnosticLevel::Info,
                                "tools.discovery",
                                format!(
                                    "Persisting {} activated tool(s) for conversation {}.",
                                    session.binding.tools.len(),
                                    session.id
                                ),
                            );
                        }
                    }
                }
                if binding_changed {
                    self.persist_session_binding(session_id, cx);
                }
                if let Some(thread) = metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("agent_thread"))
                    .and_then(|thread| {
                        serde_json::from_value::<AgentThreadSnapshot>(thread.clone()).ok()
                    })
                {
                    if let Some(session) = self
                        .sessions
                        .iter_mut()
                        .find(|session| &session.id == session_id)
                    {
                        if let Some(existing) = session
                            .agent_threads
                            .iter_mut()
                            .find(|existing| existing.id == thread.id)
                        {
                            *existing = thread;
                        } else {
                            session.agent_threads.push(thread);
                        }
                    }
                }
                let checkpoint = metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("checkpoint").cloned())
                    .and_then(|checkpoint| {
                        serde_json::from_value::<WorkCheckpoint>(checkpoint).ok()
                    });
                if let Some(checkpoint) = checkpoint {
                    self.update_checkpoint(session_id, checkpoint);
                }
                let task = metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("task").cloned())
                    .and_then(|task| serde_json::from_value::<WorkTask>(task).ok());
                if let Some(task) = task {
                    self.update_task(session_id, task);
                }
            }
            AgentStreamEvent::ContextUpdated { usage } => {
                if let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| &session.id == session_id)
                {
                    session.context_usage = usage;
                }
            }
            AgentStreamEvent::CompactionStarted { reason } => {
                self.start_compaction_activity(session_id, reason);
                if let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| &session.id == session_id)
                {
                    session.context_busy = true;
                }
            }
            AgentStreamEvent::CompactionFinished {
                original_messages,
                retained_messages,
                understood_context,
                ..
            } => {
                self.finish_compaction_activity(
                    session_id,
                    original_messages,
                    retained_messages,
                    understood_context.as_deref(),
                );
                if let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| &session.id == session_id)
                {
                    session.context_busy = false;
                    if understood_context.is_some() {
                        session.context_summary = understood_context;
                    }
                }
            }
        }
        self.remeasure_active_conversation_tail(session_id);
    }

    fn update_checkpoint(&mut self, session_id: &SessionId, checkpoint: WorkCheckpoint) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
        else {
            return;
        };
        let mut checkpoint = checkpoint;
        if checkpoint.message_position.is_none() {
            checkpoint.message_position = session.messages.len().checked_sub(1);
        }
        if let Some(existing) = session
            .checkpoints
            .iter_mut()
            .find(|existing| existing.id == checkpoint.id)
        {
            *existing = checkpoint;
        } else {
            session.checkpoints.push(checkpoint);
        }
    }

    fn update_task(&mut self, session_id: &SessionId, task: WorkTask) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
        else {
            return;
        };
        if let Some(existing) = session
            .tasks
            .iter_mut()
            .find(|existing| existing.id == task.id)
        {
            *existing = task;
        } else {
            session.tasks.push(task);
        }
        session.tasks.sort_by_key(|task| {
            (
                matches!(task.status, TaskStatus::Done),
                task.created_at,
                task.id.clone(),
            )
        });
    }

    fn submit_user_question_answer(
        &mut self,
        session_id: &SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = self.ask_user_input.read(cx).value().trim().to_owned();
        self.answer_user_question(session_id, answer, window, cx);
    }

    fn answer_user_question(
        &mut self,
        session_id: &SessionId,
        answer: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(question) = self
            .sessions
            .iter()
            .find(|session| &session.id == session_id)
            .and_then(|session| session.pending_user_question.clone())
        else {
            return;
        };
        if answer.trim().is_empty() {
            self.show_error(i18n::text(cx, "notice.enter_answer"), cx);
            return;
        }
        if !self
            .runtime
            .answer_user_question(session_id, &question.id, answer)
        {
            self.show_error(i18n::text(cx, "notice.question_missing"), cx);
            return;
        }
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == session_id)
        {
            session.pending_user_question = None;
        }
        self.ask_user_input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.remeasure_active_conversation_tail(session_id);
        cx.notify();
    }

    fn select_connection(
        &mut self,
        value: Option<&SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection_id = value.and_then(|value| {
            self.connection_labels
                .iter()
                .find(|(label, _)| label == value)
                .map(|(_, id)| id.clone())
        });
        self.active_mut().binding.connection_id = connection_id.clone();
        self.active_mut().binding.model_id = None;
        self.active_mut().binding.reasoning_effort = None;
        self.active_mut().agent = None;

        self.refresh_model_picker(window, cx);
        if let Some(id) = connection_id {
            let is_codex = self
                .runtime
                .connection(&id)
                .is_some_and(|profile| profile.kind == ConnectionKind::Codex);
            if is_codex {
                self.load_codex_models(cx);
            } else {
                self.load_direct_models(id, cx);
            }
        }
        if self.active().persisted {
            self.persist_active(cx);
        }
        cx.notify();
    }

    fn select_workspace(&mut self, value: Option<&WorkspaceChoice>, cx: &mut Context<Self>) {
        if self.active().processing || !self.active().messages.is_empty() {
            return;
        }

        let project_id = value.and_then(|choice| choice.id.clone());
        let project = project_id.as_ref().and_then(|id| {
            self.projects
                .iter()
                .find(|project| &project.id == id)
                .cloned()
        });
        if project_id.is_some() && project.is_none() {
            self.show_error(i18n::text(cx, "notice.workspace_missing"), cx);
            return;
        }

        let workspace_root = project.as_ref().map(|project| project.root.clone());
        {
            let session = self.active_mut();
            session.project_id = project_id;
            session.workspace_root = workspace_root.clone();
            session.agent = None;
        }
        if let Some(root) = workspace_root {
            self.runtime.load_workspace_tools(&root);
        }
        self.notice = None;
        cx.notify();
    }

    fn select_model(
        &mut self,
        value: Option<&ModelChoice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(choice) = value.cloned() else {
            self.active_mut().binding.model_id = None;
            self.active_mut().binding.reasoning_effort = None;
            self.active_mut().agent = None;
            self.refresh_model_picker(window, cx);
            cx.notify();
            return;
        };

        self.active_mut().binding.connection_id = Some(choice.connection_id.clone());
        self.active_mut().binding.model_id = Some(choice.info.id.clone());
        self.active_mut().binding.reasoning_effort = preferred_reasoning_effort(&choice.info);
        self.active_mut().agent = None;
        self.notice = None;
        self.sync_connection_picker(window, cx);
        self.refresh_model_picker(window, cx);
        let binding = self.active().binding.clone();
        if binding.is_ready() {
            match self.runtime.database.remember_binding(&binding) {
                Ok(()) => self.remembered_binding = binding,
                Err(error) => {
                    self.notice = Some(Notice {
                        success: false,
                        text: error.to_string(),
                    });
                }
            }
        }
        if self.active().persisted {
            self.persist_active(cx);
        }
        cx.notify();
    }

    fn select_reasoning_effort(
        &mut self,
        value: Option<&SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let effort = value.and_then(|value| reasoning_effort_from_label(value.as_str()));
        self.active_mut().binding.reasoning_effort = effort.clone();
        if let Some(agent) = self.active().agent.as_ref() {
            agent.set_reasoning_effort(effort);
        }
        self.notice = None;
        self.refresh_reasoning_picker(window, cx);
        let binding = self.active().binding.clone();
        if binding.is_ready() {
            match self.runtime.database.remember_binding(&binding) {
                Ok(()) => self.remembered_binding = binding,
                Err(error) => {
                    self.notice = Some(Notice {
                        success: false,
                        text: error.to_string(),
                    });
                }
            }
        }
        if self.active().persisted {
            self.persist_active(cx);
        }
        cx.notify();
    }

    fn sync_connection_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.active().binding.connection_id.as_ref().and_then(|id| {
            self.connection_labels
                .iter()
                .find(|(_, candidate)| candidate == id)
                .map(|(label, _)| label.clone())
        });
        self.connection_select
            .update(cx, |select, cx| match selected.as_ref() {
                Some(label) => select.set_selected_value(label, window, cx),
                None => select.set_selected_index(None, window, cx),
            });
    }

    fn sync_workspace_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project_id = self.active().project_id.clone();
        let items = workspace_choices(&self.projects);
        let selected = items
            .iter()
            .find(|choice| choice.id.as_ref() == project_id.as_ref())
            .cloned();
        self.workspace_select.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            match selected.as_ref() {
                Some(choice) => select.set_selected_value(choice, window, cx),
                None => select.set_selected_index(None, window, cx),
            }
        });
    }

    fn refresh_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_selected_model_choice();
        let items = grouped_model_items(&self.model_choices);
        let selected = self
            .active()
            .binding
            .model_id
            .as_ref()
            .and_then(|model_id| {
                self.active()
                    .binding
                    .connection_id
                    .as_ref()
                    .and_then(|connection_id| {
                        self.model_choices.iter().find(|choice| {
                            &choice.connection_id == connection_id && &choice.info.id == model_id
                        })
                    })
                    .cloned()
            });
        self.model_select.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            match selected.as_ref() {
                Some(choice) => select.set_selected_value(choice, window, cx),
                None => select.set_selected_index(None, window, cx),
            }
        });
        self.refresh_reasoning_picker(window, cx);
    }

    fn refresh_reasoning_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let choice = self
            .active()
            .binding
            .model_id
            .as_ref()
            .and_then(|model_id| {
                self.active()
                    .binding
                    .connection_id
                    .as_ref()
                    .and_then(|connection_id| {
                        self.model_choices.iter().find(|choice| {
                            &choice.connection_id == connection_id && &choice.info.id == model_id
                        })
                    })
            });
        let mut items = vec![i18n::text(cx, "reasoning.auto")];
        if let Some(choice) = choice {
            for effort in &choice.info.available_reasoning_efforts {
                let label = localized_reasoning_effort_label(cx, effort);
                if !items.iter().any(|item| item == &label) {
                    items.push(label);
                }
            }
        }
        let selected_label = self
            .active()
            .binding
            .reasoning_effort
            .as_deref()
            .map(|effort| localized_reasoning_effort_label(cx, effort))
            .unwrap_or_else(|| i18n::text(cx, "reasoning.auto"));
        self.reasoning_select.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            if select
                .selected_value()
                .is_some_and(|selected| selected == &selected_label)
            {
                return;
            }
            select.set_selected_value(&selected_label, window, cx);
        });
    }

    fn ensure_selected_model_choice(&mut self) {
        let Some(connection_id) = self.active().binding.connection_id.clone() else {
            return;
        };
        let Some(model_id) = self.active().binding.model_id.clone() else {
            return;
        };
        if self
            .model_choices
            .iter()
            .any(|choice| choice.connection_id == connection_id && choice.info.id == model_id)
        {
            return;
        }
        let connection_name = self
            .runtime
            .connection(&connection_id)
            .map(|profile| profile.name)
            .unwrap_or_else(|| "Connection".into());
        self.model_choices.push(ModelChoice {
            connection_id,
            connection_name: connection_name.into(),
            info: ModelInfo {
                id: model_id.clone(),
                display_name: model_id,
                provider: "unknown".into(),
                description: None,
                capabilities: averroes_core::provider::ModelCapabilities {
                    chat: true,
                    embeddings: false,
                    vision: false,
                    tools: true,
                },
                source: ModelSource::Curated,
                featured: false,
                default_reasoning_effort: None,
                available_reasoning_efforts: Vec::new(),
            },
        });
    }

    fn refresh_codex_account(&mut self, cx: &mut Context<Self>) {
        if self.codex_busy {
            return;
        }
        self.codex_busy = true;
        let runtime = self.runtime.clone();
        let task_runtime = runtime.clone();
        let request = runtime.spawn_background(async move { task_runtime.codex_account().await });
        cx.spawn(async move |this, cx| {
            let result = flatten_background(request.await);
            _ = this.update(cx, |app, cx| {
                app.codex_busy = false;
                match result {
                    Ok(account) => app.codex_account = Some(account),
                    Err(error) => {
                        app.codex_account = None;
                        app.notice = Some(Notice {
                            success: false,
                            text: error.to_string(),
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_codex_login(&mut self, cx: &mut Context<Self>) {
        if self.codex_busy {
            return;
        }
        self.codex_busy = true;
        self.notice = Some(Notice {
            success: true,
            text: "Starting secure ChatGPT sign-in…".into(),
        });
        let runtime = self.runtime.clone();
        let task_runtime = runtime.clone();
        let login_request =
            runtime.spawn_background(async move { task_runtime.start_codex_login().await });
        cx.spawn(async move |this, cx| {
            let login = match flatten_background(login_request.await) {
                Ok(login) => login,
                Err(error) => {
                    _ = this.update(cx, |app, cx| {
                        app.codex_busy = false;
                        app.notice = Some(Notice {
                            success: false,
                            text: error.to_string(),
                        });
                        cx.notify();
                    });
                    return;
                }
            };
            _ = this.update(cx, |app, cx| {
                cx.open_url(&login.auth_url);
                app.notice = Some(Notice {
                    success: true,
                    text: "Finish signing in in your browser. The OAuth session stays encrypted in Averroes' private storage."
                        .into(),
                });
                cx.notify();
            });
            let login_id = login.login_id.clone();
            let task_runtime = runtime.clone();
            let wait_request = runtime.spawn_background(async move {
                task_runtime.wait_for_codex_login(&login_id).await
            });
            let result = flatten_background(wait_request.await);
            _ = this.update(cx, |app, cx| {
                app.codex_busy = false;
                match result {
                    Ok(account) => {
                        app.codex_account = Some(account);
                        app.notice = Some(Notice {
                            success: true,
                            text: "ChatGPT connected to Averroes.".into(),
                        });
                        // The central catalog refresh updates every Codex
                        // connection, including one that is not the active
                        // conversation. This keeps Settings and the grouped
                        // model picker in sync after OAuth completes.
                        app.refresh_model_catalogs(cx);
                    }
                    Err(error) => {
                        app.notice = Some(Notice {
                            success: false,
                            text: error.to_string(),
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn start_copilot_login(&mut self, connection_id: ConnectionId, cx: &mut Context<Self>) {
        if self.copilot_busy {
            return;
        }
        self.copilot_busy = true;
        self.notice = Some(Notice {
            success: true,
            text: "Starting secure GitHub sign-in…".into(),
        });
        let runtime = self.runtime.clone();
        let task_runtime = runtime.clone();
        let login_connection = connection_id.clone();
        let catalog_connection = connection_id;
        let login_request = runtime.spawn_background(async move {
            task_runtime.start_copilot_login(&login_connection).await
        });
        cx.spawn(async move |this, cx| {
            let login = match flatten_background(login_request.await) {
                Ok(login) => login,
                Err(error) => {
                    _ = this.update(cx, |app, cx| {
                        app.copilot_busy = false;
                        app.notice = Some(Notice {
                            success: false,
                            text: error.to_string(),
                        });
                        cx.notify();
                    });
                    return;
                }
            };
            _ = this.update(cx, |app, cx| {
                cx.open_url(&login.auth_url);
                app.notice = Some(Notice {
                    success: true,
                    text: format!(
                        "Enter code {} in GitHub to connect this Copilot account. The token is encrypted in Averroes' private storage.",
                        login.user_code
                    ),
                });
                cx.notify();
            });
            let login_id = login.login_id.clone();
            let task_runtime = runtime.clone();
            let wait_request = runtime.spawn_background(async move {
                task_runtime.wait_for_copilot_login(&login_id).await
            });
            let result = flatten_background(wait_request.await);
            _ = this.update(cx, |app, cx| {
                app.copilot_busy = false;
                app.notice = Some(match result {
                    Ok(()) => {
                        // Populate the connection's grouped model catalog as
                        // soon as GitHub finishes authorizing it. Previously a
                        // user had to change the connection selection again
                        // before all of its models could appear.
                        app.load_direct_models_with_status(catalog_connection.clone(), true, cx);
                        Notice {
                            success: true,
                            text: "GitHub authorized. Loading the Copilot model catalog…".into(),
                        }
                    }
                    Err(error) => Notice {
                        success: false,
                        text: error.to_string(),
                    },
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_codex_models(&mut self, cx: &mut Context<Self>) {
        let Some(connection_id) = self.active().binding.connection_id.clone() else {
            return;
        };
        self.codex_busy = true;
        self.notice = Some(Notice {
            success: true,
            text: "Loading models available to your ChatGPT account…".into(),
        });
        let runtime = self.runtime.clone();
        let task_runtime = runtime.clone();
        let request_id = connection_id.clone();
        let request = runtime.spawn_background(async move {
            task_runtime.live_models_for_connection(&request_id).await
        });
        cx.spawn(async move |this, cx| {
            let result = flatten_background(request.await);
            _ = this.update_in(cx, |app, window, cx| {
                app.codex_busy = false;
                let still_codex = app
                    .active()
                    .binding
                    .connection_id
                    .as_ref()
                    .filter(|id| *id == &connection_id)
                    .and_then(|id| app.runtime.connection(id))
                    .is_some_and(|profile| profile.kind == ConnectionKind::Codex);
                if !still_codex {
                    return;
                }
                match result {
                    Ok(models) => {
                        let model_count = models.len();
                        app.replace_model_choices(&connection_id, models);
                        app.refresh_model_picker(window, cx);
                        app.refresh_agent_model_picker(window, cx);
                        app.notice = if model_count == 0 {
                            Some(Notice {
                                success: false,
                                text: "ChatGPT returned no available Codex models. Open Diagnostics to inspect the catalog response.".into(),
                            })
                        } else {
                            None
                        };
                    }
                    Err(error) => {
                        app.notice = Some(Notice {
                            success: false,
                            text: format!("{error}. Connect ChatGPT from Connections and retry."),
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn replace_model_choices(&mut self, connection_id: &ConnectionId, models: Vec<ModelInfo>) {
        let connection_name = self
            .runtime
            .connection(connection_id)
            .map(|profile| profile.name)
            .unwrap_or_else(|| "Connection".into());
        let is_copilot = self
            .runtime
            .connection(connection_id)
            .is_some_and(|profile| profile.kind == ConnectionKind::Copilot);
        let selected_model_is_unavailable = is_copilot
            && self.active().binding.connection_id.as_ref() == Some(connection_id)
            && self
                .active()
                .binding
                .model_id
                .as_ref()
                .is_some_and(|selected| !models.iter().any(|model| &model.id == selected));
        if selected_model_is_unavailable {
            self.active_mut().binding.model_id = None;
            self.active_mut().binding.reasoning_effort = None;
            self.active_mut().agent = None;
            self.notice = Some(Notice {
                success: false,
                text:
                    "That Copilot model is no longer available. Choose one from GitHub's catalog."
                        .into(),
            });
        }
        self.model_choices
            .retain(|choice| &choice.connection_id != connection_id);
        self.model_choices.extend(
            models
                .into_iter()
                .filter(|info| info.capabilities.chat)
                .map(|info| ModelChoice {
                    connection_id: connection_id.clone(),
                    connection_name: connection_name.clone().into(),
                    info,
                }),
        );
        self.ensure_selected_model_choice();
    }

    fn load_direct_models(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        self.load_direct_models_with_status(id, false, cx);
    }

    fn load_direct_models_with_status(
        &mut self,
        id: ConnectionId,
        report_status: bool,
        cx: &mut Context<Self>,
    ) {
        let runtime = self.runtime.clone();
        let task_runtime = runtime.clone();
        let request_id = id.clone();
        let request = runtime.spawn_background(async move {
            task_runtime.live_models_for_connection(&request_id).await
        });
        cx.spawn(async move |this, cx| {
            let result = flatten_background(request.await);
            _ = this.update_in(cx, |app, window, cx| {
                let active_connection = app.active().binding.connection_id.as_ref() == Some(&id);
                let is_copilot = app
                    .runtime
                    .connection(&id)
                    .is_some_and(|profile| profile.kind == ConnectionKind::Copilot);
                match result {
                    Ok(models) if !models.is_empty() => {
                        let model_count = models.len();
                        app.replace_model_choices(&id, models);
                        // The model menu is grouped across every connection,
                        // so an inactive connection's background refresh must
                        // also update its SelectState. Updating only the active
                        // connection left successfully loaded Copilot models in
                        // memory but invisible until another selection change.
                        app.refresh_model_picker(window, cx);
                        app.refresh_agent_model_picker(window, cx);
                        if is_copilot {
                            diagnostics::record(
                                DiagnosticLevel::Success,
                                "ui.models",
                                format!(
                                    "Synchronized {model_count} GitHub Copilot models into the picker."
                                ),
                            );
                        }
                        if report_status {
                            app.notice = Some(Notice {
                                success: true,
                                text: if is_copilot {
                                    format!(
                                        "GitHub Copilot connected · {model_count} models loaded."
                                    )
                                } else {
                                    format!("{model_count} models loaded.")
                                },
                            });
                        }
                    }
                    Ok(_) => {
                        if is_copilot {
                            app.replace_model_choices(&id, Vec::new());
                            if active_connection || report_status {
                                app.notice = Some(Notice {
                                    success: false,
                                    text: "GitHub returned no selectable Copilot models for this authorization. Open Diagnostics below for the endpoint and HTTP status.".into(),
                                });
                            }
                            app.refresh_model_picker(window, cx);
                        }
                    }
                    Err(error) => {
                        if is_copilot {
                            app.replace_model_choices(&id, Vec::new());
                            app.refresh_model_picker(window, cx);
                        }
                        if active_connection || report_status {
                            app.notice = Some(Notice {
                                success: false,
                                text: if is_copilot {
                                    format!(
                                        "Could not load GitHub Copilot models. {error} Open Diagnostics below for the request trace."
                                    )
                                } else {
                                    format!("Could not refresh live models; showing the cached catalog. {error}")
                                },
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connection_labels = self
            .runtime
            .connections()
            .into_iter()
            .map(|profile| (profile.name.into(), profile.id))
            .collect();
        let items = self
            .connection_labels
            .iter()
            .map(|(label, _)| label.clone())
            .collect();
        self.connection_select.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            select.set_selected_index(None, window, cx);
        });
        self.model_choices = initial_model_choices(&self.runtime);
        self.refresh_model_picker(window, cx);
        self.sync_agent_selectors(window, cx);
    }

    fn save_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected_from_control = self.kind_select.read(cx).selected_value().copied();
        let Some(kind) = effective_connection_kind(self.selected_kind, selected_from_control)
        else {
            self.show_error("Choose how you want to connect", cx);
            return;
        };
        let name = self.name_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.show_error("Give this connection a recognizable name", cx);
            return;
        }
        let base_url = self.url_input.read(cx).value().trim().to_string();
        let secret = self.key_input.read(cx).value().trim().to_string();
        let id = uuid::Uuid::new_v4().simple().to_string();
        let profile = match kind {
            ConnectionKind::QDivZero => ConnectionProfile::qdivzero(id, name),
            ConnectionKind::Codex => ConnectionProfile::codex(id, name),
            ConnectionKind::Copilot => ConnectionProfile::copilot(id, name),
            ConnectionKind::DeepSeek => ConnectionProfile::deepseek(id, name),
            ConnectionKind::Groq => ConnectionProfile::groq(id, name),
            ConnectionKind::Ollama => {
                ConnectionProfile::ollama(id, name, (!base_url.is_empty()).then_some(base_url))
            }
            ConnectionKind::OllamaCloud => ConnectionProfile::ollama_cloud(
                id,
                name,
                (!base_url.is_empty()).then_some(base_url),
            ),
            ConnectionKind::OpenAi | ConnectionKind::Anthropic | ConnectionKind::Compatible => {
                ConnectionProfile::api(id, name, kind, (!base_url.is_empty()).then_some(base_url))
            }
        };

        let should_start_copilot_login = kind == ConnectionKind::Copilot && secret.is_empty();
        let profile_id = profile.id.clone();
        let secret = kind.requires_api_key().then_some(secret.as_str());
        match self.runtime.save_connection(profile, secret) {
            Ok(()) => {
                self.refresh_connections(window, cx);
                self.refresh_embedding_connections(window, cx);
                self.selected_kind = None;
                self.show_manual_copilot_token = false;
                self.kind_select
                    .update(cx, |select, cx| select.set_selected_index(None, window, cx));
                for input in [&self.name_input, &self.url_input, &self.key_input] {
                    input.update(cx, |state, cx| state.set_value("", window, cx));
                }
                self.notice = Some(Notice {
                    success: true,
                    text: if should_start_copilot_login {
                        "Connection saved. Opening GitHub sign-in…".into()
                    } else {
                        "Connection saved. Select it explicitly in a conversation.".into()
                    },
                });
                // A newly saved API connection has no catalog in the central
                // registry yet. Refresh it immediately so QDivZero (and any
                // other remote provider) can populate the model picker
                // without requiring an app restart or a manual refresh.
                if !should_start_copilot_login {
                    self.refresh_model_catalogs(cx);
                }
                if should_start_copilot_login {
                    self.start_copilot_login(profile_id, cx);
                }
                cx.notify();
            }
            Err(error) => self.show_error(error.to_string(), cx),
        }
    }

    fn add_manual_model(
        &mut self,
        connection_id: ConnectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let model_id = self
            .manual_model_id_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if model_id.is_empty() {
            self.show_error("Enter a model ID", cx);
            return;
        }

        let display_name = self
            .manual_model_name_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let reasoning_efforts = self
            .manual_model_reasoning_input
            .read(cx)
            .value()
            .split(',')
            .map(str::trim)
            .filter(|effort| !effort.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let model = ManualModel {
            id: model_id.clone(),
            display_name: (!display_name.is_empty()).then_some(display_name),
            description: None,
            vision: false,
            embeddings: false,
            tools: true,
            default_reasoning_effort: reasoning_efforts.first().cloned(),
            reasoning_efforts,
            featured: false,
        };

        match self.runtime.add_manual_model(&connection_id, model) {
            Ok(()) => {
                self.refresh_connections(window, cx);
                self.refresh_embedding_connections(window, cx);
                self.manual_model_id_input
                    .update(cx, |state, cx| state.set_value("", window, cx));
                self.manual_model_name_input
                    .update(cx, |state, cx| state.set_value("", window, cx));
                self.manual_model_reasoning_input
                    .update(cx, |state, cx| state.set_value("", window, cx));
                self.manual_model_connection = None;
                self.notice = Some(Notice {
                    success: true,
                    text: format!("Model {model_id} added to the integration."),
                });
                cx.notify();
            }
            Err(error) => self.show_error(error.to_string(), cx),
        }
    }

    fn delete_connection(&mut self, id: ConnectionId, window: &mut Window, cx: &mut Context<Self>) {
        match self.runtime.delete_connection(&id) {
            Ok(true) => {
                for session in &mut self.sessions {
                    if session.binding.connection_id.as_ref() == Some(&id) {
                        session.binding.connection_id = None;
                        session.binding.model_id = None;
                        session.agent = None;
                    }
                }
                if self.remembered_binding.connection_id.as_ref() == Some(&id) {
                    self.remembered_binding.connection_id = None;
                    self.remembered_binding.model_id = None;
                }
                if let Err(error) = self.runtime.database.forget_binding_for_connection(&id) {
                    self.show_error(error.to_string(), cx);
                    return;
                }
                self.refresh_connections(window, cx);
                self.refresh_embedding_connections(window, cx);
                self.notice = Some(Notice {
                    success: true,
                    text: "Connection and encrypted credential removed.".into(),
                });
                cx.notify();
            }
            Ok(false) => self.show_error("That connection no longer exists", cx),
            Err(error) => self.show_error(error.to_string(), cx),
        }
    }

    fn show_error(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(Notice {
            success: false,
            text: text.into(),
        });
        cx.notify();
    }

    fn toggle_context_sidebar(&mut self, cx: &mut Context<Self>) {
        self.show_context = !self.show_context;
        if !self.show_context {
            self.selected_agent_thread = None;
        }
        cx.notify();
    }

    fn select_agent_thread(&mut self, thread_id: String, cx: &mut Context<Self>) {
        self.open_agent_thread(thread_id, cx);
    }

    fn open_agent_thread(&mut self, thread_id: String, cx: &mut Context<Self>) {
        let exists = self
            .active()
            .agent_threads
            .iter()
            .any(|thread| thread.id == thread_id)
            || self
                .runtime
                .agent_threads_for(self.active().id.as_str())
                .iter()
                .any(|thread| thread.id == thread_id);
        if !exists {
            return;
        }
        self.agent_thread_view = Some(thread_id);
        self.selected_agent_thread = None;
        self.show_context = false;
        cx.notify();
    }

    fn open_latest_agent_thread(&mut self, parent_session_id: &SessionId, cx: &mut Context<Self>) {
        let local = self
            .sessions
            .iter()
            .find(|session| &session.id == parent_session_id)
            .and_then(|session| {
                session
                    .agent_threads
                    .iter()
                    .max_by_key(|thread| thread.updated_at)
                    .map(|thread| thread.id.clone())
            });
        let remote = self
            .runtime
            .agent_threads_for(parent_session_id.as_str())
            .into_iter()
            .max_by_key(|thread| thread.updated_at)
            .map(|thread| thread.id);
        if let Some(thread_id) = local.or(remote) {
            self.open_agent_thread(thread_id, cx);
        }
    }

    fn open_agent_thread_for_tool(
        &mut self,
        parent_session_id: &SessionId,
        tool_output: &str,
        cx: &mut Context<Self>,
    ) {
        let thread_id = tool_output.lines().find_map(|line| {
            line.trim()
                .strip_prefix("thread_id:")
                .map(str::trim)
                .filter(|thread_id| !thread_id.is_empty())
                .map(str::to_owned)
        });
        if let Some(thread_id) = thread_id {
            self.open_agent_thread(thread_id, cx);
        } else {
            self.open_latest_agent_thread(parent_session_id, cx);
        }
    }

    fn close_agent_thread(&mut self, cx: &mut Context<Self>) {
        self.agent_thread_view = None;
        self.selected_agent_thread = None;
        cx.notify();
    }

    fn force_compact_active(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self.active().agent.clone() else {
            self.show_error("Send a message before compacting the context", cx);
            return;
        };
        if self.active().context_busy || self.active().processing {
            return;
        }

        let session_id = self.active().id.clone();
        self.start_compaction_activity(&session_id, "Manual context compaction requested.".into());
        self.active_mut().context_busy = true;
        cx.notify();

        let runtime = self.runtime.clone();
        let request = runtime.spawn_background(async move {
            let original_messages = agent.message_count().await;
            let result = agent.force_compact().await;
            let retained_messages = agent.message_count().await;
            let understood_context = agent.understood_context();
            (
                result,
                original_messages,
                retained_messages,
                understood_context,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = request
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()));
            _ = this.update(cx, |app, cx| {
                let mut error = None;
                match result {
                    Ok((Ok(usage), original_messages, retained_messages, understood_context)) => {
                        app.finish_compaction_activity(
                            &session_id,
                            original_messages,
                            retained_messages,
                            understood_context.as_deref(),
                        );
                        if let Some(session) = app
                            .sessions
                            .iter_mut()
                            .find(|session| session.id == session_id)
                        {
                            session.context_busy = false;
                            session.context_usage = usage;
                            session.context_summary = understood_context;
                        }
                        app.persist_session(&session_id, cx);
                    }
                    Ok((Err(result_error), _, _, _)) => {
                        app.fail_compaction_activity(&session_id, result_error.to_string());
                        if let Some(session) = app
                            .sessions
                            .iter_mut()
                            .find(|session| session.id == session_id)
                        {
                            session.context_busy = false;
                        }
                        error = Some(result_error.to_string());
                    }
                    Err(result_error) => {
                        app.fail_compaction_activity(&session_id, result_error.to_string());
                        if let Some(session) = app
                            .sessions
                            .iter_mut()
                            .find(|session| session.id == session_id)
                        {
                            session.context_busy = false;
                        }
                        error = Some(result_error.to_string());
                    }
                }
                if let Some(error) = error {
                    app.show_error(format!("Could not compact context: {error}"), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.route != Route::Chat {
            return;
        }
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }

        if self.active().processing {
            self.queue_composer_message(window, cx);
            return;
        }

        let attachments = std::mem::take(&mut self.attachments);
        self.start_message_request(text, attachments, true, window, cx);
    }

    fn start_message_request(
        &mut self,
        text: String,
        attachments: Vec<ComposerAttachment>,
        clear_composer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let binding = self.active().binding.clone();
        if let Err(error) = self.runtime.validate_binding(&binding) {
            self.attachments.extend(attachments);
            self.show_error(error.to_string(), cx);
            return;
        }
        let request_provider = binding
            .connection_id
            .as_ref()
            .and_then(|id| self.runtime.connection(id))
            .map(|profile| profile.kind.label())
            .unwrap_or("Unknown provider");
        diagnostics::record(
            DiagnosticLevel::Info,
            "agent.request",
            format!(
                "Starting {request_provider} request with model {}.",
                binding.model_id.as_deref().unwrap_or("unknown")
            ),
        );

        let session_id = self.active().id.clone();
        let existing_agent = self.active().agent.clone();
        let reuse_existing_agent = existing_agent.is_some();
        let working_dir = self.active().workspace_root.clone();
        let restored_history = shell_messages_to_agent_history(&self.active().messages);
        let restored_context = self.active().context_summary.clone();
        let restored_usage = self.active().context_usage;
        let attachment_paths = attachments
            .iter()
            .map(|attachment| attachment.path.clone())
            .collect::<Vec<_>>();
        let display_text = composer_message_label(&text, &attachments);
        let title_source = if text.is_empty() {
            attachments
                .first()
                .map(|attachment| composer_attachment_name(&attachment.path))
                .unwrap_or_else(|| "New conversation".into())
        } else {
            text.clone()
        };
        let should_generate_title = self.active().messages.is_empty();
        let fallback_title = short_title(&title_source);

        if should_generate_title {
            self.active_mut().title = fallback_title.clone();
        }
        self.active_mut()
            .messages
            .push(ShellMessage::user(display_text));
        self.active_mut().messages.push(ShellMessage::assistant());
        self.active_mut().processing = true;
        self.active_mut().unread = false;
        self.reset_conversation_scroll();
        self.notice = None;
        if clear_composer {
            self.composer
                .update(cx, |state, cx| state.set_value("", window, cx));
        }
        if !self.persist_active(cx) {
            self.active_mut().processing = false;
            self.attachments = attachments;
            return;
        }
        cx.notify();

        let runtime = self.runtime.clone();
        let stream_session_id = session_id.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let request_text = if attachment_paths.is_empty() {
                Ok::<String, anyhow::Error>(text)
            } else {
                match runtime
                    .spawn_background(load_attachment_context(text, attachment_paths))
                    .await
                {
                    Ok(result) => result,
                    Err(error) => Err(anyhow::anyhow!(error.to_string())),
                }
            };
            let request_text = match request_text {
                Ok(request_text) => request_text,
                Err(error) => {
                    diagnostics::record(
                        DiagnosticLevel::Error,
                        "agent.attachments",
                        format!("Could not load attached files: {error}"),
                    );
                    _ = this.update(cx, |app, cx| {
                        if let Some(session) = app
                            .sessions
                            .iter_mut()
                            .find(|session| session.id == stream_session_id)
                        {
                            session.processing = false;
                            session.task = None;
                            if let Some(message) = session.messages.last_mut() {
                                message.role = MessageRole::Error;
                                message.text = format!("Could not attach files: {error}");
                            }
                        }
                        app.remeasure_active_conversation_tail(&stream_session_id);
                        app.persist_session(&stream_session_id, cx);
                    });
                    return;
                }
            };
            let agent = match existing_agent {
                Some(agent) => agent,
                None => {
                    let task_runtime = runtime.clone();
                    let agent_session_id = stream_session_id.clone();
                    let agent_binding = binding.clone();
                    let agent_working_dir = working_dir.clone();
                    let agent_history = restored_history.clone();
                    let agent_context = restored_context.clone();
                    let agent_usage = restored_usage;
                    let request = runtime.spawn_background(async move {
                        let agent = task_runtime
                            .new_agent(
                                &agent_session_id,
                                &agent_binding,
                                agent_working_dir.as_deref(),
                            )
                            .await?;
                        agent.restore_conversation_history(agent_history).await;
                        agent.set_understood_context(agent_context);
                        agent.set_context_usage(agent_usage);
                        Ok(agent)
                    });
                    match flatten_background(request.await) {
                        Ok(agent) => {
                            _ = this.update(cx, |app, _| {
                                if let Some(session) = app
                                    .sessions
                                    .iter_mut()
                                    .find(|session| session.id == stream_session_id)
                                {
                                    session.agent = Some(agent.clone());
                                }
                            });
                            agent
                        }
                        Err(error) => {
                            diagnostics::record(
                                DiagnosticLevel::Error,
                                "agent.request",
                                format!("Could not create the provider request: {error}"),
                            );
                            _ = this.update(cx, |app, cx| {
                                let unread = conversation_has_unread_update(
                                    app.route,
                                    &app.active().id,
                                    &stream_session_id,
                                );
                                if let Some(session) = app
                                    .sessions
                                    .iter_mut()
                                    .find(|session| session.id == stream_session_id)
                                {
                                    session.processing = false;
                                    session.task = None;
                                    session.unread = unread;
                                    if let Some(message) = session.messages.last_mut() {
                                        message.role = MessageRole::Error;
                                        message.text = format!("Could not start request: {error}");
                                    }
                                }
                                app.remeasure_active_conversation_tail(&stream_session_id);
                                app.persist_session(&stream_session_id, cx);
                            });
                            return;
                        }
                    }
                }
            };
            if reuse_existing_agent {
                if let Some(workspace_root) = working_dir.as_deref() {
                    let refresh_runtime = runtime.clone();
                    let refresh_agent = agent.clone();
                    let refresh_root = workspace_root.to_path_buf();
                    let refresh = runtime.spawn_background(async move {
                        refresh_runtime.refresh_agent_skills(&refresh_agent, &refresh_root);
                    });
                    if let Err(error) = refresh.await {
                        diagnostics::record(
                            DiagnosticLevel::Warning,
                            "skills.agent",
                            format!("Workspace skill refresh task failed: {error}."),
                        );
                    }
                } else {
                    diagnostics::record(
                        DiagnosticLevel::Info,
                        "skills.agent",
                        "Active conversation has no workspace; no project skills to refresh.",
                    );
                }
            }
            let mut stream = runtime.spawn_agent_stream(agent.clone(), request_text);
            loop {
                let Some(first_event) = stream.next_event().await else {
                    break;
                };
                let mut events = vec![first_event];
                let mut stream_ended = false;

                // Text deltas are batched to keep the window responsive, but
                // lifecycle events must be painted as soon as they arrive.
                // In particular, waiting for the batch window here made a
                // tool look detached from the point where the agent invoked
                // it (and was very visible for fast tools).
                if !stream_event_requires_immediate_flush(&events[0]) {
                    let deadline = Instant::now() + STREAM_UI_BATCH_WINDOW;
                    while events.len() < STREAM_UI_MAX_EVENTS {
                        let Some(remaining) = deadline.checked_duration_since(Instant::now())
                        else {
                            break;
                        };
                        match stream
                            .next_event()
                            .with_timeout(remaining, cx.background_executor())
                            .await
                        {
                            Ok(Some(event)) => {
                                let flush_now = stream_event_requires_immediate_flush(&event);
                                events.push(event);
                                if flush_now {
                                    break;
                                }
                            }
                            Ok(None) => {
                                stream_ended = true;
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                }

                _ = this.update(cx, |app, cx| {
                    for event in events {
                        app.apply_agent_stream_event(&stream_session_id, event, cx);
                    }
                    cx.notify();
                });

                if stream_ended {
                    break;
                }
            }

            let result = stream.finish().await;
            let understood_context = agent.understood_context();
            if should_generate_title && matches!(&result, Ok(Ok(_))) {
                diagnostics::record(
                    DiagnosticLevel::Info,
                    "conversation.title",
                    format!("Starting title generation for conversation {stream_session_id}."),
                );
                let title_agent = agent.clone();
                let title_input = title_source.clone();
                let title_fallback = fallback_title.clone();
                let title_session_id = stream_session_id.clone();
                let title_runtime = runtime.clone();
                let title_view = this.clone();

                // This must be detached from the request task. The request
                // task clears `session.task` below, and GPUI cancels a task
                // when its handle is dropped. Keeping title generation here
                // used to cancel it before the provider call completed.
                cx.spawn(async move |cx| {
                    let title_result = title_runtime
                        .spawn_background(
                            async move { title_agent.generate_title(&title_input).await },
                        )
                        .await;

                    match title_result {
                        Ok(Ok(generated_title))
                            if !generated_title.trim().is_empty()
                                && generated_title.trim() != "New session" =>
                        {
                            let generated_title = generated_title.trim().to_string();
                            _ = title_view.update(cx, |app, cx| {
                                let Some(session) = app
                                    .sessions
                                    .iter_mut()
                                    .find(|session| session.id == title_session_id)
                                else {
                                    diagnostics::record(
                                        DiagnosticLevel::Info,
                                        "conversation.title",
                                        "Title completed after the conversation was closed.",
                                    );
                                    return;
                                };

                                // Respect a manual rename made while the
                                // provider was generating the title.
                                if !can_apply_generated_title(&session.title, &title_fallback) {
                                    diagnostics::record(
                                        DiagnosticLevel::Info,
                                        "conversation.title",
                                        "Keeping the title because it was renamed manually.",
                                    );
                                    return;
                                }

                                session.title = generated_title;
                                diagnostics::record(
                                    DiagnosticLevel::Success,
                                    "conversation.title",
                                    "Conversation title generated successfully.",
                                );
                                app.persist_session(&title_session_id, cx);
                            });
                        }
                        Ok(Ok(_)) => diagnostics::record(
                            DiagnosticLevel::Warning,
                            "conversation.title",
                            "Title provider returned an empty or placeholder title.",
                        ),
                        Ok(Err(error)) => diagnostics::record(
                            DiagnosticLevel::Warning,
                            "conversation.title",
                            format!("Could not generate conversation title: {error}"),
                        ),
                        Err(error) => diagnostics::record(
                            DiagnosticLevel::Warning,
                            "conversation.title",
                            format!("Conversation title task failed: {error}"),
                        ),
                    }
                })
                .detach();
            } else if should_generate_title {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "conversation.title",
                    "Title generation skipped because the provider request failed.",
                );
            }
            let next_queued_message = this.update(cx, |app, cx| {
                let unread =
                    conversation_has_unread_update(app.route, &app.active().id, &stream_session_id);
                {
                    let Some(session) = app
                        .sessions
                        .iter_mut()
                        .find(|session| session.id == stream_session_id)
                    else {
                        return None;
                    };
                    session.processing = false;
                    session.task = None;
                    session.unread = unread;
                    if understood_context.is_some() {
                        session.context_summary = understood_context.clone();
                    }
                    match result {
                        Ok(Ok(response)) => {
                            diagnostics::record(
                                DiagnosticLevel::Success,
                                "agent.request",
                                "Provider request completed successfully.",
                            );
                            if let Some(message) = session.messages.last_mut() {
                                if message.text.is_empty() {
                                    message.text = response;
                                }
                            }
                        }
                        Ok(Err(error)) => {
                            diagnostics::record(
                                DiagnosticLevel::Error,
                                "agent.request",
                                format!("Provider request failed: {error}"),
                            );
                            session
                                .messages
                                .push(ShellMessage::error(format!("Request failed: {error}")));
                        }
                        Err(error) => {
                            diagnostics::record(
                                DiagnosticLevel::Error,
                                "agent.request",
                                format!("Provider task failed: {error}"),
                            );
                            session
                                .messages
                                .push(ShellMessage::error(format!("Task failed: {error}")));
                        }
                    }
                }
                app.remeasure_active_conversation_tail(&stream_session_id);
                app.persist_session(&stream_session_id, cx);
                if app.active().id != stream_session_id {
                    return None;
                }
                app.sessions
                    .iter_mut()
                    .find(|session| session.id == stream_session_id)
                    .and_then(|session| {
                        if !session.queue_autostart || session.queued_messages.is_empty() {
                            session.queue_autostart = false;
                            None
                        } else {
                            Some(session.queued_messages.remove(0))
                        }
                    })
            });
            if let Ok(Some(message)) = next_queued_message {
                let _ = this.update_in(cx, |app, window, cx| {
                    app.start_message_request(message.text, message.attachments, false, window, cx)
                });
            }
        });
        self.active_mut().task = Some(task);
    }

    fn regenerate_assistant_message(
        &mut self,
        session_id: &SessionId,
        message_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if &self.active().id != session_id || self.active().processing {
            return;
        }
        if let Err(error) = self.runtime.validate_binding(&self.active().binding) {
            self.show_error(error.to_string(), cx);
            return;
        }
        let messages = &self.active().messages;
        if messages.get(message_index).map(|message| message.role) != Some(MessageRole::Assistant) {
            return;
        }
        let Some(user_index) = (0..message_index)
            .rev()
            .find(|index| messages[*index].role == MessageRole::User)
        else {
            return;
        };
        let prompt = messages[user_index].text.clone();
        {
            let session = self.active_mut();
            session.messages.truncate(user_index);
            session.agent = None;
            session.checkpoints.clear();
            session.sources.clear();
        }
        self.composer
            .update(cx, |composer, cx| composer.set_value(prompt, window, cx));
        self.submit_message(window, cx);
    }

    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_session_for_project(None, window, cx);
    }

    fn close_active_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sessions.remove(self.active_session);
        if self.sessions.is_empty() {
            self.sessions
                .push(ShellSession::new(None, self.remembered_binding.clone()));
        }
        self.active_session = self.active_session.min(self.sessions.len() - 1);
        self.route = Route::Chat;
        self.show_context = false;
        self.selected_agent_thread = None;
        self.agent_thread_view = None;
        self.attachments.clear();
        self.reset_conversation_scroll();
        self.mark_active_read(cx);
        self.sync_selectors_to_active(window, cx);
        cx.notify();
    }

    fn select_session(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.sessions.len() {
            return;
        }
        self.active_session = index;
        self.route = Route::Chat;
        self.show_context = false;
        self.selected_agent_thread = None;
        self.agent_thread_view = None;
        self.attachments.clear();
        self.reset_conversation_scroll();
        self.mark_active_read(cx);
        self.remember_active_setup(cx);
        self.sync_selectors_to_active(window, cx);
        self.start_next_queued_message(window, cx);
        cx.notify();
    }

    fn sync_selectors_to_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let connection_id = self.active().binding.connection_id.clone();
        self.sync_connection_picker(window, cx);
        self.sync_workspace_picker(window, cx);
        self.refresh_model_picker(window, cx);
        let is_codex = connection_id
            .as_ref()
            .and_then(|id| self.runtime.connection(id))
            .is_some_and(|profile| profile.kind == ConnectionKind::Codex);
        if is_codex {
            self.load_codex_models(cx);
        }
    }

    fn handle_new_session(&mut self, _: &NewSession, window: &mut Window, cx: &mut Context<Self>) {
        self.new_session(window, cx);
    }

    fn handle_close_session(
        &mut self,
        _: &CloseSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_active_session(window, cx);
    }

    fn handle_focus_input(&mut self, _: &FocusInput, window: &mut Window, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        self.mark_active_read(cx);
        self.composer
            .update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn handle_send_message(
        &mut self,
        _: &SendMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.submit_message(window, cx);
    }

    fn handle_toggle_settings(
        &mut self,
        _: &ToggleSettings,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.route == Route::Connections {
            self.route = Route::Chat;
        } else {
            self.settings_tab = settings_entry_tab();
            self.route = Route::Connections;
        }
        if self.route == Route::Chat {
            self.mark_active_read(cx);
        }
        cx.notify();
    }

    fn handle_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }

    fn render_queued_messages(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.active().queued_messages.is_empty() {
            return div().into_any_element();
        }

        let theme = UiTheme::current(cx);
        let rows = self
            .active()
            .queued_messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let label = composer_message_label(&message.text, &message.attachments);
                div()
                    .id(SharedString::from(format!(
                        "queued-message-{}-{index}",
                        self.active().id.as_str()
                    )))
                    .w_full()
                    .min_w(px(0.0))
                    .h(px(30.0))
                    .px(px(9.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .rounded(px(7.0))
                    .bg(theme.surface_subtle)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(11.0))
                            .text_color(theme.muted)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(label),
                    )
                    .child(
                        Button::new(format!("send-queued-message-{index}"))
                            .ghost()
                            .small()
                            .icon(IconName::ArrowUp)
                            .tooltip(i18n::text(cx, "composer.send_now"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.force_send_queued_message(index, window, cx);
                            })),
                    )
                    .child(
                        Button::new(format!("remove-queued-message-{index}"))
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .tooltip(i18n::text(cx, "composer.remove_queued"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_queued_message(index, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .px(px(4.0))
                    .text_size(px(10.0))
                    .text_color(theme.faint)
                    .child(i18n::text(cx, "composer.queued_messages")),
            )
            .children(rows)
            .into_any_element()
    }

    fn render_composer_stack(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .max_w(if compact { px(700.0) } else { px(760.0) })
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(self.render_queued_messages(cx))
            .child(self.render_composer(compact, cx))
            .into_any_element()
    }

    fn render_rail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::current(cx);
        let active_id = self.active().id.to_string();
        let session_states = self
            .sessions
            .iter()
            .map(|session| (session.id.to_string(), (session.processing, session.unread)))
            .collect::<HashMap<_, _>>();
        let (global_conversations, mut workspace_conversations) =
            group_conversations_by_workspace(&self.conversations, &self.projects);
        let mut pinned_rows = Vec::new();
        let mut recent_rows = Vec::new();
        for conversation in global_conversations {
            let id = conversation.id.clone();
            let select_id = id.clone();
            let selected = self.route == Route::Chat && id == active_id;
            let (processing, unread) = session_states
                .get(&id)
                .copied()
                .unwrap_or((false, conversation.unread));
            let group = SharedString::from(format!("conversation-row-{id}"));
            let actions = conversation_actions_button(
                id.clone(),
                format!("conversation-actions-{id}"),
                Some(group.clone()),
                Some(conversation.pinned),
                processing,
                unread,
                cx,
            );
            let row = div()
                .id(SharedString::from(format!("conversation-{id}")))
                .flex_none()
                .w_full()
                .h(px(31.0))
                .px(px(9.0))
                .flex()
                .items_center()
                .rounded(px(7.0))
                .overflow_hidden()
                .cursor_pointer()
                .group(group)
                .text_size(px(13.0))
                .when(selected, |row| {
                    row.mt(px(1.0)).mb(px(1.0)).bg(theme.accent_soft)
                })
                .hover(|style| style.bg(theme.accent_soft))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(conversation.title),
                )
                .child(actions)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_conversation(&select_id, window, cx)
                }))
                .into_any_element();
            if conversation.pinned {
                pinned_rows.push(row);
            } else {
                recent_rows.push(row);
            }
        }

        let mut project_rows = Vec::new();
        if self.projects_expanded {
            for project in self.projects.clone() {
                let id = project.id.clone();
                let conversations = workspace_conversations.remove(&id).unwrap_or_default();
                let conversation_count = conversations.len();
                let new_work_project_id = id.clone();
                let project_group = SharedString::from(format!("project-row-{id}"));
                let new_conversation_project = project.clone();
                let project_conversation_count = div()
                    .absolute()
                    .top(px(0.0))
                    .left(px(0.0))
                    .size(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.0))
                    .text_color(theme.faint)
                    .when(conversation_count == 0, |this| this.opacity(0.0))
                    .group_hover(project_group.clone(), |style| style.opacity(0.0))
                    .child(conversation_count.to_string());
                let new_conversation_button = Button::new(SharedString::from(format!(
                    "new-conversation-in-project-{id}"
                )))
                .ghost()
                .small()
                .with_size(px(24.0))
                .icon(IconName::Plus)
                .tooltip(i18n::text(cx, "sidebar.new_workspace_conversation"))
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .opacity(0.0)
                .group_hover(project_group.clone(), |style| style.opacity(1.0))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.new_session_for_project(Some(new_conversation_project.clone()), window, cx)
                }));
                project_rows.push(
                    div()
                        .id(SharedString::from(format!("project-{id}")))
                        .flex_none()
                        .w_full()
                        .h(px(31.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .rounded(px(7.0))
                        .cursor_pointer()
                        .group(project_group)
                        .text_size(px(13.0))
                        .hover(|style| style.bg(theme.accent_soft))
                        .child(
                            Icon::new(IconName::Folder)
                                .size(px(15.0))
                                .text_color(theme.muted),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .overflow_hidden()
                                .child(project.name),
                        )
                        .child(
                            div()
                                .relative()
                                .flex_none()
                                .size(px(24.0))
                                .child(project_conversation_count)
                                .child(new_conversation_button),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_project(&new_work_project_id, window, cx)
                        }))
                        .into_any_element(),
                );
                if conversations.is_empty() {
                    project_rows.push(
                        div()
                            .h(px(27.0))
                            .pl(px(33.0))
                            .flex()
                            .items_center()
                            .text_size(px(11.0))
                            .text_color(theme.faint)
                            .child(i18n::text(cx, "sidebar.no_conversations"))
                            .into_any_element(),
                    );
                    continue;
                }
                for conversation in conversations {
                    let conversation_id = conversation.id.clone();
                    let select_id = conversation_id.clone();
                    let selected = self.route == Route::Chat && conversation_id == active_id;
                    let (processing, unread) = session_states
                        .get(&conversation_id)
                        .copied()
                        .unwrap_or((false, conversation.unread));
                    let group =
                        SharedString::from(format!("workspace-conversation-row-{conversation_id}"));
                    let actions = conversation_actions_button(
                        conversation_id.clone(),
                        format!("workspace-conversation-actions-{conversation_id}"),
                        Some(group.clone()),
                        Some(conversation.pinned),
                        processing,
                        unread,
                        cx,
                    );
                    project_rows.push(
                        div()
                            .id(SharedString::from(format!(
                                "workspace-conversation-{conversation_id}"
                            )))
                            .flex_none()
                            .w_full()
                            .h(px(29.0))
                            .pl(px(33.0))
                            .pr(px(9.0))
                            .flex()
                            .items_center()
                            .rounded(px(7.0))
                            .overflow_hidden()
                            .cursor_pointer()
                            .group(group)
                            .text_size(px(12.0))
                            .text_color(theme.muted)
                            .when(selected, |row| {
                                row.mt(px(1.0))
                                    .mb(px(1.0))
                                    .bg(theme.accent_soft)
                                    .text_color(theme.foreground)
                            })
                            .hover(|style| style.bg(theme.accent_soft))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(conversation.title),
                            )
                            .child(actions)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_conversation(&select_id, window, cx)
                            }))
                            .into_any_element(),
                    );
                }
            }
        }

        let search_rows = self
            .conversation_search_results
            .iter()
            .cloned()
            .map(|result| {
                let conversation_id = result.conversation_id.clone();
                div()
                    .id(SharedString::from(format!(
                        "conversation-search-result-{}",
                        result.conversation_id
                    )))
                    .w_full()
                    .px(px(9.0))
                    .py(px(7.0))
                    .rounded(px(7.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent_soft))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.foreground)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(result.title),
                    )
                    .child(
                        div()
                            .mt(px(2.0))
                            .text_size(px(10.0))
                            .text_color(theme.faint)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(result.snippet),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_conversation(&conversation_id, window, cx)
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let search_query = self.conversation_search.read(cx).value().trim().to_owned();
        let search_panel = if self.conversation_search_open {
            div()
                .flex_none()
                .px(px(9.0))
                .pb(px(8.0))
                .child(
                    Input::new(&self.conversation_search)
                        .prefix(IconName::Search)
                        .w_full(),
                )
                .child(
                    div()
                        .mt(px(5.0))
                        .when(search_rows.is_empty() && search_query.is_empty(), |this| {
                            this.child(sidebar_empty(i18n::text(cx, "sidebar.search_all"), theme))
                        })
                        .when(search_rows.is_empty() && !search_query.is_empty(), |this| {
                            this.child(sidebar_empty(
                                i18n::text(cx, "sidebar.no_search_results"),
                                theme,
                            ))
                        })
                        .children(search_rows),
                )
                .into_any_element()
        } else {
            div().into_any_element()
        };

        div()
            .flex_none()
            .flex()
            .flex_col()
            .w(px(274.0))
            .h_full()
            .bg(theme.rail)
            .pt(px(40.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(42.0))
                    .px(px(17.0))
                    .child(
                        div()
                            .text_size(px(16.0))
                            .font_weight(FontWeight::BOLD)
                            .child(i18n::text(cx, "sidebar.brand")),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("search-conversations")
                            .ghost()
                            .small()
                            .icon(IconName::Search)
                            .tooltip(i18n::text(cx, "sidebar.search"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.conversation_search_open = !this.conversation_search_open;
                                if !this.conversation_search_open {
                                    this.conversation_search_results.clear();
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(search_panel)
            .child(
                div()
                    .flex_none()
                    .px(px(9.0))
                    .pb(px(8.0))
                    .child(
                        div()
                            .id("new-work")
                            .h(px(34.0))
                            .px(px(8.0))
                            .flex()
                            .items_center()
                            .gap(px(9.0))
                            .rounded(px(7.0))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.accent_soft))
                            .child(Icon::new(IconName::Plus).size(px(15.0)))
                            .child(i18n::text(cx, "sidebar.new_work"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_session(window, cx)),
                            ),
                    )
                    .child(
                        div()
                            .id("open-workspace")
                            .h(px(34.0))
                            .px(px(8.0))
                            .flex()
                            .items_center()
                            .gap(px(9.0))
                            .rounded(px(7.0))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.accent_soft))
                            .child(Icon::new(IconName::FolderOpen).size(px(15.0)))
                            .child(i18n::text(cx, "sidebar.open_workspace"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_workspace(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scrollbar()
                    .px(px(8.0))
                    .child(sidebar_heading(
                        i18n::text(cx, "sidebar.pinned"),
                        theme,
                        12.0,
                    ))
                    .when(pinned_rows.is_empty(), |this| {
                        this.child(sidebar_empty(i18n::text(cx, "sidebar.no_pinned"), theme))
                    })
                    .children(pinned_rows)
                    .child(
                        div()
                            .mt(px(12.0))
                            .h(px(34.0))
                            .pl(px(9.0))
                            .pr(px(4.0))
                            .flex()
                            .items_center()
                            .text_size(px(13.0))
                            .text_color(theme.faint)
                            .child(
                                div()
                                    .id("projects-toggle")
                                    .flex_1()
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .child(i18n::text(cx, "sidebar.projects"))
                                    .child(
                                        Icon::new(if self.projects_expanded {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .size(px(13.0)),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.projects_expanded = !this.projects_expanded;
                                        cx.notify();
                                    })),
                            )
                            .child(Icon::new(IconName::Ellipsis).size(px(15.0)))
                            .child(
                                Button::new("create-project")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Plus)
                                    .tooltip(i18n::text(cx, "sidebar.create_project"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_workspace(window, cx)
                                    })),
                            ),
                    )
                    .children(project_rows)
                    .child(sidebar_heading(
                        i18n::text(cx, "sidebar.recent"),
                        theme,
                        16.0,
                    ))
                    .when(recent_rows.is_empty(), |this| {
                        this.child(sidebar_empty(i18n::text(cx, "sidebar.recent_empty"), theme))
                    })
                    .children(recent_rows),
            )
            .child(
                div()
                    .id("settings-nav")
                    .flex_none()
                    .h(px(50.0))
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .cursor_pointer()
                    .when(self.route == Route::Connections, |this| {
                        this.bg(theme.accent_soft)
                    })
                    .hover(|style| style.bg(theme.accent_soft))
                    .child(
                        div()
                            .size(px(18.0))
                            .rounded(px(5.0))
                            .bg(gpui::rgb(0x1ca7b8))
                            .text_color(theme.rail)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(10.0))
                            .font_weight(FontWeight::BOLD)
                            .child("V"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.0))
                            .child(format!("v{APP_VERSION}")),
                    )
                    .when(self.background_indexing, |this| {
                        this.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .text_size(px(10.0))
                                .text_color(theme.muted)
                                .child(Icon::new(IconName::Loader).size(px(12.0)))
                                .child(i18n::text(cx, "sidebar.indexing")),
                        )
                    })
                    .child(
                        Icon::new(IconName::Settings2)
                            .size(px(15.0))
                            .text_color(theme.muted),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_tab = settings_entry_tab();
                        this.route = Route::Connections;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_composer(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::current(cx);
        let session = self.active();
        let has_connection = session.binding.connection_id.is_some();
        let has_model = session.binding.model_id.is_some();
        let workspace_locked = session.processing || !session.messages.is_empty();
        let attachment_chips = self
            .attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| {
                let name = composer_attachment_name(&attachment.path);
                div()
                    .id(SharedString::from(format!("attachment-{index}")))
                    .max_w(px(220.0))
                    .h(px(27.0))
                    .px(px(7.0))
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .rounded(px(6.0))
                    .bg(theme.surface_subtle)
                    .text_size(px(11.0))
                    .child(Icon::new(IconName::File).size(px(13.0)))
                    .child(div().min_w(px(0.0)).truncate().child(name))
                    .child(
                        Button::new(format!("remove-attachment-{index}"))
                            .ghost()
                            .with_size(px(20.0))
                            .icon(IconName::CircleX)
                            .tooltip(i18n::text(cx, "composer.remove_attachment"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_attachment(index, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let has_attachments = !attachment_chips.is_empty();
        // `Button::loading(true)` intentionally disables pointer events in
        // gpui-component. The composer action is also the stop action while a
        // request is running, so keep the button interactive and use a clear
        // stop icon instead of the component's loading mode.
        let send_icon = if session.processing {
            IconName::CircleX
        } else {
            IconName::ArrowUp
        };
        div()
            .w_full()
            .max_w(if compact { px(700.0) } else { px(760.0) })
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(12.0))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .when(has_attachments, |this| {
                        this.child(
                            div()
                                .px(px(14.0))
                                .pt(px(9.0))
                                .flex()
                                .flex_wrap()
                                .gap(px(6.0))
                                .max_h(px(58.0))
                                .overflow_y_scrollbar()
                                .children(attachment_chips),
                        )
                    })
                    .child(
                        div()
                            .min_h(px(66.0))
                            .px(px(14.0))
                            .py(px(10.0))
                            .flex()
                            .items_start()
                            .child(
                                Textarea::new(&self.composer)
                                    .flex_1()
                                    .min_w(px(120.0))
                                    .min_h(px(42.0))
                                    .appearance(false)
                                    .bordered(false),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(40.0))
                    .px(px(9.0))
                    .pb(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .text_size(px(12.0))
                    .text_color(theme.faint)
                    .child(
                        Button::new(if compact {
                            "context-new"
                        } else {
                            "context-open"
                        })
                        .ghost()
                        .with_size(px(28.0))
                        .icon(IconName::Plus)
                        .tooltip(i18n::text(cx, "composer.attach_files"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_attachment_picker(window, cx)
                        })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .flex_none()
                            .child(
                                Select::new(&self.workspace_select)
                                    .w(px(124.0))
                                    .h(px(28.0))
                                    .small()
                                    .appearance(false)
                                    .placeholder(i18n::text(cx, "composer.workspace"))
                                    .search_placeholder(i18n::text(
                                        cx,
                                        "composer.search_workspaces",
                                    ))
                                    .disabled(workspace_locked || self.projects.is_empty()),
                            )
                            .child(
                                Select::new(&self.model_select)
                                    .w(px(148.0))
                                    .h(px(28.0))
                                    .small()
                                    .appearance(false)
                                    .placeholder(i18n::text(cx, "composer.model"))
                                    .search_placeholder(i18n::text(cx, "composer.search_models"))
                                    .disabled(self.model_choices.is_empty()),
                            )
                            .child(
                                Select::new(&self.reasoning_select)
                                    .w(px(68.0))
                                    .h(px(28.0))
                                    .small()
                                    .appearance(false)
                                    .placeholder(i18n::text(cx, "composer.effort"))
                                    .search_placeholder(i18n::text(cx, "composer.search_effort"))
                                    .disabled(!has_model),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new(if compact { "send-new" } else { "send-open" })
                            .primary()
                            .with_size(px(28.0))
                            .rounded(ButtonRounded::Large)
                            .icon(send_icon)
                            .tooltip(if session.processing {
                                i18n::text(cx, "composer.stop")
                            } else {
                                i18n::text(cx, "composer.send")
                            })
                            .disabled(!has_connection || !has_model)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.active().processing {
                                    this.stop_active_stream(cx);
                                } else {
                                    this.submit_message(window, cx);
                                }
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Lazily renders the conversation. The list retains height measurements
    /// for prior entries and asks for only the visible range plus a small
    /// overdraw, keeping trackpad scrolling smooth even after long tool-heavy
    /// runs.
    fn render_conversation_entries(&mut self, cx: &mut Context<Self>) -> gpui::List {
        let list_state = self.conversation_list.clone();
        list(
            list_state,
            cx.processor(|this, index: usize, _window, cx| {
                let (
                    session_id,
                    message,
                    processing,
                    streaming,
                    is_last_assistant,
                    conversation_sources,
                    show_tool_activity,
                    show_sources,
                    pending_user_question,
                    ask_user_input,
                ) = {
                    let session = this.active();
                    let Some(message) = session.messages.get(index).cloned() else {
                        return div().into_any_element();
                    };
                    let is_last_assistant = session
                        .messages
                        .iter()
                        .rposition(|message| message.role == MessageRole::Assistant)
                        == Some(index);
                    let streaming = session.processing
                        && index + 1 == session.messages.len()
                        && message.role == MessageRole::Assistant;
                    (
                        session.id.clone(),
                        message,
                        session.processing,
                        streaming,
                        is_last_assistant,
                        if is_last_assistant {
                            session.sources.clone()
                        } else {
                            Vec::new()
                        },
                        this.show_tool_activity,
                        this.show_sources,
                        if is_last_assistant {
                            session.pending_user_question.clone()
                        } else {
                            None
                        },
                        this.ask_user_input.clone(),
                    )
                };
                let theme = UiTheme::current(cx);
                div()
                    .id(SharedString::from(format!(
                        "conversation-message-{}-{index}",
                        session_id.as_str()
                    )))
                    .w_full()
                    .pt(if index == 0 { px(28.0) } else { px(0.0) })
                    .pb(px(26.0))
                    .flex()
                    .justify_center()
                    .child(div().w_full().max_w(px(820.0)).child(render_message(
                        &session_id,
                        index,
                        &message,
                        processing,
                        streaming,
                        show_tool_activity,
                        show_sources,
                        if is_last_assistant {
                            conversation_sources.as_slice()
                        } else {
                            &[]
                        },
                        pending_user_question.as_ref(),
                        &ask_user_input,
                        theme,
                        cx,
                    )))
                    .into_any_element()
            }),
        )
        .with_sizing_behavior(gpui::ListSizingBehavior::Auto)
        .size_full()
        .px(px(30.0))
        .pb(px(22.0))
    }

    fn render_chat(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.sync_conversation_list_state();
        if let Some(thread_id) = self.agent_thread_view.clone() {
            return self.render_agent_thread_chat(&thread_id, cx);
        }
        let theme = UiTheme::current(cx);
        let brand_asset = averroes_logo_asset(cx);
        let session = self.active();
        let title = session.title.clone();
        let pinned = session.pinned;
        let processing = session.processing;
        let context_usage = session.context_usage;
        let context_busy = session.context_busy;
        let has_agent = session.agent.is_some();
        let mut agent_threads = session.agent_threads.clone();
        let is_empty = session.messages.is_empty();
        let checkpoints = session.checkpoints.clone();
        let tasks = session.tasks.clone();
        let sources = session.sources.clone();
        let has_tool_activity = session
            .messages
            .iter()
            .any(|message| !message.tool_activities.is_empty());
        let session_id = session.id.clone();

        for thread in self.runtime.agent_threads_for(session_id.as_str()) {
            if let Some(existing) = agent_threads
                .iter_mut()
                .find(|existing| existing.id == thread.id)
            {
                *existing = thread;
            } else {
                agent_threads.push(thread);
            }
        }

        if is_empty {
            return div()
                .flex()
                .flex_col()
                .size_full()
                .min_w(px(0.0))
                .child(
                    div().flex_1().flex().items_center().justify_center().child(
                        div()
                            .w_full()
                            .max_w(px(690.0))
                            .px(px(15.0))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(26.0))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .gap(px(12.0))
                                    .child(img(brand_asset).size(px(112.0)).flex_none())
                                    .text_size(px(27.0))
                                    .font_weight(FontWeight::NORMAL)
                                    .child(i18n::text(cx, "chat.ready")),
                            )
                            .child(self.render_composer_stack(true, cx)),
                    ),
                )
                .into_any_element();
        }

        let checkpoint_rows = checkpoints
            .into_iter()
            .map(|checkpoint| {
                let checkpoint_id = checkpoint.id.clone();
                let message_position = checkpoint.message_position;
                let (icon, color) = match checkpoint.status {
                    CheckpointStatus::Completed => (IconName::CircleCheck, theme.success),
                    CheckpointStatus::InProgress => (IconName::Loader, theme.warning),
                    CheckpointStatus::Blocked => (IconName::CircleX, theme.destructive),
                    CheckpointStatus::Pending => (IconName::Ellipsis, theme.faint),
                };
                let hover_group = SharedString::from(format!(
                    "checkpoint-hover-{}-{checkpoint_id}",
                    session_id.as_str()
                ));
                let detail = checkpoint
                    .detail
                    .clone()
                    .filter(|detail| !detail.trim().is_empty())
                    .unwrap_or_else(|| {
                        checkpoint
                            .status
                            .as_str()
                            .replace('_', " ")
                            .to_ascii_uppercase()
                    });
                let title = checkpoint.title.clone();
                let tooltip = div()
                    .absolute()
                    .left(px(31.0))
                    .top(px(-5.0))
                    .w(px(320.0))
                    .p(px(10.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .shadow_md()
                    .opacity(0.0)
                    .group_hover(hover_group.clone(), |style| style.opacity(1.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .child(Icon::new(icon).size(px(13.0)).text_color(color))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(title),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(6.0))
                            .max_h(px(34.0))
                            .overflow_hidden()
                            .text_size(px(12.0))
                            .text_color(theme.muted)
                            .child(detail),
                    );
                div()
                    .id(SharedString::from(format!(
                        "checkpoint-{}-{checkpoint_id}",
                        session_id.as_str()
                    )))
                    .relative()
                    .group(hover_group)
                    .flex()
                    .items_center()
                    .w_full()
                    .h(px(30.0))
                    .child(
                        div()
                            .w(px(23.0))
                            .h(px(2.0))
                            .rounded_full()
                            .bg(color)
                            .hover(|style| style.h(px(3.0))),
                    )
                    .child(tooltip)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.surface_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.scroll_to_checkpoint(message_position, cx);
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let task_rows = tasks
            .into_iter()
            .map(|task| {
                let done = task.status == TaskStatus::Done;
                let hover_group =
                    SharedString::from(format!("task-hover-{}-{}", session_id.as_str(), task.id));
                let task_title = task.title.clone();
                let task_status = if done { "DONE" } else { "PENDING" };
                let tooltip = div()
                    .absolute()
                    .left(px(31.0))
                    .top(px(-5.0))
                    .w(px(320.0))
                    .p(px(10.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .shadow_md()
                    .opacity(0.0)
                    .group_hover(hover_group.clone(), |style| style.opacity(1.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .child(
                                Icon::new(if done {
                                    IconName::CircleCheck
                                } else {
                                    IconName::Ellipsis
                                })
                                .size(px(13.0))
                                .text_color(if done {
                                    theme.success
                                } else {
                                    theme.faint
                                }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(task_title),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(6.0))
                            .text_size(px(11.0))
                            .text_color(if done { theme.success } else { theme.faint })
                            .child(task_status),
                    );
                div()
                    .id(SharedString::from(format!(
                        "task-{}-{}",
                        session_id.as_str(),
                        task.id
                    )))
                    .relative()
                    .group(hover_group)
                    .flex()
                    .items_center()
                    .w_full()
                    .h(px(24.0))
                    .child(div().w(px(15.0)).h(px(2.0)).rounded_full().bg(if done {
                        theme.success
                    } else {
                        theme.faint
                    }))
                    .child(tooltip)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let has_sources = !sources.is_empty();
        let web_source_rows = if self.show_sources {
            sources
                .iter()
                .filter(|source| source.url.is_some())
                .map(|source| render_source_row(source, theme))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let tool_source_chips = if self.show_sources {
            sources
                .iter()
                .filter(|source| source.url.is_none())
                .map(|source| render_source_tool_chip(&session_id, source, theme, cx))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let has_checkpoints = !checkpoint_rows.is_empty();
        let has_tasks = !task_rows.is_empty();
        let header_actions = conversation_actions_button(
            session_id.to_string(),
            format!("header-conversation-actions-{}", session_id.as_str()),
            None,
            None,
            false,
            false,
            cx,
        );
        let conversation_entries = self.render_conversation_entries(cx);

        div()
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .h(px(46.0))
                    .px(px(18.0))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child(
                                div()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(title),
                            )
                            .child(header_actions),
                    )
                    .child(
                        div()
                            .mr(px(10.0))
                            .text_size(px(11.0))
                            .text_color(if processing {
                                theme.warning
                            } else {
                                theme.faint
                            })
                            .child(if processing {
                                render_activity_indicator(
                                    format!("header-working-{}", session_id.as_str()),
                                    theme,
                                    3.0,
                                )
                            } else {
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.faint)
                                    .child(i18n::text(cx, "chat.work"))
                                    .into_any_element()
                            }),
                    )
                    .child(
                        Button::new("pin-conversation")
                            .ghost()
                            .small()
                            .icon(if pinned {
                                IconName::CircleCheck
                            } else {
                                IconName::Check
                            })
                            .tooltip(if pinned {
                                i18n::text(cx, "chat.unpin")
                            } else {
                                i18n::text(cx, "chat.pin")
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_pin(cx))),
                    )
                    .when(has_sources, |this| {
                        this.child(
                            Button::new("toggle-sources")
                                .ghost()
                                .small()
                                .icon(if self.show_sources {
                                    IconName::PanelRightClose
                                } else {
                                    IconName::PanelRightOpen
                                })
                                .tooltip(i18n::text(cx, "chat.show_sources"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_sources = !this.show_sources;
                                    this.conversation_list.remeasure();
                                    cx.notify();
                                })),
                        )
                    })
                    .when(has_tool_activity, |this| {
                        this.child(
                            Button::new("toggle-tool-activity")
                                .ghost()
                                .small()
                                .icon(if self.show_tool_activity {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .tooltip(if self.show_tool_activity {
                                    "Hide tool activity"
                                } else {
                                    "Show tool activity"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_tool_activity_visibility(cx);
                                })),
                        )
                    })
                    .child(
                        div()
                            .id("context-usage-button")
                            .flex_none()
                            .size(px(28.0))
                            .rounded_full()
                            .border_1()
                            .border_color(theme.border)
                            .bg(if self.show_context {
                                theme.accent_soft
                            } else {
                                theme.surface_subtle
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.accent_soft))
                            .text_size(px(9.0))
                            .text_color(theme.muted)
                            .child(if context_busy {
                                "…".to_string()
                            } else {
                                context_usage
                                    .percentage()
                                    .map(|percentage| format!("{percentage}%"))
                                    .unwrap_or_else(|| "—".into())
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_context_sidebar(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .when(has_checkpoints || has_tasks, |this| this.pl(px(44.0)))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .id("conversation-list")
                                    .relative()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .w_full()
                                    .child(conversation_entries)
                                    .vertical_scrollbar(&self.conversation_list),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px(px(22.0))
                                    .pb(px(14.0))
                                    .flex()
                                    .justify_center()
                                    .child(self.render_composer_stack(false, cx)),
                            ),
                    )
                    .when(has_checkpoints || has_tasks, |this| {
                        this.child(
                            div()
                                .absolute()
                                .left(px(0.0))
                                .top(px(0.0))
                                .bottom(px(0.0))
                                .w(px(44.0))
                                .px(px(10.0))
                                .py(px(20.0))
                                .overflow_y_scrollbar()
                                .when(has_checkpoints, |this| this.children(checkpoint_rows))
                                .when(has_tasks, |this| {
                                    this.child(div().h(px(if has_checkpoints {
                                        12.0
                                    } else {
                                        0.0
                                    })))
                                    .children(task_rows)
                                }),
                        )
                    })
                    .when(self.show_context, |this| {
                        this.child(self.render_context_sidebar(
                            context_usage,
                            context_busy,
                            has_agent,
                            &agent_threads,
                            cx,
                        ))
                    })
                    .when(
                        !self.show_context && self.show_sources && has_sources,
                        |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .w(px(320.0))
                                    .h_full()
                                    .min_h(px(0.0))
                                    .flex()
                                    .flex_col()
                                    .px(px(14.0))
                                    .py(px(16.0))
                                    .child(
                                        div()
                                            .id("sources-scroll")
                                            .flex_1()
                                            .min_h(px(0.0))
                                            .overflow_y_scrollbar()
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .gap(px(10.0))
                                                    .rounded(px(16.0))
                                                    .border_1()
                                                    .border_color(theme.border)
                                                    .bg(theme.surface)
                                                    .shadow_md()
                                                    .p(px(14.0))
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .pb(px(10.0))
                                                            .child(
                                                                div()
                                                                    .flex_1()
                                                                    .text_size(px(13.0))
                                                                    .font_weight(FontWeight::MEDIUM)
                                                                    .child(i18n::text(
                                                                        cx,
                                                                        "chat.results",
                                                                    )),
                                                            )
                                                            .child(
                                                                div()
                                                                    .size(px(20.0))
                                                                    .rounded_full()
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                                    .hover(|style| style.bg(theme.surface_hover))
                                                                    .child(
                                                                        Icon::new(IconName::Plus)
                                                                            .size(px(15.0))
                                                                            .text_color(theme.faint),
                                                                    ),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .h(px(31.0))
                                                            .flex()
                                                            .items_center()
                                                            .text_size(px(12.0))
                                                            .text_color(theme.muted)
                                                            .child(i18n::text(
                                                                cx,
                                                                "chat.create_file_or_site",
                                                            )),
                                                    )
                                                    .child(div().h(px(1.0)).bg(theme.border))
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .pt(px(2.0))
                                                            .pb(px(1.0))
                                                            .child(
                                                                div()
                                                                    .flex_1()
                                                                    .text_size(px(13.0))
                                                                    .font_weight(FontWeight::MEDIUM)
                                                                    .child(i18n::text(
                                                                        cx,
                                                                        "chat.sources",
                                                                    )),
                                                            )
                                                            .child(
                                                                div()
                                                                    .size(px(20.0))
                                                                    .rounded_full()
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                                    .hover(|style| style.bg(theme.surface_hover))
                                                                    .child(
                                                                        Icon::new(IconName::Plus)
                                                                            .size(px(15.0))
                                                                            .text_color(theme.faint),
                                                                    ),
                                                            ),
                                                    )
                                                    .when(
                                                        !web_source_rows.is_empty()
                                                            || !tool_source_chips.is_empty(),
                                                        |this| {
                                                            this.child(
                                                                div()
                                                                    .flex()
                                                                    .flex_col()
                                                                    .gap(px(2.0))
                                                                    .children(web_source_rows)
                                                                    .children(tool_source_chips)
                                                                    .child(
                                                                        div()
                                                                            .flex()
                                                                            .items_center()
                                                                            .gap(px(8.0))
                                                                            .h(px(31.0))
                                                                            .px(px(7.0))
                                                                            .rounded(px(7.0))
                                                                            .text_size(px(12.0))
                                                                            .text_color(theme.muted)
                                                                            .hover(|style| {
                                                                                style.bg(theme.surface_hover)
                                                                            })
                                                                            .child(
                                                                                Icon::new(
                                                                                    IconName::ExternalLink,
                                                                                )
                                                                                .size(px(14.0))
                                                                                .text_color(theme.faint),
                                                                            )
                                                                            .child(i18n::text(
                                                                                cx,
                                                                                "chat.see_all_sources",
                                                                            )),
                                                                    ),
                                                            )
                                                        },
                                                    ),
                                            ),
                                    ),
                            )
                        },
                    ),
            )
            .into_any_element()
    }

    fn render_agent_thread_chat(&mut self, thread_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::current(cx);
        let parent_session_id = self.active().id.clone();
        let thread = self
            .active()
            .agent_threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .cloned()
            .or_else(|| {
                self.runtime
                    .agent_threads_for(parent_session_id.as_str())
                    .into_iter()
                    .find(|thread| thread.id == thread_id)
            });

        let Some(thread) = thread else {
            return div()
                .flex()
                .flex_col()
                .size_full()
                .items_center()
                .justify_center()
                .child(
                    Button::new("agent-thread-back-missing")
                        .secondary()
                        .icon(IconName::ArrowLeft)
                        .label(i18n::text(cx, "chat.back_to_conversation"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.close_agent_thread(cx);
                        })),
                )
                .into_any_element();
        };

        let messages = self
            .active()
            .agent_thread_transcripts
            .get(thread_id)
            .map(|transcript| transcript.messages.clone())
            .unwrap_or_default();
        let transcript = if messages.is_empty() {
            if thread.output.trim().is_empty() {
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.faint)
                    .child(if thread.status == AgentThreadStatus::Running {
                        render_activity_indicator(
                            format!("agent-thread-waiting-{thread_id}"),
                            theme,
                            4.0,
                        )
                    } else {
                        i18n::text(cx, "chat.waiting_output").into_any_element()
                    })
                    .into_any_element()
            } else {
                TextView::markdown(
                    format!("agent-thread-output-{thread_id}"),
                    thread.output.clone(),
                )
                .selectable(true)
                .into_any_element()
            }
        } else {
            render_agent_thread_transcript(
                thread_id,
                &messages,
                thread.status == AgentThreadStatus::Running,
                theme,
                cx,
            )
        };
        let status = if thread.status == AgentThreadStatus::Running {
            render_activity_indicator(format!("agent-thread-status-{thread_id}"), theme, 3.0)
        } else {
            div()
                .text_size(px(11.0))
                .text_color(agent_thread_status_color(thread.status, theme))
                .child(agent_thread_status_label(thread.status))
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.0))
            .child(
                div()
                    .flex_none()
                    .h(px(46.0))
                    .px(px(18.0))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("agent-thread-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .label(i18n::text(cx, "chat.back_to_conversation"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_agent_thread(cx);
                            })),
                    )
                    .child(
                        div()
                            .ml(px(10.0))
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(thread.title),
                    )
                    .child(status),
            )
            .child(
                div()
                    .id("agent-thread-conversation-list")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scrollbar()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(820.0))
                            .mx_auto()
                            .px(px(30.0))
                            .py(px(22.0))
                            .child(transcript),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(22.0))
                    .pb(px(14.0))
                    .flex()
                    .justify_center()
                    .child(
                        div().w_full().max_w(px(820.0)).child(
                            Button::new("agent-thread-back-bottom")
                                .secondary()
                                .w_full()
                                .icon(IconName::ArrowLeft)
                                .label(i18n::text(cx, "chat.back_to_conversation"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.close_agent_thread(cx);
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_context_sidebar(
        &self,
        usage: ContextUsage,
        context_busy: bool,
        has_agent: bool,
        agent_threads: &[AgentThreadSnapshot],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = UiTheme::current(cx);
        let selected_thread = self.selected_agent_thread.as_ref().and_then(|thread_id| {
            agent_threads
                .iter()
                .find(|thread| &thread.id == thread_id)
                .cloned()
        });
        let selected_transcript = selected_thread.as_ref().and_then(|thread| {
            self.active()
                .agent_thread_transcripts
                .get(&thread.id)
                .map(|transcript| transcript.messages.clone())
        });
        let has_selected_transcript = selected_transcript
            .as_ref()
            .is_some_and(|messages| !messages.is_empty());
        let input_tokens = format_context_tokens(usage.input_tokens);
        let output_tokens = format_context_tokens(usage.output_tokens);
        let context_limit = format_context_limit(usage.context_limit);
        let percentage = usage.percentage();
        let progress = percentage.map(|percentage| {
            div()
                .h(px(5.0))
                .w(px((percentage as f32 * 2.46).min(246.0)))
                .rounded_full()
                .bg(if percentage >= 80 {
                    theme.warning
                } else {
                    theme.accent
                })
        });

        let thread_rows = agent_threads
            .iter()
            .map(|thread| {
                let thread_id = thread.id.clone();
                div()
                    .id(SharedString::from(format!(
                        "agent-thread-row-{}",
                        thread.id
                    )))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .w_full()
                    .px(px(8.0))
                    .py(px(7.0))
                    .rounded(px(7.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.surface_hover))
                    .child(
                        div()
                            .size(px(7.0))
                            .rounded_full()
                            .bg(agent_thread_status_color(thread.status, theme)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(12.0))
                            .overflow_hidden()
                            .child(thread.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme.faint)
                            .child(agent_thread_status_label(thread.status)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_agent_thread(thread_id.clone(), cx);
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let selected_panel = selected_thread.map(|thread| {
            let output = if thread.output.is_empty() {
                i18n::text(cx, "chat.waiting_output").to_string()
            } else {
                thread.output.clone()
            };
            let transcript = selected_transcript
                .as_deref()
                .filter(|messages| !messages.is_empty())
                .map(|messages| {
                    render_agent_thread_transcript(
                        &thread.id,
                        messages,
                        thread.status == AgentThreadStatus::Running,
                        theme,
                        cx,
                    )
                });
            div()
                .mt(px(14.0))
                .pt(px(14.0))
                .border_t_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::MEDIUM)
                                .child(thread.title.clone()),
                        )
                        .child(
                            Button::new("close-agent-thread")
                                .ghost()
                                .small()
                                .icon(IconName::Close)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.selected_agent_thread = None;
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .text_size(px(10.0))
                        .text_color(theme.faint)
                        .child(format!(
                            "{} · {}",
                            agent_thread_status_label(thread.status),
                            thread.model_id
                        )),
                )
                .child(
                    div()
                        .mt(px(12.0))
                        .text_size(px(11.0))
                        .text_color(theme.muted)
                        .child(i18n::text(cx, "chat.prompt")),
                )
                .child(
                    div()
                        .mt(px(4.0))
                        .text_size(px(12.0))
                        .child(thread.prompt.clone()),
                )
                .when_some(transcript, |this, transcript| {
                    this.child(
                        div()
                            .mt(px(12.0))
                            .text_size(px(11.0))
                            .text_color(theme.muted)
                            .child(i18n::text(cx, "chat.output")),
                    )
                    .child(transcript)
                })
                .when(!has_selected_transcript, |this| {
                    this.child(
                        div()
                            .mt(px(12.0))
                            .text_size(px(11.0))
                            .text_color(theme.muted)
                            .child(i18n::text(cx, "chat.output")),
                    )
                    .child(
                        div()
                            .mt(px(4.0))
                            .text_size(px(12.0))
                            .text_color(theme.foreground)
                            .child(output),
                    )
                })
        });

        div()
            .flex_none()
            .w(px(286.0))
            .min_h(px(0.0))
            .bg(theme.surface_subtle)
            .p(px(16.0))
            .overflow_y_scrollbar()
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child(i18n::text(cx, "chat.context")),
                    )
                    .child(
                        Button::new("close-context-sidebar")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_context_sidebar(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .mt(px(16.0))
                    .p(px(12.0))
                    .rounded(px(9.0))
                    .bg(theme.surface)
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.muted)
                            .child(i18n::text(cx, "chat.latest_usage")),
                    )
                    .child(
                        div()
                            .mt(px(10.0))
                            .flex()
                            .justify_between()
                            .text_size(px(12.0))
                            .child(
                                div()
                                    .text_color(theme.muted)
                                    .child(i18n::text(cx, "chat.input")),
                            )
                            .child(input_tokens),
                    )
                    .child(
                        div()
                            .mt(px(6.0))
                            .flex()
                            .justify_between()
                            .text_size(px(12.0))
                            .child(
                                div()
                                    .text_color(theme.muted)
                                    .child(i18n::text(cx, "chat.output")),
                            )
                            .child(output_tokens),
                    )
                    .child(
                        div()
                            .mt(px(6.0))
                            .flex()
                            .justify_between()
                            .text_size(px(12.0))
                            .child(
                                div()
                                    .text_color(theme.muted)
                                    .child(i18n::text(cx, "chat.context_window")),
                            )
                            .child(context_limit),
                    )
                    .when_some(progress, |this, progress| {
                        this.child(
                            div()
                                .mt(px(12.0))
                                .h(px(5.0))
                                .w(px(246.0))
                                .rounded_full()
                                .bg(theme.surface_hover)
                                .child(progress),
                        )
                    })
                    .child(
                        div()
                            .mt(px(8.0))
                            .text_size(px(10.0))
                            .text_color(theme.faint)
                            .child(if percentage.is_some() {
                                i18n::text(cx, "chat.measured_usage")
                            } else {
                                i18n::text(cx, "chat.waiting_usage")
                            }),
                    ),
            )
            .child(
                div().mt(px(12.0)).child(
                    Button::new("force-context-compaction")
                        .w_full()
                        .secondary()
                        .label(if context_busy {
                            i18n::text(cx, "chat.compacting")
                        } else {
                            i18n::text(cx, "chat.compact_now")
                        })
                        .loading(context_busy)
                        .disabled(context_busy || !has_agent)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.force_compact_active(cx);
                        })),
                ),
            )
            .when(!agent_threads.is_empty(), |this| {
                this.child(
                    div()
                        .mt(px(18.0))
                        .text_size(px(11.0))
                        .text_color(theme.faint)
                        .child(i18n::text(cx, "chat.delegated_agents")),
                )
                .children(thread_rows)
            })
            .when_some(selected_panel, |this, panel| this.child(panel))
            .into_any_element()
    }

    fn render_legacy_connections(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::current(cx);
        let profiles = self.runtime.connections();
        let codex_account = self.codex_account.clone();
        let mut cards = Vec::new();
        for profile in profiles {
            let id = profile.id.clone();
            let is_codex = profile.kind == ConnectionKind::Codex;
            let is_copilot = profile.kind == ConnectionKind::Copilot;
            let codex_connected = is_codex
                && codex_account
                    .as_ref()
                    .is_some_and(|account| account.authenticated);
            let copilot_connected = is_copilot && self.runtime.has_connection_credential(&id);
            let copilot_model_count = if is_copilot {
                self.runtime
                    .models_for_connection(&id)
                    .unwrap_or_default()
                    .len()
            } else {
                0
            };
            let needs_sign_in =
                (is_codex && !codex_connected) || (is_copilot && !copilot_connected);
            let login_id = id.clone();
            let subtitle = match profile.kind {
                ConnectionKind::Codex => codex_account
                    .as_ref()
                    .filter(|account| account.authenticated)
                    .map(|account| {
                        format!(
                            "{} · {} plan · direct from Averroes",
                            account.email.as_deref().unwrap_or("ChatGPT"),
                            account.plan.as_deref().unwrap_or("unknown")
                        )
                    })
                    .unwrap_or_else(|| "ChatGPT not signed in · no CLI required".to_string()),
                ConnectionKind::Copilot if copilot_model_count > 0 => format!(
                    "GitHub Copilot · {copilot_model_count} live models · encrypted GitHub token"
                ),
                ConnectionKind::Copilot => {
                    "GitHub Copilot · encrypted GitHub token · no models loaded".to_string()
                }
                ConnectionKind::QDivZero => {
                    "QDivZero API · live serving catalog · encrypted credential".to_string()
                }
                ConnectionKind::OpenAi => "OpenAI API · encrypted credential".to_string(),
                ConnectionKind::Anthropic => "Anthropic API · encrypted credential".to_string(),
                ConnectionKind::DeepSeek => "DeepSeek API · encrypted credential".to_string(),
                ConnectionKind::Groq => "Groq API · encrypted credential".to_string(),
                ConnectionKind::Ollama => profile
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "http://localhost:11434".into()),
                ConnectionKind::OllamaCloud => profile
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://ollama.com/v1".into()),
                ConnectionKind::Compatible => profile
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "Compatible API".into()),
            };
            cards.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .p(px(14.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(36.0))
                            .rounded(px(10.0))
                            .bg(gpui::rgb(0xf3f4f6))
                            .child(provider_logo(profile.kind, 19.0)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(profile.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.muted)
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        div()
                            .px(px(9.0))
                            .py(px(4.0))
                            .rounded_full()
                            .bg(if needs_sign_in {
                                theme.accent_soft
                            } else {
                                theme.success_soft
                            })
                            .text_color(if needs_sign_in {
                                theme.accent_hover
                            } else {
                                theme.success
                            })
                            .text_size(px(10.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if codex_connected {
                                "CONNECTED"
                            } else if copilot_connected {
                                "CONNECTED"
                            } else if needs_sign_in {
                                "SIGN IN"
                            } else {
                                "SAVED"
                            }),
                    )
                    .when(is_codex && !codex_connected, |this| {
                        this.child(
                            Button::new(format!("login-codex-{}", profile.id))
                                .secondary()
                                .small()
                                .label(i18n::text(cx, "settings.connect"))
                                .loading(self.codex_busy)
                                .on_click(cx.listener(|this, _, _, cx| this.start_codex_login(cx))),
                        )
                    })
                    .when(is_copilot, |this| {
                        this.child(
                            Button::new(format!("login-copilot-{}", profile.id))
                                .secondary()
                                .small()
                                .icon(IconName::Github)
                                .label(if copilot_connected {
                                    "Reconnect"
                                } else {
                                    "Sign in with GitHub"
                                })
                                .loading(self.copilot_busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_copilot_login(login_id.clone(), cx)
                                })),
                        )
                    })
                    .child(
                        Button::new(format!("delete-connection-{}", profile.id))
                            .ghost()
                            .small()
                            .icon(IconName::Delete)
                            .tooltip(i18n::text(cx, "settings.remove_connection"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.delete_connection(id.clone(), window, cx)
                            })),
                    )
                    .into_any_element(),
            );
        }

        let needs_base_url = matches!(
            self.selected_kind,
            Some(ConnectionKind::Compatible | ConnectionKind::Ollama | ConnectionKind::OllamaCloud)
        );
        let codex = self.selected_kind == Some(ConnectionKind::Codex);
        let copilot = self.selected_kind == Some(ConnectionKind::Copilot);
        let ollama_cloud = self.selected_kind == Some(ConnectionKind::OllamaCloud);
        let needs_key = self
            .selected_kind
            .is_some_and(ConnectionKind::requires_api_key)
            && (!copilot || self.show_manual_copilot_token);
        let copilot_uses_login = copilot && self.key_input.read(cx).value().trim().is_empty();
        let notice = self.notice.clone().map(|notice| {
            div()
                .px(px(12.0))
                .py(px(9.0))
                .rounded(px(9.0))
                .bg(if notice.success {
                    theme.success_soft
                } else {
                    theme.destructive_soft
                })
                .text_color(if notice.success {
                    theme.success
                } else {
                    theme.destructive
                })
                .text_size(px(12.0))
                .child(notice.text)
                .into_any_element()
        });

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.0))
            .child(
                div()
                    .h(px(72.0))
                    .px(px(32.0))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .child(
                                div()
                                    .font(UiTheme::display_font())
                                    .font_weight(FontWeight::BOLD)
                                    .child(i18n::text(cx, "settings.title")),
                            )
                            .child(div().text_size(px(11.0)).text_color(theme.faint).child(
                                i18n::text(cx, settings_tab_description(self.settings_tab)),
                            )),
                    )
                    .child(
                        Button::new("back-to-chat")
                            .secondary()
                            .icon(IconName::ArrowLeft)
                            .label(i18n::text(cx, "settings.back_to_work"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.route = Route::Chat;
                                cx.notify();
                            })),
                    ),
            )
            .child(self.render_settings_tabs(theme, cx))
            .when(self.settings_tab == SettingsTab::Connections, |this| {
                this.child(
                    div().flex_1().overflow_y_scrollbar().child(
                        div()
                            .mx_auto()
                            .w_full()
                            .max_w(px(1050.0))
                            .px(px(32.0))
                            .py(px(26.0))
                            .flex()
                            .gap(px(24.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(
                                        div()
                                            .font(UiTheme::display_font())
                                            .text_size(px(24.0))
                                            .font_weight(FontWeight::BOLD)
                                            .child(i18n::text(cx, "settings.connections_title")),
                                    )
                                    .child(
                                        div()
                                            .mt(px(6.0))
                                            .mb(px(18.0))
                                            .text_color(theme.muted)
                                            .child(i18n::text(
                                                cx,
                                                "settings.connections_description",
                                            )),
                                    )
                                    .when(cards.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .p(px(24.0))
                                                .rounded(px(14.0))
                                                .bg(theme.surface_subtle)
                                                .text_color(theme.muted)
                                                .child(i18n::text(cx, "settings.no_connections")),
                                        )
                                    })
                                    .child(div().flex().flex_col().gap(px(10.0)).children(cards)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(360.0))
                                    .p(px(20.0))
                                    .rounded(px(16.0))
                                    .bg(theme.surface)
                                    .child(
                                        div()
                                            .font(UiTheme::display_font())
                                            .text_size(px(18.0))
                                            .font_weight(FontWeight::BOLD)
                                            .child(i18n::text(cx, "settings.add_connection")),
                                    )
                                    .child(
                                        div()
                                            .mt(px(4.0))
                                            .mb(px(18.0))
                                            .text_size(px(12.0))
                                            .text_color(theme.muted)
                                            .child(i18n::text(
                                                cx,
                                                "settings.credentials_description",
                                            )),
                                    )
                                    .child(form_label(
                                        i18n::text(cx, "settings.connection_type"),
                                        theme,
                                    ))
                                    .child(Select::new(&self.kind_select).w_full().placeholder(
                                        i18n::text(cx, "settings.choose_connection_type"),
                                    ))
                                    .child(
                                        form_label(i18n::text(cx, "settings.name"), theme)
                                            .mt(px(14.0)),
                                    )
                                    .child(Input::new(&self.name_input).w_full())
                                    .when(needs_base_url, |this| {
                                        this.child(
                                            form_label(i18n::text(cx, "settings.base_url"), theme)
                                                .mt(px(14.0)),
                                        )
                                        .child(Input::new(&self.url_input).w_full())
                                    })
                                    .when(needs_key, |this| {
                                        this.child(
                                            form_label(
                                                if copilot {
                                                    "Access token (optional)"
                                                } else {
                                                    "API key"
                                                },
                                                theme,
                                            )
                                            .mt(px(14.0)),
                                        )
                                        .child(Input::new(&self.key_input).w_full().mask_toggle())
                                    })
                                    .when(codex, |this| {
                                        this.child(
                                            div()
                                                .mt(px(14.0))
                                                .p(px(11.0))
                                                .rounded(px(9.0))
                                                .bg(theme.accent_soft)
                                                .text_color(theme.accent_hover)
                                                .text_size(px(12.0))
                                                .child(i18n::text(
                                                    cx,
                                                    "settings.codex_full_description",
                                                )),
                                        )
                                    })
                                    .when(
                                        self.selected_kind == Some(ConnectionKind::Copilot),
                                        |this| {
                                            this.child(
                                                div()
                                                    .mt(px(14.0))
                                                    .p(px(11.0))
                                                    .rounded(px(9.0))
                                                    .bg(theme.accent_soft)
                                                    .text_color(theme.accent_hover)
                                                    .text_size(px(12.0))
                                                    .child(i18n::text(
                                                        cx,
                                                        "settings.copilot_full_description",
                                                    )),
                                            )
                                            .child(
                                                Button::new("toggle-copilot-token")
                                                    .ghost()
                                                    .small()
                                                    .label(if self.show_manual_copilot_token {
                                                        "Sign in with GitHub instead"
                                                    } else {
                                                        "Use an access token instead"
                                                    })
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.show_manual_copilot_token =
                                                                !this.show_manual_copilot_token;
                                                            if !this.show_manual_copilot_token {
                                                                this.key_input.update(
                                                                    cx,
                                                                    |state, cx| {
                                                                        state.set_value(
                                                                            "", window, cx,
                                                                        )
                                                                    },
                                                                );
                                                            }
                                                            cx.notify();
                                                        },
                                                    )),
                                            )
                                        },
                                    )
                                    .when(
                                        self.selected_kind == Some(ConnectionKind::Ollama),
                                        |this| {
                                            this.child(
                                                div()
                                                    .mt(px(14.0))
                                                    .p(px(11.0))
                                                    .rounded(px(9.0))
                                                    .bg(theme.accent_soft)
                                                    .text_color(theme.accent_hover)
                                                    .text_size(px(12.0))
                                                    .child(i18n::text(
                                                        cx,
                                                        "settings.ollama_local_description",
                                                    )),
                                            )
                                        },
                                    )
                                    .when(ollama_cloud, |this| {
                                        this.child(
                                            div()
                                                .mt(px(14.0))
                                                .p(px(11.0))
                                                .rounded(px(9.0))
                                                .bg(theme.accent_soft)
                                                .text_color(theme.accent_hover)
                                                .text_size(px(12.0))
                                                .child(i18n::text(
                                                    cx,
                                                    "settings.ollama_cloud_description",
                                                )),
                                        )
                                    })
                                    .when_some(notice, |this, notice| {
                                        this.child(div().mt(px(14.0)).child(notice))
                                    })
                                    .child(
                                        div()
                                            .mt(px(18.0))
                                            .flex()
                                            .items_center()
                                            .gap(px(9.0))
                                            .when(codex, |this| {
                                                this.child(
                                                    Button::new("connect-chatgpt")
                                                        .secondary()
                                                        .icon(IconName::ExternalLink)
                                                        .label(
                                                            if self
                                                                .codex_account
                                                                .as_ref()
                                                                .is_some_and(|account| {
                                                                    account.authenticated
                                                                })
                                                            {
                                                                i18n::text(cx, "settings.reconnect")
                                                            } else {
                                                                i18n::text(
                                                                    cx,
                                                                    "settings.continue_chatgpt",
                                                                )
                                                            },
                                                        )
                                                        .loading(self.codex_busy)
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.start_codex_login(cx)
                                                        })),
                                                )
                                            })
                                            .child(
                                                Button::new("save-connection")
                                                    .primary()
                                                    .icon(if copilot_uses_login {
                                                        IconName::Github
                                                    } else {
                                                        IconName::CircleCheck
                                                    })
                                                    .label(if copilot_uses_login {
                                                        i18n::text(cx, "settings.continue_github")
                                                    } else {
                                                        i18n::text(cx, "settings.save_connection")
                                                    })
                                                    .disabled(self.selected_kind.is_none())
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.save_connection(window, cx)
                                                        },
                                                    )),
                                            ),
                                    ),
                            ),
                    ),
                )
            })
            .when(self.settings_tab == SettingsTab::Models, |this| {
                this.child(self.render_settings_models(cx))
            })
            .when(self.settings_tab == SettingsTab::Agents, |this| {
                this.child(self.render_settings_agents(cx))
            })
            .when(self.settings_tab == SettingsTab::Diagnostics, |this| {
                this.child(self.render_settings_diagnostics(cx))
            })
            .when(self.settings_tab == SettingsTab::Storage, |this| {
                this.child(self.render_settings_storage(cx))
            })
            .when(self.settings_tab == SettingsTab::About, |this| {
                this.child(self.render_settings_about(cx))
            })
            .child(self.render_status_line(cx))
            .into_any_element()
    }

    fn render_connections(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::current(cx);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .overflow_hidden()
            .child(
                div()
                    .h(px(72.0))
                    .px(px(32.0))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .child(
                                div()
                                    .font(UiTheme::display_font())
                                    .font_weight(FontWeight::BOLD)
                                    .child(i18n::text(cx, "settings.title")),
                            )
                            .child(div().text_size(px(11.0)).text_color(theme.faint).child(
                                i18n::text(cx, settings_tab_description(self.settings_tab)),
                            )),
                    )
                    .child(
                        Button::new("back-to-chat")
                            .secondary()
                            .icon(IconName::ArrowLeft)
                            .label(i18n::text(cx, "settings.back_to_work"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.route = Route::Chat;
                                cx.notify();
                            })),
                    ),
            )
            .child(self.render_settings_tabs(theme, cx))
            .when(self.settings_tab == SettingsTab::Models, |this| {
                this.child(self.render_settings_models(cx))
            })
            .when(self.settings_tab == SettingsTab::Agents, |this| {
                this.child(self.render_settings_agents(cx))
            })
            .when(self.settings_tab == SettingsTab::Diagnostics, |this| {
                this.child(self.render_settings_diagnostics(cx))
            })
            .when(self.settings_tab == SettingsTab::Storage, |this| {
                this.child(self.render_settings_storage(cx))
            })
            .when(self.settings_tab == SettingsTab::About, |this| {
                this.child(self.render_settings_about(cx))
            })
            .child(self.render_status_line(cx))
            .into_any_element()
    }

    fn render_settings_tabs(&self, theme: UiTheme, cx: &mut Context<Self>) -> AnyElement {
        let tabs = [
            (SettingsTab::Models, "settings.models", "models"),
            (SettingsTab::Agents, "settings.agents", "agents"),
            (
                SettingsTab::Diagnostics,
                "settings.diagnostics",
                "diagnostics",
            ),
            (SettingsTab::Storage, "settings.storage", "storage"),
            (SettingsTab::About, "settings.about", "about"),
        ];
        div()
            .flex_none()
            .h(px(46.0))
            .px(px(32.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .border_b_1()
            .border_color(theme.border)
            .children(tabs.into_iter().map(|(tab, label, key)| {
                let selected = self.settings_tab == tab;
                div()
                    .id(SharedString::from(format!("settings-tab-{key}")))
                    .h(px(32.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .rounded(px(7.0))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(if selected {
                        theme.foreground
                    } else {
                        theme.muted
                    })
                    .when(selected, |this| this.bg(theme.surface))
                    .hover(|style| style.bg(theme.surface))
                    .child(i18n::text(cx, label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_tab = tab;
                        cx.notify();
                    }))
                    .into_any_element()
            }))
            .into_any_element()
    }

    fn render_embedding_settings_card(
        &mut self,
        theme: UiTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let status = self.embedding_status.clone();
        let status_text = match status.as_ref() {
            Some(status) if status.config.is_some() => format!(
                "Indexed {} of {} conversations · {} fragments",
                status.indexed_conversations, status.total_conversations, status.indexed_fragments
            ),
            _ => i18n::text(cx, "settings.not_configured").to_string(),
        };
        let has_provider = self.embedding_connection_id.is_some();
        let has_model = self.embedding_model_id.is_some();
        div()
            .mt(px(22.0))
            .p(px(18.0))
            .bg(theme.surface_subtle)
            .rounded(px(12.0))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(i18n::text(cx, "settings.memory_title")),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .text_size(px(12.0))
                    .text_color(theme.muted)
                    .child(i18n::text(cx, "settings.memory_description")),
            )
            .child(
                div()
                    .mt(px(15.0))
                    .flex()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .child(form_label(
                                i18n::text(cx, "settings.embedding_provider"),
                                theme,
                            ))
                            .child(
                                Select::new(&self.embedding_connection_select)
                                    .w_full()
                                    .placeholder(i18n::text(cx, "settings.choose_provider"))
                                    .search_placeholder(i18n::text(
                                        cx,
                                        "settings.search_providers",
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(form_label(
                                i18n::text(cx, "settings.embedding_model"),
                                theme,
                            ))
                            .child(
                                Select::new(&self.embedding_model_select)
                                    .w_full()
                                    .placeholder(i18n::text(cx, "settings.choose_embedding_model"))
                                    .search_placeholder(i18n::text(cx, "settings.search_models"))
                                    .disabled(
                                        !has_provider || self.embedding_model_labels.is_empty(),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .mt(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.0))
                            .text_color(theme.faint)
                            .child(status_text),
                    )
                    .child(
                        Button::new("build-conversation-index")
                            .primary()
                            .small()
                            .icon(IconName::CircleCheck)
                            .label(if self.embedding_index_busy {
                                i18n::text(cx, "settings.building")
                            } else {
                                i18n::text(cx, "settings.save_build_index")
                            })
                            .loading(self.embedding_index_busy)
                            .disabled(!has_provider || !has_model || self.embedding_index_busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.compile_embedding_index(window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_settings_models(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::current(cx);
        let profiles = self.runtime.connections();
        let mut rows = Vec::new();
        for profile in profiles {
            let connection_id = profile.id.clone();
            let delete_connection_id = connection_id.clone();
            let models = self
                .runtime
                .models_for_connection(&profile.id)
                .unwrap_or_default();
            let model_count = models.len();
            let source = if models.iter().any(|model| model.source == ModelSource::Live) {
                "settings.live_catalog"
            } else if models
                .iter()
                .any(|model| model.source == ModelSource::Manual)
            {
                "settings.manual_catalog"
            } else {
                "settings.curated_catalog"
            };
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(16.0))
                    .py(px(13.0))
                    .bg(theme.surface_subtle)
                    .rounded(px(10.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(34.0))
                            .rounded(px(9.0))
                            .bg(gpui::rgb(0xf3f4f6))
                            .child(provider_logo(profile.kind, 18.0)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(profile.name))
                            .child(
                                div()
                                    .mt(px(3.0))
                                    .text_size(px(11.0))
                                    .text_color(theme.muted)
                                    .child(format!(
                                        "{} · {}",
                                        profile.kind.label(),
                                        i18n::text(cx, source)
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(if model_count == 0 {
                                theme.faint
                            } else {
                                theme.foreground
                            })
                            .child(format!(
                                "{model_count} {}",
                                i18n::text(
                                    cx,
                                    if model_count == 1 {
                                        "settings.model"
                                    } else {
                                        "settings.models_count"
                                    }
                                )
                            )),
                    )
                    .child(
                        Button::new(format!("add-model-{}", connection_id))
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .label(i18n::text(cx, "settings.add_model"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.manual_model_connection = Some(connection_id.clone());
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(format!("remove-provider-{}", profile.id))
                            .ghost()
                            .small()
                            .icon(IconName::Delete)
                            .tooltip(i18n::text(cx, "settings.remove_provider"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.delete_connection(delete_connection_id.clone(), window, cx)
                            })),
                    )
                    .into_any_element(),
            );
        }

        let manual_model_card = self.render_manual_model_card(theme, cx);

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_hidden()
            .child(
                div()
                    .mx_auto()
                    .w_full()
                    .max_w(px(1100.0))
                    .flex_1()
                    .min_h(px(0.0))
                    .px(px(32.0))
                    .py(px(30.0))
                    .flex()
                    .gap(px(24.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.0))
                            .min_h(px(0.0))
                            .child(settings_page_title(
                                i18n::text(cx, "settings.models"),
                                i18n::text(cx, "settings.models_description"),
                                theme,
                            ))
                            .child(self.render_embedding_settings_card(theme, cx))
                            .child(
                                div()
                                    .mt(px(22.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(i18n::text(cx, "settings.connected_providers")),
                                    )
                                    .child(
                                        Button::new("refresh-model-catalogs")
                                            .ghost()
                                            .small()
                                            .icon(IconName::Redo2)
                                            .label(i18n::text(cx, "settings.refresh_catalogs"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.refresh_model_catalogs(cx)
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .id("connected-providers-scroll")
                                    .mt(px(10.0))
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .overflow_y_scrollbar()
                                    .flex()
                                    .flex_col()
                                    .gap(px(9.0))
                                    .when(rows.is_empty(), |this| {
                                        this.child(settings_empty_state(
                                            i18n::text(cx, "settings.no_providers"),
                                            i18n::text(cx, "settings.no_providers_description"),
                                            theme,
                                        ))
                                    })
                                    .children(rows),
                            )
                            .child(manual_model_card),
                    )
                    .child(self.render_add_provider_card(theme, cx)),
            )
            .into_any_element()
    }

    fn render_settings_agents(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::current(cx);
        let agents = self.runtime.agents();
        let rows = agents
            .iter()
            .cloned()
            .map(|agent| {
                let edit_agent = agent.clone();
                let delete_id = agent.id.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .p(px(14.0))
                    .rounded(px(11.0))
                    .bg(theme.surface_subtle)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(agent.name.clone()),
                            )
                            .child(
                                div()
                                    .mt(px(3.0))
                                    .text_size(px(11.0))
                                    .text_color(theme.muted)
                                    .child(format!("{} · {}", agent.connection_id, agent.model_id)),
                            )
                            .when(!agent.description.trim().is_empty(), |this| {
                                this.child(
                                    div()
                                        .mt(px(4.0))
                                        .text_size(px(11.0))
                                        .text_color(theme.faint)
                                        .child(agent.description.clone()),
                                )
                            }),
                    )
                    .child(
                        Button::new(format!("edit-agent-{}", agent.id))
                            .ghost()
                            .small()
                            .icon(IconName::Settings2)
                            .label(i18n::text(cx, "settings.edit_agent"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_agent_profile(edit_agent.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new(format!("delete-agent-{}", agent.id))
                            .ghost()
                            .small()
                            .icon(IconName::Delete)
                            .tooltip(i18n::text(cx, "settings.remove_agent"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_agent_profile(delete_id.clone(), cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let editing = self.editing_agent_id.is_some();
        let connection_selected = self.agent_form_connection_id.is_some();
        let model_selected = self.agent_form_model_id.is_some();
        let notice = self.notice.clone().map(|notice| {
            div()
                .mt(px(12.0))
                .p(px(10.0))
                .rounded(px(8.0))
                .bg(if notice.success {
                    theme.success_soft
                } else {
                    theme.destructive_soft
                })
                .text_color(if notice.success {
                    theme.success
                } else {
                    theme.destructive
                })
                .text_size(px(11.0))
                .child(notice.text)
                .into_any_element()
        });

        div()
            .flex_1()
            .min_h(px(0.0))
            .overflow_hidden()
            .child(
                div()
                    .mx_auto()
                    .w_full()
                    .max_w(px(1100.0))
                    .h_full()
                    .px(px(32.0))
                    .py(px(30.0))
                    .flex()
                    .gap(px(24.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.0))
                            .min_h(px(0.0))
                            .child(settings_page_title(
                                i18n::text(cx, "settings.agents"),
                                i18n::text(cx, "settings.agents_description"),
                                theme,
                            ))
                            .child(
                                div()
                                    .id("configured-agents-scroll")
                                    .mt(px(20.0))
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .overflow_y_scrollbar()
                                    .flex()
                                    .flex_col()
                                    .gap(px(9.0))
                                    .when(rows.is_empty(), |this| {
                                        this.child(settings_empty_state(
                                            i18n::text(cx, "settings.no_agents"),
                                            i18n::text(cx, "settings.no_agents_description"),
                                            theme,
                                        ))
                                    })
                                    .children(rows),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(330.0))
                            .h_full()
                            .min_h(px(0.0))
                            .overflow_y_scrollbar()
                            .p(px(18.0))
                            .rounded(px(12.0))
                            .bg(theme.surface_subtle)
                            .child(
                                div()
                                    .font(UiTheme::display_font())
                                    .text_size(px(17.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child(if editing {
                                        i18n::text(cx, "settings.edit_agent")
                                    } else {
                                        i18n::text(cx, "settings.add_agent")
                                    }),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .mb(px(16.0))
                                    .text_size(px(11.0))
                                    .text_color(theme.muted)
                                    .child(i18n::text(cx, "settings.agent_description_help")),
                            )
                            .child(form_label(i18n::text(cx, "settings.agent_id"), theme))
                            .child(Input::new(&self.agent_id_input).w_full())
                            .child(
                                form_label(i18n::text(cx, "settings.agent_name"), theme)
                                    .mt(px(13.0)),
                            )
                            .child(Input::new(&self.agent_name_input).w_full())
                            .child(
                                form_label(i18n::text(cx, "settings.agent_description"), theme)
                                    .mt(px(13.0)),
                            )
                            .child(Input::new(&self.agent_description_input).w_full())
                            .child(
                                form_label(i18n::text(cx, "settings.agent_connection"), theme)
                                    .mt(px(13.0)),
                            )
                            .child(
                                Select::new(&self.agent_connection_select)
                                    .w_full()
                                    .placeholder(i18n::text(cx, "settings.choose_provider"))
                                    .search_placeholder(i18n::text(
                                        cx,
                                        "settings.search_providers",
                                    )),
                            )
                            .child(
                                form_label(i18n::text(cx, "settings.agent_model"), theme)
                                    .mt(px(13.0)),
                            )
                            .child(
                                Select::new(&self.agent_model_select)
                                    .w_full()
                                    .disabled(
                                        !connection_selected || self.agent_model_labels.is_empty(),
                                    )
                                    .placeholder(i18n::text(cx, "settings.choose_model"))
                                    .search_placeholder(i18n::text(cx, "settings.search_models")),
                            )
                            .when_some(notice, |this, notice| this.child(notice))
                            .child(
                                div()
                                    .mt(px(18.0))
                                    .flex()
                                    .gap(px(8.0))
                                    .child(
                                        Button::new("clear-agent-form")
                                            .secondary()
                                            .label(i18n::text(cx, "settings.new_agent"))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.clear_agent_form(window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("save-agent")
                                            .primary()
                                            .icon(IconName::CircleCheck)
                                            .label(i18n::text(cx, "settings.save_agent"))
                                            .disabled(!connection_selected || !model_selected)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.save_agent_profile(window, cx)
                                            })),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_add_provider_card(&mut self, theme: UiTheme, cx: &mut Context<Self>) -> AnyElement {
        let selected_kind = effective_connection_kind(
            self.selected_kind,
            self.kind_select.read(cx).selected_value().copied(),
        );
        let needs_base_url = matches!(
            selected_kind,
            Some(ConnectionKind::Compatible | ConnectionKind::Ollama | ConnectionKind::OllamaCloud)
        );
        let codex = selected_kind == Some(ConnectionKind::Codex);
        let copilot = selected_kind == Some(ConnectionKind::Copilot);
        let qdivzero = selected_kind == Some(ConnectionKind::QDivZero);
        let ollama_cloud = selected_kind == Some(ConnectionKind::OllamaCloud);
        let needs_key = selected_kind.is_some_and(ConnectionKind::requires_api_key)
            && (!copilot || self.show_manual_copilot_token);
        let copilot_uses_login = copilot && self.key_input.read(cx).value().trim().is_empty();
        let notice = self.notice.clone().map(|notice| {
            div()
                .px(px(11.0))
                .py(px(9.0))
                .rounded(px(9.0))
                .bg(if notice.success {
                    theme.success_soft
                } else {
                    theme.destructive_soft
                })
                .text_color(if notice.success {
                    theme.success
                } else {
                    theme.destructive
                })
                .text_size(px(11.0))
                .child(notice.text)
                .into_any_element()
        });

        div()
            .id("settings-add-provider-form")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(330.0))
            .p(px(18.0))
            .rounded(px(12.0))
            .bg(theme.surface_subtle)
            .child(
                div()
                    .font(UiTheme::display_font())
                    .text_size(px(17.0))
                    .font_weight(FontWeight::BOLD)
                    .child(i18n::text(cx, "settings.add_provider")),
            )
            .child(
                div()
                    .mt(px(5.0))
                    .mb(px(18.0))
                    .text_size(px(11.0))
                    .text_color(theme.muted)
                    .child(i18n::text(cx, "settings.credentials_direct")),
            )
            .child(form_label(
                i18n::text(cx, "settings.connection_type"),
                theme,
            ))
            .child(
                div().flex_none().w_full().h(px(34.0)).child(
                    Select::new(&self.kind_select)
                        .w_full()
                        .placeholder(i18n::text(cx, "settings.choose_provider")),
                ),
            )
            .child(form_label(i18n::text(cx, "settings.name"), theme).mt(px(14.0)))
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .h(px(34.0))
                    .child(Input::new(&self.name_input).w_full()),
            )
            .when(needs_base_url, |this| {
                this.child(form_label(i18n::text(cx, "settings.base_url"), theme).mt(px(14.0)))
                    .child(
                        div()
                            .flex_none()
                            .w_full()
                            .h(px(34.0))
                            .child(Input::new(&self.url_input).w_full()),
                    )
            })
            .when(needs_key, |this| {
                this.child(
                    form_label(
                        if copilot {
                            i18n::text(cx, "settings.access_token")
                        } else {
                            i18n::text(cx, "settings.api_key")
                        },
                        theme,
                    )
                    .mt(px(14.0)),
                )
                .child(
                    div()
                        .flex_none()
                        .w_full()
                        .h(px(34.0))
                        .child(Input::new(&self.key_input).w_full().mask_toggle()),
                )
            })
            .when(codex, |this| {
                this.child(
                    div()
                        .mt(px(14.0))
                        .text_size(px(11.0))
                        .text_color(theme.muted)
                        .child(i18n::text(cx, "settings.codex_description")),
                )
            })
            .when(copilot, |this| {
                this.child(
                    div()
                        .mt(px(14.0))
                        .text_size(px(11.0))
                        .text_color(theme.muted)
                        .child(i18n::text(cx, "settings.copilot_description")),
                )
                .child(
                    Button::new("toggle-models-copilot-token")
                        .ghost()
                        .small()
                        .label(if self.show_manual_copilot_token {
                            i18n::text(cx, "settings.sign_in_github_instead")
                        } else {
                            i18n::text(cx, "settings.use_access_token")
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_manual_copilot_token = !this.show_manual_copilot_token;
                            if !this.show_manual_copilot_token {
                                this.key_input
                                    .update(cx, |state, cx| state.set_value("", window, cx));
                            }
                            cx.notify();
                        })),
                )
            })
            .when(qdivzero, |this| {
                this.child(
                    div()
                        .mt(px(14.0))
                        .text_size(px(11.0))
                        .text_color(theme.muted)
                        .child(i18n::text(cx, "settings.qdivzero_description")),
                )
            })
            .when(selected_kind == Some(ConnectionKind::Ollama), |this| {
                this.child(
                    div()
                        .mt(px(14.0))
                        .text_size(px(11.0))
                        .text_color(theme.muted)
                        .child(i18n::text(cx, "settings.ollama_description")),
                )
            })
            .when(ollama_cloud, |this| {
                this.child(
                    div()
                        .mt(px(14.0))
                        .text_size(px(11.0))
                        .text_color(theme.muted)
                        .child(i18n::text(cx, "settings.ollama_cloud_description")),
                )
            })
            .when_some(notice, |this, notice| {
                this.child(div().mt(px(14.0)).child(notice))
            })
            .child(
                div()
                    .mt(px(18.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .when(codex, |this| {
                        this.child(
                            Button::new("models-connect-chatgpt")
                                .secondary()
                                .icon(IconName::ExternalLink)
                                .label(
                                    if self
                                        .codex_account
                                        .as_ref()
                                        .is_some_and(|account| account.authenticated)
                                    {
                                        "Reconnect ChatGPT"
                                    } else {
                                        "Continue with ChatGPT"
                                    },
                                )
                                .loading(self.codex_busy)
                                .on_click(cx.listener(|this, _, _, cx| this.start_codex_login(cx))),
                        )
                    })
                    .child(
                        Button::new("models-save-provider")
                            .primary()
                            .icon(if copilot_uses_login {
                                IconName::Github
                            } else {
                                IconName::CircleCheck
                            })
                            .label(if copilot_uses_login {
                                "Continue with GitHub"
                            } else {
                                "Save provider"
                            })
                            .disabled(selected_kind.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save_connection(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_manual_model_card(&mut self, theme: UiTheme, cx: &mut Context<Self>) -> AnyElement {
        let Some(connection_id) = self.manual_model_connection.clone() else {
            return div().into_any_element();
        };
        let connection_name = self
            .runtime
            .connection(&connection_id)
            .map(|profile| profile.name)
            .unwrap_or_else(|| "integration".into());
        let save_connection_id = connection_id.clone();
        div()
            .mt(px(18.0))
            .p(px(16.0))
            .rounded(px(12.0))
            .bg(theme.surface_subtle)
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Add a model to {connection_name}")),
                    )
                    .child(
                        Button::new("cancel-manual-model")
                            .ghost()
                            .small()
                            .label(i18n::text(cx, "dialog.cancel"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.manual_model_connection = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .mt(px(13.0))
                    .flex()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .child(form_label(i18n::text(cx, "settings.model_id"), theme))
                            .child(Input::new(&self.manual_model_id_input).w_full()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(form_label(i18n::text(cx, "settings.display_name"), theme))
                            .child(Input::new(&self.manual_model_name_input).w_full()),
                    ),
            )
            .child(
                div()
                    .mt(px(12.0))
                    .child(form_label(
                        i18n::text(cx, "settings.reasoning_levels"),
                        theme,
                    ))
                    .child(Input::new(&self.manual_model_reasoning_input).w_full()),
            )
            .child(
                div()
                    .mt(px(7.0))
                    .text_size(px(11.0))
                    .text_color(theme.faint)
                    .child(i18n::text(cx, "settings.reasoning_help")),
            )
            .child(
                div().mt(px(14.0)).flex().justify_end().child(
                    Button::new("save-manual-model")
                        .primary()
                        .icon(IconName::Plus)
                        .label(i18n::text(cx, "settings.add_model"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.add_manual_model(save_connection_id.clone(), window, cx)
                        })),
                ),
            )
            .into_any_element()
    }

    fn render_settings_diagnostics(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::current(cx);
        let entries = diagnostics::entries();
        let query = self
            .diagnostics_search
            .read(cx)
            .value()
            .trim()
            .to_ascii_lowercase();
        let filtered_entries = entries
            .iter()
            .filter(|entry| {
                query.is_empty()
                    || entry.component.to_ascii_lowercase().contains(&query)
                    || entry.message.to_ascii_lowercase().contains(&query)
                    || entry.level.label().to_ascii_lowercase().contains(&query)
            })
            .collect::<Vec<_>>();
        let has_matches = !filtered_entries.is_empty();
        let rows = filtered_entries.into_iter().map(|entry| {
            let color = match entry.level {
                DiagnosticLevel::Info => theme.muted,
                DiagnosticLevel::Success => theme.success,
                DiagnosticLevel::Warning => theme.warning,
                DiagnosticLevel::Error => theme.destructive,
            };
            div()
                .flex()
                .items_start()
                .gap(px(12.0))
                .py(px(6.0))
                .text_size(px(11.0))
                .child(
                    div()
                        .flex_none()
                        .w(px(52.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(color)
                        .child(entry.level.label()),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(122.0))
                        .text_color(theme.faint)
                        .child(entry.component.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_color(theme.muted)
                        .child(entry.message.clone()),
                )
                .into_any_element()
        });

        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .child(
                        div()
                            .h_full()
                            .flex()
                            .flex_col()
                            .mx_auto()
                            .w_full()
                            .max_w(px(1100.0))
                            .px(px(32.0))
                            .py(px(30.0))
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap(px(20.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .child(settings_page_title(
                                                "Diagnostics",
                                                "Session-only request trace. Credentials and recognizable token values are redacted.",
                                                theme,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .gap(px(6.0))
                                            .child(
                                                Input::new(&self.diagnostics_search)
                                                    .prefix(IconName::Search)
                                                    .w(px(250.0)),
                                            )
                                            .child(
                                                Button::new("refresh-diagnostics")
                                                    .ghost()
                                                    .small()
                                                    .icon(IconName::Redo2)
                                                    .label(i18n::text(cx, "settings.refresh"))
                                                    .on_click(cx.listener(|_, _, _, cx| cx.notify())),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .mt(px(22.0))
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .p(px(16.0))
                                    .flex()
                                    .flex_col()
                                    .bg(theme.surface_subtle)
                                    .rounded(px(12.0))
                                    .child(
                                        div()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_end()
                                            .gap(px(4.0))
                                            .child(
                                                Button::new("copy-settings-diagnostics")
                                                    .ghost()
                                                    .small()
                                                    .icon(IconName::Copy)
                                                    .label(i18n::text(cx, "settings.copy"))
                                                    .on_click(|_, _, cx| {
                                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                                            diagnostics::export_text(),
                                                        ));
                                                    }),
                                            )
                                            .child(
                                                Button::new("clear-settings-diagnostics")
                                                    .ghost()
                                                    .small()
                                                    .label(i18n::text(cx, "settings.clear"))
                                                    .on_click(cx.listener(|_, _, _, cx| {
                                                        diagnostics::clear();
                                                        cx.notify();
                                                    })),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .mt(px(10.0))
                                            .flex_1()
                                            .min_h(px(0.0))
                                            .overflow_y_scrollbar()
                                            .when(entries.is_empty(), |this| {
                                                this.child(
                                                    div()
                                                        .py(px(24.0))
                                                        .text_size(px(11.0))
                                                        .text_color(theme.faint)
                                                        .child(i18n::text(cx, "settings.no_diagnostics")),
                                                )
                                            })
                                            .when(!entries.is_empty() && !has_matches, |this| {
                                                this.child(
                                                    div()
                                                        .py(px(24.0))
                                                        .text_size(px(11.0))
                                                        .text_color(theme.faint)
                                                        .child(i18n::text(cx, "settings.no_diagnostics_match")),
                                                )
                                            })
                                            .children(rows),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_settings_storage(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::current(cx);
        let locale = cx.global::<i18n::Localization>().locale();
        let next_locale = locale.toggle();
        let view = cx.entity();
        div()
            .flex_1()
            .overflow_y_scrollbar()
            .child(
                div()
                    .mx_auto()
                    .w_full()
                    .max_w(px(760.0))
                    .px(px(32.0))
                    .py(px(30.0))
                    .child(settings_page_title(
                        i18n::text(cx, "settings.storage"),
                        i18n::text(cx, "settings.local_by_design"),
                        theme,
                    ))
                    .child(
                        div()
                            .mt(px(22.0))
                            .p(px(18.0))
                            .bg(theme.surface_subtle)
                            .rounded(px(12.0))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(i18n::text(cx, "settings.private_storage")),
                            )
                            .child(
                                div()
                                    .mt(px(7.0))
                                    .text_size(px(12.0))
                                    .text_color(theme.muted)
                                    .child(i18n::text(cx, "settings.storage_description")),
                            )
                            .child(
                                div()
                                    .mt(px(16.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(8.0))
                                    .child(storage_path_row(
                                        i18n::text(cx, "settings.encrypted_credentials"),
                                        "~/.averroes/config/providers.enc",
                                        theme,
                                    ))
                                    .child(storage_path_row(
                                        i18n::text(cx, "settings.work_database"),
                                        "~/.averroes/config/averroes.db",
                                        theme,
                                    ))
                                    .child(storage_path_row(
                                        i18n::text(cx, "settings.settings_file"),
                                        "~/.averroes/config/settings.toml",
                                        theme,
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(14.0))
                            .p(px(18.0))
                            .bg(theme.surface_subtle)
                            .rounded(px(12.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_1()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(i18n::text(cx, "settings.language")),
                                    )
                                    .child(
                                        Button::new("toggle-language")
                                            .secondary()
                                            .small()
                                            .label(locale.label())
                                            .on_click(move |_, _, cx| {
                                                cx.set_global(i18n::Localization::new(next_locale));
                                                view.update(cx, |_, cx| cx.notify());
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .mt(px(7.0))
                                    .text_size(px(12.0))
                                    .text_color(theme.muted)
                                    .child(i18n::text(cx, "settings.language_description")),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_settings_about(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::current(cx);
        div()
            .flex_1()
            .overflow_y_scrollbar()
            .child(
                div()
                    .mx_auto()
                    .w_full()
                    .max_w(px(760.0))
                    .px(px(32.0))
                    .py(px(30.0))
                    .child(settings_page_title(
                        i18n::text(cx, "about.title"),
                        i18n::text(cx, "settings.about_description"),
                        theme,
                    ))
                    .child(
                        div()
                            .mt(px(22.0))
                            .p(px(22.0))
                            .bg(theme.surface_subtle)
                            .rounded(px(14.0))
                            .flex()
                            .items_center()
                            .gap(px(18.0))
                            .child(img("brand/averroes.png").size(px(76.0)).flex_none())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(
                                        div()
                                            .font(UiTheme::display_font())
                                            .text_size(px(20.0))
                                            .font_weight(FontWeight::BOLD)
                                            .child("Averroes"),
                                    )
                                    .child(
                                        div()
                                            .mt(px(5.0))
                                            .text_size(px(12.0))
                                            .text_color(theme.muted)
                                            .child(format!(
                                                "{}: {APP_VERSION}",
                                                i18n::text(cx, "about.version")
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(14.0))
                            .p(px(18.0))
                            .bg(theme.surface_subtle)
                            .rounded(px(12.0))
                            .text_size(px(13.0))
                            .text_color(theme.muted)
                            .child(i18n::text(cx, "about.philosopher")),
                    )
                    .child(
                        div()
                            .mt(px(14.0))
                            .p(px(18.0))
                            .bg(theme.surface_subtle)
                            .rounded(px(12.0))
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .child(img("brand/valendra.svg").size(px(34.0)).flex_none())
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme.muted)
                                    .child(i18n::text(cx, "about.attribution")),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_status_line(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = UiTheme::current(cx);
        let connection_count = self.runtime.connections().len();
        let selection_status = if self.remembered_binding.is_ready() {
            i18n::text(cx, "settings.last_setup_remembered")
        } else {
            i18n::text(cx, "settings.choose_per_conversation")
        };
        div()
            .flex_none()
            .h(px(28.0))
            .px(px(14.0))
            .flex()
            .items_center()
            .justify_between()
            .bg(theme.rail)
            .text_size(px(10.0))
            .text_color(theme.faint)
            .font(UiTheme::mono_font())
            .child(format!(
                "{} {} · {}",
                connection_count,
                i18n::text(cx, "settings.connections"),
                selection_status
            ))
            .child(i18n::text(cx, "settings.storage_location"))
            .into_any_element()
    }
}

impl Render for AverroesApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::current(cx);
        let content = match self.route {
            Route::Chat => self.render_chat(cx),
            Route::Connections => self.render_connections(cx),
        };
        let sheet_layer = ComponentRoot::render_sheet_layer(window, cx);
        let dialog_layer = ComponentRoot::render_dialog_layer(window, cx);

        div()
            .key_context("Averroes")
            .on_action(cx.listener(Self::handle_new_session))
            .on_action(cx.listener(Self::handle_close_session))
            .on_action(cx.listener(Self::handle_focus_input))
            .on_action(cx.listener(Self::handle_send_message))
            .on_action(cx.listener(Self::handle_toggle_settings))
            .on_action(cx.listener(Self::handle_quit))
            .flex()
            .size_full()
            .overflow_hidden()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font(UiTheme::ui_font())
            .child(self.render_rail(cx))
            .child(content)
            .children(sheet_layer)
            .children(dialog_layer)
    }
}

fn averroes_logo_asset(_: &gpui::App) -> &'static str {
    "brand/averroes.png"
}

fn conversation_actions_button(
    conversation_id: String,
    button_id: String,
    hover_group: Option<SharedString>,
    pinned: Option<bool>,
    processing: bool,
    unread: bool,
    cx: &mut Context<AverroesApp>,
) -> AnyElement {
    let theme = UiTheme::current(cx);
    let app_view = cx.entity().downgrade();
    let pin_view = app_view.clone();
    let rename_view = app_view.clone();
    let delete_view = app_view;
    let pin_id = conversation_id.clone();
    let rename_id = conversation_id.clone();
    let delete_id = conversation_id;
    let wrapper_id = format!("{button_id}-wrapper");
    let spinner_id = SharedString::from(format!("{button_id}-spinner"));
    let indicator = if processing {
        Icon::new(IconName::Loader)
            .size(px(12.0))
            .text_color(theme.muted)
            .with_animation(
                spinner_id,
                Animation::new(std::time::Duration::from_millis(800)).repeat(),
                |icon, delta| icon.transform(Transformation::rotate(gpui::percentage(delta))),
            )
            .into_any_element()
    } else {
        div()
            .size(px(6.0))
            .rounded_full()
            .bg(theme.focus_ring)
            .into_any_element()
    };
    let status = div()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .size(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .when_some(hover_group.clone(), |status, group| {
            status.group_hover(group, |style| style.opacity(0.0))
        })
        .child(indicator);
    let trigger = Button::new(button_id)
        .ghost()
        .small()
        .icon(IconName::Ellipsis)
        .tooltip(i18n::text(cx, "menu.conversation_actions"))
        .when_some(hover_group, |button, group| {
            button
                .opacity(0.0)
                .group_hover(group, |style| style.opacity(1.0))
        })
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _window, cx| {
            let pin_view = pin_view.clone();
            let rename_view = rename_view.clone();
            let delete_view = delete_view.clone();
            let pin_id = pin_id.clone();
            let rename_id = rename_id.clone();
            let delete_id = delete_id.clone();
            let menu = menu.min_w(px(178.0));
            let menu = if let Some(pinned) = pinned {
                menu.item(
                    PopupMenuItem::new(if pinned {
                        i18n::text(cx, "menu.unpin")
                    } else {
                        i18n::text(cx, "menu.pin")
                    })
                    .icon(Icon::default().path("icons/pin.svg"))
                    .on_click(move |_, _, cx| {
                        diagnostics::record(
                            DiagnosticLevel::Info,
                            "conversation.action",
                            format!("Pin menu click received for conversation {pin_id}."),
                        );
                        let pin_view = pin_view.clone();
                        let pin_id = pin_id.clone();
                        if let Err(error) = pin_view.update(cx, |app, cx| {
                            app.set_conversation_pinned(&pin_id, !pinned, cx);
                        }) {
                            diagnostics::record(
                                DiagnosticLevel::Error,
                                "conversation.action",
                                format!("Pin action could not reach the app: {error}"),
                            );
                        }
                    }),
                )
            } else {
                menu
            };
            let rename_view = rename_view.clone();
            let delete_view = delete_view.clone();
            menu.item(
                PopupMenuItem::new(i18n::text(cx, "menu.rename"))
                    .icon(Icon::default().path("icons/pencil.svg"))
                    .on_click(move |_, window, cx| {
                        diagnostics::record(
                            DiagnosticLevel::Info,
                            "conversation.action",
                            format!("Rename menu click received for conversation {rename_id}."),
                        );
                        if let Err(error) = rename_view.update(cx, |app, cx| {
                            app.open_rename_conversation(&rename_id, window, cx)
                        }) {
                            diagnostics::record(
                                DiagnosticLevel::Error,
                                "conversation.action",
                                format!("Rename action could not reach the app: {error}"),
                            );
                        }
                    }),
            )
            .item(
                PopupMenuItem::new(i18n::text(cx, "menu.delete_chat"))
                    .icon(Icon::default().path("icons/trash.svg"))
                    .on_click(move |_, window, cx| {
                        diagnostics::record(
                            DiagnosticLevel::Info,
                            "conversation.action",
                            format!("Delete menu click received for conversation {delete_id}."),
                        );
                        if let Err(error) = delete_view.update(cx, |app, cx| {
                            app.open_delete_conversation(&delete_id, window, cx)
                        }) {
                            diagnostics::record(
                                DiagnosticLevel::Error,
                                "conversation.action",
                                format!("Delete action could not reach the app: {error}"),
                            );
                        }
                    }),
            )
        });
    div()
        .id(SharedString::from(wrapper_id))
        .flex_none()
        .relative()
        .size(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .when(processing || unread, |wrapper| wrapper.child(status))
        .child(trigger)
        .into_any_element()
}

fn workspace_choices(projects: &[WorkProject]) -> Vec<WorkspaceChoice> {
    let mut choices = vec![WorkspaceChoice {
        id: None,
        name: "No workspace".into(),
        root: PathBuf::new(),
    }];
    choices.extend(projects.iter().map(|project| WorkspaceChoice {
        id: Some(project.id.clone()),
        name: project.name.clone().into(),
        root: project.root.clone(),
    }));
    choices
}

fn initial_model_choices(runtime: &AppRuntime) -> Vec<ModelChoice> {
    runtime
        .connections()
        .into_iter()
        .flat_map(|profile| {
            let connection_id = profile.id.clone();
            let connection_name: SharedString = profile.name.clone().into();
            runtime
                .models_for_connection(&profile.id)
                .unwrap_or_default()
                .into_iter()
                .filter(|info| info.capabilities.chat)
                .map(move |info| ModelChoice {
                    connection_id: connection_id.clone(),
                    connection_name: connection_name.clone(),
                    info,
                })
        })
        .collect()
}

fn agent_model_choices(
    choices: &[ModelChoice],
    connection_id: Option<&ConnectionId>,
) -> Vec<(SharedString, String)> {
    let Some(connection_id) = connection_id else {
        return Vec::new();
    };

    choices
        .iter()
        .filter(|choice| &choice.connection_id == connection_id && choice.info.capabilities.chat)
        .map(|choice| {
            let model = &choice.info;
            let label = if model.display_name.trim().is_empty() || model.display_name == model.id {
                model.id.clone()
            } else {
                format!("{} · {}", model.display_name, model.id)
            };
            (label.into(), model.id.clone())
        })
        .collect()
}

fn embedding_model_choices(
    runtime: &AppRuntime,
    connection_id: Option<&ConnectionId>,
) -> Vec<(SharedString, String)> {
    let Some(connection_id) = connection_id else {
        return Vec::new();
    };
    runtime
        .embedding_models_for_connection(connection_id)
        .unwrap_or_default()
        .into_iter()
        .map(|model| {
            let label = if model.display_name == model.id {
                model.id.clone()
            } else {
                format!("{} · {}", model.display_name, model.id)
            };
            (label.into(), model.id)
        })
        .collect()
}

fn grouped_model_items(choices: &[ModelChoice]) -> SearchableVec<SelectGroup<ModelChoice>> {
    let mut groups: Vec<(ConnectionId, SelectGroup<ModelChoice>)> = Vec::new();
    for choice in choices {
        if let Some((_, group)) = groups
            .iter_mut()
            .find(|(connection_id, _)| connection_id == &choice.connection_id)
        {
            group.items.push(choice.clone());
        } else {
            groups.push((
                choice.connection_id.clone(),
                SelectGroup::new(choice.connection_name.clone()).item(choice.clone()),
            ));
        }
    }
    SearchableVec::new(
        groups
            .into_iter()
            .map(|(_, group)| group)
            .collect::<Vec<_>>(),
    )
}

fn preferred_reasoning_effort(model: &ModelInfo) -> Option<String> {
    if model
        .available_reasoning_efforts
        .iter()
        .any(|effort| effort.eq_ignore_ascii_case("max"))
    {
        Some("max".into())
    } else {
        model.default_reasoning_effort.clone()
    }
}

fn localized_reasoning_effort_label(cx: &App, effort: &str) -> SharedString {
    let key = match effort.to_ascii_lowercase().as_str() {
        "none" => "reasoning.none",
        "low" => "reasoning.low",
        "medium" => "reasoning.medium",
        "high" => "reasoning.high",
        "xhigh" => "reasoning.xhigh",
        "max" => "reasoning.max",
        _ => "reasoning.auto",
    };
    i18n::text(cx, key)
}

fn reasoning_effort_from_label(label: &str) -> Option<String> {
    match label.to_ascii_lowercase().as_str() {
        "auto" | "automatico" | "automático" => None,
        "none" | "ninguno" => Some("none".into()),
        "low" | "bajo" => Some("low".into()),
        "medium" | "medio" => Some("medium".into()),
        "high" | "alto" => Some("high".into()),
        "xhigh" | "muy alto" => Some("xhigh".into()),
        "max" | "máximo" => Some("max".into()),
        _ => None,
    }
}

fn ensure_binding_tools(binding: &mut SessionBinding, default_tools: &[String]) -> bool {
    if binding.tools.is_empty() {
        binding.tools = default_tools.to_vec();
        return true;
    }
    false
}

fn apply_enabled_tools(binding: &mut SessionBinding, enabled_tools: Vec<String>) -> bool {
    if binding.tools == enabled_tools {
        return false;
    }
    binding.tools = enabled_tools;
    true
}

fn conversation_has_unread_update(
    route: Route,
    active_session: &SessionId,
    updated_session: &SessionId,
) -> bool {
    route != Route::Chat || active_session != updated_session
}

fn inherited_session_binding(
    active: &SessionBinding,
    remembered: &SessionBinding,
    default_tools: &[String],
) -> SessionBinding {
    let mut inherited = if active.is_ready() {
        active.clone()
    } else {
        remembered.clone()
    };
    if !active.tools.is_empty() {
        inherited.tools = active.tools.clone();
    }
    ensure_binding_tools(&mut inherited, default_tools);
    inherited
}

fn shell_messages_to_agent_history(messages: &[ShellMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::User => Role::User,
                MessageRole::Assistant if !message.text.is_empty() => Role::Assistant,
                MessageRole::Assistant | MessageRole::Error => return None,
            };
            Some(ChatMessage {
                role,
                content: MessageContent::Text(message.text.clone()),
                tool_call_id: None,
                tool_calls: None,
            })
        })
        .collect()
}

fn group_conversations_by_workspace(
    conversations: &[ConversationSummary],
    projects: &[WorkProject],
) -> (
    Vec<ConversationSummary>,
    HashMap<String, Vec<ConversationSummary>>,
) {
    let known_projects = projects
        .iter()
        .map(|project| project.id.as_str())
        .collect::<HashSet<_>>();
    let mut global = Vec::new();
    let mut by_workspace: HashMap<String, Vec<ConversationSummary>> = HashMap::new();
    for conversation in conversations.iter().cloned() {
        match conversation.project_id.as_deref() {
            Some(project_id) if known_projects.contains(project_id) => {
                by_workspace
                    .entry(project_id.to_string())
                    .or_default()
                    .push(conversation);
            }
            _ => global.push(conversation),
        }
    }
    sort_conversation_summaries(&mut global);
    for conversations in by_workspace.values_mut() {
        sort_conversation_summaries(conversations);
    }
    (global, by_workspace)
}

fn sort_conversation_summaries(conversations: &mut [ConversationSummary]) {
    conversations.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| {
                left.title
                    .to_ascii_lowercase()
                    .cmp(&right.title.to_ascii_lowercase())
            })
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn sidebar_heading(label: impl Into<SharedString>, theme: UiTheme, top: f32) -> AnyElement {
    let label = label.into();
    div()
        .px(px(9.0))
        .pt(px(top))
        .pb(px(7.0))
        .text_size(px(13.0))
        .text_color(theme.faint)
        .child(label)
        .into_any_element()
}

fn format_context_tokens(tokens: Option<u64>) -> String {
    tokens
        .map(|tokens| tokens.to_string())
        .unwrap_or_else(|| "—".into())
}

fn format_context_limit(limit: u64) -> String {
    if limit == 0 {
        "—".into()
    } else {
        limit.to_string()
    }
}

fn format_tool_input(input: &serde_json::Value) -> String {
    serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string())
}

fn tool_display_name(name: &str) -> String {
    match name {
        "bash" | "shell" | "terminal" => "Shell".into(),
        "file_read" | "read_file" => "Read file".into(),
        "file_write" | "write_file" => "Write file".into(),
        "glob" | "find_files" => "Find files".into(),
        "grep" => "Search files".into(),
        "web_search_intrernal" => "Search web".into(),
        "web_fetch" => "Open web page".into(),
        "checkpoint" => "Checkpoint".into(),
        "task_list" => "List tasks".into(),
        "add_task" => "Add task".into(),
        "mark_task_as_done" => "Complete task".into(),
        "ask_user" => "Ask user".into(),
        "list_tools" => "List tools".into(),
        "list_skills" => "List skills".into(),
        "load_skill" => "Load skill".into(),
        "list_agents" => "List agents".into(),
        "call_agents" | "call_agent" => "Call agent".into(),
        "compact_conversation" => "Compact conversation".into(),
        _ => name
            .split(['_', '-'])
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn localized_tool_display_name(cx: &App, name: &str) -> SharedString {
    let key = match name {
        "bash" | "shell" | "terminal" => Some("tool.shell"),
        "file_read" | "read_file" => Some("tool.read_file"),
        "file_write" | "write_file" => Some("tool.write_file"),
        "glob" | "find_files" => Some("tool.find_files"),
        "grep" => Some("tool.search_files"),
        "web_search_intrernal" => Some("tool.search_web"),
        "web_fetch" => Some("tool.open_web_page"),
        "checkpoint" => Some("tool.checkpoint"),
        "task_list" => Some("tool.list_tasks"),
        "add_task" => Some("tool.add_task"),
        "mark_task_as_done" => Some("tool.complete_task"),
        "ask_user" => Some("tool.ask_user"),
        "list_tools" => Some("tool.list_tools"),
        "list_skills" => Some("tool.list_skills"),
        "load_skill" => Some("tool.load_skill"),
        "list_agents" => Some("tool.list_agents"),
        "call_agents" | "call_agent" => Some("tool.call_agent"),
        "compact_conversation" => Some("tool.compact_conversation"),
        _ => None,
    };
    key.map(|key| i18n::text(cx, key))
        .unwrap_or_else(|| SharedString::new(tool_display_name(name)))
}

fn localized_tool_activity_state_label(cx: &App, state: ToolActivityState) -> SharedString {
    let key = match state {
        ToolActivityState::Running => "tool.running",
        ToolActivityState::Completed => "tool.done",
        ToolActivityState::Failed => "tool.failed",
    };
    i18n::text(cx, key)
}

fn tool_activity_state_color(state: ToolActivityState, theme: UiTheme) -> gpui::Rgba {
    match state {
        ToolActivityState::Running => theme.warning,
        ToolActivityState::Completed => theme.success,
        ToolActivityState::Failed => theme.destructive,
    }
}

fn format_tool_duration(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        format!("{milliseconds}ms")
    } else {
        format!("{:.1}s", milliseconds as f32 / 1_000.0)
    }
}

fn normalize_tool_sources(sources: Vec<WorkSource>) -> Vec<WorkSource> {
    let mut grouped = HashMap::<String, WorkSource>::new();
    for source in sources {
        if source.kind.trim().is_empty() {
            continue;
        }
        let kind = source.kind.clone();
        let grouping_key = source
            .url
            .as_ref()
            .map(|url| format!("web:{url}"))
            .unwrap_or_else(|| format!("tool:{kind}"));
        let entry = grouped.entry(grouping_key).or_insert_with(|| WorkSource {
            key: source.key.clone(),
            kind: kind.clone(),
            label: source.label.clone(),
            url: source.url.clone(),
            title: source.title.clone(),
            detail: source.detail.clone(),
            count: 0,
            last_used_at: source.last_used_at,
        });
        if entry.url.is_none() {
            entry.url = source.url.clone();
        }
        if entry.title.is_none() {
            entry.title = source.title.clone();
        }
        if entry.detail.is_none() {
            entry.detail = source.detail.clone();
        }
        if entry.label.is_empty() {
            entry.label = source.label.clone();
        }
        entry.count = entry.count.saturating_add(source.count);
        entry.last_used_at = entry.last_used_at.max(source.last_used_at);
    }
    let mut sources = grouped.into_values().collect::<Vec<_>>();
    sources.sort_by(|left, right| right.last_used_at.cmp(&left.last_used_at));
    sources
}

fn normalize_web_source_url(raw_url: &str) -> Option<String> {
    let url = raw_url.trim();
    let (scheme, _) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    if url.chars().any(char::is_whitespace) {
        return None;
    }
    Some(url.to_string())
}

fn agent_thread_status_label(status: AgentThreadStatus) -> &'static str {
    match status {
        AgentThreadStatus::Running => "Running",
        AgentThreadStatus::Completed => "Done",
        AgentThreadStatus::Failed => "Failed",
    }
}

fn agent_thread_status_color(status: AgentThreadStatus, theme: UiTheme) -> gpui::Rgba {
    match status {
        AgentThreadStatus::Running => theme.warning,
        AgentThreadStatus::Completed => theme.success,
        AgentThreadStatus::Failed => theme.destructive,
    }
}

fn flatten_background<T>(
    result: Result<Result<T, crate::runtime::RuntimeError>, tokio::task::JoinError>,
) -> Result<T, crate::runtime::RuntimeError> {
    result.unwrap_or_else(|error| Err(crate::runtime::RuntimeError::Runtime(error.to_string())))
}

const MAX_ATTACHMENT_BYTES: u64 = 512 * 1024;
const MAX_ATTACHMENT_TOTAL_BYTES: usize = 1_024 * 1_024;

fn composer_attachment_name(path: &PathBuf) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn composer_message_label(text: &str, attachments: &[ComposerAttachment]) -> String {
    if attachments.is_empty() {
        return text.to_owned();
    }
    let names = attachments
        .iter()
        .map(|attachment| composer_attachment_name(&attachment.path))
        .collect::<Vec<_>>()
        .join(", ");
    if text.trim().is_empty() {
        format!("Attached files: {names}")
    } else {
        format!("{text}\n\nAttached files: {names}")
    }
}

async fn load_attachment_context(
    mut text: String,
    paths: Vec<PathBuf>,
) -> Result<String, anyhow::Error> {
    let mut total_bytes = 0usize;
    for path in paths {
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(anyhow::anyhow!("{} is not a file", path.display()));
        }
        if metadata.len() > MAX_ATTACHMENT_BYTES {
            return Err(anyhow::anyhow!(
                "{} is larger than the 512 KB attachment limit",
                path.display()
            ));
        }

        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))?;
        total_bytes = total_bytes.saturating_add(bytes.len());
        if total_bytes > MAX_ATTACHMENT_TOTAL_BYTES {
            return Err(anyhow::anyhow!("attachments exceed the 1 MB total limit"));
        }
        let content = String::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!("{} is not a UTF-8 text file", path.display()))?;
        text.push_str("\n\n--- Attached file: ");
        text.push_str(&path.to_string_lossy());
        text.push_str(" ---\n");
        text.push_str(&content);
        text.push_str("\n--- End attached file ---");
    }
    Ok(text)
}

fn sidebar_empty(label: impl Into<SharedString>, theme: UiTheme) -> AnyElement {
    let label = label.into();
    div()
        .px(px(9.0))
        .pb(px(8.0))
        .text_size(px(12.0))
        .text_color(theme.faint)
        .child(label)
        .into_any_element()
}

fn tool_activity_groups(activities: &[ToolActivity]) -> Vec<(usize, Vec<usize>)> {
    let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
    for (activity_index, activity) in activities.iter().enumerate() {
        if activity.inside_reasoning {
            continue;
        }
        // Activities created by the live stream always have a group. The
        // fallback keeps older in-memory messages renderable during a hot
        // reload without ever mixing them with reasoning tools.
        let group_id = activity.group_id.unwrap_or(activity_index);
        if let Some((_, indexes)) = groups.iter_mut().find(|(id, _)| *id == group_id) {
            indexes.push(activity_index);
        } else {
            groups.push((group_id, vec![activity_index]));
        }
    }
    groups
}

fn render_tool_group(
    session_id: &SessionId,
    message_index: usize,
    group_id: usize,
    activity_indices: &[usize],
    activities: &[ToolActivity],
    active_group_id: Option<usize>,
    expanded: bool,
    theme: UiTheme,
    cx: &mut Context<AverroesApp>,
) -> AnyElement {
    let should_show_all = expanded || active_group_id != Some(group_id);
    if !should_show_all {
        let activity_index = *activity_indices.last().expect("tool group is non-empty");
        return render_tool_activity(
            session_id,
            message_index,
            std::slice::from_ref(&activities[activity_index]),
            activity_index,
            theme,
            cx,
        );
    }

    if expanded {
        return div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .children(activity_indices.iter().map(|activity_index| {
                render_tool_activity(
                    session_id,
                    message_index,
                    std::slice::from_ref(&activities[*activity_index]),
                    *activity_index,
                    theme,
                    cx,
                )
            }))
            .into_any_element();
    }

    render_collapsed_tool_group(
        session_id,
        message_index,
        group_id,
        activity_indices,
        activities,
        theme,
        cx,
    )
}

fn render_collapsed_tool_group(
    session_id: &SessionId,
    message_index: usize,
    group_id: usize,
    activity_indices: &[usize],
    activities: &[ToolActivity],
    theme: UiTheme,
    cx: &mut Context<AverroesApp>,
) -> AnyElement {
    let names = activity_indices
        .iter()
        .map(|index| activities[*index].name.as_str())
        .collect::<Vec<_>>();
    let name_counts = summarize_tool_names(&names);
    let name_summary = name_counts
        .iter()
        .map(|item| {
            let label = localized_tool_display_name(cx, &item.name);
            if item.count == 1 {
                label.to_string()
            } else {
                format!("{} ×{}", label, item.count)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let group_title = i18n::format(
        cx,
        "chat.tool_group",
        &[("count", activity_indices.len().to_string())],
    );
    let group_id_string = format!(
        "tool-group-{}-{message_index}-{group_id}",
        session_id.as_str()
    );
    let toggle_session_id = session_id.clone();
    let status = activity_indices
        .iter()
        .map(|index| activities[*index].state)
        .find(|state| *state == ToolActivityState::Failed)
        .or_else(|| {
            activity_indices
                .iter()
                .map(|index| activities[*index].state)
                .find(|state| *state == ToolActivityState::Running)
        })
        .unwrap_or(ToolActivityState::Completed);

    div()
        .id(SharedString::from(group_id_string.clone()))
        .flex()
        .items_center()
        .gap(px(8.0))
        .w_full()
        .p(px(9.0))
        .rounded(px(10.0))
        .bg(theme.surface_subtle)
        .hover(|style| style.bg(theme.surface_hover))
        .cursor_pointer()
        .on_click(cx.listener(move |app, _, _, cx| {
            app.toggle_tool_group(&toggle_session_id, message_index, group_id, cx);
        }))
        .child(tool_icon(&activities[activity_indices[0]].name, 15.0).text_color(theme.muted))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.foreground)
                        .child(group_title),
                )
                .child(
                    div()
                        .min_w(px(0.0))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(px(11.0))
                        .text_color(theme.muted)
                        .child(name_summary),
                ),
        )
        .child(
            div()
                .text_size(px(10.0))
                .text_color(tool_activity_state_color(status, theme))
                .child(localized_tool_activity_state_label(cx, status)),
        )
        .child(
            Icon::new(IconName::ChevronRight)
                .size(px(13.0))
                .text_color(theme.faint),
        )
        .into_any_element()
}

fn render_tool_activity(
    session_id: &SessionId,
    message_index: usize,
    activities: &[ToolActivity],
    activity_index_offset: usize,
    theme: UiTheme,
    cx: &mut Context<AverroesApp>,
) -> AnyElement {
    let rows = activities
        .iter()
        .enumerate()
        .map(|(relative_index, activity)| {
            let activity_index = activity_index_offset + relative_index;
            let activity_id = format!(
                "tool-activity-{}-{message_index}-{activity_index}",
                session_id.as_str()
            );
            let toggle_session_id = session_id.clone();
            let agent_parent_session_id = session_id.clone();
            let opens_agent = matches!(activity.name.as_str(), "call_agents" | "call_agent");
            let expanded = activity.expanded;
            let state_label = localized_tool_activity_state_label(cx, activity.state);
            let state_color = tool_activity_state_color(activity.state, theme);
            let duration = activity
                .duration_ms
                .map(format_tool_duration)
                .unwrap_or_else(|| i18n::text(cx, "tool.running").to_string());
            let input = activity.input.clone();
            let output = if activity.output.is_empty() {
                activity.summary.clone()
            } else {
                activity.output.clone()
            };
            let agent_tool_output = output.clone();
            let details = if expanded {
                Some(
                    div()
                        .mt(px(9.0))
                        .pl(px(23.0))
                        .flex()
                        .flex_col()
                        .gap(px(7.0))
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(theme.faint)
                                .child(i18n::text(cx, "tool.arguments")),
                        )
                        .child(
                            div()
                                .max_h(px(180.0))
                                .overflow_y_scrollbar()
                                .font(UiTheme::mono_font())
                                .text_size(px(11.0))
                                .text_color(theme.muted)
                                .whitespace_normal()
                                .child(input),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(theme.faint)
                                .child(i18n::text(cx, "tool.result")),
                        )
                        .child(
                            div()
                                .max_h(px(220.0))
                                .overflow_y_scrollbar()
                                .font(UiTheme::mono_font())
                                .text_size(px(11.0))
                                .text_color(if activity.state == ToolActivityState::Failed {
                                    theme.destructive
                                } else {
                                    theme.muted
                                })
                                .whitespace_normal()
                                .child(if output.is_empty() {
                                    i18n::text(cx, "tool.no_output").to_string()
                                } else {
                                    output.clone()
                                }),
                        ),
                )
            } else {
                None
            };
            div()
                .id(SharedString::from(activity_id.clone()))
                .flex()
                .flex_col()
                .p(px(9.0))
                .rounded(px(10.0))
                .bg(theme.surface_subtle)
                .hover(|style| style.bg(theme.surface_hover))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(tool_icon(&activity.name, 15.0).text_color(theme.muted))
                        .child(
                            div()
                                .id(SharedString::from(format!("{activity_id}-agent")))
                                .flex_1()
                                .min_w(px(0.0))
                                .text_size(px(12.0))
                                .text_color(theme.foreground)
                                .child(localized_tool_display_name(cx, &activity.name))
                                .when(opens_agent, |this| {
                                    this.cursor_pointer().on_click(cx.listener(
                                        move |app, _, _, cx| {
                                            app.open_agent_thread_for_tool(
                                                &agent_parent_session_id,
                                                &agent_tool_output,
                                                cx,
                                            );
                                        },
                                    ))
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(state_color)
                                .child(state_label),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(theme.faint)
                                .child(duration),
                        )
                        .child(
                            Button::new(format!("{activity_id}-toggle"))
                                .ghost()
                                .small()
                                .icon(if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .tooltip(if expanded {
                                    i18n::text(cx, "chat.hide_tool_details")
                                } else {
                                    i18n::text(cx, "chat.show_tool_details")
                                })
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    app.toggle_tool_activity(
                                        &toggle_session_id,
                                        message_index,
                                        activity_index,
                                        cx,
                                    );
                                })),
                        ),
                )
                .when_some(details, |this, details| this.child(details))
                .into_any_element()
        })
        .collect::<Vec<_>>();

    div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .children(rows)
        .into_any_element()
}

fn render_agent_thread_transcript(
    thread_id: &str,
    messages: &[ShellMessage],
    streaming: bool,
    theme: UiTheme,
    cx: &mut Context<AverroesApp>,
) -> AnyElement {
    let rows = messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let message_id = format!("agent-thread-{thread_id}-{index}");
            if message.role == MessageRole::User {
                return div()
                    .flex()
                    .justify_end()
                    .child(
                        div()
                            .max_w(px(245.0))
                            .px(px(10.0))
                            .py(px(8.0))
                            .rounded(px(10.0))
                            .bg(theme.surface_subtle)
                            .child(
                                TextView::markdown(message_id, message.text.clone())
                                    .selectable(true),
                            ),
                    )
                    .into_any_element();
            }

            if streaming
                && message.role == MessageRole::Assistant
                && message.text.is_empty()
                && message.reasoning.is_empty()
                && message.tool_activities.is_empty()
            {
                return div()
                    .flex()
                    .items_center()
                    .child(render_activity_indicator(
                        format!("agent-thread-waiting-{thread_id}-{index}"),
                        theme,
                        4.0,
                    ))
                    .into_any_element();
            }

            let reasoning = if message.reasoning.is_empty() {
                None
            } else {
                let reasoning = normalize_reasoning_for_display(&message.reasoning);
                let reasoning_content = if message.reasoning_complete {
                    render_markdown(theme, &reasoning)
                } else {
                    render_streaming_markdown(theme, &reasoning)
                };
                let reasoning_tools = message
                    .tool_activities
                    .iter()
                    .enumerate()
                    .filter(|(_, activity)| activity.inside_reasoning)
                    .map(|(tool_index, activity)| {
                        render_agent_thread_tool_activity(
                            &message_id,
                            tool_index,
                            activity,
                            theme,
                            cx,
                        )
                    })
                    .collect::<Vec<_>>();
                Some(
                    div()
                        .p(px(8.0))
                        .rounded(px(9.0))
                        .bg(theme.surface_subtle)
                        .text_size(px(11.0))
                        .text_color(theme.muted)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(7.0))
                                .child(reasoning_content)
                                .children(reasoning_tools),
                        ),
                )
            };
            let text = message.text.as_str();
            let mut content = Vec::with_capacity(message.tool_activities.len() * 2 + 1);
            let mut cursor = 0usize;
            for (tool_index, activity) in message.tool_activities.iter().enumerate() {
                if activity.inside_reasoning {
                    continue;
                }
                let offset = activity.text_offset.min(text.len());
                let offset = if text.is_char_boundary(offset) {
                    offset.max(cursor)
                } else {
                    cursor
                };
                if let Some(segment) = text
                    .get(cursor..offset)
                    .filter(|segment| !segment.is_empty())
                {
                    content.push(
                        TextView::markdown(
                            format!("{message_id}-segment-{tool_index}"),
                            segment.to_owned(),
                        )
                        .selectable(true)
                        .into_any_element(),
                    );
                }
                content.push(render_agent_thread_tool_activity(
                    &message_id,
                    tool_index,
                    activity,
                    theme,
                    cx,
                ));
                cursor = offset;
            }
            if let Some(segment) = text.get(cursor..).filter(|segment| !segment.is_empty()) {
                content.push(
                    TextView::markdown(format!("{message_id}-tail"), segment.to_owned())
                        .selectable(true)
                        .into_any_element(),
                );
            }

            div()
                .flex()
                .flex_col()
                .gap(px(7.0))
                .when_some(reasoning, |this, reasoning| this.child(reasoning))
                .children(content)
                .into_any_element()
        })
        .collect::<Vec<_>>();

    div()
        .mt(px(6.0))
        .flex()
        .flex_col()
        .gap(px(9.0))
        .children(rows)
        .into_any_element()
}

fn render_agent_thread_tool_activity(
    message_id: &str,
    tool_index: usize,
    activity: &ToolActivity,
    theme: UiTheme,
    cx: &mut Context<AverroesApp>,
) -> AnyElement {
    let output = if activity.output.is_empty() {
        activity.summary.clone()
    } else {
        activity.output.clone()
    };
    div()
        .p(px(8.0))
        .rounded(px(9.0))
        .bg(theme.surface_subtle)
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(tool_icon(&activity.name, 14.0).text_color(theme.muted))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(px(11.0))
                        .child(localized_tool_display_name(cx, &activity.name)),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(tool_activity_state_color(activity.state, theme))
                        .child(localized_tool_activity_state_label(cx, activity.state)),
                ),
        )
        .when(!activity.input.is_empty(), |this| {
            this.child(
                div()
                    .id(SharedString::from(format!(
                        "{message_id}-tool-{tool_index}-input"
                    )))
                    .max_h(px(110.0))
                    .overflow_y_scrollbar()
                    .font(UiTheme::mono_font())
                    .text_size(px(10.0))
                    .text_color(theme.faint)
                    .child(activity.input.clone()),
            )
        })
        .when(!output.is_empty(), |this| {
            this.child(
                div()
                    .id(SharedString::from(format!(
                        "{message_id}-tool-{tool_index}-output"
                    )))
                    .max_h(px(150.0))
                    .overflow_y_scrollbar()
                    .font(UiTheme::mono_font())
                    .text_size(px(10.0))
                    .text_color(if activity.state == ToolActivityState::Failed {
                        theme.destructive
                    } else {
                        theme.muted
                    })
                    .child(output),
            )
        })
        .into_any_element()
}

fn render_activity_indicator(id: String, theme: UiTheme, dot_size: f32) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(3.0))
        .children((0..3).map(move |index| {
            let animation_id = format!("{id}-{index}");
            div()
                .size(px(dot_size))
                .rounded_full()
                .bg(theme.muted)
                .with_animation(
                    animation_id,
                    Animation::new(Duration::from_millis(900)).repeat(),
                    move |dot, delta| {
                        let phase = (delta + index as f32 / 3.0) % 1.0;
                        let wave = if phase < 0.5 {
                            phase * 2.0
                        } else {
                            (1.0 - phase) * 2.0
                        };
                        dot.opacity(0.3 + wave * 0.7)
                    },
                )
                .into_any_element()
        }))
        .into_any_element()
}

fn render_source_tool_chip(
    session_id: &SessionId,
    source: &WorkSource,
    theme: UiTheme,
    cx: &mut Context<AverroesApp>,
) -> AnyElement {
    let opens_agent = matches!(source.kind.as_str(), "call_agents" | "call_agent");
    let session_id = session_id.clone();
    div()
        .id(SharedString::from(format!(
            "source-tool-{}-{}",
            session_id.as_str(),
            source.key
        )))
        .flex()
        .items_center()
        .gap(px(8.0))
        .w_full()
        .h(px(31.0))
        .px(px(7.0))
        .rounded(px(7.0))
        .text_size(px(12.0))
        .text_color(theme.muted)
        .hover(|style| style.bg(theme.surface_hover))
        .child(tool_icon(&source.kind, 14.0).text_color(theme.faint))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .whitespace_nowrap()
                .text_ellipsis()
                .child(localized_tool_display_name(cx, &source.kind)),
        )
        .when(source.count > 1, |this| {
            this.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.faint)
                    .child(format!("×{}", source.count)),
            )
        })
        .when(opens_agent, |this| {
            this.cursor_pointer()
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.open_latest_agent_thread(&session_id, cx);
                }))
        })
        .into_any_element()
}

fn render_source_row(source: &WorkSource, theme: UiTheme) -> AnyElement {
    let title = source_display_name(source);
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .w_full()
        .h(px(32.0))
        .px(px(7.0))
        .rounded(px(7.0))
        .hover(|style| style.bg(theme.surface_hover))
        .child(
            div()
                .flex_none()
                .size(px(18.0))
                .rounded(px(4.0))
                .flex()
                .items_center()
                .justify_center()
                .child(render_source_icon(source, 15.0, theme)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .text_size(px(12.0))
                .text_color(theme.muted)
                .child(title),
        )
        .when(source.count > 1, |this| {
            this.child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(theme.faint)
                    .child(format!("×{}", source.count)),
            )
        })
        .into_any_element()
}

fn render_source_summary(
    source_list: &[WorkSource],
    theme: UiTheme,
    cx: &mut Context<AverroesApp>,
) -> Option<AnyElement> {
    let mut sources = Vec::new();
    for source in source_list {
        if !sources
            .iter()
            .any(|existing: &&WorkSource| existing.key == source.key)
        {
            sources.push(source);
        }
    }
    if sources.is_empty() {
        return None;
    }

    let icon_count = sources.len().min(4);
    let icons = sources
        .iter()
        .take(icon_count)
        .map(|source| {
            div()
                .size(px(17.0))
                .rounded_full()
                .bg(theme.surface)
                .flex()
                .items_center()
                .justify_center()
                .child(render_source_icon(source, 11.0, theme))
                .into_any_element()
        })
        .collect::<Vec<_>>();
    let remaining = sources.len().saturating_sub(icon_count);

    Some(
        div()
            .flex()
            .items_center()
            .gap(px(5.0))
            .h(px(26.0))
            .px(px(7.0))
            .rounded(px(7.0))
            .bg(theme.surface_subtle)
            .child(div().flex().items_center().gap(px(2.0)).children(icons))
            .when(remaining > 0, |this| {
                this.child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.faint)
                        .child(format!("+{remaining}")),
                )
            })
            .child(
                div()
                    .text_size(px(10.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted)
                    .child(i18n::text(cx, "chat.sources")),
            )
            .into_any_element(),
    )
}

fn source_host(url: &str) -> Option<String> {
    let (_, remainder) = url.split_once("://")?;
    let authority = remainder.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?.trim();
    (!host.is_empty()).then(|| host.to_string())
}

fn source_favicon_url(source: &WorkSource) -> Option<String> {
    let url = source.url.as_deref()?;
    if let Some(declared) = source.detail.as_deref().and_then(normalize_web_source_url) {
        return Some(declared);
    }
    let host = source_host(url)?;
    let scheme = url
        .split_once("://")
        .map(|(scheme, _)| scheme)
        .filter(|scheme| scheme.eq_ignore_ascii_case("http"))
        .unwrap_or("https");
    Some(format!("{scheme}://{host}/favicon.ico"))
}

fn source_display_name(source: &WorkSource) -> String {
    source
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| source.url.as_deref().and_then(source_host))
        .unwrap_or_else(|| {
            if source.label.is_empty() {
                tool_display_name(&source.kind)
            } else {
                source.label.clone()
            }
        })
}

fn source_initial(source: &WorkSource) -> String {
    source_display_name(source)
        .chars()
        .find(|character| character.is_alphanumeric())
        .map(|character| character.to_uppercase().collect())
        .unwrap_or_else(|| "?".into())
}

fn source_icon_fallback(size: f32, initial: String, theme: UiTheme) -> AnyElement {
    div()
        .flex_none()
        .size(px(size))
        .rounded_full()
        .bg(theme.surface_hover)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px((size * 0.62).max(8.0)))
        .text_color(theme.muted)
        .child(initial)
        .into_any_element()
}

fn render_source_icon(source: &WorkSource, size: f32, theme: UiTheme) -> AnyElement {
    let initial = source_initial(source);
    if let Some(favicon_url) = source_favicon_url(source) {
        let loading_initial = initial.clone();
        return img(favicon_url)
            .flex_none()
            .size(px(size))
            .with_loading(move || source_icon_fallback(size, loading_initial.clone(), theme))
            .with_fallback(move || source_icon_fallback(size, initial.clone(), theme))
            .into_any_element();
    }
    tool_icon(&source.kind, size)
        .text_color(theme.muted)
        .into_any_element()
}

fn render_message_source_summary(
    activities: &[ToolActivity],
    theme: UiTheme,
    cx: &mut Context<AverroesApp>,
) -> Option<AnyElement> {
    let sources = activities
        .iter()
        .map(|activity| WorkSource {
            key: activity.name.clone(),
            kind: activity.name.clone(),
            label: localized_tool_display_name(cx, &activity.name).to_string(),
            url: None,
            title: None,
            detail: None,
            count: 1,
            last_used_at: 0,
        })
        .collect::<Vec<_>>();
    render_source_summary(&sources, theme, cx)
}

fn render_conversation_source_summary(
    sources: &[WorkSource],
    theme: UiTheme,
    cx: &mut Context<AverroesApp>,
) -> Option<AnyElement> {
    render_source_summary(sources, theme, cx)
}

fn render_user_question(
    session_id: &SessionId,
    question: &averroes_core::tool::builtin::ask_user::UserQuestion,
    input: &Entity<InputState>,
    theme: UiTheme,
    cx: &mut Context<AverroesApp>,
) -> AnyElement {
    let option_buttons = question
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let session_id = session_id.clone();
            let answer = option.clone();
            Button::new(format!("user-question-{}-{index}", question.id))
                .outline()
                .small()
                .label(option.clone())
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.answer_user_question(&session_id, answer.clone(), window, cx);
                }))
                .into_any_element()
        })
        .collect::<Vec<_>>();
    let send_session_id = session_id.clone();
    div()
        .p(px(12.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_subtle)
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted)
                .child(i18n::text(cx, "chat.input_needed")),
        )
        .child(
            div()
                .text_size(px(14.0))
                .text_color(theme.foreground)
                .child(question.question.clone()),
        )
        .when(!option_buttons.is_empty(), |this| {
            this.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.0))
                    .children(option_buttons),
            )
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(Input::new(input).flex_1())
                .child(
                    Button::new(format!("send-user-question-{}", question.id))
                        .primary()
                        .small()
                        .label(i18n::text(cx, "composer.send"))
                        .on_click(cx.listener(move |app, _, window, cx| {
                            app.submit_user_question_answer(&send_session_id, window, cx);
                        })),
                ),
        )
        .into_any_element()
}

fn render_assistant_text_segment(
    session_id: &SessionId,
    message_index: usize,
    segment_index: usize,
    text: &str,
    streaming: bool,
    theme: UiTheme,
) -> AnyElement {
    if streaming {
        render_streaming_markdown(theme, text)
            .text_size(px(14.0))
            .into_any_element()
    } else {
        TextView::markdown(
            format!(
                "message-{}-{message_index}-segment-{segment_index}",
                session_id.as_str()
            ),
            text.to_owned(),
        )
        .selectable(true)
        .into_any_element()
    }
}

fn render_message(
    session_id: &SessionId,
    index: usize,
    message: &ShellMessage,
    processing: bool,
    streaming: bool,
    show_tool_activity: bool,
    show_sources: bool,
    conversation_sources: &[WorkSource],
    pending_user_question: Option<&averroes_core::tool::builtin::ask_user::UserQuestion>,
    ask_user_input: &Entity<InputState>,
    theme: UiTheme,
    cx: &mut Context<AverroesApp>,
) -> AnyElement {
    let body = message.text.clone();

    if message.role == MessageRole::User {
        return div()
            .flex()
            .justify_end()
            .child(
                div()
                    .max_w(px(620.0))
                    .px(px(15.0))
                    .py(px(11.0))
                    .rounded(px(13.0))
                    .bg(theme.surface_subtle)
                    .child(
                        TextView::markdown(
                            format!("message-{}-{index}", session_id.as_str()),
                            body,
                        )
                        .selectable(true),
                    ),
            )
            .into_any_element();
    }

    let error = message.role == MessageRole::Error;
    let assistant = message.role == MessageRole::Assistant;
    let copy_text = message.text.clone();
    let copy_disabled = copy_text.is_empty();
    let retry_session_id = session_id.clone();
    let reasoning_element = if message.reasoning.is_empty() {
        None
    } else {
        let reasoning_text = normalize_reasoning_for_display(&message.reasoning);
        let reasoning_expanded = message.reasoning_expanded;
        let reasoning_session_id = session_id.clone();
        let reasoning_content = if message.reasoning_complete {
            render_markdown(theme, &reasoning_text)
        } else {
            render_streaming_markdown(theme, &reasoning_text)
        };
        let reasoning_tools = message
            .tool_activities
            .iter()
            .enumerate()
            .filter(|(_, activity)| activity.inside_reasoning)
            .map(|(activity_index, activity)| {
                render_tool_activity(
                    &reasoning_session_id,
                    index,
                    std::slice::from_ref(activity),
                    activity_index,
                    theme,
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let toggle_reasoning_session_id = reasoning_session_id.clone();
        Some(
            div()
                .id(format!("reasoning-{}-{index}", session_id.as_str()))
                .px(px(12.0))
                .py(px(9.0))
                .rounded(px(10.0))
                .bg(theme.surface_subtle)
                .text_color(theme.muted)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(if message.reasoning_complete {
                            Icon::new(IconName::Check)
                                .size(px(12.0))
                                .text_color(theme.success)
                                .into_any_element()
                        } else {
                            render_activity_indicator(
                                format!("reasoning-{}-{index}", session_id.as_str()),
                                theme,
                                3.0,
                            )
                        })
                        .child(
                            Button::new(format!(
                                "toggle-reasoning-{}-{index}",
                                session_id.as_str()
                            ))
                            .ghost()
                            .small()
                            .icon(if reasoning_expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .label(i18n::text(cx, "chat.reasoning"))
                            .on_click(cx.listener(
                                move |app, _, _, cx| {
                                    app.toggle_reasoning(&toggle_reasoning_session_id, index, cx);
                                },
                            )),
                        ),
                )
                .when(reasoning_expanded, |this| {
                    this.child(
                        div()
                            .mt(px(7.0))
                            .pt(px(7.0))
                            .border_t_1()
                            .border_color(theme.border)
                            .text_size(px(12.0))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(7.0))
                                    .child(reasoning_content)
                                    .children(reasoning_tools),
                            ),
                    )
                }),
        )
    };
    let content_elements = if assistant && show_tool_activity && !message.tool_activities.is_empty()
    {
        let groups = tool_activity_groups(&message.tool_activities);
        let mut elements = Vec::with_capacity(groups.len() * 2 + 1);
        let text = message.text.as_str();
        let mut cursor = 0usize;
        let mut segment_index = 0usize;
        let active_group_id = streaming.then(|| message.active_tool_group()).flatten();

        for (group_id, activity_indices) in groups {
            let first_activity_index = activity_indices[0];
            let offset = message.tool_activities[first_activity_index]
                .text_offset
                .min(text.len());
            let offset = if text.is_char_boundary(offset) {
                offset.max(cursor)
            } else {
                cursor
            };
            if let Some(segment) = text
                .get(cursor..offset)
                .filter(|segment| !segment.is_empty())
            {
                elements.push(render_assistant_text_segment(
                    session_id,
                    index,
                    segment_index,
                    segment,
                    false,
                    theme,
                ));
                segment_index += 1;
            }
            elements.push(render_tool_group(
                session_id,
                index,
                group_id,
                &activity_indices,
                &message.tool_activities,
                active_group_id,
                message.is_tool_group_expanded(group_id),
                theme,
                cx,
            ));
            cursor = offset;
        }

        if let Some(segment) = text.get(cursor..).filter(|segment| !segment.is_empty()) {
            elements.push(render_assistant_text_segment(
                session_id,
                index,
                segment_index,
                segment,
                streaming,
                theme,
            ));
        }
        elements
    } else if assistant && body.is_empty() && streaming {
        vec![render_activity_indicator(
            format!("message-working-{}-{index}", session_id.as_str()),
            theme,
            4.0,
        )]
    } else {
        vec![render_assistant_text_segment(
            session_id, index, 0, &body, streaming, theme,
        )]
    };
    let user_question_element = if assistant {
        pending_user_question
            .map(|question| render_user_question(session_id, question, ask_user_input, theme, cx))
    } else {
        None
    };
    let source_summary_element = if assistant && show_sources {
        if !message.tool_activities.is_empty() {
            render_message_source_summary(&message.tool_activities, theme, cx)
        } else if !processing {
            render_conversation_source_summary(conversation_sources, theme, cx)
        } else {
            None
        }
    } else {
        None
    };
    let message_element = div()
        .flex()
        .flex_col()
        .gap(px(9.0))
        .when(error, |this| {
            this.p(px(13.0))
                .rounded(px(12.0))
                .bg(theme.destructive_soft)
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.destructive)
                        .child(i18n::text(cx, "chat.error")),
                )
        })
        .when_some(reasoning_element, |this, reasoning| this.child(reasoning))
        .children(content_elements)
        .when_some(user_question_element, |this, question| this.child(question))
        .when_some(source_summary_element, |this, sources| this.child(sources))
        .when(assistant, |this| {
            this.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .text_color(theme.faint)
                    .child(
                        Button::new(format!("copy-message-{}-{index}", session_id.as_str()))
                            .ghost()
                            .small()
                            .icon(IconName::Copy)
                            .tooltip(i18n::text(cx, "chat.copy_response"))
                            .disabled(copy_disabled)
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                            }),
                    )
                    .child(
                        Button::new(format!(
                            "regenerate-message-{}-{index}",
                            session_id.as_str()
                        ))
                        .ghost()
                        .small()
                        .icon(IconName::Redo2)
                        .tooltip(i18n::text(cx, "chat.regenerate"))
                        .disabled(processing)
                        .on_click(cx.listener(
                            move |app, _, window, cx| {
                                app.regenerate_assistant_message(
                                    &retry_session_id,
                                    index,
                                    window,
                                    cx,
                                )
                            },
                        )),
                    ),
            )
        });

    // Animate only the assistant message while it is receiving content. The
    // stable element id lets GPUI continue the same one-shot animation across
    // the batched stream updates instead of restarting it on every delta.
    let should_fade_in = message.animate_in
        && assistant
        && (!message.text.is_empty() || !message.reasoning.is_empty());
    if should_fade_in {
        message_element
            .with_animation(
                format!("stream-message-fade-{}-{index}", session_id.as_str()),
                Animation::new(STREAM_MESSAGE_FADE_DURATION).with_easing(gpui::ease_out_quint()),
                |element, delta| element.opacity(delta),
            )
            .into_any_element()
    } else {
        message_element.into_any_element()
    }
}

fn form_label(label: impl Into<SharedString>, theme: UiTheme) -> gpui::Div {
    let label = label.into();
    div()
        .mb(px(6.0))
        .text_size(px(11.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.muted)
        .child(label)
}

fn settings_page_title(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    theme: UiTheme,
) -> gpui::Div {
    let title = title.into();
    let description = description.into();
    div()
        .child(
            div()
                .font(UiTheme::display_font())
                .text_size(px(24.0))
                .font_weight(FontWeight::BOLD)
                .child(title),
        )
        .child(
            div()
                .mt(px(7.0))
                .max_w(px(680.0))
                .text_size(px(12.0))
                .text_color(theme.muted)
                .child(description),
        )
}

fn settings_tab_description(tab: SettingsTab) -> &'static str {
    match tab {
        SettingsTab::Connections => "settings.global_connections",
        SettingsTab::Models => "settings.provider_catalogs",
        SettingsTab::Agents => "settings.delegated_agents_description",
        SettingsTab::Diagnostics => "settings.session_trace_short",
        SettingsTab::Storage => "settings.local_storage",
        SettingsTab::About => "settings.about_description",
    }
}

/// The select control updates its internal value before the parent receives
/// the deferred `SelectEvent`. Prefer that value while rendering and saving
/// so the provider form cannot lag one frame behind the user's selection.
fn effective_connection_kind(
    state_kind: Option<ConnectionKind>,
    control_kind: Option<ConnectionKind>,
) -> Option<ConnectionKind> {
    control_kind.or(state_kind)
}

fn settings_entry_tab() -> SettingsTab {
    SettingsTab::Models
}

fn settings_empty_state(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    theme: UiTheme,
) -> gpui::Div {
    let title = title.into();
    let description = description.into();
    div()
        .p(px(20.0))
        .bg(theme.surface_subtle)
        .rounded(px(10.0))
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
        .child(
            div()
                .mt(px(5.0))
                .text_size(px(12.0))
                .text_color(theme.muted)
                .child(description),
        )
}

fn storage_path_row(
    label: impl Into<SharedString>,
    path: &'static str,
    theme: UiTheme,
) -> gpui::Div {
    let label = label.into();
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .text_size(px(12.0))
        .child(div().flex_1().text_color(theme.muted).child(label))
        .child(
            div()
                .text_color(theme.faint)
                .font(UiTheme::mono_font())
                .child(path),
        )
}

#[cfg(test)]
mod connection_kind_tests {
    use super::{
        effective_connection_kind, settings_entry_tab, ConnectionKindChoice, SelectItem,
        SettingsTab,
    };
    use averroes_core::connection::ConnectionKind;

    #[test]
    fn provider_form_selection_keeps_a_stable_kind_value() {
        let cases = [
            ConnectionKind::QDivZero,
            ConnectionKind::Codex,
            ConnectionKind::Copilot,
            ConnectionKind::OpenAi,
            ConnectionKind::Anthropic,
            ConnectionKind::DeepSeek,
            ConnectionKind::Groq,
            ConnectionKind::Ollama,
            ConnectionKind::OllamaCloud,
            ConnectionKind::Compatible,
        ];

        for expected in cases {
            let choice = ConnectionKindChoice::new(expected, "localized label".into());
            assert_eq!(*choice.value(), expected);
        }
    }

    #[test]
    fn provider_form_uses_selector_value_before_event_sync() {
        assert_eq!(
            effective_connection_kind(None, Some(ConnectionKind::Groq)),
            Some(ConnectionKind::Groq)
        );
        assert_eq!(
            effective_connection_kind(Some(ConnectionKind::OpenAi), Some(ConnectionKind::Groq)),
            Some(ConnectionKind::Groq)
        );
    }

    #[test]
    fn opening_settings_starts_on_the_provider_models_tab() {
        assert_eq!(settings_entry_tab(), SettingsTab::Models);
    }
}

#[cfg(test)]
mod update_start_tests {
    use super::{
        update_check_can_start, update_dialog_can_retry_open, update_dialog_is_downloading,
    };
    use crate::update::{UpdateInfo, UpdateState};
    use semver::Version;
    use std::path::PathBuf;

    fn sample_update() -> UpdateInfo {
        UpdateInfo {
            version: Version::new(1, 2, 3),
            tag_name: "v1.2.3".into(),
            release_url: "https://github.com/valendra-tech/averroes/releases/tag/v1.2.3".into(),
            release_notes: "Release notes".into(),
            dmg_url:
                "https://github.com/valendra-tech/averroes/releases/download/v1.2.3/Averroes.dmg"
                    .into(),
            dmg_name: "Averroes.dmg".into(),
            dmg_size: 1,
            dmg_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
        }
    }

    #[test]
    fn update_check_only_starts_from_idle() {
        let info = sample_update();
        let states = [
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::Available(info.clone()),
            UpdateState::Downloading(info.clone()),
            UpdateState::ReadyToOpen {
                info: info.clone(),
                path: PathBuf::from("/tmp/Averroes.dmg"),
            },
            UpdateState::Failed {
                info: Some(info),
                message: "download failed".into(),
            },
        ];

        assert!(update_check_can_start(&states[0]));
        assert!(states[1..]
            .iter()
            .all(|state| !update_check_can_start(state)));
    }

    #[test]
    fn ready_to_open_is_retryable_only_after_open_failure() {
        let info = sample_update();
        let downloading = UpdateState::Downloading(info.clone());
        let ready_to_open = UpdateState::ReadyToOpen {
            info,
            path: PathBuf::from("/tmp/Averroes.dmg"),
        };

        assert!(update_dialog_is_downloading(&downloading));
        assert!(!update_dialog_is_downloading(&ready_to_open));
        assert!(!update_dialog_can_retry_open(&ready_to_open, None));
        assert!(update_dialog_can_retry_open(
            &ready_to_open,
            Some("installer failed")
        ));
    }
}

fn short_title(text: &str) -> String {
    let title = text
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    let title = title.chars().take(44).collect::<String>();
    if title.is_empty() {
        "New conversation".into()
    } else {
        title
    }
}

fn can_apply_generated_title(current: &str, fallback: &str) -> bool {
    current == fallback || matches!(current.trim(), "" | "New conversation" | "New session")
}

fn update_check_can_start(state: &UpdateState) -> bool {
    matches!(state, UpdateState::Idle)
}

fn update_dialog_is_downloading(state: &UpdateState) -> bool {
    matches!(state, UpdateState::Downloading(_))
}

fn update_dialog_can_retry_open(state: &UpdateState, open_error: Option<&str>) -> bool {
    matches!(state, UpdateState::ReadyToOpen { .. }) && open_error.is_some()
}

#[cfg(test)]
mod workspace_grouping_tests {
    use super::*;

    fn summary(id: &str, project_id: Option<&str>, pinned: bool) -> ConversationSummary {
        ConversationSummary {
            id: id.into(),
            title: id.into(),
            project_id: project_id.map(str::to_string),
            pinned,
            unread: false,
            updated_at: 1,
        }
    }

    #[test]
    fn workspace_conversations_are_nested_instead_of_global() {
        let projects = vec![WorkProject {
            id: "workspace-1".into(),
            name: "Workspace".into(),
            root: PathBuf::from("/tmp/workspace"),
            created_at: 1,
            last_opened_at: 1,
        }];
        let conversations = vec![
            summary("global", None, false),
            summary("inside", Some("workspace-1"), false),
            summary("pinned-inside", Some("workspace-1"), true),
            summary("orphan", Some("missing-workspace"), false),
        ];

        let (global, nested) = group_conversations_by_workspace(&conversations, &projects);
        assert_eq!(
            global
                .iter()
                .map(|conversation| conversation.id.as_str())
                .collect::<Vec<_>>(),
            vec!["global", "orphan"]
        );
        assert_eq!(
            nested["workspace-1"]
                .iter()
                .map(|conversation| conversation.id.as_str())
                .collect::<Vec<_>>(),
            vec!["inside", "pinned-inside"]
        );
    }

    #[test]
    fn agent_history_skips_errors_and_unfinished_assistant_messages() {
        let mut answer = ShellMessage::assistant();
        answer.text = "Finished answer".into();
        let messages = vec![
            ShellMessage::user("Original question".into()),
            answer,
            ShellMessage::error("Request failed"),
            ShellMessage::assistant(),
        ];

        let history = shell_messages_to_agent_history(&messages);

        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(
            history[0].content,
            MessageContent::Text("Original question".into())
        );
        assert_eq!(history[1].role, Role::Assistant);
        assert_eq!(
            history[1].content,
            MessageContent::Text("Finished answer".into())
        );
    }

    #[test]
    fn new_conversations_inherit_the_active_connection_model_and_tools() {
        let active = SessionBinding {
            connection_id: Some(ConnectionId("active".into())),
            model_id: Some("active-model".into()),
            reasoning_effort: Some("high".into()),
            tools: vec!["grep".into(), "checkpoint".into()],
        };
        let remembered = SessionBinding {
            connection_id: Some(ConnectionId("remembered".into())),
            model_id: Some("remembered-model".into()),
            reasoning_effort: Some("low".into()),
            tools: vec!["web_fetch".into()],
        };

        let inherited = inherited_session_binding(&active, &remembered, &["bash".into()]);

        assert_eq!(inherited, active);
    }

    #[test]
    fn enabled_tool_updates_report_when_the_binding_needs_persistence() {
        let mut binding = SessionBinding {
            connection_id: Some(ConnectionId("connection".into())),
            model_id: Some("model".into()),
            reasoning_effort: None,
            tools: vec!["discover_tools".into()],
        };

        assert!(apply_enabled_tools(
            &mut binding,
            vec!["discover_tools".into(), "web_search_intrernal".into()],
        ));
        assert_eq!(
            binding.tools,
            vec!["discover_tools", "web_search_intrernal"]
        );
        assert!(!apply_enabled_tools(
            &mut binding,
            vec!["discover_tools".into(), "web_search_intrernal".into()],
        ));
    }

    #[test]
    fn legacy_empty_tool_bindings_are_filled_only_once() {
        let mut binding = SessionBinding::default();

        assert!(ensure_binding_tools(
            &mut binding,
            &["discover_tools".into(), "enable_tools".into()],
        ));
        assert!(!ensure_binding_tools(
            &mut binding,
            &["different_default".into()],
        ));
        assert_eq!(binding.tools, vec!["discover_tools", "enable_tools"]);
    }

    #[test]
    fn legacy_selections_receive_default_tools_when_reused() {
        let remembered = SessionBinding {
            connection_id: Some(ConnectionId("remembered".into())),
            model_id: Some("remembered-model".into()),
            reasoning_effort: None,
            tools: Vec::new(),
        };

        let inherited = inherited_session_binding(
            &SessionBinding::default(),
            &remembered,
            &["bash".into(), "file_read".into()],
        );

        assert_eq!(inherited.connection_id, remembered.connection_id);
        assert_eq!(inherited.model_id, remembered.model_id);
        assert_eq!(inherited.tools, vec!["bash", "file_read"]);
    }

    #[test]
    fn completed_background_work_is_unread_until_its_conversation_is_opened() {
        let active = SessionId("active".into());
        let background = SessionId("background".into());

        assert!(!conversation_has_unread_update(
            Route::Chat,
            &active,
            &active
        ));
        assert!(conversation_has_unread_update(
            Route::Chat,
            &active,
            &background
        ));
        assert!(conversation_has_unread_update(
            Route::Connections,
            &active,
            &active
        ));
    }

    #[test]
    fn agent_model_choices_are_filtered_by_connection_and_keep_model_ids_visible() {
        let choices = vec![
            ModelChoice {
                connection_id: ConnectionId("openai".into()),
                connection_name: "OpenAI".into(),
                info: test_model("gpt-5.6", "GPT-5.6", true),
            },
            ModelChoice {
                connection_id: ConnectionId("openai".into()),
                connection_name: "OpenAI".into(),
                info: test_model("text-embedding-3-small", "Text embedding", false),
            },
            ModelChoice {
                connection_id: ConnectionId("copilot".into()),
                connection_name: "Copilot".into(),
                info: test_model("claude-sonnet-5", "Claude Sonnet 5", true),
            },
        ];

        let models = agent_model_choices(&choices, Some(&ConnectionId("openai".into())));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].1, "gpt-5.6");
        assert!(models[0].0.contains("gpt-5.6"));
    }

    fn test_model(id: &str, display_name: &str, chat: bool) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            display_name: display_name.into(),
            provider: "test".into(),
            description: None,
            capabilities: averroes_core::provider::ModelCapabilities {
                chat,
                embeddings: !chat,
                vision: false,
                tools: chat,
            },
            source: ModelSource::Live,
            featured: false,
            default_reasoning_effort: None,
            available_reasoning_efforts: Vec::new(),
        }
    }
}

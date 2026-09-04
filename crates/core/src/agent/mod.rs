pub mod orchestration;
pub mod registry;

mod budget;
mod context;
mod streaming;
mod tools;

use crate::agent::orchestration::AgentRunner;
use crate::compaction::strategies::{HybridStrategy, SummaryStrategy, TrimStrategy};
use crate::compaction::{
    compact_tool_outputs, sanitize_tool_history, CompactionConfig, CompactionStrategy,
    CompactionStrategyType,
};
use crate::provider::types::{MessageContent, Role};
use crate::provider::{ChatMessage, ChatRequest, ChatResponse, Provider, ToolDefinition};
use crate::runtime::ResourceGovernor;
use crate::skill::SkillIndex;
use crate::tool::{ToolActivation, ToolApprovalPolicy, ToolRegistry};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use context::ContextUsage;

const MAX_AUTO_SKILLS: usize = 3;
const MAX_AUTO_SKILL_CONTEXT_BYTES: usize = 32 * 1024;
const MAX_SKILL_CATALOG_BYTES: usize = 8 * 1024;
const PROVIDER_INITIAL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_SILENT_PROVIDER_RETRIES: usize = 1;
const ITERATION_LIMIT_FINAL_CONTEXT: &str = concat!(
    "[Tool execution budget reached]\n\n",
    "Tool use is disabled for this response. Use the results already available in the conversation ",
    "to answer the user now. Summarize what was completed, clearly identify anything still unfinished, ",
    "and do not claim that unverified work succeeded. Respond in the user's language."
);
const ITERATION_LIMIT_FALLBACK: &str = "I reached the tool execution safety limit. The work completed so far is preserved; ask me to continue and I will resume from there.";
const PROJECT_INSTRUCTIONS_CONTEXT: &str = "[Project instructions for the current directory]";

pub(super) fn is_delegation_tool(name: &str) -> bool {
    matches!(name, "list_agents" | "call_agent" | "call_agents")
}

use budget::{message_text, GovernedProvider, RunStateGuard};

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub model: String,
    pub system_prompt: Option<String>,
    /// Workspace root used to refresh AGENTS.md instructions after a directory
    /// change. Custom prompts remain untouched when this is not set.
    pub project_instructions_root: Option<PathBuf>,
    pub tools: Vec<String>,
    /// Maximum number of tool-call rounds before the agent must synthesize a
    /// final response from the work completed so far.
    pub max_iterations: usize,
    pub compaction: CompactionConfig,
    pub temperature: Option<f32>,
    pub reasoning_effort: Option<String>,
    pub tool_approval_policy: ToolApprovalPolicy,
    /// Child agents are deliberately leaf workers and cannot start another
    /// delegation chain.
    pub allow_delegation: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "default".into(),
            model: "claude-sonnet-4-20250514".into(),
            system_prompt: None,
            project_instructions_root: None,
            tools: Vec::new(),
            max_iterations: 50,
            compaction: CompactionConfig::default(),
            temperature: None,
            reasoning_effort: None,
            tool_approval_policy: ToolApprovalPolicy::default(),
            allow_delegation: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Thinking,
    Acting,
    WaitingForTool,
    Compacting,
    Completed,
    Errored,
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum AgentStreamEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ReasoningFinished,
    /// A provider has announced a tool call while its response is still
    /// streaming. The UI can show the call immediately and update it when
    /// execution begins.
    ToolPreparing {
        call_id: String,
        name: String,
        input: serde_json::Value,
        /// Whether the provider announced this call while emitting the
        /// assistant's reasoning stream. The UI uses this to keep the tool
        /// inside the reasoning disclosure even after the provider closes
        /// the message and execution starts.
        inside_reasoning: bool,
    },
    ToolStarted {
        call_id: Option<String>,
        name: String,
        input: serde_json::Value,
    },
    ToolConfirmationRequested {
        session_id: String,
        name: String,
        input: serde_json::Value,
        question: crate::tool::builtin::ask_user::UserQuestion,
    },
    ToolConfirmationResolved {
        session_id: String,
        question_id: String,
        approved: bool,
    },
    ToolFinished {
        call_id: Option<String>,
        name: String,
        success: bool,
        summary: String,
        /// A bounded detail payload for the in-conversation tool inspector.
        /// It is deliberately not persisted in `WorkSource` or conversation
        /// history as a source entry.
        output: String,
        metadata: Option<serde_json::Value>,
        /// Provider-ready images from the tool result, such as desktop or
        /// browser screenshots. The UI can relay them without re-running the
        /// tool.
        images: Vec<crate::provider::types::ImageSource>,
    },
    ContextUpdated {
        usage: ContextUsage,
    },
    CompactionStarted {
        reason: String,
    },
    CompactionFinished {
        reason: String,
        original_messages: usize,
        retained_messages: usize,
        understood_context: Option<String>,
    },
    /// A delegated thread has been created and is about to receive its first
    /// streamed event. Keeping the snapshot in the event lets the UI render
    /// the child immediately, before its first token or tool call arrives.
    DelegatedAgentStarted {
        thread: crate::agent::orchestration::AgentThreadSnapshot,
    },
    /// Stream events emitted by a child agent. The wrapper is recursive so a
    /// delegated agent can delegate again without flattening thread identity.
    DelegatedAgentEvent {
        thread_id: String,
        event: Box<AgentStreamEvent>,
    },
}

pub struct Agent {
    config: AgentConfig,
    runtime: Arc<std::sync::RwLock<AgentRuntime>>,
    tool_registry: Arc<ToolRegistry>,
    tool_activation: Arc<ToolActivation>,
    state: Arc<Mutex<AgentState>>,
    run_lock: Arc<tokio::sync::Mutex<()>>,
    messages: Arc<tokio::sync::Mutex<Vec<ChatMessage>>>,
    last_context_usage: Arc<Mutex<Option<ContextUsage>>>,
    understood_context: Arc<std::sync::RwLock<Option<String>>>,
    global_memory_prompt: Arc<std::sync::RwLock<Option<String>>>,
    skill_index: Arc<std::sync::RwLock<Option<Arc<SkillIndex>>>>,
    memory_search_backend:
        Arc<std::sync::RwLock<Option<Arc<dyn crate::tool::MemorySearchBackend>>>>,
    agent_runner: Arc<Mutex<Option<Arc<dyn AgentRunner>>>>,
    agent_id: String,
    session_id: String,
    working_dir: PathBuf,
}

#[derive(Clone)]
struct AgentRuntime {
    provider: Arc<dyn Provider>,
    model: String,
    governor: Arc<ResourceGovernor>,
    reasoning_effort: Option<String>,
}

impl Agent {
    pub fn new(
        config: AgentConfig,
        provider: Arc<dyn Provider>,
        tool_registry: Arc<ToolRegistry>,
        governor: Arc<ResourceGovernor>,
        session_id: String,
        working_dir: PathBuf,
    ) -> Self {
        let agent_id = uuid::Uuid::new_v4().to_string();
        let model = config.model.clone();
        let messages = {
            let mut msgs = Vec::new();
            if let Some(ref prompt) = config.system_prompt {
                msgs.push(ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text(prompt.clone()),
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
            Arc::new(tokio::sync::Mutex::new(msgs))
        };
        let reasoning_effort = config.reasoning_effort.clone();
        let tool_activation = Arc::new(ToolActivation::new(config.tools.iter().cloned()));
        tool_activation.set_approval_policy(config.tool_approval_policy);

        Self {
            config,
            runtime: Arc::new(std::sync::RwLock::new(AgentRuntime {
                provider,
                model,
                governor,
                reasoning_effort,
            })),
            tool_registry,
            tool_activation,
            state: Arc::new(Mutex::new(AgentState::Idle)),
            run_lock: Arc::new(tokio::sync::Mutex::new(())),
            messages,
            last_context_usage: Arc::new(Mutex::new(None)),
            understood_context: Arc::new(std::sync::RwLock::new(None)),
            global_memory_prompt: Arc::new(std::sync::RwLock::new(None)),
            skill_index: Arc::new(std::sync::RwLock::new(None)),
            memory_search_backend: Arc::new(std::sync::RwLock::new(None)),
            agent_runner: Arc::new(Mutex::new(None)),
            agent_id,
            session_id,
            working_dir,
        }
    }

    pub fn id(&self) -> &str {
        &self.agent_id
    }

    pub async fn state(&self) -> AgentState {
        *self.state.lock().unwrap()
    }

    /// Returns the latest provider usage. Before the first provider response,
    /// token usage is intentionally unknown.
    pub async fn context_usage(&self) -> ContextUsage {
        let runtime = self.runtime_snapshot();
        let context_limit = runtime.provider.context_window(&runtime.model);
        if let Some(usage) = self.last_context_usage.lock().unwrap().as_ref() {
            return *usage;
        }
        ContextUsage::unknown(context_limit)
    }

    /// Generates a short conversation title with the selected provider.
    ///
    /// Title generation is intentionally separate from the agent history, so
    /// it cannot pollute the conversation context or appear as an assistant
    /// message. It still goes through the governor to share provider limits
    /// with normal requests.
    pub async fn generate_title(&self, user_message: &str) -> Result<String> {
        let runtime = self.runtime_snapshot();
        let provider = GovernedProvider {
            provider: runtime.provider,
            governor: runtime.governor,
        };
        crate::storage::session::generate_session_title(&provider, &runtime.model, user_message)
            .await
            .map_err(anyhow::Error::msg)
    }

    /// Compacts the agent history immediately and returns the resulting
    /// context usage. This is deliberately serialized with normal runs so a
    /// manual compaction can never mutate the request while it is in flight.
    pub async fn force_compact(&self) -> Result<ContextUsage> {
        let _run_lock = self.run_lock.lock().await;
        let runtime = self.runtime_snapshot();
        *self.last_context_usage.lock().unwrap() = None;
        self.compact_with_runtime(&runtime).await?;
        self.set_state(AgentState::Idle);
        let usage = self.context_usage().await;
        *self.last_context_usage.lock().unwrap() = Some(usage);
        Ok(usage)
    }

    /// Returns the number of messages currently held by the agent. The UI
    /// uses this around manual compaction to report the actual reduction.
    pub async fn message_count(&self) -> usize {
        self.messages.lock().await.len()
    }

    /// Restores the provider-reported usage associated with a persisted
    /// conversation. Unknown usage is intentionally not turned into a guess.
    pub fn set_context_usage(&self, mut usage: ContextUsage) {
        let has_usage = usage.input_tokens.is_some() || usage.output_tokens.is_some();
        if has_usage {
            let runtime = self.runtime_snapshot();
            usage.context_limit = runtime.provider.context_window(&runtime.model) as u64;
        }
        *self.last_context_usage.lock().unwrap() = has_usage.then_some(usage);
    }

    /// Returns the latest compact, model-generated understanding of this
    /// conversation. It is kept separate from visible messages so a later
    /// request can carry the useful state without replaying stale detail.
    pub fn understood_context(&self) -> Option<String> {
        self.understood_context.read().unwrap().clone()
    }

    /// Restores a previously generated conversation understanding when the
    /// UI reopens an existing session.
    pub fn set_understood_context(&self, context: Option<String>) {
        *self.understood_context.write().unwrap() = context
            .map(|context| context.trim().to_owned())
            .filter(|context| !context.is_empty());
    }

    pub async fn restore_conversation_history(&self, history: Vec<ChatMessage>) {
        let _run_lock = self.run_lock.lock().await;
        let mut messages = self.messages.lock().await;
        let history = crate::compaction::sanitize_tool_history(history);
        let system_message = messages
            .first()
            .filter(|message| message.role == Role::System)
            .cloned();
        messages.clear();
        if let Some(system_message) = system_message {
            messages.push(system_message);
        }
        messages.extend(
            history.into_iter().filter(|message| {
                matches!(&message.role, Role::User | Role::Assistant | Role::Tool)
            }),
        );
    }

    /// Returns a snapshot suitable for continuing a delegated thread. Tool
    /// messages are retained here because they complete the assistant
    /// function calls required by the Responses API.
    pub async fn conversation_history(&self) -> Vec<ChatMessage> {
        self.messages.lock().await.clone()
    }

    /// Returns the complete activation set so a reconstructed delegated
    /// agent can continue the same thread without rediscovering its tools.
    pub fn enabled_tool_names(&self) -> Vec<String> {
        self.tool_activation.names()
    }

    pub fn reconfigure_provider(
        &self,
        provider: Arc<dyn Provider>,
        model: String,
        governor: Arc<ResourceGovernor>,
    ) {
        let mut runtime = self.runtime.write().unwrap();
        runtime.provider = provider;
        runtime.model = model;
        runtime.governor = governor;
    }

    pub fn set_reasoning_effort(&self, effort: Option<String>) {
        let mut runtime = self.runtime.write().unwrap();
        runtime.reasoning_effort = effort;
    }

    pub fn set_tool_approval_policy(&self, policy: ToolApprovalPolicy) {
        self.tool_activation.set_approval_policy(policy);
    }

    /// Installs the workspace skill index on this agent and refreshes the
    /// scoped skill tools without changing the agent's conversation history.
    pub fn set_skill_index(&self, index: Option<Arc<SkillIndex>>) {
        if let Some(index) = index.as_ref() {
            crate::tool::builtin::register_skill_tools(&self.tool_registry, index.clone());
        }
        *self.skill_index.write().unwrap() = index;
    }

    pub fn set_memory_search_backend(
        &self,
        backend: Option<Arc<dyn crate::tool::MemorySearchBackend>>,
    ) {
        *self.memory_search_backend.write().unwrap() = backend;
    }

    /// Replaces the globally scoped prompt fragment. It is injected into every
    /// request without being persisted in the conversation's message history.
    pub fn set_global_memory_prompt(&self, prompt: Option<String>) {
        *self.global_memory_prompt.write().unwrap() = prompt
            .map(|prompt| prompt.trim().to_owned())
            .filter(|prompt| !prompt.is_empty());
    }

    async fn resolve_skill_context(&self, user_input: &str) -> Option<String> {
        let index = self.skill_index.read().ok()?.clone()?;
        let user_input = user_input.to_owned();
        tokio::task::spawn_blocking(move || {
            if index.is_empty() {
                return None;
            }

            let mut context = String::from(concat!(
                "## Workspace Skills\n\n",
                "Available skill names are listed compactly below. Compare them with the user's request. ",
                "When a skill clearly applies, load that exact skill directly and follow it for this turn. ",
                "Use the filtered skill listing only when the names are not enough to decide.\n\n",
            ));
            let mut catalog_count = 0;
            for skill in index.list() {
                let entry = format!("- `{}`\n", skill.name);
                if context.len().saturating_add(entry.len()) > MAX_SKILL_CATALOG_BYTES {
                    break;
                }
                context.push_str(&entry);
                catalog_count += 1;
            }

            let matches = index.find_relevant(&user_input);
            let mut loaded = Vec::new();

            for skill in matches.into_iter().take(MAX_AUTO_SKILLS) {
                let name = skill.name.clone();
                let description = skill.description.clone();
                let content = match index.load(&name) {
                    Ok(content) => content,
                    Err(error) => {
                        crate::observability::diagnostics::record(
                            crate::observability::diagnostics::DiagnosticLevel::Warning,
                            "skills.resolution",
                            format!("Could not auto-load skill '{name}': {error}."),
                        );
                        continue;
                    }
                };

                let heading = format!("\n### Loaded skill: {name}\n");
                let description_context = if description.trim().is_empty() {
                    String::new()
                } else {
                    format!("{description}\n\n")
                };
                let remaining = MAX_AUTO_SKILL_CONTEXT_BYTES
                    .saturating_sub(
                        context
                            .len()
                            .saturating_add(heading.len())
                            .saturating_add(description_context.len()),
                    );
                if remaining == 0 {
                    break;
                }
                let content = if content.len() > remaining {
                    let truncated = truncate_utf8(&content, remaining);
                    format!(
                        "{truncated}\n\n[Skill content truncated for context safety.]\n"
                    )
                } else {
                    content
                };
                context.push_str(&heading);
                context.push_str(&description_context);
                context.push_str(&content);
                context.push_str("\n\n");
                loaded.push(name);
            }

            if loaded.is_empty() {
                crate::observability::diagnostics::record(
                    crate::observability::diagnostics::DiagnosticLevel::Info,
                    "skills.resolution",
                    format!(
                        "Exposed the workspace skill catalogue ({} of {} skill(s)); no skill was auto-loaded for this request.",
                        catalog_count,
                        index.len()
                    ),
                );
                Some(context)
            } else {
                crate::observability::diagnostics::record(
                    crate::observability::diagnostics::DiagnosticLevel::Success,
                    "skills.resolution",
                    format!(
                        "Exposed {} skill(s) and automatically loaded {} workspace skill(s): {}.",
                        catalog_count,
                        loaded.len(),
                        loaded.join(", ")
                    ),
                );
                Some(context)
            }
        })
        .await
        .ok()
        .flatten()
    }

    pub fn set_agent_runner(&self, runner: Arc<dyn AgentRunner>) {
        *self.agent_runner.lock().unwrap() = Some(runner);
    }

    pub(crate) fn agent_runner(&self) -> Option<Arc<dyn AgentRunner>> {
        self.agent_runner.lock().unwrap().clone()
    }

    fn runtime_snapshot(&self) -> AgentRuntime {
        self.runtime.read().unwrap().clone()
    }

    pub async fn run(&self, user_input: &str) -> Result<String> {
        self.run_inner(user_input, None, None).await
    }

    pub async fn run_streaming(
        &self,
        user_input: &str,
        events: tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>,
    ) -> Result<String> {
        self.run_inner(user_input, None, Some(events)).await
    }

    /// Runs a request whose user message contains provider-native content,
    /// such as an image part. `user_input` remains the text projection used
    /// for skill resolution and diagnostics.
    pub async fn run_streaming_with_content(
        &self,
        user_input: &str,
        content: MessageContent,
        events: tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>,
    ) -> Result<String> {
        self.run_inner(user_input, Some(content), Some(events))
            .await
    }

    async fn run_inner(
        &self,
        user_input: &str,
        user_content: Option<MessageContent>,
        stream_events: Option<tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>>,
    ) -> Result<String> {
        let _run_lock = self.run_lock.lock().await;
        let mut run_state = RunStateGuard::new(self.state.clone());
        let skill_context = self.resolve_skill_context(user_input).await;

        {
            let mut msgs = self.messages.lock().await;
            if let Some(system_prompt) = msgs
                .iter_mut()
                .find(|message| message.role == Role::System)
                .and_then(|message| match &mut message.content {
                    MessageContent::Text(text) => Some(text),
                    MessageContent::Parts(_) => None,
                })
            {
                crate::prompt::refresh_system_environment_time(system_prompt);
            }
            msgs.push(ChatMessage {
                role: Role::User,
                content: user_content
                    .unwrap_or_else(|| MessageContent::Text(user_input.to_string())),
                tool_call_id: None,
                tool_calls: None,
            });
        }

        let mut context_retries = 0;
        let mut tool_iterations = 0;
        while tool_iterations < self.config.max_iterations {
            let runtime = self.runtime_snapshot();
            if self.should_compact_with_runtime(&runtime).await {
                if let Err(error) = self
                    .compact_with_runtime_with_events(&runtime, stream_events.as_ref())
                    .await
                {
                    self.set_state(AgentState::Errored);
                    run_state.finish();
                    return Err(error);
                }
            }

            let runtime = self.runtime_snapshot();
            let messages = self.messages.lock().await.clone();
            let request =
                self.build_request(messages, runtime.model.clone(), skill_context.clone());

            self.set_state(AgentState::Thinking);

            let response_result = match stream_events.as_ref() {
                Some(events) => {
                    self.chat_stream_with_events(&runtime, request, events)
                        .await
                }
                None => self.chat_with_governor(&runtime, request).await,
            };
            let response = match response_result {
                Ok(response) => response,
                Err(error) if is_context_error(&error) && context_retries < 2 => {
                    context_retries += 1;
                    crate::observability::diagnostics::record(
                        crate::observability::diagnostics::DiagnosticLevel::Warning,
                        "agent.compaction",
                        format!("Provider rejected the context; compacting and retrying: {error}"),
                    );
                    if let Err(compaction_error) = self
                        .compact_with_runtime_with_events(&runtime, stream_events.as_ref())
                        .await
                    {
                        self.set_state(AgentState::Errored);
                        run_state.finish();
                        return Err(compaction_error);
                    }
                    continue;
                }
                Err(error) => {
                    self.set_state(AgentState::Errored);
                    run_state.finish();
                    return Err(error);
                }
            };

            self.record_context_usage(
                &response,
                runtime.provider.context_window(&runtime.model),
                stream_events.as_ref(),
            );

            if response
                .message
                .tool_calls
                .as_ref()
                .map_or(false, |tc| !tc.is_empty())
            {
                tool_iterations += 1;
                self.set_state(AgentState::Acting);

                let tool_execution =
                    match self.execute_tools(&response, stream_events.as_ref()).await {
                        Ok(execution) => execution,
                        Err(error) => {
                            self.set_state(AgentState::Errored);
                            run_state.finish();
                            return Err(error);
                        }
                    };
                {
                    let mut msgs = self.messages.lock().await;
                    msgs.push(response.message.clone());
                    msgs.extend(tool_execution.messages);
                }

                continue;
            }

            {
                let mut messages = self.messages.lock().await;
                messages.push(response.message.clone());
            }
            self.set_state(AgentState::Completed);
            run_state.finish();
            return Ok(message_text(&response.message));
        }

        crate::observability::diagnostics::record(
            crate::observability::diagnostics::DiagnosticLevel::Warning,
            "agent.iterations",
            format!(
                "Tool execution budget reached after {tool_iterations} round(s); requesting a final response without tools."
            ),
        );

        let runtime = self.runtime_snapshot();
        if self.should_compact_with_runtime(&runtime).await {
            if let Err(error) = self
                .compact_with_runtime_with_events(&runtime, stream_events.as_ref())
                .await
            {
                self.set_state(AgentState::Errored);
                run_state.finish();
                return Err(error);
            }
        }

        let runtime = self.runtime_snapshot();
        let messages = self.messages.lock().await.clone();
        // The final synthesis does not need the per-turn skill catalogue. In
        // addition to saving context, omitting it avoids instructions that
        // may encourage another tool call when tools are deliberately off.
        let mut request = self.build_request(messages, runtime.model.clone(), None);
        request.tools.clear();
        insert_system_context(
            &mut request.messages,
            ITERATION_LIMIT_FINAL_CONTEXT.to_owned(),
        );

        self.set_state(AgentState::Thinking);
        let response_result = match stream_events.as_ref() {
            Some(events) => {
                self.chat_stream_with_events(&runtime, request, events)
                    .await
            }
            None => self.chat_with_governor(&runtime, request).await,
        };
        let response = match response_result {
            Ok(response) => response,
            Err(error) => {
                self.set_state(AgentState::Errored);
                run_state.finish();
                return Err(error);
            }
        };
        self.record_context_usage(
            &response,
            runtime.provider.context_window(&runtime.model),
            stream_events.as_ref(),
        );

        let mut final_message = response.message;
        final_message.tool_calls = None;
        let mut final_text = message_text(&final_message);
        if final_text.trim().is_empty() {
            final_text = ITERATION_LIMIT_FALLBACK.to_owned();
            final_message.content = MessageContent::Text(final_text.clone());
            if let Some(events) = stream_events.as_ref() {
                let _ = events.send(AgentStreamEvent::TextDelta {
                    text: final_text.clone(),
                });
            }
        }
        self.messages.lock().await.push(final_message);
        self.set_state(AgentState::Completed);
        run_state.finish();
        Ok(final_text)
    }

    async fn chat_with_governor(
        &self,
        runtime: &AgentRuntime,
        request: ChatRequest,
    ) -> Result<ChatResponse> {
        let _permit = runtime.governor.acquire_call_permit().await;
        runtime
            .provider
            .chat(request)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    async fn should_compact_with_runtime(&self, runtime: &AgentRuntime) -> bool {
        let context_limit = runtime.provider.context_window(&runtime.model);
        let message_count = self.messages.lock().await.len();
        let usage_pressure = self
            .last_context_usage
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|usage| usage.input_tokens)
            .is_some_and(|input_tokens| {
                input_tokens as f64 > self.config.compaction.threshold * context_limit as f64
                    && message_count > 2
            });
        usage_pressure
    }

    async fn compact_with_runtime(&self, runtime: &AgentRuntime) -> Result<()> {
        self.compact_with_runtime_with_events(runtime, None).await
    }

    async fn compact_with_runtime_with_events(
        &self,
        runtime: &AgentRuntime,
        events: Option<&tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>>,
    ) -> Result<()> {
        let reason = "Provider-reported context usage is high.".to_string();
        if let Some(events) = events {
            let _ = events.send(AgentStreamEvent::CompactionStarted {
                reason: reason.clone(),
            });
        }
        self.set_state(AgentState::Compacting);

        let strategy = self.compaction_strategy();
        let context_limit = runtime.provider.context_window(&runtime.model);
        let governed_provider = GovernedProvider {
            provider: runtime.provider.clone(),
            governor: runtime.governor.clone(),
        };
        let provider_ref: Option<&dyn Provider> = match self.config.compaction.strategy {
            CompactionStrategyType::Trim => None,
            CompactionStrategyType::Summary | CompactionStrategyType::Hybrid => {
                Some(&governed_provider)
            }
        };

        let (compaction_result, original_messages) = {
            let mut msgs = self.messages.lock().await.clone();
            if let Some(context) = self.understood_context.read().unwrap().clone() {
                insert_understood_context(&mut msgs, &context);
            }
            let msgs = compact_tool_outputs(msgs);
            let original_messages = msgs.len();
            let result = strategy
                .compact(
                    &msgs,
                    context_limit,
                    &self.config.compaction,
                    provider_ref,
                    runtime.model.as_str(),
                )
                .await;
            (result, original_messages)
        };
        let mut compacted = match compaction_result {
            Ok(compacted) => compacted,
            Err(error) if self.config.compaction.strategy != CompactionStrategyType::Trim => {
                crate::observability::diagnostics::record(
                    crate::observability::diagnostics::DiagnosticLevel::Warning,
                    "agent.compaction",
                    format!(
                        "Summary compaction failed ({error}); falling back to deterministic trim."
                    ),
                );
                let mut msgs = self.messages.lock().await.clone();
                if let Some(context) = self.understood_context.read().unwrap().clone() {
                    insert_understood_context(&mut msgs, &context);
                }
                let msgs = compact_tool_outputs(msgs);
                TrimStrategy
                    .compact(
                        &msgs,
                        context_limit,
                        &self.config.compaction,
                        None,
                        runtime.model.as_str(),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?
            }
            Err(error) => return Err(anyhow::anyhow!(error.to_string())),
        };
        compacted.messages = compact_tool_outputs(sanitize_tool_history(compacted.messages));

        let understood_context = extract_understood_context(&compacted.messages);
        compacted.messages.retain(|message| {
            !message_text(message).starts_with("[Previous conversation summary]")
        });
        compacted.compacted_count = compacted.messages.len();
        if understood_context.is_some() {
            *self.understood_context.write().unwrap() = understood_context.clone();
        }

        {
            let mut msgs = self.messages.lock().await;
            *msgs = compacted.messages;
        }
        // The previous provider measurement describes the pre-compaction
        // request and must never trigger another compaction by itself. The
        // next model response will replace it with an exact fresh value.
        *self.last_context_usage.lock().unwrap() = None;
        if let Some(events) = events {
            let _ = events.send(AgentStreamEvent::CompactionFinished {
                reason,
                original_messages,
                retained_messages: compacted.compacted_count,
                understood_context,
            });
        }
        Ok(())
    }

    fn record_context_usage(
        &self,
        response: &ChatResponse,
        context_limit: usize,
        events: Option<&tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>>,
    ) {
        let Some(provider_usage) = response.usage.as_ref() else {
            return;
        };
        let usage = ContextUsage::from_provider_usage(provider_usage, context_limit);
        *self.last_context_usage.lock().unwrap() = Some(usage);
        if let Some(events) = events {
            let _ = events.send(AgentStreamEvent::ContextUpdated { usage });
        }
    }

    fn compaction_strategy(&self) -> Box<dyn CompactionStrategy> {
        match self.config.compaction.strategy {
            CompactionStrategyType::Trim => Box::new(TrimStrategy),
            CompactionStrategyType::Summary => Box::new(SummaryStrategy),
            CompactionStrategyType::Hybrid => Box::new(HybridStrategy),
        }
    }

    fn set_state(&self, state: AgentState) {
        *self.state.lock().unwrap() = state;
    }

    fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tool_activation
            .names()
            .iter()
            .filter(|name| self.config.allow_delegation || !is_delegation_tool(name))
            .filter_map(|name| {
                self.tool_registry.get(name).map(|tool| ToolDefinition {
                    name: tool.name().to_string(),
                    description: tool.description().to_string(),
                    input_schema: tool.parameters(),
                })
            })
            .collect()
    }

    fn build_request(
        &self,
        mut messages: Vec<ChatMessage>,
        model: String,
        skill_context: Option<String>,
    ) -> ChatRequest {
        let runtime = self.runtime_snapshot();
        let current_dir = self.tool_activation.current_directory(&self.working_dir);
        let current_dir_display = current_dir.display().to_string();
        if let Some(system_prompt) = messages
            .iter_mut()
            .find(|message| message.role == Role::System)
            .and_then(|message| match &mut message.content {
                MessageContent::Text(text) => Some(text),
                MessageContent::Parts(_) => None,
            })
        {
            crate::prompt::refresh_system_working_directory(system_prompt, &current_dir_display);
        }
        refresh_project_instructions(
            &mut messages,
            self.config.project_instructions_root.as_deref(),
            &current_dir,
        );
        if let Some(context) = self.understood_context.read().unwrap().clone() {
            insert_understood_context(&mut messages, &context);
        }
        if let Some(global_memory) = self.global_memory_prompt.read().unwrap().clone() {
            insert_system_context(&mut messages, global_memory);
        }
        ChatRequest {
            model,
            messages,
            tools: self.build_tool_definitions(),
            temperature: self.config.temperature,
            system: skill_context,
            reasoning_effort: runtime.reasoning_effort,
        }
    }
}

fn refresh_project_instructions(
    messages: &mut Vec<ChatMessage>,
    workspace_root: Option<&std::path::Path>,
    working_dir: &std::path::Path,
) {
    messages.retain(|message| {
        !(message.role == Role::System
            && message_text(message).starts_with(PROJECT_INSTRUCTIONS_CONTEXT))
    });
    let Some(workspace_root) = workspace_root else {
        return;
    };
    let instructions = crate::prompt::ProjectInstructions::load(workspace_root, working_dir);
    if instructions.is_empty() {
        return;
    }
    insert_system_context(
        messages,
        format!(
            "{PROJECT_INSTRUCTIONS_CONTEXT}\n\n{}",
            instructions.content()
        ),
    );
}

fn insert_system_context(messages: &mut Vec<ChatMessage>, context: String) {
    let position = usize::from(
        messages
            .first()
            .is_some_and(|message| message.role == Role::System),
    );
    messages.insert(
        position,
        ChatMessage {
            role: Role::System,
            content: MessageContent::Text(context),
            tool_call_id: None,
            tool_calls: None,
        },
    );
}

fn insert_understood_context(messages: &mut Vec<ChatMessage>, context: &str) {
    insert_system_context(
        messages,
        format!("[Understood conversation context]\n\n{context}"),
    );
}

fn extract_understood_context(messages: &[ChatMessage]) -> Option<String> {
    messages.iter().find_map(|message| {
        let text = message_text(message);
        text.strip_prefix("[Previous conversation summary]")
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn is_context_error(error: &anyhow::Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("context")
        && (text.contains("exceed") || text.contains("window") || text.contains("limit"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{FunctionCall, Role as ProviderRole, TokenUsage, ToolCall};
    use crate::provider::{ProviderError, StreamEvent};
    use crate::tool::{ToolContext, ToolResult};
    use async_trait::async_trait;
    use futures::{Stream, StreamExt};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use std::time::Duration;

    struct TestProvider {
        responses: Vec<ChatResponse>,
        call_count: std::sync::Mutex<usize>,
        requests: std::sync::Mutex<Vec<ChatRequest>>,
    }

    struct BlockingProvider {
        started: Arc<tokio::sync::Notify>,
    }

    struct PanickingProvider;

    struct GatedProvider {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        calls: Arc<AtomicUsize>,
    }

    struct StreamProvider {
        with_reasoning: bool,
    }

    struct ReasoningToolStreamProvider {
        calls: AtomicUsize,
    }

    struct ErrorStreamProvider;

    struct IncompleteStreamProvider;

    struct TruncatedStreamProvider;

    #[derive(Clone, Copy)]
    enum DropStreamMode {
        MessageEnd,
        ProviderError,
        EventError,
        Eof,
    }

    struct DropAwareStreamProvider {
        mode: DropStreamMode,
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }

    struct SmallContextProvider;

    struct DropAwareStream {
        mode: DropStreamMode,
        dropped: Arc<std::sync::atomic::AtomicBool>,
        emitted: bool,
    }

    impl Drop for DropAwareStream {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl Stream for DropAwareStream {
        type Item = crate::provider::Result<StreamEvent>;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if self.emitted {
                return Poll::Pending;
            }
            self.emitted = true;
            match self.mode {
                DropStreamMode::MessageEnd => {
                    Poll::Ready(Some(Ok(StreamEvent::MessageEnd { usage: None })))
                }
                DropStreamMode::ProviderError => {
                    Poll::Ready(Some(Err(ProviderError::Other("stream failed".into()))))
                }
                DropStreamMode::EventError => Poll::Ready(Some(Ok(StreamEvent::Error {
                    message: "stream failed".into(),
                }))),
                DropStreamMode::Eof => Poll::Ready(None),
            }
        }
    }

    impl TestProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses,
                call_count: std::sync::Mutex::new(0),
                requests: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl StreamProvider {
        fn new(with_reasoning: bool) -> Self {
            Self { with_reasoning }
        }
    }

    impl ReasoningToolStreamProvider {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl Provider for TestProvider {
        async fn chat(&self, request: ChatRequest) -> crate::provider::Result<ChatResponse> {
            self.requests.lock().unwrap().push(request);
            let mut count = self.call_count.lock().unwrap();
            let idx = *count;
            *count += 1;
            Ok(self.responses.get(idx).cloned().unwrap_or(ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text("fallback".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                reasoning: None,
                stop_reason: None,
            }))
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            unimplemented!()
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    #[async_trait]
    impl Provider for BlockingProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            self.started.notify_one();
            std::future::pending::<crate::provider::Result<ChatResponse>>().await
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            unimplemented!()
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    #[async_trait]
    impl Provider for PanickingProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            panic!("provider panic")
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            unimplemented!()
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    #[async_trait]
    impl Provider for GatedProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            Ok(ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text("gated response".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                reasoning: None,
                stop_reason: None,
            })
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            unimplemented!()
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    #[async_trait]
    impl Provider for StreamProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            unimplemented!()
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            let mut events = Vec::new();
            if self.with_reasoning {
                events.push(Ok(StreamEvent::ReasoningDelta {
                    text:
                        "**Inspecting** the request.\n\n- Check the context\n- Prepare the answer"
                            .into(),
                }));
            }
            events.extend([
                Ok(StreamEvent::TextDelta {
                    text: "part".into(),
                }),
                Ok(StreamEvent::MessageEnd {
                    usage: Some(TokenUsage {
                        input_tokens: 2,
                        output_tokens: 3,
                        cache_read_input_tokens: None,
                        cache_creation_input_tokens: None,
                        reasoning_output_tokens: None,
                    }),
                }),
            ]);
            Ok(Box::new(futures::stream::iter(events)))
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    #[async_trait]
    impl Provider for ReasoningToolStreamProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            unimplemented!()
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            let first_response = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
            let events = if first_response {
                vec![
                    Ok(StreamEvent::ReasoningDelta {
                        text: "I need to inspect this first.\n".into(),
                    }),
                    Ok(StreamEvent::ToolCallDelta {
                        id: "call-1".into(),
                        name: "echo".into(),
                        arguments_delta: r#"{"text":"ok"}"#.into(),
                    }),
                    Ok(StreamEvent::ToolCallEnd {
                        id: "call-1".into(),
                    }),
                    Ok(StreamEvent::MessageEnd { usage: None }),
                ]
            } else {
                vec![
                    Ok(StreamEvent::TextDelta {
                        text: "done".into(),
                    }),
                    Ok(StreamEvent::MessageEnd { usage: None }),
                ]
            };
            Ok(Box::new(futures::stream::iter(events)))
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    #[async_trait]
    impl Provider for ErrorStreamProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            unimplemented!()
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            Ok(Box::new(futures::stream::iter(vec![Err(
                ProviderError::Other("stream failed".into()),
            )])))
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    #[async_trait]
    impl Provider for IncompleteStreamProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            unimplemented!()
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            Ok(Box::new(futures::stream::iter(vec![Ok(
                StreamEvent::MessageEnd { usage: None },
            )])))
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    #[async_trait]
    impl Provider for TruncatedStreamProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            unimplemented!()
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            Ok(Box::new(futures::stream::iter(vec![Ok(
                StreamEvent::TextDelta {
                    text: "partial".into(),
                },
            )])))
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    #[async_trait]
    impl Provider for DropAwareStreamProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            unimplemented!()
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            Ok(Box::new(DropAwareStream {
                mode: self.mode,
                dropped: self.dropped.clone(),
                emitted: false,
            }))
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    #[async_trait]
    impl Provider for SmallContextProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            Ok(ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text("summary".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                reasoning: None,
                stop_reason: None,
            })
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            unimplemented!()
        }

        fn context_window(&self, _m: &str) -> usize {
            10
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    fn test_agent_config() -> AgentConfig {
        AgentConfig {
            name: "test-agent".into(),
            model: "test-model".into(),
            system_prompt: Some("You are a test agent.".into()),
            tools: vec!["echo".into()],
            ..Default::default()
        }
    }

    fn test_tool_registry() -> Arc<ToolRegistry> {
        use crate::tool::Tool;
        use async_trait::async_trait;
        use serde_json::json;

        struct EchoTool;
        #[async_trait]
        impl Tool for EchoTool {
            fn name(&self) -> &str {
                "echo"
            }
            fn description(&self) -> &str {
                "Echoes input"
            }
            fn parameters(&self) -> serde_json::Value {
                json!({"type": "object", "properties": {"text": {"type": "string"}}})
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                params: &serde_json::Value,
            ) -> crate::tool::Result<ToolResult> {
                Ok(ToolResult::ok(params["text"].as_str().unwrap_or("")))
            }
        }

        let registry = ToolRegistry::new();
        registry.register(EchoTool);
        Arc::new(registry)
    }

    fn blocking_tool_registry(started: Arc<tokio::sync::Notify>) -> Arc<ToolRegistry> {
        use crate::tool::Tool;

        struct BlockingTool {
            started: Arc<tokio::sync::Notify>,
        }

        #[async_trait]
        impl Tool for BlockingTool {
            fn name(&self) -> &str {
                "block"
            }

            fn description(&self) -> &str {
                "Blocks until canceled"
            }

            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({"type": "object", "properties": {}})
            }

            async fn execute(
                &self,
                _ctx: &ToolContext,
                _params: &serde_json::Value,
            ) -> crate::tool::Result<ToolResult> {
                self.started.notify_one();
                std::future::pending::<crate::tool::Result<ToolResult>>().await
            }
        }

        let registry = ToolRegistry::new();
        registry.register(BlockingTool { started });
        Arc::new(registry)
    }

    fn test_governor() -> Arc<ResourceGovernor> {
        Arc::new(ResourceGovernor::new(10, 200_000))
    }

    fn compaction_messages() -> Vec<ChatMessage> {
        vec![
            ChatMessage {
                role: ProviderRole::System,
                content: MessageContent::Text("system".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: ProviderRole::User,
                content: MessageContent::Text("old user".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text("old assistant".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: ProviderRole::User,
                content: MessageContent::Text("recent user".into()),
                tool_call_id: None,
                tool_calls: None,
            },
        ]
    }

    fn compaction_agent(
        strategy: CompactionStrategyType,
        provider: Arc<dyn Provider>,
        governor: Arc<ResourceGovernor>,
    ) -> Agent {
        Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                compaction: CompactionConfig {
                    strategy,
                    keep_last: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
            provider,
            test_tool_registry(),
            governor,
            "compaction-session".into(),
            PathBuf::from("/tmp"),
        )
    }

    async fn seed_compaction_messages(agent: &Agent) {
        *agent.messages.lock().await = compaction_messages();
    }

    #[test]
    fn test_agent_config_defaults() {
        let config = AgentConfig::default();
        assert_eq!(config.name, "default");
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        assert_eq!(config.max_iterations, 50);
    }

    #[test]
    fn test_agent_config_custom() {
        let config = AgentConfig {
            name: "custom".into(),
            model: "gpt-5".into(),
            max_iterations: 10,
            temperature: Some(0.5),
            ..Default::default()
        };
        assert_eq!(config.name, "custom");
        assert_eq!(config.temperature, Some(0.5));
    }

    #[test]
    fn test_agent_state_copy() {
        let state = AgentState::Idle;
        assert_eq!(state, AgentState::Idle);
        let copy = state;
        assert_eq!(copy, AgentState::Idle);
        assert_ne!(state, AgentState::Completed);
    }

    #[tokio::test]
    async fn test_agent_new() {
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![])),
            test_tool_registry(),
            test_governor(),
            "session-1".into(),
            PathBuf::from("/tmp"),
        );
        assert!(!agent.id().is_empty());
    }

    #[tokio::test]
    async fn test_agent_state_transitions() {
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![])),
            test_tool_registry(),
            test_governor(),
            "session-1".into(),
            PathBuf::from("/tmp"),
        );
        assert_eq!(agent.state().await, AgentState::Idle);
    }

    #[tokio::test]
    async fn test_agent_run_simple_response() {
        let provider = Arc::new(TestProvider::new(vec![ChatResponse {
            message: ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text("Hello, world!".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            usage: None,
            reasoning: None,
            stop_reason: None,
        }]));

        let agent = Agent::new(
            test_agent_config(),
            provider,
            test_tool_registry(),
            test_governor(),
            "session-1".into(),
            PathBuf::from("/tmp"),
        );

        let result = agent.run("hi").await.unwrap();
        assert_eq!(result, "Hello, world!");
        assert_eq!(agent.state().await, AgentState::Completed);
    }

    #[tokio::test]
    async fn successful_response_is_retained_in_history() {
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text("remember me".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                reasoning: None,
                stop_reason: None,
            }])),
            test_tool_registry(),
            test_governor(),
            "history-session".into(),
            PathBuf::from("/tmp"),
        );

        agent.run("hello").await.unwrap();

        let messages = agent.messages.lock().await;
        assert!(messages.iter().any(|message| {
            message.role == ProviderRole::Assistant
                && message.content == MessageContent::Text("remember me".into())
        }));
    }

    #[tokio::test]
    async fn restored_history_keeps_the_agent_system_prompt() {
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![])),
            test_tool_registry(),
            test_governor(),
            "restored-history".into(),
            PathBuf::from("/tmp"),
        );
        agent
            .restore_conversation_history(vec![
                ChatMessage {
                    role: ProviderRole::System,
                    content: MessageContent::Text("untrusted replacement".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                ChatMessage {
                    role: ProviderRole::User,
                    content: MessageContent::Text("Earlier question".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text("Earlier answer".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
            ])
            .await;

        let messages = agent.messages.lock().await;
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, ProviderRole::System);
        assert_eq!(
            messages[0].content,
            MessageContent::Text("You are a test agent.".into())
        );
        assert_eq!(messages[1].role, ProviderRole::User);
        assert_eq!(messages[2].role, ProviderRole::Assistant);
    }

    #[tokio::test]
    async fn reconfiguring_agent_replaces_resource_governor() {
        let new_governor = Arc::new(ResourceGovernor::new(1, 100));
        let provider: Arc<dyn Provider> = Arc::new(TestProvider::new(vec![ChatResponse {
            message: ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text("reconfigured response".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            usage: Some(TokenUsage {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                reasoning_output_tokens: None,
            }),
            reasoning: None,
            stop_reason: None,
        }]));
        let agent = Agent::new(
            AgentConfig {
                ..Default::default()
            },
            Arc::new(TestProvider::new(vec![])),
            test_tool_registry(),
            Arc::new(ResourceGovernor::new(1, 1)),
            "reconfigure-session".into(),
            PathBuf::from("/tmp"),
        );

        agent.reconfigure_provider(provider, "new-model".into(), new_governor.clone());

        let runtime = agent.runtime_snapshot();
        assert_eq!(runtime.model, "new-model");
        assert!(Arc::ptr_eq(&runtime.governor, &new_governor));

        assert_eq!(agent.run("hello").await.unwrap(), "reconfigured response");
        assert_eq!(new_governor.tokens_available(), 100);
    }

    #[tokio::test]
    async fn test_agent_does_not_gate_requests_on_the_legacy_token_budget() {
        let provider = Arc::new(TestProvider::new(vec![]));
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                ..Default::default()
            },
            provider.clone(),
            test_tool_registry(),
            Arc::new(ResourceGovernor::new(1, 1)),
            "reservation-session".into(),
            PathBuf::from("/tmp"),
        );

        let response = agent.run("request").await.unwrap();

        assert_eq!(response, "fallback");
        assert_eq!(*provider.call_count.lock().unwrap(), 1);
        assert_eq!(agent.state().await, AgentState::Completed);
    }

    #[tokio::test]
    async fn test_agent_keeps_provider_responses_when_legacy_token_budget_is_exhausted() {
        let response = ChatResponse {
            message: ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text("budgeted response".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            usage: Some(TokenUsage {
                input_tokens: 3,
                output_tokens: 2,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                reasoning_output_tokens: None,
            }),
            reasoning: None,
            stop_reason: None,
        };
        let provider = Arc::new(TestProvider::new(vec![response.clone(), response]));
        let governor = Arc::new(ResourceGovernor::new(1, 5));
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                ..Default::default()
            },
            provider,
            test_tool_registry(),
            governor.clone(),
            "token-budget-session".into(),
            PathBuf::from("/tmp"),
        );

        assert_eq!(agent.run("first").await.unwrap(), "budgeted response");
        assert_eq!(agent.run("second").await.unwrap(), "budgeted response");
        assert_eq!(agent.state().await, AgentState::Completed);
        assert_eq!(governor.tokens_available(), 5);
    }

    #[tokio::test]
    async fn dropping_in_flight_provider_future_releases_reserved_tokens() {
        let started = Arc::new(tokio::sync::Notify::new());
        let provider = Arc::new(BlockingProvider {
            started: started.clone(),
        });
        let governor = Arc::new(ResourceGovernor::new(1, 10_000));
        let agent = Arc::new(Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                ..Default::default()
            },
            provider,
            test_tool_registry(),
            governor.clone(),
            "cancellation-session".into(),
            PathBuf::from("/tmp"),
        ));

        let task_agent = agent.clone();
        let task = tokio::spawn(async move { task_agent.run("request").await });
        started.notified().await;
        assert_eq!(governor.tokens_available(), 10_000);

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(governor.tokens_available(), 10_000);
        assert_eq!(agent.state().await, AgentState::Cancelled);
    }

    #[tokio::test]
    async fn panicking_provider_future_releases_reserved_tokens() {
        let governor = Arc::new(ResourceGovernor::new(1, 10_000));
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                ..Default::default()
            },
            Arc::new(PanickingProvider),
            test_tool_registry(),
            governor.clone(),
            "panic-session".into(),
            PathBuf::from("/tmp"),
        );

        let task = tokio::spawn(async move { agent.run("request").await });
        assert!(task.await.unwrap_err().is_panic());
        assert_eq!(governor.tokens_available(), 10_000);
    }

    #[tokio::test]
    async fn cache_usage_is_not_double_counted_as_context_input() {
        let provider = Arc::new(TestProvider::new(vec![ChatResponse {
            message: ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text("cached response".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            usage: Some(TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_input_tokens: Some(5),
                cache_creation_input_tokens: Some(7),
                reasoning_output_tokens: Some(2),
            }),
            reasoning: None,
            stop_reason: None,
        }]));
        let governor = Arc::new(ResourceGovernor::new(1, 100));
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                ..Default::default()
            },
            provider,
            test_tool_registry(),
            governor.clone(),
            "cache-session".into(),
            PathBuf::from("/tmp"),
        );

        agent.run("x").await.unwrap();

        assert_eq!(governor.tokens_available(), 100);
        let usage = agent.context_usage().await;
        assert_eq!(usage.input_tokens, Some(0));
        assert_eq!(usage.cache_read_input_tokens, Some(5));
        assert_eq!(usage.cache_creation_input_tokens, Some(7));
        assert_eq!(usage.reasoning_output_tokens, Some(2));
    }

    fn stream_request() -> ChatRequest {
        ChatRequest {
            model: "test-model".into(),
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            system: None,
            reasoning_effort: None,
        }
    }

    #[tokio::test]
    async fn governed_stream_keeps_a_completed_response_above_the_legacy_budget() {
        let provider = Arc::new(StreamProvider::new(false));
        let governor = Arc::new(ResourceGovernor::new(1, 1));
        let governed = GovernedProvider {
            provider,
            governor: governor.clone(),
        };

        let mut stream = governed.chat_stream(stream_request()).await.unwrap();

        assert_eq!(governor.active_calls(), 1);
        assert_eq!(governor.tokens_available(), 1);
        assert!(stream.next().await.is_some());
        assert_eq!(governor.active_calls(), 1);

        assert!(matches!(
            stream.next().await,
            Some(Ok(StreamEvent::MessageEnd { .. }))
        ));
        assert_eq!(governor.active_calls(), 0);
        assert_eq!(governor.tokens_available(), 1);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn run_streaming_emits_text_deltas_and_returns_complete_response() {
        let agent = Agent::new(
            AgentConfig {
                tools: Vec::new(),
                ..Default::default()
            },
            Arc::new(StreamProvider::new(true)),
            test_tool_registry(),
            test_governor(),
            "stream-session".into(),
            PathBuf::from("/tmp"),
        );
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        let response = agent.run_streaming("hello", sender).await.unwrap();
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }

        assert_eq!(response, "part");
        assert!(events.iter().any(|event| matches!(
            event,
            AgentStreamEvent::TextDelta { text } if text == "part"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentStreamEvent::ReasoningDelta { text }
                if text.contains("**Inspecting**")
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentStreamEvent::ReasoningFinished)));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentStreamEvent::ContextUpdated { usage }
                if usage.input_tokens == Some(2) && usage.output_tokens == Some(3)
        )));
    }

    #[tokio::test]
    async fn streaming_marks_tools_announced_during_reasoning() {
        let agent = Agent::new(
            AgentConfig {
                tools: vec!["echo".into()],
                max_iterations: 2,
                ..Default::default()
            },
            Arc::new(ReasoningToolStreamProvider::new()),
            test_tool_registry(),
            test_governor(),
            "reasoning-tool-session".into(),
            PathBuf::from("/tmp"),
        );
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        agent.run_streaming("inspect", sender).await.unwrap();
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }

        let preparing = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    AgentStreamEvent::ToolPreparing {
                        inside_reasoning: true,
                        ..
                    }
                )
            })
            .expect("the tool should retain its reasoning association");
        let reasoning_finished = events
            .iter()
            .position(|event| matches!(event, AgentStreamEvent::ReasoningFinished))
            .expect("the reasoning phase should eventually finish");

        assert!(preparing < reasoning_finished);
    }

    #[tokio::test]
    async fn governed_stream_does_not_charge_when_usage_is_missing() {
        let provider = Arc::new(IncompleteStreamProvider);
        let governor = Arc::new(ResourceGovernor::new(1, 100));
        let governed = GovernedProvider {
            provider,
            governor: governor.clone(),
        };

        let mut stream = governed.chat_stream(stream_request()).await.unwrap();

        assert!(matches!(
            stream.next().await,
            Some(Ok(StreamEvent::MessageEnd { usage: None }))
        ));
        assert_eq!(governor.tokens_available(), 100);
        assert_eq!(governor.active_calls(), 0);
    }

    #[tokio::test]
    async fn governed_stream_does_not_charge_when_stream_usage_is_missing() {
        let provider = Arc::new(TruncatedStreamProvider);
        let governor = Arc::new(ResourceGovernor::new(1, 100));
        let governed = GovernedProvider {
            provider,
            governor: governor.clone(),
        };

        let mut stream = governed.chat_stream(stream_request()).await.unwrap();

        assert!(stream.next().await.is_some());
        assert!(stream.next().await.is_none());
        assert_eq!(governor.tokens_available(), 100);
        assert_eq!(governor.active_calls(), 0);
    }

    #[tokio::test]
    async fn governed_stream_releases_on_drop_and_provider_error() {
        let governor = Arc::new(ResourceGovernor::new(1, 100));
        let provider = Arc::new(StreamProvider::new(false));
        let governed = GovernedProvider {
            provider,
            governor: governor.clone(),
        };
        let stream = governed.chat_stream(stream_request()).await.unwrap();

        drop(stream);
        assert_eq!(governor.active_calls(), 0);
        assert_eq!(governor.tokens_available(), 100);

        let error_provider = Arc::new(ErrorStreamProvider);
        let error_governed = GovernedProvider {
            provider: error_provider,
            governor: governor.clone(),
        };
        let mut error_stream = error_governed.chat_stream(stream_request()).await.unwrap();
        assert!(matches!(
            error_stream.next().await,
            Some(Err(ProviderError::Other(_)))
        ));
        assert_eq!(governor.active_calls(), 0);
        assert_eq!(governor.tokens_available(), 100);
    }

    #[tokio::test]
    async fn governed_stream_drops_inner_on_terminal_events() {
        for mode in [
            DropStreamMode::MessageEnd,
            DropStreamMode::ProviderError,
            DropStreamMode::EventError,
            DropStreamMode::Eof,
        ] {
            let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let provider = DropAwareStreamProvider {
                mode,
                dropped: dropped.clone(),
            };
            let governor = Arc::new(ResourceGovernor::new(1, 100));
            let governed = GovernedProvider {
                provider: Arc::new(provider),
                governor,
            };
            let mut stream = governed.chat_stream(stream_request()).await.unwrap();

            let _ = stream.next().await;

            assert!(dropped.load(Ordering::SeqCst));
        }
    }

    #[tokio::test]
    async fn concurrent_runs_on_one_agent_are_serialized() {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(GatedProvider {
            started: started.clone(),
            release: release.clone(),
            calls: calls.clone(),
        });
        let agent = Arc::new(Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                ..Default::default()
            },
            provider,
            test_tool_registry(),
            Arc::new(ResourceGovernor::new(2, 10_000)),
            "serialized-session".into(),
            PathBuf::from("/tmp"),
        ));

        let first_agent = agent.clone();
        let first = tokio::spawn(async move { first_agent.run("first").await });
        started.notified().await;

        let second_agent = agent.clone();
        let second = tokio::spawn(async move { second_agent.run("second").await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        release.notify_one();
        started.notified().await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        release.notify_one();

        assert!(first.await.unwrap().is_ok());
        assert!(second.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn summary_compaction_respects_token_budget() {
        let provider = Arc::new(TestProvider::new(vec![]));
        let governor = Arc::new(ResourceGovernor::new(1, 0));
        let agent = compaction_agent(CompactionStrategyType::Summary, provider.clone(), governor);
        seed_compaction_messages(&agent).await;

        let runtime = agent.runtime_snapshot();
        assert!(agent.compact_with_runtime(&runtime).await.is_ok());
        assert_eq!(*provider.call_count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn summary_compaction_charges_reserved_tokens() {
        let provider = Arc::new(TestProvider::new(vec![]));
        let governor = Arc::new(ResourceGovernor::new(1, 100_000));
        let agent = compaction_agent(
            CompactionStrategyType::Summary,
            provider.clone(),
            governor.clone(),
        );
        seed_compaction_messages(&agent).await;
        let available_before = governor.tokens_available();

        let runtime = agent.runtime_snapshot();
        agent.compact_with_runtime(&runtime).await.unwrap();

        assert_eq!(governor.tokens_available(), available_before);
        assert_eq!(*provider.call_count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn summary_compaction_respects_concurrency_limit() {
        let provider = Arc::new(TestProvider::new(vec![]));
        let governor = Arc::new(ResourceGovernor::new(1, 100_000));
        let agent = compaction_agent(
            CompactionStrategyType::Summary,
            provider.clone(),
            governor.clone(),
        );
        seed_compaction_messages(&agent).await;
        let permit = governor.acquire_call_permit().await;

        let runtime = agent.runtime_snapshot();
        let result = tokio::time::timeout(
            Duration::from_millis(20),
            agent.compact_with_runtime(&runtime),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(*provider.call_count.lock().unwrap(), 0);
        drop(permit);
    }

    #[tokio::test]
    async fn compaction_trigger_uses_actual_request_shape() {
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: vec!["echo".into()],
                compaction: CompactionConfig {
                    strategy: CompactionStrategyType::Summary,
                    threshold: 0.8,
                    keep_last: 1,
                },
                ..Default::default()
            },
            Arc::new(SmallContextProvider),
            test_tool_registry(),
            Arc::new(ResourceGovernor::new(1, 100_000)),
            "request-shape-session".into(),
            PathBuf::from("/tmp"),
        );
        *agent.messages.lock().await = vec![
            ChatMessage {
                role: ProviderRole::User,
                content: MessageContent::Text("a".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text("b".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: ProviderRole::User,
                content: MessageContent::Text("c".into()),
                tool_call_id: None,
                tool_calls: None,
            },
        ];
        *agent.last_context_usage.lock().unwrap() = Some(ContextUsage::from_usage(9, 1, 10));

        let runtime = agent.runtime_snapshot();
        assert!(agent.should_compact_with_runtime(&runtime).await);

        *agent.last_context_usage.lock().unwrap() = Some(ContextUsage::from_usage(7, 1, 10));
        assert!(!agent.should_compact_with_runtime(&runtime).await);
    }

    #[tokio::test]
    async fn hybrid_compaction_can_trigger_for_a_short_tool_heavy_history() {
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                compaction: CompactionConfig {
                    strategy: CompactionStrategyType::Hybrid,
                    threshold: 0.8,
                    keep_last: 20,
                },
                ..Default::default()
            },
            Arc::new(SmallContextProvider),
            test_tool_registry(),
            Arc::new(ResourceGovernor::new(1, 100_000)),
            "short-heavy-session".into(),
            PathBuf::from("/tmp"),
        );
        *agent.messages.lock().await = vec![
            ChatMessage {
                role: ProviderRole::User,
                content: MessageContent::Text("question".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text("tool call".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: ProviderRole::Tool,
                content: MessageContent::Text("large result".into()),
                tool_call_id: Some("call-1".into()),
                tool_calls: None,
            },
        ];
        *agent.last_context_usage.lock().unwrap() = Some(ContextUsage::from_usage(9, 0, 10));

        let runtime = agent.runtime_snapshot();

        assert!(agent.should_compact_with_runtime(&runtime).await);
    }

    #[tokio::test]
    async fn completed_compaction_discards_the_pre_compaction_usage_sample() {
        let provider = Arc::new(TestProvider::new(vec![]));
        let agent = compaction_agent(
            CompactionStrategyType::Summary,
            provider,
            Arc::new(ResourceGovernor::new(1, 100_000)),
        );
        seed_compaction_messages(&agent).await;
        *agent.last_context_usage.lock().unwrap() =
            Some(ContextUsage::from_usage(180_000, 20, 200_000));

        let runtime = agent.runtime_snapshot();
        agent.compact_with_runtime(&runtime).await.unwrap();

        assert!(agent.last_context_usage.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn compaction_does_not_use_history_signals_without_provider_usage() {
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: vec!["echo".into()],
                compaction: CompactionConfig {
                    strategy: CompactionStrategyType::Summary,
                    threshold: 0.8,
                    keep_last: 1,
                },
                ..Default::default()
            },
            Arc::new(SmallContextProvider),
            test_tool_registry(),
            Arc::new(ResourceGovernor::new(1, 100_000)),
            "no-usage-signal-session".into(),
            PathBuf::from("/tmp"),
        );
        *agent.messages.lock().await = vec![
            ChatMessage {
                role: ProviderRole::User,
                content: MessageContent::Text("repeat this exact request".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text("repeat this exact answer".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: ProviderRole::User,
                content: MessageContent::Text("repeat this exact request".into()),
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        let runtime = agent.runtime_snapshot();
        assert!(!agent.should_compact_with_runtime(&runtime).await);
    }

    #[tokio::test]
    async fn compaction_rejects_result_that_still_exceeds_context_window() {
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                compaction: CompactionConfig {
                    strategy: CompactionStrategyType::Summary,
                    keep_last: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
            Arc::new(SmallContextProvider),
            test_tool_registry(),
            Arc::new(ResourceGovernor::new(1, 100_000)),
            "post-compaction-session".into(),
            PathBuf::from("/tmp"),
        );
        let original = vec![
            ChatMessage {
                role: ProviderRole::User,
                content: MessageContent::Text("a".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text("b".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: ProviderRole::User,
                content: MessageContent::Text("c".into()),
                tool_call_id: None,
                tool_calls: None,
            },
        ];
        *agent.messages.lock().await = original.clone();

        let runtime = agent.runtime_snapshot();
        agent.compact_with_runtime(&runtime).await.unwrap();
        assert_ne!(*agent.messages.lock().await, original);
    }

    #[tokio::test]
    async fn compaction_fits_a_large_latest_tool_payload_instead_of_failing() {
        let provider = Arc::new(TestProvider::new(vec![]));
        let agent = compaction_agent(
            CompactionStrategyType::Summary,
            provider.clone(),
            Arc::new(ResourceGovernor::new(1, 2_000_000)),
        );
        let mut messages = compaction_messages();
        messages.push(ChatMessage {
            role: ProviderRole::User,
            content: MessageContent::Text("large tool payload ".repeat(45_000)),
            tool_call_id: None,
            tool_calls: None,
        });
        *agent.messages.lock().await = messages;

        let runtime = agent.runtime_snapshot();
        agent.compact_with_runtime(&runtime).await.unwrap();

        let compacted = agent.messages.lock().await.clone();
        assert_eq!(compacted.len(), 2);
        assert_eq!(agent.understood_context().as_deref(), Some("fallback"));
    }

    #[tokio::test]
    async fn trim_compaction_does_not_call_provider_or_charge_tokens() {
        let provider = Arc::new(TestProvider::new(vec![]));
        let governor = Arc::new(ResourceGovernor::new(1, 0));
        let agent = compaction_agent(
            CompactionStrategyType::Trim,
            provider.clone(),
            governor.clone(),
        );
        seed_compaction_messages(&agent).await;
        let available_before = governor.tokens_available();

        let runtime = agent.runtime_snapshot();
        let result = agent.compact_with_runtime(&runtime).await;

        assert!(result.is_ok());
        assert_eq!(*provider.call_count.lock().unwrap(), 0);
        assert_eq!(governor.tokens_available(), available_before);
    }

    #[tokio::test]
    async fn test_agent_run_with_tool_calls() {
        let provider = Arc::new(TestProvider::new(vec![
            ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text(String::new()),
                    tool_call_id: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "tc_1".into(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: "echo".into(),
                            arguments: r#"{"text": "hello from tool"}"#.into(),
                        },
                    }]),
                },
                usage: None,
                reasoning: None,
                stop_reason: None,
            },
            ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text("Done after tool.".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                reasoning: None,
                stop_reason: None,
            },
        ]));

        let agent = Agent::new(
            test_agent_config(),
            provider,
            test_tool_registry(),
            test_governor(),
            "session-2".into(),
            PathBuf::from("/tmp"),
        );

        let result = agent.run("do something").await.unwrap();
        assert_eq!(result, "Done after tool.");
        assert_eq!(agent.state().await, AgentState::Completed);
    }

    #[tokio::test]
    async fn canceled_tool_execution_does_not_append_partial_history() {
        let started = Arc::new(tokio::sync::Notify::new());
        let provider = Arc::new(TestProvider::new(vec![ChatResponse {
            message: ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text(String::new()),
                tool_call_id: None,
                tool_calls: Some(vec![ToolCall {
                    id: "tc_block".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "block".into(),
                        arguments: "{}".into(),
                    },
                }]),
            },
            usage: None,
            reasoning: None,
            stop_reason: None,
        }]));
        let agent = Arc::new(Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: vec!["block".into()],
                max_iterations: 1,
                ..Default::default()
            },
            provider,
            blocking_tool_registry(started.clone()),
            Arc::new(ResourceGovernor::new(1, 10_000)),
            "tool-cancel-session".into(),
            PathBuf::from("/tmp"),
        ));

        let task_agent = agent.clone();
        let task = tokio::spawn(async move { task_agent.run("run the tool").await });
        started.notified().await;

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(agent.state().await, AgentState::Cancelled);

        let messages = agent.messages.lock().await;
        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0].role, ProviderRole::User));
    }

    #[tokio::test]
    async fn test_agent_synthesizes_after_tool_iteration_budget_is_exhausted() {
        let provider = Arc::new(TestProvider::new({
            let mut responses = Vec::new();
            for index in 0..2 {
                responses.push(ChatResponse {
                    message: ChatMessage {
                        role: ProviderRole::Assistant,
                        content: MessageContent::Text(String::new()),
                        tool_call_id: None,
                        tool_calls: Some(vec![ToolCall {
                            id: format!("tc_loop_{index}"),
                            call_type: "function".into(),
                            function: FunctionCall {
                                name: "echo".into(),
                                arguments: r#"{"text": "loop"}"#.into(),
                            },
                        }]),
                    },
                    usage: None,
                    reasoning: None,
                    stop_reason: None,
                });
            }
            responses.push(ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text(
                        "I completed two tool rounds and still need to continue.".into(),
                    ),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                reasoning: None,
                stop_reason: None,
            });
            responses
        }));

        let agent = Agent::new(
            AgentConfig {
                max_iterations: 2,
                ..test_agent_config()
            },
            provider.clone(),
            test_tool_registry(),
            test_governor(),
            "session-3".into(),
            PathBuf::from("/tmp"),
        );

        let result = agent.run("loop forever").await.unwrap();
        assert_eq!(
            result,
            "I completed two tool rounds and still need to continue."
        );
        assert_eq!(agent.state().await, AgentState::Completed);

        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(!requests[0].tools.is_empty());
        assert!(!requests[1].tools.is_empty());
        assert!(requests[2].tools.is_empty());
        assert!(requests[2].messages.iter().any(|message| {
            message.role == ProviderRole::System
                && message_text(message).contains("Tool execution budget reached")
        }));
    }

    #[tokio::test]
    async fn test_agent_builds_tool_definitions() {
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![])),
            test_tool_registry(),
            test_governor(),
            "session-4".into(),
            PathBuf::from("/tmp"),
        );

        let defs = agent.build_tool_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "echo");
    }

    #[test]
    fn delegated_tool_names_are_reserved_for_parent_agents() {
        assert!(is_delegation_tool("list_agents"));
        assert!(is_delegation_tool("call_agents"));
        assert!(is_delegation_tool("call_agent"));
        assert!(!is_delegation_tool("web_search_intrernal"));
    }

    #[tokio::test]
    async fn test_agent_discovers_skill_tools_before_activating_them() {
        let workspace = tempfile::tempdir().unwrap();
        let skills = workspace.path().join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("release.md"),
            "# Release workflow\n\nAlways verify the changelog before publishing.\n",
        )
        .unwrap();
        let index = Arc::new(
            crate::skill::SkillIndex::build(crate::skill::SkillLoader::new(vec![skills])).unwrap(),
        );
        let registry = test_tool_registry();
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![])),
            registry.clone(),
            test_governor(),
            "session-skills".into(),
            workspace.path().to_path_buf(),
        );

        assert!(agent
            .build_tool_definitions()
            .iter()
            .all(|tool| { tool.name != "list_skills" && tool.name != "load_skill" }));

        agent.set_skill_index(Some(index));

        let definitions = agent.build_tool_definitions();
        assert!(definitions.iter().all(|tool| tool.name != "list_skills"));
        assert!(definitions.iter().all(|tool| tool.name != "load_skill"));
        agent
            .tool_activation
            .enable(
                &registry.catalog(),
                vec!["list_skills".into(), "load_skill".into()],
            )
            .unwrap();
        let definitions = agent.build_tool_definitions();
        assert!(definitions.iter().any(|tool| tool.name == "list_skills"));
        assert!(definitions.iter().any(|tool| tool.name == "load_skill"));
        let context = agent
            .resolve_skill_context("Please use the release skill for this task.")
            .await
            .unwrap();
        assert!(context.contains("release"));
        assert!(context.contains("Always verify the changelog"));
    }

    #[tokio::test]
    async fn skill_catalog_prioritizes_all_names_over_verbose_descriptions() {
        let workspace = tempfile::tempdir().unwrap();
        let skills = workspace.path().join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        for index in 0..30 {
            std::fs::write(
                skills.join(format!("a-{index:02}.md")),
                format!("# {}\n", "verbose description ".repeat(35)),
            )
            .unwrap();
        }
        std::fs::write(skills.join("z-daily-work.md"), "# Daily work\n").unwrap();
        let index = Arc::new(
            crate::skill::SkillIndex::build(crate::skill::SkillLoader::new(vec![skills])).unwrap(),
        );
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![])),
            test_tool_registry(),
            test_governor(),
            "compact-skill-catalog".into(),
            workspace.path().to_path_buf(),
        );
        agent.set_skill_index(Some(index));

        let context = agent
            .resolve_skill_context("What should I focus on?")
            .await
            .unwrap();

        assert!(context.contains("`z-daily-work`"));
        assert!(context.len() <= MAX_SKILL_CATALOG_BYTES);
        assert!(!context.contains("verbose description"));
    }

    #[tokio::test]
    async fn test_agent_system_prompt() {
        let config = AgentConfig {
            system_prompt: Some("You are helpful.".into()),
            tools: vec![],
            ..test_agent_config()
        };

        let provider = Arc::new(TestProvider::new(vec![ChatResponse {
            message: ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text("ok".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            usage: None,
            reasoning: None,
            stop_reason: None,
        }]));

        let agent = Agent::new(
            config,
            provider,
            test_tool_registry(),
            test_governor(),
            "session-5".into(),
            PathBuf::from("/tmp"),
        );

        let result = agent.run("test").await.unwrap();
        assert_eq!(result, "ok");
    }

    #[test]
    fn global_memory_is_injected_as_a_separate_system_message() {
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![])),
            test_tool_registry(),
            test_governor(),
            "global-memory-session".into(),
            PathBuf::from("/tmp"),
        );
        agent.set_global_memory_prompt(Some(
            "## Confirmed Global Memory\n- [abcd1234] Prefer concise answers.".into(),
        ));

        let request = agent.build_request(
            vec![ChatMessage {
                role: ProviderRole::System,
                content: MessageContent::Text("Base instructions".into()),
                tool_call_id: None,
                tool_calls: None,
            }],
            "test-model".into(),
            None,
        );

        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, ProviderRole::System);
        assert_eq!(request.messages[1].role, ProviderRole::System);
        assert!(matches!(
            &request.messages[1].content,
            MessageContent::Text(content) if content.contains("Confirmed Global Memory")
        ));
    }

    #[test]
    fn understood_context_is_injected_as_system_context() {
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![])),
            test_tool_registry(),
            test_governor(),
            "understood-context-session".into(),
            PathBuf::from("/tmp"),
        );
        agent.set_understood_context(Some(
            "Objective: ship the release.\nNext action: run the checks.".into(),
        ));

        let request = agent.build_request(
            vec![ChatMessage {
                role: ProviderRole::System,
                content: MessageContent::Text("Base instructions".into()),
                tool_call_id: None,
                tool_calls: None,
            }],
            "test-model".into(),
            None,
        );

        assert_eq!(request.messages.len(), 2);
        assert!(matches!(
            &request.messages[1].content,
            MessageContent::Text(content) if content.contains("Understood conversation context")
        ));
    }
}

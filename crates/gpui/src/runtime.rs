use crate::session::SessionId;
use async_trait::async_trait;
use averroes_core::agent::orchestration::{
    AgentCallRequest, AgentDescriptor, AgentRunner, AgentThreadSnapshot, AgentThreadStatus,
};
use averroes_core::agent::{Agent, AgentConfig, AgentStreamEvent};
use averroes_core::codex::{CodexAccount, CodexClient, CodexError, CodexLogin, CodexModel};
use averroes_core::compaction::{CompactionConfig, CompactionStrategyType};
use averroes_core::config::AgentProfile;
use averroes_core::config::{AppConfig, ConfigError, ConfigPaths};
use averroes_core::connection::{ConnectionId, ConnectionKind, ConnectionProfile, SessionBinding};
use averroes_core::credentials::{CredentialVault, VaultError, VaultKeyProvider};
use averroes_core::diagnostics::{self, DiagnosticLevel};
use averroes_core::github::{
    CopilotEndpoint, CopilotModel, GitHubCopilotClient, GitHubError, GitHubLogin,
};
use averroes_core::memory::{compile_fragments_with_context, cosine_similarity, decode_embedding};
use averroes_core::models::{ManualModel, ModelRegistry};
use averroes_core::prompt::{ProjectInstructions, PromptBuilder};
use averroes_core::provider::codex::ChatGptCodexProvider;
use averroes_core::provider::factory::{
    create_copilot_provider, create_direct_provider, ProviderFactoryError,
};
use averroes_core::provider::{
    EmbeddingRequest, ModelDiscovery, ModelInfo, Provider, ProviderRegistry,
};
use averroes_core::runtime::ResourceGovernor;
use averroes_core::skill::{SkillIndex, SkillLoader};
use averroes_core::tool::builtin::ask_user::{AskUserBroker, AskUserParams, UserQuestion};
use averroes_core::tool::{builtin, MemorySearchBackend, ToolRegistry};
use averroes_core::work::{
    ConversationSearchResult, EmbeddingConfig, VectorSearchHit, WorkDatabase, WorkDatabaseError,
};
use futures::stream::{self, StreamExt};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DEFAULT_COMPACTION_THRESHOLD: f64 = 0.8;
const MAX_CONCURRENT_CALLS: usize = tokio::sync::Semaphore::MAX_PERMITS;

/// Provider-neutral application services.
///
/// A provider is created only after a conversation explicitly selects both a
/// connection and a model, so an empty installation can always reach the UI.
pub struct AppRuntime {
    config: Arc<RwLock<AppConfig>>,
    paths: ConfigPaths,
    vault: Arc<CredentialVault>,
    pub tools: Arc<ToolRegistry>,
    pub governor: Arc<ResourceGovernor>,
    pub runtime: Arc<tokio::runtime::Runtime>,
    pub database: Arc<WorkDatabase>,
    pub model_registry: Arc<ModelRegistry>,
    prompt: PromptBuilder,
    codex: Arc<tokio::sync::Mutex<Option<Arc<CodexClient>>>>,
    copilot: Arc<tokio::sync::Mutex<Option<Arc<GitHubCopilotClient>>>>,
    copilot_models: RwLock<HashMap<ConnectionId, Vec<CopilotModel>>>,
    workspace_skills: RwLock<HashMap<PathBuf, Arc<SkillIndex>>>,
    workspace_instructions: RwLock<HashMap<PathBuf, Arc<ProjectInstructions>>>,
    provider_hooks: ProviderRegistry,
    agent_threads: Arc<AgentThreadRegistry>,
    user_questions: Arc<AskUserBroker>,
}

pub struct AgentThreadRegistry {
    threads: RwLock<HashMap<String, AgentThreadSnapshot>>,
    contexts: RwLock<HashMap<String, Vec<averroes_core::provider::ChatMessage>>>,
    locks: RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl AgentThreadRegistry {
    fn new() -> Self {
        Self {
            threads: RwLock::new(HashMap::new()),
            contexts: RwLock::new(HashMap::new()),
            locks: RwLock::new(HashMap::new()),
        }
    }

    fn upsert(&self, thread: AgentThreadSnapshot) {
        self.threads.write().insert(thread.id.clone(), thread);
    }

    fn get(&self, thread_id: &str) -> Option<AgentThreadSnapshot> {
        self.threads.read().get(thread_id).cloned()
    }

    fn context(&self, thread_id: &str) -> Option<Vec<averroes_core::provider::ChatMessage>> {
        self.contexts.read().get(thread_id).cloned()
    }

    fn set_context(&self, thread_id: &str, context: Vec<averroes_core::provider::ChatMessage>) {
        self.contexts.write().insert(thread_id.to_owned(), context);
    }

    fn lock_for(&self, thread_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        if let Some(lock) = self.locks.read().get(thread_id) {
            return lock.clone();
        }
        let mut locks = self.locks.write();
        locks
            .entry(thread_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub fn for_parent(&self, parent_session_id: &str) -> Vec<AgentThreadSnapshot> {
        let mut threads = self
            .threads
            .read()
            .values()
            .filter(|thread| thread.parent_session_id == parent_session_id)
            .cloned()
            .collect::<Vec<_>>();
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
        threads
    }
}

impl Default for AgentThreadRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolves the explicit connection assigned to a configured delegated
/// agent. It shares the application credential clients, so child agents do
/// not create a second login session for Codex or Copilot.
struct RuntimeProviderResolver {
    config: Arc<RwLock<AppConfig>>,
    vault: Arc<CredentialVault>,
    codex: Arc<tokio::sync::Mutex<Option<Arc<CodexClient>>>>,
    copilot: Arc<tokio::sync::Mutex<Option<Arc<GitHubCopilotClient>>>>,
}

impl RuntimeProviderResolver {
    async fn provider_for(
        &self,
        connection_id: &str,
        model: &str,
    ) -> Result<Arc<dyn Provider>, String> {
        let id = ConnectionId(connection_id.to_owned());
        let profile = self
            .config
            .read()
            .connection(&id)
            .cloned()
            .ok_or_else(|| format!("connection '{connection_id}' is not configured"))?;
        match profile.kind {
            ConnectionKind::Codex => Ok(Arc::new(ChatGptCodexProvider::new(
                self.codex_client().await?,
                model,
            ))),
            ConnectionKind::Copilot => {
                let credential = profile
                    .credential_ref
                    .as_ref()
                    .ok_or_else(|| "Copilot connection has no credential".to_owned())?;
                let client = self.copilot_client().await?;
                let selected = client
                    .list_models(credential)
                    .await
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .find(|entry| entry.id == model)
                    .ok_or_else(|| {
                        format!(
                            "model '{model}' is not available for GitHub Copilot connection '{connection_id}'"
                        )
                    })?;
                let secret = client
                    .copilot_api_token(credential)
                    .await
                    .map_err(|error| error.to_string())?;
                let mut request_profile = profile;
                request_profile.base_url = Some(selected.api_base_url);
                create_copilot_provider(&request_profile, &secret, model, selected.endpoint)
                    .map_err(|error| error.to_string())
            }
            ConnectionKind::Ollama => {
                create_direct_provider(&profile, "", model).map_err(|error| error.to_string())
            }
            _ => {
                let credential = profile
                    .credential_ref
                    .as_ref()
                    .ok_or_else(|| format!("connection '{connection_id}' has no credential"))?;
                let secret = self
                    .vault
                    .get(credential)
                    .map_err(|error| error.to_string())?;
                create_direct_provider(&profile, &secret, model).map_err(|error| error.to_string())
            }
        }
    }

    async fn codex_client(&self) -> Result<Arc<CodexClient>, String> {
        let mut client = self.codex.lock().await;
        if let Some(client) = client.as_ref() {
            return Ok(client.clone());
        }
        let connected = CodexClient::connect(self.vault.clone())
            .await
            .map_err(|error| error.to_string())?;
        *client = Some(connected.clone());
        Ok(connected)
    }

    async fn copilot_client(&self) -> Result<Arc<GitHubCopilotClient>, String> {
        let mut client = self.copilot.lock().await;
        if let Some(client) = client.as_ref() {
            return Ok(client.clone());
        }
        let connected =
            GitHubCopilotClient::connect(self.vault.clone()).map_err(|error| error.to_string())?;
        *client = Some(connected.clone());
        Ok(connected)
    }
}

struct RuntimeAgentRunner {
    tool_registry: Arc<ToolRegistry>,
    governor: Arc<ResourceGovernor>,
    system_prompt: String,
    default_model: String,
    compaction: CompactionConfig,
    reasoning_effort: Option<String>,
    threads: Arc<AgentThreadRegistry>,
    config: Arc<RwLock<AppConfig>>,
    provider_resolver: Arc<RuntimeProviderResolver>,
    parent_connection_id: ConnectionId,
    memory_search_backend: Option<Arc<dyn MemorySearchBackend>>,
    global_memory_prompt: Option<String>,
}

#[async_trait]
impl AgentRunner for RuntimeAgentRunner {
    async fn list_agents(&self, _parent_session_id: &str) -> Result<Vec<AgentDescriptor>, String> {
        let mut agents = vec![AgentDescriptor::default()];
        agents.extend(
            self.config
                .read()
                .agents
                .iter()
                .map(|agent| AgentDescriptor {
                    id: agent.id.clone(),
                    name: agent.name.clone(),
                    description: agent.description.clone(),
                    connection_id: Some(agent.connection_id.clone()),
                    model_id: Some(agent.model_id.clone()),
                }),
        );
        Ok(agents)
    }

    async fn call_agent(&self, request: AgentCallRequest) -> Result<AgentThreadSnapshot, String> {
        self.run_agent(request, None).await
    }

    async fn call_agent_streaming(
        &self,
        request: AgentCallRequest,
        events: tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>,
    ) -> Result<AgentThreadSnapshot, String> {
        self.run_agent(request, Some(events)).await
    }
}

impl RuntimeAgentRunner {
    async fn run_agent(
        &self,
        request: AgentCallRequest,
        parent_events: Option<tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>>,
    ) -> Result<AgentThreadSnapshot, String> {
        let thread_id = request.thread_id.trim();
        if thread_id.is_empty() {
            return Err("thread_id cannot be empty".into());
        }
        let descriptor = self
            .list_agents(&request.parent_session_id)
            .await?
            .into_iter()
            .find(|agent| agent.id == request.agent_id)
            .ok_or_else(|| format!("agent '{}' is not available", request.agent_id))?;
        let lock = self.threads.lock_for(thread_id);
        let _thread_guard = lock.lock().await;
        if let Some(existing) = self.threads.get(thread_id) {
            if existing.agent_id != request.agent_id {
                return Err(format!(
                    "thread '{thread_id}' already belongs to agent '{}'",
                    existing.agent_id
                ));
            }
        }
        let configured = self
            .config
            .read()
            .agents
            .iter()
            .find(|agent| agent.id == request.agent_id)
            .cloned();
        let connection_id = configured
            .as_ref()
            .map(|agent| agent.connection_id.clone())
            .or_else(|| descriptor.connection_id.clone())
            .unwrap_or_else(|| self.parent_connection_id.to_string());
        let model_id = request
            .model_id
            .filter(|model| !model.trim().is_empty() && model != "default")
            .or_else(|| configured.as_ref().map(|agent| agent.model_id.clone()))
            .or_else(|| descriptor.model_id.clone())
            .unwrap_or_else(|| self.default_model.clone());
        let title = delegated_agent_title(&request.prompt);
        let now = averroes_core::work::now();
        let running = AgentThreadSnapshot {
            id: thread_id.to_owned(),
            thread_id: thread_id.to_owned(),
            agent_id: request.agent_id.clone(),
            parent_session_id: request.parent_session_id.clone(),
            title,
            model_id: model_id.clone(),
            status: AgentThreadStatus::Running,
            prompt: request.prompt.clone(),
            output: String::new(),
            created_at: now,
            updated_at: now,
        };
        self.threads.upsert(running.clone());

        let provider = self
            .provider_resolver
            .provider_for(&connection_id, &model_id)
            .await?;
        let context = self
            .threads
            .context(thread_id)
            .unwrap_or_else(|| request.context.clone());
        let parent_context = if request.parent_objective.trim().is_empty() {
            String::new()
        } else {
            format!(
                "\n\n## Parent objective\nThe parent agent is working toward this objective:\n{}\n",
                request.parent_objective.trim()
            )
        };
        let tools = request
            .tools
            .iter()
            .filter(|tool| !matches!(tool.as_str(), "list_agents" | "call_agent" | "call_agents"))
            .cloned()
            .collect::<Vec<_>>();
        let agent = Arc::new(Agent::new(
            AgentConfig {
                name: format!("delegated-{}", &thread_id[..thread_id.len().min(8)]),
                model: model_id,
                system_prompt: Some(format!(
                    "{}{parent_context}\n\n## Delegation boundary\nYou are a delegated leaf agent. Do not call `list_agents`, `call_agent`, or `call_agents`, and do not start another subagent. Complete the assigned objective yourself and return your result to the parent agent.",
                    self.system_prompt
                )),
                // A delegated agent receives precisely the parent's current
                // tool set, including discovery and delegation tools.
                tools: tools.clone(),
                max_iterations: 12,
                compaction: self.compaction.clone(),
                reasoning_effort: self.reasoning_effort.clone(),
                allow_delegation: false,
                ..Default::default()
            },
            provider,
            self.tool_registry.clone(),
            self.governor.clone(),
            format!("agent-thread:{thread_id}"),
            request.working_dir,
        ));
        agent.set_memory_search_backend(self.memory_search_backend.clone());
        agent.set_global_memory_prompt(self.global_memory_prompt.clone());
        agent.restore_conversation_history(context).await;
        agent.set_agent_runner(Arc::new(RuntimeAgentRunner {
            tool_registry: self.tool_registry.clone(),
            governor: self.governor.clone(),
            system_prompt: self.system_prompt.clone(),
            default_model: self.default_model.clone(),
            compaction: self.compaction.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            threads: self.threads.clone(),
            config: self.config.clone(),
            provider_resolver: self.provider_resolver.clone(),
            parent_connection_id: ConnectionId(connection_id),
            memory_search_backend: self.memory_search_backend.clone(),
            global_memory_prompt: self.global_memory_prompt.clone(),
        }));

        if let Some(events) = parent_events.as_ref() {
            let _ = events.send(AgentStreamEvent::DelegatedAgentStarted {
                thread: running.clone(),
            });
        }

        let result = if let Some(parent_events) = parent_events {
            let (child_sender, mut child_events) = tokio::sync::mpsc::unbounded_channel();
            let child_agent = agent.clone();
            let child_prompt = request.prompt.clone();
            let child_task = tokio::spawn(async move {
                child_agent.run_streaming(&child_prompt, child_sender).await
            });
            while let Some(event) = child_events.recv().await {
                let _ = parent_events.send(AgentStreamEvent::DelegatedAgentEvent {
                    thread_id: thread_id.to_owned(),
                    event: Box::new(event),
                });
            }
            child_task
                .await
                .map_err(|error| format!("delegated agent task failed: {error}"))?
        } else {
            agent.run(&request.prompt).await
        };
        self.threads
            .set_context(thread_id, agent.conversation_history().await);
        let now = averroes_core::work::now();
        let finished = match result {
            Ok(output) => AgentThreadSnapshot {
                status: AgentThreadStatus::Completed,
                output,
                updated_at: now,
                ..running
            },
            Err(error) => AgentThreadSnapshot {
                status: AgentThreadStatus::Failed,
                output: error.to_string(),
                updated_at: now,
                ..running
            },
        };
        self.threads.upsert(finished.clone());
        Ok(finished)
    }
}

struct RuntimeMemorySearchBackend {
    database: Arc<WorkDatabase>,
    provider: Arc<dyn Provider>,
    connection_id: ConnectionId,
    model_id: String,
}

#[async_trait]
impl MemorySearchBackend for RuntimeMemorySearchBackend {
    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> std::result::Result<Vec<ConversationSearchResult>, String> {
        match semantic_conversation_search(
            &self.database,
            self.provider.clone(),
            &self.connection_id,
            &self.model_id,
            query,
            limit,
        )
        .await
        {
            Ok(results) if !results.is_empty() => Ok(results),
            Ok(_) => self
                .database
                .search_conversations_text(query, limit)
                .map_err(|error| error.to_string()),
            Err(error) => {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "memory.search",
                    format!(
                        "Agent semantic memory search unavailable; using text fallback: {error}"
                    ),
                );
                self.database
                    .search_conversations_text(query, limit)
                    .map_err(|fallback| fallback.to_string())
            }
        }
    }
}

async fn semantic_conversation_search(
    database: &WorkDatabase,
    provider: Arc<dyn Provider>,
    connection_id: &ConnectionId,
    model_id: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<ConversationSearchResult>, RuntimeError> {
    let response = provider
        .embed(EmbeddingRequest {
            model: model_id.to_owned(),
            input: vec![query.trim().to_owned()],
        })
        .await
        .map_err(|error| RuntimeError::Runtime(error.to_string()))?;
    let query_embedding = response
        .embeddings
        .first()
        .ok_or_else(|| RuntimeError::Runtime("embedding provider returned no vector".into()))?;
    let vector_config = EmbeddingConfig {
        connection_id: connection_id.clone(),
        model_id: model_id.to_owned(),
    };
    match database.search_conversations_vector(
        &vector_config,
        query_embedding,
        limit.saturating_mul(4).max(32),
    ) {
        Ok(hits) if !hits.is_empty() => return Ok(rank_vector_hits(hits, limit)),
        Ok(_) => {}
        Err(error) => diagnostics::record(
            DiagnosticLevel::Warning,
            "memory.search",
            format!("sqlite-vector-rs search unavailable; using linear fallback: {error}"),
        ),
    }
    let mut best = HashMap::<String, (f32, ConversationSearchResult)>::new();
    for fragment in database.indexed_fragments(connection_id, model_id)? {
        let Some(embedding) = decode_embedding(&fragment.embedding) else {
            continue;
        };
        let score = cosine_similarity(query_embedding, &embedding);
        let result = ConversationSearchResult {
            conversation_id: fragment.conversation_id.clone(),
            title: fragment.title,
            project_id: fragment.project_id,
            snippet: fragment.text,
            updated_at: fragment.updated_at,
            score: (score.clamp(0.0, 1.0) * 100.0) as u32,
        };
        match best.get(&fragment.conversation_id) {
            Some((previous, _)) if *previous >= score => {}
            _ => {
                best.insert(fragment.conversation_id, (score, result));
            }
        }
    }
    let mut results = best.into_values().collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| right.1.updated_at.cmp(&left.1.updated_at))
            .then_with(|| left.1.conversation_id.cmp(&right.1.conversation_id))
    });
    Ok(results
        .into_iter()
        .take(limit)
        .map(|(_, result)| result)
        .collect())
}

fn rank_vector_hits(hits: Vec<VectorSearchHit>, limit: usize) -> Vec<ConversationSearchResult> {
    let mut best = HashMap::<String, (f32, ConversationSearchResult)>::new();
    for hit in hits {
        let score = (1.0 - hit.distance).clamp(0.0, 1.0);
        let result = ConversationSearchResult {
            conversation_id: hit.conversation_id.clone(),
            title: hit.title,
            project_id: hit.project_id,
            snippet: hit.text,
            updated_at: hit.updated_at,
            score: (score * 100.0) as u32,
        };
        match best.get(&hit.conversation_id) {
            Some((previous, _)) if *previous >= score => {}
            _ => {
                best.insert(hit.conversation_id, (score, result));
            }
        }
    }
    let mut results = best.into_values().collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| right.1.updated_at.cmp(&left.1.updated_at))
            .then_with(|| left.1.conversation_id.cmp(&right.1.conversation_id))
    });
    results
        .into_iter()
        .take(limit)
        .map(|(_, result)| result)
        .collect()
}

fn delegated_agent_title(prompt: &str) -> String {
    let title = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        "Delegated agent".into()
    } else {
        title.chars().take(48).collect()
    }
}

pub struct AgentStreamHandle {
    handle: Option<tokio::task::JoinHandle<anyhow::Result<String>>>,
    events: tokio::sync::mpsc::UnboundedReceiver<AgentStreamEvent>,
}

impl AgentStreamHandle {
    pub async fn next_event(&mut self) -> Option<AgentStreamEvent> {
        self.events.recv().await
    }

    pub async fn finish(mut self) -> Result<anyhow::Result<String>, tokio::task::JoinError> {
        self.handle
            .take()
            .expect("finished agent stream handle")
            .await
    }
}

impl Drop for AgentStreamHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("configuration error: {0}")]
    Configuration(#[from] ConfigError),
    #[error("secure credential storage error: {0}")]
    Vault(#[from] VaultError),
    #[error("connection not found: {0}")]
    ConnectionNotFound(String),
    #[error("choose a connection for this conversation")]
    ConnectionRequired,
    #[error("choose a model for this conversation")]
    ModelRequired,
    #[error("enter an API key directly for this connection")]
    CredentialRequired,
    #[error("provider error: {0}")]
    Provider(#[from] ProviderFactoryError),
    #[error("Codex error: {0}")]
    Codex(#[from] CodexError),
    #[error("GitHub Copilot error: {0}")]
    GitHub(#[from] GitHubError),
    #[error("runtime error: {0}")]
    Runtime(String),
    #[error("work database error: {0}")]
    Database(#[from] WorkDatabaseError),
}

impl AppRuntime {
    pub fn load(key_provider: Arc<dyn VaultKeyProvider>) -> Result<Self, RuntimeError> {
        let paths = ConfigPaths::discover()?;
        Self::load_from(paths, key_provider)
    }

    pub fn load_from(
        paths: ConfigPaths,
        key_provider: Arc<dyn VaultKeyProvider>,
    ) -> Result<Self, RuntimeError> {
        let config = AppConfig::load_from(&paths)?;
        let runtime = Arc::new(
            tokio::runtime::Runtime::new()
                .map_err(|error| RuntimeError::Runtime(error.to_string()))?,
        );
        let tools = Arc::new(ToolRegistry::new());
        builtin::register_all(&tools);
        let database = WorkDatabase::open(&paths)?;
        tools.register(builtin::checkpoint::CheckpointTool::new(database.clone()));
        tools.register(builtin::task::TaskListTool::new(database.clone()));
        tools.register(builtin::task::AddTaskTool::new(database.clone()));
        tools.register(builtin::task::MarkTaskAsDoneTool::new(database.clone()));
        tools.register(builtin::global_memory::CreateGlobalMemoryTool::new(
            database.clone(),
        ));
        tools.register(builtin::global_memory::DeleteGlobalMemoryTool::new(
            database.clone(),
        ));
        tools.register(builtin::deep_memory::SearchDeepMemoryTool::new(
            database.clone(),
        ));
        tools.register(builtin::deep_memory::GetDeepMemoryTool::new(
            database.clone(),
        ));
        let (max_concurrent_calls, token_budget_per_minute) = runtime_limits(&config);
        let vault = Arc::new(CredentialVault::new(paths.clone(), key_provider));
        let model_registry = Arc::new(ModelRegistry::default());
        let provider_hooks = ProviderRegistry::with_builtins();
        provider_hooks.bootstrap(&config.connections, &model_registry);
        let agent_threads = Arc::new(AgentThreadRegistry::default());
        let user_questions = Arc::new(AskUserBroker::default());

        Ok(Self {
            vault,
            config: Arc::new(RwLock::new(config)),
            paths,
            tools,
            governor: Arc::new(ResourceGovernor::new(
                max_concurrent_calls,
                token_budget_per_minute,
            )),
            runtime,
            database,
            model_registry,
            prompt: PromptBuilder::new(),
            codex: Arc::new(tokio::sync::Mutex::new(None)),
            copilot: Arc::new(tokio::sync::Mutex::new(None)),
            copilot_models: RwLock::new(HashMap::new()),
            workspace_skills: RwLock::new(HashMap::new()),
            workspace_instructions: RwLock::new(HashMap::new()),
            provider_hooks,
            agent_threads,
            user_questions,
        })
    }

    pub fn config(&self) -> AppConfig {
        self.config.read().clone()
    }

    pub fn ensure_secure_storage_access(&self) -> Result<(), RuntimeError> {
        self.vault.ensure_access()?;
        Ok(())
    }

    pub fn connections(&self) -> Vec<ConnectionProfile> {
        self.config.read().connections.clone()
    }

    pub fn agents(&self) -> Vec<AgentProfile> {
        self.config.read().agents.clone()
    }

    pub fn save_agent(&self, agent: AgentProfile) -> Result<(), RuntimeError> {
        if agent.id.trim().is_empty()
            || agent.name.trim().is_empty()
            || agent.connection_id.trim().is_empty()
            || agent.model_id.trim().is_empty()
        {
            return Err(RuntimeError::Runtime(
                "an agent requires an id, name, connection and model".into(),
            ));
        }
        if self
            .connection(&ConnectionId(agent.connection_id.clone()))
            .is_none()
        {
            return Err(RuntimeError::ConnectionNotFound(agent.connection_id));
        }
        let mut next = self.config();
        if let Some(existing) = next
            .agents
            .iter_mut()
            .find(|existing| existing.id == agent.id)
        {
            *existing = agent;
        } else {
            next.agents.push(agent);
        }
        next.save_to(&self.paths)?;
        *self.config.write() = next;
        Ok(())
    }

    pub fn delete_agent(&self, id: &str) -> Result<bool, RuntimeError> {
        let mut next = self.config();
        let before = next.agents.len();
        next.agents.retain(|agent| agent.id != id);
        if next.agents.len() == before {
            return Ok(false);
        }
        next.save_to(&self.paths)?;
        *self.config.write() = next;
        Ok(true)
    }

    pub fn embedding_connections(&self) -> Vec<ConnectionProfile> {
        self.connections()
            .into_iter()
            .filter(|profile| {
                profile.kind.supports_embeddings()
                    && (matches!(profile.kind, ConnectionKind::Codex | ConnectionKind::Ollama)
                        || self.has_connection_credential(&profile.id))
            })
            .collect()
    }

    pub fn embedding_models_for_connection(
        &self,
        id: &ConnectionId,
    ) -> Result<Vec<ModelInfo>, RuntimeError> {
        let profile = self
            .connection(id)
            .ok_or_else(|| RuntimeError::ConnectionNotFound(id.to_string()))?;
        if !profile.kind.supports_embeddings() {
            return Ok(Vec::new());
        }
        let models = self.models_for_connection(id)?;

        // Prefer explicit capability metadata. Providers with arbitrary model
        // IDs (for example Ollama and Copilot) deliberately keep their full
        // catalog as a fallback when no embedding metadata is advertised.
        let embedding_models = models
            .iter()
            .filter(|model| model.capabilities.embeddings)
            .cloned()
            .collect::<Vec<_>>();
        if profile.kind == ConnectionKind::QDivZero {
            // QDivZero declares workload kind in its serving catalog. Never
            // offer its chat endpoints as embedding models just because the
            // account has no active embedding endpoint today.
            return Ok(embedding_models);
        }
        Ok(if embedding_models.is_empty() {
            models
        } else {
            embedding_models
        })
    }

    pub fn connection(&self, id: &ConnectionId) -> Option<ConnectionProfile> {
        self.config.read().connection(id).cloned()
    }

    pub fn default_agent_tools(&self) -> Vec<String> {
        self.default_agent_tools_for(&self.tools)
    }

    pub fn agent_threads_for(&self, parent_session_id: &str) -> Vec<AgentThreadSnapshot> {
        self.agent_threads.for_parent(parent_session_id)
    }

    fn default_agent_tools_for(&self, registry: &ToolRegistry) -> Vec<String> {
        registry.bootstrap_names()
    }

    fn agent_tools(&self, binding: &SessionBinding, registry: &ToolRegistry) -> Vec<String> {
        let mut enabled = self.default_agent_tools_for(registry);
        // Bindings created by the discovery flow always contain its bootstrap
        // root. Older bindings are intentionally not treated as an allow-list:
        // they may have been written by the former static catalogue.
        if binding.tools.iter().any(|name| name == "discover_tools") {
            for name in &binding.tools {
                if registry.get(name).is_some() && !enabled.iter().any(|item| item == name) {
                    enabled.push(name.clone());
                }
            }
        }
        enabled.sort_unstable();
        enabled
    }

    pub fn save_connection(
        &self,
        profile: ConnectionProfile,
        secret: Option<&str>,
    ) -> Result<(), RuntimeError> {
        profile
            .validate()
            .map_err(|error| ConfigError::InvalidConnection(error.to_string()))?;

        let credential = profile.credential_ref.clone();
        let previous_secret = credential
            .as_ref()
            .and_then(|credential| self.vault.get(credential).ok());
        let supplied_secret = secret.filter(|value| !value.trim().is_empty());

        if profile.kind.requires_api_key()
            && profile.kind != ConnectionKind::Copilot
            && supplied_secret.is_none()
            && previous_secret.is_none()
        {
            return Err(RuntimeError::CredentialRequired);
        }

        if let (Some(credential), Some(secret)) = (credential.as_ref(), supplied_secret) {
            self.vault.put(credential, secret)?;
        }

        let mut next = self.config();
        if let Some(existing) = next
            .connections
            .iter_mut()
            .find(|existing| existing.id == profile.id)
        {
            *existing = profile.clone();
        } else {
            next.connections.push(profile.clone());
        }

        if let Err(error) = next.save_to(&self.paths) {
            if let Some(credential) = credential.as_ref() {
                match previous_secret {
                    Some(previous) => {
                        let _ = self.vault.put(credential, &previous);
                    }
                    None if supplied_secret.is_some() => {
                        let _ = self.vault.delete(credential);
                    }
                    None => {}
                }
            }
            return Err(error.into());
        }

        *self.config.write() = next;
        if let Some(hook) = self.provider_hooks.hook(profile.kind) {
            hook.bootstrap_models(&profile, &self.model_registry);
        }
        Ok(())
    }

    /// Adds or replaces a user-declared model in one connection and persists
    /// it alongside that connection. This is the escape hatch for providers
    /// without a usable `/models` endpoint.
    pub fn add_manual_model(
        &self,
        connection_id: &ConnectionId,
        model: ManualModel,
    ) -> Result<(), RuntimeError> {
        let mut profile = self
            .connection(connection_id)
            .ok_or_else(|| RuntimeError::ConnectionNotFound(connection_id.to_string()))?;
        profile
            .manual_models
            .retain(|existing| existing.id != model.id);
        profile.manual_models.push(model);
        self.save_connection(profile, None)
    }

    pub fn delete_connection(&self, id: &ConnectionId) -> Result<bool, RuntimeError> {
        let mut next = self.config();
        let Some(index) = next
            .connections
            .iter()
            .position(|profile| &profile.id == id)
        else {
            return Ok(false);
        };
        let removed = next.connections.remove(index);
        next.agents
            .retain(|agent| agent.connection_id != removed.id.0);
        next.save_to(&self.paths)?;
        *self.config.write() = next;

        if let Some(credential) = removed.credential_ref.as_ref() {
            self.vault.delete(credential)?;
        }
        self.model_registry.remove(id);
        Ok(true)
    }

    pub fn models_for_connection(&self, id: &ConnectionId) -> Result<Vec<ModelInfo>, RuntimeError> {
        self.connection(id)
            .ok_or_else(|| RuntimeError::ConnectionNotFound(id.to_string()))?;
        Ok(self.model_registry.models(id).unwrap_or_default())
    }

    pub async fn live_models_for_connection(
        &self,
        id: &ConnectionId,
    ) -> Result<Vec<ModelInfo>, RuntimeError> {
        let profile = self
            .connection(id)
            .ok_or_else(|| RuntimeError::ConnectionNotFound(id.to_string()))?;
        let hook = self
            .provider_hooks
            .hook(profile.kind)
            .ok_or_else(|| RuntimeError::Runtime("provider hook is not registered".into()))?;
        match hook.discovery() {
            ModelDiscovery::CodexAccount => {
                let models = self
                    .codex_models()
                    .await?
                    .into_iter()
                    .map(|model| ModelInfo {
                        id: model.id,
                        display_name: model.display_name,
                        provider: "codex".into(),
                        description: Some(model.description),
                        capabilities: averroes_core::provider::ModelCapabilities {
                            chat: true,
                            embeddings: false,
                            vision: true,
                            tools: true,
                        },
                        source: averroes_core::provider::ModelSource::Live,
                        featured: false,
                        default_reasoning_effort: None,
                        available_reasoning_efforts: model.reasoning_efforts,
                    })
                    .collect::<Vec<_>>();
                return Ok(self
                    .model_registry
                    .replace_provider_models(&profile, "codex", models));
            }
            ModelDiscovery::CopilotAccount => {
                let credential = profile
                    .credential_ref
                    .as_ref()
                    .ok_or(RuntimeError::CredentialRequired)?;
                let models = self.copilot_client().await?.list_models(credential).await?;
                self.copilot_models
                    .write()
                    .insert(profile.id.clone(), models.clone());
                return Ok(self.model_registry.replace_provider_models(
                    &profile,
                    "copilot",
                    copilot_model_infos(&models),
                ));
            }
            ModelDiscovery::RemoteApi => {}
            ModelDiscovery::ManualOnly => {
                return Ok(self.models_for_connection(id)?);
            }
        }
        let secret = match profile.kind {
            ConnectionKind::Ollama => String::new(),
            _ => {
                let credential = profile
                    .credential_ref
                    .as_ref()
                    .ok_or(RuntimeError::CredentialRequired)?;
                self.vault.get(credential)?.to_string()
            }
        };
        let provider = create_direct_provider(&profile, &secret, "model-discovery")?;
        let is_qdivzero = profile.kind == ConnectionKind::QDivZero;
        if is_qdivzero {
            diagnostics::record(
                DiagnosticLevel::Info,
                "qdivzero.catalog",
                "Requesting the authenticated /serving-endpoints catalog.",
            );
        }
        let live = match provider
            .list_models()
            .await
            .map_err(|error| RuntimeError::Runtime(error.to_string()))
        {
            Ok(models) => {
                if is_qdivzero {
                    diagnostics::record(
                        if models.is_empty() {
                            DiagnosticLevel::Warning
                        } else {
                            DiagnosticLevel::Success
                        },
                        "qdivzero.catalog",
                        format!(
                            "QDivZero serving catalog returned {} usable model(s).",
                            models.len()
                        ),
                    );
                }
                models
            }
            Err(error) => {
                if is_qdivzero {
                    diagnostics::record(
                        DiagnosticLevel::Error,
                        "qdivzero.catalog",
                        format!("QDivZero serving catalog request failed: {error}"),
                    );
                }
                return Err(error);
            }
        };
        let family = hook.catalog_provider().unwrap_or("generic");
        Ok(self.model_registry.register_live(&profile, family, &live))
    }

    /// Refreshes all connections concurrently. Network discovery is I/O-bound,
    /// so bounded async fan-out gives a faster bootstrap without creating an
    /// unbounded number of sockets or competing with active agent calls.
    pub async fn refresh_model_catalogs_parallel(
        &self,
    ) -> Vec<(ConnectionId, Result<Vec<ModelInfo>, RuntimeError>)> {
        let connections = self
            .connections()
            .into_iter()
            .filter_map(|profile| {
                // Codex has a separate account/authentication lifecycle, but
                // its catalog still belongs in this central refresh. The
                // Codex hook obtains credentials from its own OAuth session;
                // excluding it here left Settings and inactive connections
                // with an empty catalog.
                let should_refresh = match profile.kind {
                    ConnectionKind::Codex | ConnectionKind::Ollama => true,
                    _ => self.has_connection_credential(&profile.id),
                };
                if !should_refresh && profile.kind == ConnectionKind::QDivZero {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "qdivzero.catalog",
                        "Skipping QDivZero catalog refresh because its API credential is unavailable.",
                    );
                }
                should_refresh.then_some(profile)
            })
            .collect::<Vec<_>>();
        let concurrency = self
            .config()
            .runtime
            .max_concurrent_calls
            .unwrap_or(4)
            .clamp(1, 8);

        stream::iter(connections.into_iter().map(|profile| {
            let id = profile.id.clone();
            async move {
                let result = self.live_models_for_connection(&id).await;
                (id, result)
            }
        }))
        .buffer_unordered(concurrency)
        .collect()
        .await
    }

    pub async fn codex_account(&self) -> Result<CodexAccount, RuntimeError> {
        self.codex_client()
            .await?
            .account()
            .await
            .map_err(Into::into)
    }

    pub async fn start_codex_login(&self) -> Result<CodexLogin, RuntimeError> {
        self.codex_client()
            .await?
            .start_chatgpt_login()
            .await
            .map_err(Into::into)
    }

    pub async fn wait_for_codex_login(&self, login_id: &str) -> Result<CodexAccount, RuntimeError> {
        self.codex_client()
            .await?
            .wait_for_login(login_id)
            .await
            .map_err(Into::into)
    }

    pub async fn start_copilot_login(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<GitHubLogin, RuntimeError> {
        let profile = self
            .connection(connection_id)
            .ok_or_else(|| RuntimeError::ConnectionNotFound(connection_id.to_string()))?;
        if profile.kind != ConnectionKind::Copilot {
            return Err(RuntimeError::Runtime(
                "GitHub sign-in is only available for GitHub Copilot connections".into(),
            ));
        }
        let credential = profile
            .credential_ref
            .ok_or(RuntimeError::CredentialRequired)?;
        self.copilot_client()
            .await?
            .start_login(credential)
            .await
            .map_err(Into::into)
    }

    pub async fn wait_for_copilot_login(&self, login_id: &str) -> Result<(), RuntimeError> {
        self.copilot_client()
            .await?
            .wait_for_login(login_id)
            .await
            .map_err(Into::into)
    }

    pub async fn codex_models(&self) -> Result<Vec<CodexModel>, RuntimeError> {
        self.codex_client()
            .await?
            .list_models()
            .await
            .map_err(Into::into)
    }

    async fn codex_client(&self) -> Result<Arc<CodexClient>, RuntimeError> {
        let mut client = self.codex.lock().await;
        if let Some(client) = client.as_ref() {
            return Ok(client.clone());
        }
        let connected = CodexClient::connect(self.vault.clone()).await?;
        *client = Some(connected.clone());
        Ok(connected)
    }

    async fn copilot_client(&self) -> Result<Arc<GitHubCopilotClient>, RuntimeError> {
        let mut client = self.copilot.lock().await;
        if let Some(client) = client.as_ref() {
            return Ok(client.clone());
        }
        let connected = GitHubCopilotClient::connect(self.vault.clone())?;
        *client = Some(connected.clone());
        Ok(connected)
    }

    pub fn has_connection_credential(&self, id: &ConnectionId) -> bool {
        self.connection(id)
            .and_then(|profile| profile.credential_ref)
            .is_some_and(|credential| self.vault.contains(&credential).unwrap_or(false))
    }

    async fn embedding_provider(
        &self,
        config: &EmbeddingConfig,
    ) -> Result<Arc<dyn Provider>, RuntimeError> {
        let profile = self
            .connection(&config.connection_id)
            .ok_or_else(|| RuntimeError::ConnectionNotFound(config.connection_id.to_string()))?;
        if !profile.kind.supports_embeddings() {
            return Err(RuntimeError::Runtime(format!(
                "{} does not expose an embeddings endpoint",
                profile.kind.label()
            )));
        }
        let provider = match profile.kind {
            ConnectionKind::Codex => Arc::new(ChatGptCodexProvider::new(
                self.codex_client().await?,
                &config.model_id,
            )) as Arc<dyn Provider>,
            ConnectionKind::Copilot => {
                let credential = profile
                    .credential_ref
                    .as_ref()
                    .ok_or(RuntimeError::CredentialRequired)?;
                let client = self.copilot_client().await?;
                let model = client
                    .list_models(credential)
                    .await?
                    .into_iter()
                    .find(|model| model.id == config.model_id)
                    .ok_or_else(|| {
                        RuntimeError::Runtime(format!(
                            "{} is not available for this GitHub Copilot account",
                            config.model_id
                        ))
                    })?;
                let secret = client.copilot_api_token(credential).await?;
                let mut request_profile = profile.clone();
                request_profile.base_url = Some(model.api_base_url);
                create_copilot_provider(
                    &request_profile,
                    &secret,
                    &config.model_id,
                    averroes_core::github::CopilotEndpoint::ChatCompletions,
                )?
            }
            ConnectionKind::Ollama => create_direct_provider(&profile, "", &config.model_id)?,
            ConnectionKind::OpenAi
            | ConnectionKind::DeepSeek
            | ConnectionKind::Groq
            | ConnectionKind::QDivZero
            | ConnectionKind::OllamaCloud
            | ConnectionKind::Compatible => {
                let credential = profile
                    .credential_ref
                    .as_ref()
                    .ok_or(RuntimeError::CredentialRequired)?;
                let secret = self.vault.get(credential)?;
                create_direct_provider(&profile, &secret, &config.model_id)?
            }
            ConnectionKind::Anthropic => {
                return Err(RuntimeError::Runtime(
                    "this provider cannot be used for conversation embeddings".into(),
                ));
            }
        };
        Ok(provider)
    }

    async fn configured_memory_search_backend(&self) -> Option<Arc<dyn MemorySearchBackend>> {
        let config = match self.database.embedding_config() {
            Ok(config) => config?,
            Err(error) => {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "memory.search",
                    format!("Could not read embedding configuration: {error}"),
                );
                return None;
            }
        };
        let provider = match self.embedding_provider(&config).await {
            Ok(provider) => provider,
            Err(error) => {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "memory.search",
                    format!(
                        "Could not prepare the configured memory provider; text fallback remains available: {error}"
                    ),
                );
                return None;
            }
        };
        Some(Arc::new(RuntimeMemorySearchBackend {
            database: self.database.clone(),
            provider,
            connection_id: config.connection_id,
            model_id: config.model_id,
        }))
    }

    pub async fn rebuild_conversation_index(
        &self,
        config: EmbeddingConfig,
    ) -> Result<(usize, usize), RuntimeError> {
        self.build_conversation_index(config, false).await
    }

    pub async fn index_pending_conversations(
        &self,
        config: EmbeddingConfig,
    ) -> Result<(usize, usize), RuntimeError> {
        self.build_conversation_index(config, true).await
    }

    async fn build_conversation_index(
        &self,
        config: EmbeddingConfig,
        pending_only: bool,
    ) -> Result<(usize, usize), RuntimeError> {
        diagnostics::record(
            DiagnosticLevel::Info,
            "memory.index",
            format!(
                "Compiling {} conversation index with {} / {}.",
                if pending_only { "pending" } else { "full" },
                config.connection_id,
                config.model_id
            ),
        );
        let provider = self.embedding_provider(&config).await?;
        let documents = if pending_only {
            self.database.pending_conversation_documents(&config)?
        } else {
            self.database.conversation_documents()?
        };
        let document_count = documents.len();
        let cached_embeddings = self
            .database
            .indexed_fragments(&config.connection_id, &config.model_id)?
            .into_iter()
            .filter_map(|fragment| {
                decode_embedding(&fragment.embedding)
                    .filter(|embedding| !embedding.is_empty())
                    .map(|embedding| (fragment.content_hash, embedding))
            })
            .collect::<HashMap<_, _>>();
        let cached_fragment_count = cached_embeddings.len();
        let cached_embeddings = Arc::new(cached_embeddings);
        diagnostics::record(
            DiagnosticLevel::Info,
            "memory.index",
            format!("Loaded {cached_fragment_count} reusable fragment vector(s) before rebuild."),
        );
        let concurrency = self
            .config()
            .runtime
            .max_concurrent_calls
            .unwrap_or(4)
            .clamp(1, 8);
        let model_id = config.model_id.clone();
        let jobs = documents.into_iter().map(|document| {
            let provider = provider.clone();
            let cached_embeddings = cached_embeddings.clone();
            let model_id = model_id.clone();
            async move {
                let fragments = compile_fragments_with_context(
                    &document.messages,
                    document.context_summary.as_deref(),
                );
                let mut embeddings = vec![None; fragments.len()];
                let mut missing = Vec::new();
                for (index, fragment) in fragments.iter().enumerate() {
                    if let Some(embedding) = cached_embeddings.get(&fragment.content_hash) {
                        embeddings[index] = Some(embedding.clone());
                    } else {
                        missing.push(index);
                    }
                }
                for batch in missing.chunks(64) {
                    let response = provider
                        .embed(EmbeddingRequest {
                            model: model_id.clone(),
                            input: batch
                                .iter()
                                .map(|&index| fragments[index].text.clone())
                                .collect(),
                        })
                        .await
                        .map_err(|error| RuntimeError::Runtime(error.to_string()))?;
                    if response.embeddings.len() != batch.len() {
                        return Err(RuntimeError::Runtime(format!(
                            "embedding provider returned {} vectors for {} fragments",
                            response.embeddings.len(),
                            batch.len()
                        )));
                    }
                    for (&index, embedding) in batch.iter().zip(response.embeddings) {
                        embeddings[index] = Some(embedding);
                    }
                }
                let embeddings = embeddings
                    .into_iter()
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| RuntimeError::Runtime("missing fragment embedding".into()))?;
                Ok::<_, RuntimeError>((document.id, fragments, embeddings))
            }
        });
        let results = stream::iter(jobs)
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;

        let mut indexed_conversations = 0;
        let mut indexed_fragments = 0;
        for result in results {
            let (conversation_id, fragments, embeddings) = match result {
                Ok(result) => result,
                Err(error) => {
                    diagnostics::record(
                        DiagnosticLevel::Error,
                        "memory.index",
                        format!("Conversation index build failed: {error}"),
                    );
                    return Err(error);
                }
            };
            indexed_fragments += fragments.len();
            if !fragments.is_empty() {
                indexed_conversations += 1;
            }
            self.database.replace_conversation_embeddings(
                &conversation_id,
                &config.connection_id,
                &config.model_id,
                &fragments,
                &embeddings,
            )?;
        }
        if self.database.vector_index_available() {
            match self.database.rebuild_vector_index(&config) {
                Ok(vector_count) => diagnostics::record(
                    DiagnosticLevel::Info,
                    "memory.index",
                    format!("sqlite-vector-rs HNSW index ready with {vector_count} vectors."),
                ),
                Err(error) => diagnostics::record(
                    DiagnosticLevel::Warning,
                    "memory.index",
                    format!(
                        "SQLite embeddings saved, but sqlite-vector-rs index rebuild failed; using fallback search: {error}"
                    ),
                ),
            }
        } else {
            diagnostics::record(
                DiagnosticLevel::Warning,
                "memory.index",
                "sqlite-vector-rs is unavailable; SQLite text/linear semantic fallback remains active.",
            );
        }
        diagnostics::record(
            DiagnosticLevel::Success,
            "memory.index",
            format!(
                "Conversation index ready: {indexed_conversations}/{document_count} conversations, {indexed_fragments} fragments."
            ),
        );
        Ok((indexed_conversations, indexed_fragments))
    }

    pub async fn search_conversations(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ConversationSearchResult>, RuntimeError> {
        let query = query.trim();
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let config = self.database.embedding_config()?;
        if let Some(config) = config {
            match self.embedding_provider(&config).await {
                Ok(provider) => match semantic_conversation_search(
                    &self.database,
                    provider,
                    &config.connection_id,
                    &config.model_id,
                    query,
                    limit,
                )
                .await
                {
                    Ok(results) if !results.is_empty() => return Ok(results),
                    Ok(_) => {}
                    Err(error) => diagnostics::record(
                        DiagnosticLevel::Warning,
                        "memory.search",
                        format!(
                            "Semantic conversation search unavailable; using text fallback: {error}"
                        ),
                    ),
                },
                Err(error) => {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "memory.search",
                        format!(
                            "Semantic conversation search provider unavailable; using text fallback: {error}"
                        ),
                    );
                }
            }
        }
        Ok(self.database.search_conversations_text(query, limit)?)
    }

    pub fn validate_binding(&self, binding: &SessionBinding) -> Result<(), RuntimeError> {
        let connection_id = binding
            .connection_id
            .as_ref()
            .ok_or(RuntimeError::ConnectionRequired)?;
        self.connection(connection_id)
            .ok_or_else(|| RuntimeError::ConnectionNotFound(connection_id.to_string()))?;
        if binding
            .model_id
            .as_deref()
            .is_none_or(|model| model.trim().is_empty())
        {
            return Err(RuntimeError::ModelRequired);
        }
        Ok(())
    }

    pub async fn new_agent(
        &self,
        session_id: &SessionId,
        binding: &SessionBinding,
        working_dir: Option<&Path>,
    ) -> Result<Arc<Agent>, RuntimeError> {
        self.validate_binding(binding)?;
        let connection_id = binding
            .connection_id
            .as_ref()
            .expect("validated connection");
        let model = binding.model_id.as_deref().expect("validated model");
        let profile = self
            .connection(connection_id)
            .ok_or_else(|| RuntimeError::ConnectionNotFound(connection_id.to_string()))?;
        let workspace_root = working_dir.map(Path::to_path_buf);
        let working_dir = workspace_root.clone().unwrap_or_else(|| PathBuf::from("."));
        let scoped_tools = self.tools.fork();
        scoped_tools.register(builtin::ask_user::AskUserTool::new(
            self.user_questions.clone(),
        ));
        let project_instructions = workspace_root
            .as_deref()
            .and_then(|workspace_root| self.workspace_instructions(workspace_root));
        let workspace_skill_index = workspace_root
            .as_deref()
            .and_then(|workspace_root| self.refresh_workspace_skills(workspace_root));
        if let Some(skill_index) = workspace_skill_index.as_ref() {
            builtin::register_skill_tools(&scoped_tools, skill_index.clone());
        } else {
            if workspace_root.is_none() {
                diagnostics::record(
                    DiagnosticLevel::Info,
                    "skills.workspace",
                    "Agent has no open workspace; skipping project skill discovery.",
                );
            }
        }

        let provider: Arc<dyn Provider> = match profile.kind {
            ConnectionKind::Codex => {
                Arc::new(ChatGptCodexProvider::new(self.codex_client().await?, model))
            }
            ConnectionKind::Ollama => create_direct_provider(&profile, "", model)?,
            ConnectionKind::Copilot => {
                let credential = profile
                    .credential_ref
                    .as_ref()
                    .ok_or(RuntimeError::CredentialRequired)?;
                let client = self.copilot_client().await?;
                // GitHub can change a model's eligible route while the app is
                // open. Refresh immediately before a run rather than trusting
                // a picker cache and accidentally posting a valid ID to the
                // wrong API surface.
                let models = client.list_models(credential).await?;
                let selected_model = models
                    .iter()
                    .find(|entry| entry.id == model)
                    .ok_or_else(|| {
                        RuntimeError::Runtime(format!(
                            "{model} is not available for this GitHub Copilot account; choose a model from the refreshed catalog"
                        ))
                    })?;
                let endpoint = selected_model.endpoint;
                let api_base_url = selected_model.api_base_url.clone();
                self.copilot_models
                    .write()
                    .insert(connection_id.clone(), models);
                // Exchange the per-user GitHub credential for the same
                // short-lived Copilot session that exposed this catalog. The
                // exchange is native HTTP and never invokes an external CLI.
                let secret = client.copilot_api_token(credential).await?;
                // Keep the endpoint discovered from GitHub's authenticated
                // catalog local to this request. It is account routing data,
                // not a user-configured URL to persist over the connection.
                let mut request_profile = profile.clone();
                request_profile.base_url = Some(api_base_url);
                create_copilot_provider(&request_profile, &secret, model, endpoint)?
            }
            ConnectionKind::OpenAi
            | ConnectionKind::Anthropic
            | ConnectionKind::DeepSeek
            | ConnectionKind::Groq
            | ConnectionKind::QDivZero
            | ConnectionKind::OllamaCloud
            | ConnectionKind::Compatible => {
                let credential = profile
                    .credential_ref
                    .as_ref()
                    .ok_or(RuntimeError::CredentialRequired)?;
                let secret = self.vault.get(credential)?;
                create_direct_provider(&profile, &secret, model)?
            }
        };
        let memory_search_backend = self.configured_memory_search_backend().await;
        let global_memory_prompt = self.database.global_memory_prompt()?;

        let model_supports_tools = profile.kind != ConnectionKind::Copilot
            || self
                .copilot_models
                .read()
                .get(connection_id)
                .and_then(|models| models.iter().find(|entry| entry.id == model))
                .is_some_and(|entry| entry.supports_tools);
        let tools = if model_supports_tools {
            self.agent_tools(binding, &scoped_tools)
        } else {
            Vec::new()
        };
        let system_prompt = self.prompt.build_system(
            &scoped_tools,
            &tools,
            &working_dir.display().to_string(),
            project_instructions
                .as_ref()
                .map(|instructions| instructions.content()),
        );
        let config = self.config();
        let agent_config = AgentConfig {
            name: "averroes".into(),
            model: model.to_owned(),
            system_prompt: Some(system_prompt.clone()),
            tools: tools.clone(),
            max_iterations: 30,
            compaction: compaction_config(&config),
            ..Default::default()
        };
        let agent_compaction = agent_config.compaction.clone();

        let scoped_tools = Arc::new(scoped_tools);
        let agent = Arc::new(Agent::new(
            agent_config,
            provider.clone(),
            scoped_tools.clone(),
            self.governor.clone(),
            session_id.to_string(),
            working_dir,
        ));
        agent.set_skill_index(workspace_skill_index);
        agent.set_reasoning_effort(binding.reasoning_effort.clone());
        agent.set_memory_search_backend(memory_search_backend.clone());
        agent.set_global_memory_prompt(global_memory_prompt.clone());
        agent.set_agent_runner(Arc::new(RuntimeAgentRunner {
            tool_registry: scoped_tools,
            governor: self.governor.clone(),
            system_prompt,
            default_model: model.to_owned(),
            compaction: agent_compaction,
            reasoning_effort: binding.reasoning_effort.clone(),
            threads: self.agent_threads.clone(),
            config: self.config.clone(),
            provider_resolver: Arc::new(RuntimeProviderResolver {
                config: self.config.clone(),
                vault: self.vault.clone(),
                codex: self.codex.clone(),
                copilot: self.copilot.clone(),
            }),
            parent_connection_id: connection_id.clone(),
            memory_search_backend,
            global_memory_prompt,
        }));
        Ok(agent)
    }

    pub fn load_workspace_tools(&self, workspace_root: &Path) {
        let workspace_root = workspace_root.to_path_buf();
        self.load_workspace_instructions(&workspace_root);
        if self.workspace_skills.read().contains_key(&workspace_root) {
            diagnostics::record(
                DiagnosticLevel::Info,
                "skills.workspace",
                format!(
                    "Using cached skill index for workspace {}.",
                    workspace_root.display()
                ),
            );
            return;
        }

        if let Some(index) = self.build_workspace_skill_index(&workspace_root) {
            self.workspace_skills.write().insert(workspace_root, index);
        }
    }

    /// Rebuilds the skill index for a workspace. This is deliberately
    /// separate from the cached startup loader so an already-open conversation
    /// can see skills added or edited while the app is running.
    pub fn refresh_workspace_skills(&self, workspace_root: &Path) -> Option<Arc<SkillIndex>> {
        self.load_workspace_instructions(workspace_root);
        let index = self.build_workspace_skill_index(workspace_root)?;
        self.workspace_skills
            .write()
            .insert(workspace_root.to_path_buf(), index.clone());
        Some(index)
    }

    pub fn refresh_agent_skills(&self, agent: &Agent, workspace_root: &Path) {
        if let Some(index) = self.refresh_workspace_skills(workspace_root) {
            diagnostics::record(
                DiagnosticLevel::Success,
                "skills.agent",
                format!(
                    "Attached {} workspace skill(s) to the active agent for {}.",
                    index.len(),
                    workspace_root.display()
                ),
            );
            agent.set_skill_index(Some(index));
        } else {
            diagnostics::record(
                DiagnosticLevel::Warning,
                "skills.agent",
                format!(
                    "Could not attach workspace skills to the active agent for {}.",
                    workspace_root.display()
                ),
            );
        }
    }

    fn load_workspace_instructions(&self, workspace_root: &Path) {
        if self
            .workspace_instructions
            .read()
            .contains_key(workspace_root)
        {
            diagnostics::record(
                DiagnosticLevel::Info,
                "agents.instructions",
                format!(
                    "Using cached AGENTS.md instructions for workspace {}.",
                    workspace_root.display()
                ),
            );
            return;
        }

        let instructions = ProjectInstructions::load(workspace_root, workspace_root);
        self.workspace_instructions
            .write()
            .insert(workspace_root.to_path_buf(), Arc::new(instructions));
    }

    fn workspace_instructions(&self, workspace_root: &Path) -> Option<Arc<ProjectInstructions>> {
        if let Some(instructions) = self
            .workspace_instructions
            .read()
            .get(workspace_root)
            .cloned()
        {
            return (!instructions.is_empty()).then_some(instructions);
        }
        self.load_workspace_instructions(workspace_root);
        self.workspace_instructions
            .read()
            .get(workspace_root)
            .cloned()
            .filter(|instructions| !instructions.is_empty())
    }

    fn workspace_skill_index(&self, workspace_root: &Path) -> Option<Arc<SkillIndex>> {
        if let Some(index) = self.workspace_skills.read().get(workspace_root).cloned() {
            return Some(index);
        }
        self.load_workspace_tools(workspace_root);
        self.workspace_skills.read().get(workspace_root).cloned()
    }

    fn build_workspace_skill_index(&self, workspace_root: &Path) -> Option<Arc<SkillIndex>> {
        let project_paths = [
            workspace_root.join(".averroes").join("skills"),
            workspace_root.join(".agents").join("skills"),
            workspace_root.join(".codex").join("skills"),
            workspace_root.join(".claude").join("skills"),
            workspace_root.join("skills"),
        ];
        let global_paths = self
            .config()
            .skills
            .paths
            .unwrap_or_default()
            .into_iter()
            .filter_map(|raw_path| {
                let raw_path = raw_path.trim().to_string();
                if raw_path.is_empty() {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "skills.workspace",
                        "Ignoring an empty global skill path from settings.",
                    );
                    None
                } else {
                    Some(resolve_skill_path(&raw_path))
                }
            })
            .collect::<Vec<_>>();

        diagnostics::record(
            DiagnosticLevel::Info,
            "skills.workspace",
            format!(
                "Refreshing skills for workspace {}; checking {} project path(s) and {} global fallback path(s).",
                workspace_root.display(),
                project_paths.len(),
                global_paths.len()
            ),
        );

        let mut paths = project_paths.to_vec();
        paths.extend(global_paths);
        match SkillIndex::build(SkillLoader::new(paths)) {
            Ok(index) => {
                if index.is_empty() {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "skills.workspace",
                        format!(
                            "No skills found for workspace {}.",
                            workspace_root.display()
                        ),
                    );
                } else {
                    diagnostics::record(
                        DiagnosticLevel::Success,
                        "skills.workspace",
                        format!(
                            "Workspace skill index ready for {} with {} skill(s).",
                            workspace_root.display(),
                            index.len()
                        ),
                    );
                }
                Some(Arc::new(index))
            }
            Err(error) => {
                diagnostics::record(
                    DiagnosticLevel::Error,
                    "skills.workspace",
                    format!(
                        "Could not build the skill index for workspace {}: {error}.",
                        workspace_root.display()
                    ),
                );
                None
            }
        }
    }

    pub fn spawn_agent_stream(&self, agent: Arc<Agent>, prompt: String) -> AgentStreamHandle {
        match self.database.global_memory_prompt() {
            Ok(global_memory_prompt) => agent.set_global_memory_prompt(global_memory_prompt),
            Err(error) => diagnostics::record(
                DiagnosticLevel::Warning,
                "memory.global",
                format!("Could not refresh the global-memory prompt: {error}"),
            ),
        }
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        let handle = self
            .runtime
            .spawn(async move { agent.run_streaming(&prompt, sender).await });
        AgentStreamHandle {
            handle: Some(handle),
            events,
        }
    }

    pub fn prepare_user_question(
        &self,
        session_id: &SessionId,
        input: &serde_json::Value,
    ) -> Result<UserQuestion, String> {
        let params = AskUserParams::parse(input)?;
        Ok(self.user_questions.request(session_id.as_str(), params))
    }

    pub fn answer_user_question(
        &self,
        session_id: &SessionId,
        question_id: &str,
        answer: String,
    ) -> bool {
        self.user_questions
            .answer(session_id.as_str(), question_id, answer)
    }

    pub fn spawn_background<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }
}

fn resolve_skill_path(raw_path: &str) -> PathBuf {
    if raw_path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw_path));
    }
    if let Some(relative) = raw_path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(relative);
        }
    }
    PathBuf::from(raw_path)
}

fn copilot_model_infos(models: &[CopilotModel]) -> Vec<ModelInfo> {
    models
        .iter()
        .map(|model| ModelInfo {
            id: model.id.clone(),
            display_name: model.display_name.clone(),
            provider: "copilot".into(),
            description: Some(match model.endpoint {
                CopilotEndpoint::ChatCompletions => "GitHub Copilot · chat completions".into(),
                CopilotEndpoint::Responses => "GitHub Copilot · responses".into(),
                CopilotEndpoint::Messages => "GitHub Copilot · Anthropic messages".into(),
            }),
            capabilities: averroes_core::provider::ModelCapabilities {
                chat: true,
                embeddings: false,
                vision: false,
                tools: model.supports_tools,
            },
            source: averroes_core::provider::ModelSource::Live,
            featured: false,
            default_reasoning_effort: None,
            available_reasoning_efforts: model.reasoning_efforts.clone(),
        })
        .collect()
}

fn runtime_limits(config: &AppConfig) -> (usize, u64) {
    (
        config
            .runtime
            .max_concurrent_calls
            .unwrap_or(10)
            .clamp(1, MAX_CONCURRENT_CALLS),
        config
            .runtime
            .token_budget_per_minute
            .unwrap_or(200_000)
            .max(1),
    )
}

fn compaction_config(config: &AppConfig) -> CompactionConfig {
    let threshold = config
        .compaction
        .threshold
        .unwrap_or(DEFAULT_COMPACTION_THRESHOLD);

    CompactionConfig {
        strategy: match config.compaction.strategy.as_deref() {
            Some("trim") => CompactionStrategyType::Trim,
            Some("summary") => CompactionStrategyType::Summary,
            _ => CompactionStrategyType::Hybrid,
        },
        threshold: if threshold.is_finite() {
            threshold.clamp(0.0, 1.0)
        } else {
            DEFAULT_COMPACTION_THRESHOLD
        },
        ..Default::default()
    }
}

// Transitional alias for peripheral views that are not compiled by the new shell.
pub type AgentFactory = AppRuntime;

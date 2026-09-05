use crate::session::SessionId;
use anyhow::anyhow;
use async_trait::async_trait;
use averroes_core::agent::orchestration::{
    AgentCallRequest, AgentDescriptor, AgentRunner, AgentThreadSnapshot, AgentThreadStatus,
};
use averroes_core::agent::{Agent, AgentConfig, AgentStreamEvent};
use averroes_core::codex::{CodexAccount, CodexClient, CodexError, CodexLogin, CodexModel};
use averroes_core::compaction::{CompactionConfig, CompactionStrategyType};
use averroes_core::config::AgentProfile;
use averroes_core::config::{AppConfig, ConfigError, ConfigPaths, RemoteAgentSection};
use averroes_core::connection::{
    ConnectionId, ConnectionKind, ConnectionProfile, CredentialRef, SessionBinding,
};
use averroes_core::credentials::{CredentialVault, VaultError, VaultKeyProvider};
use averroes_core::diagnostics::{self, DiagnosticLevel};
use averroes_core::github::{
    CopilotEndpoint, CopilotModel, GitHubCopilotClient, GitHubError, GitHubLogin,
};
use averroes_core::integrations::mcp::{
    McpClient, ProjectMcpConfig, ProjectMcpServer, PROJECT_MCP_FILE,
};
use averroes_core::memory::{compile_fragments_with_context, cosine_similarity, decode_embedding};
use averroes_core::models::{ManualModel, ModelRegistry};
use averroes_core::prompt::{ProjectInstructions, PromptBuilder};
use averroes_core::provider::codex::ChatGptCodexProvider;
use averroes_core::provider::factory::{
    create_copilot_provider, create_direct_provider, ProviderFactoryError,
};
use averroes_core::provider::types::MessageContent;
use averroes_core::provider::{
    EmbeddingRequest, ModelDiscovery, ModelInfo, Provider, ProviderRegistry,
};
use averroes_core::runtime::ResourceGovernor;
use averroes_core::skill::{SkillIndex, SkillLoader};
use averroes_core::tool::builtin::ask_user::{AskUserBroker, AskUserParams, UserQuestion};
use averroes_core::tool::dynamic::{DynamicTool, DynamicToolConfig, DynamicToolHandler};
use averroes_core::tool::{
    builtin, MemorySearchBackend, SkillMarketplaceBackend, SkillMarketplaceEntry, ToolRegistry,
};
use averroes_core::work::{
    ConversationSearchResult, EmbeddingConfig, VectorSearchHit, WorkDatabase, WorkDatabaseError,
};
use futures::stream::{self, StreamExt};
use oxibrowser_core::network::IpFilter;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::future::Future;
use std::io::{Cursor, Read};
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroize::Zeroizing;

const DEFAULT_COMPACTION_THRESHOLD: f64 = 0.8;
const MAX_CONCURRENT_CALLS: usize = tokio::sync::Semaphore::MAX_PERMITS;
const MAX_MARKETPLACE_SKILL_FILES: usize = 256;
const MAX_MARKETPLACE_SKILL_BYTES: usize = 32 * 1024 * 1024;
const REMOTE_AGENT_CREDENTIAL_REF: &str = "credential:remote-agent:telegram-bot";

struct MarketplaceSkillFile {
    path: PathBuf,
    contents: Vec<u8>,
}
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

#[derive(Debug, Clone)]
pub struct MarketplaceSkill {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub source: String,
    pub slug: String,
    pub installs: u64,
    pub url: Option<String>,
}

impl MarketplaceSkill {
    fn from_value(value: serde_json::Value) -> Option<Self> {
        let id = value.get("id")?.as_str()?.trim().to_owned();
        let name = value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("skillId").and_then(serde_json::Value::as_str))
            .or_else(|| value.get("slug").and_then(serde_json::Value::as_str))?
            .trim()
            .to_owned();
        if id.is_empty() || name.is_empty() {
            return None;
        }
        let public_url = format!("https://skills.sh/{id}");
        Some(Self {
            id,
            name,
            description: value
                .get("description")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.get("summary").and_then(serde_json::Value::as_str))
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(str::to_owned),
            source: value
                .get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            slug: value
                .get("slug")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.get("skillId").and_then(serde_json::Value::as_str))
                .unwrap_or_default()
                .to_owned(),
            installs: value
                .get("installs")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
            url: value
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or(Some(public_url)),
        })
    }
}

fn parse_public_trending_skills(body: &str) -> Vec<MarketplaceSkill> {
    let marker = r#"\"initialSkills\":["#;
    let Some(marker_start) = body.find(marker) else {
        return Vec::new();
    };
    let start = marker_start + marker.len() - 1;
    let mut depth = 0;
    let mut end = None;
    for (offset, character) in body[start..].char_indices() {
        match character {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + offset + character.len_utf8());
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else {
        return Vec::new();
    };
    let decoded = body[start..end].replace("\\\"", "\"");
    let Ok(values) = serde_json::from_str::<Vec<serde_json::Value>>(&decoded) else {
        return Vec::new();
    };
    values
        .into_iter()
        .filter_map(|mut value| {
            let source = value.get("source").and_then(serde_json::Value::as_str)?;
            let skill_id = value.get("skillId").and_then(serde_json::Value::as_str)?;
            value["id"] = serde_json::Value::String(format!("{source}/{skill_id}"));
            MarketplaceSkill::from_value(value)
        })
        .take(20)
        .collect()
}

#[cfg(test)]
mod marketplace_skill_tests {
    use super::{is_well_known_skill_domain, parse_public_trending_skills, MarketplaceSkill};
    use serde_json::json;

    #[test]
    fn parses_public_skills_directory_results() {
        let skill = MarketplaceSkill::from_value(json!({
            "id": "vercel-labs/agent-skills/web-design-guidelines",
            "skillId": "web-design-guidelines",
            "name": "web-design-guidelines",
            "description": "Guidelines for building polished web interfaces",
            "installs": 598673,
            "source": "vercel-labs/agent-skills"
        }))
        .expect("public skills result should parse");

        assert_eq!(skill.name, "web-design-guidelines");
        assert_eq!(skill.slug, "web-design-guidelines");
        assert_eq!(skill.installs, 598673);
        assert_eq!(
            skill.description.as_deref(),
            Some("Guidelines for building polished web interfaces")
        );
        assert_eq!(
            skill.url.as_deref(),
            Some("https://skills.sh/vercel-labs/agent-skills/web-design-guidelines")
        );
    }

    #[test]
    fn parses_public_trending_page_data() {
        let skills = parse_public_trending_skills(
            r#"50:["$","$L57",null,{\"initialSkills\":[{\"source\":\"owner/repo\",\"skillId\":\"release\",\"name\":\"release\",\"installs\":42}]}]"#,
        );

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "owner/repo/release");
        assert_eq!(skills[0].installs, 42);
    }

    #[test]
    fn accepts_only_dns_like_marketplace_domains() {
        assert!(is_well_known_skill_domain("skills.example.com"));
        assert!(!is_well_known_skill_domain("127.0.0.1"));
        assert!(!is_well_known_skill_domain("localhost"));
        assert!(!is_well_known_skill_domain("-invalid.example"));
    }
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

    fn restore(&self, threads: impl IntoIterator<Item = AgentThreadSnapshot>) {
        let mut current = self.threads.write();
        for mut thread in threads {
            if thread.status == AgentThreadStatus::Running {
                thread.status = AgentThreadStatus::Interrupted;
            }
            let should_restore = current
                .get(&thread.id)
                .is_none_or(|existing| thread.updated_at > existing.updated_at);
            if should_restore {
                current.insert(thread.id.clone(), thread);
            }
        }
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

    fn interrupt_for_parent(&self, parent_session_id: &str) {
        let mut threads = self.threads.write();
        for thread in threads.values_mut() {
            if thread.parent_session_id == parent_session_id
                && thread.status == AgentThreadStatus::Running
            {
                thread.status = AgentThreadStatus::Interrupted;
                thread.output = "Delegated agent interrupted.".into();
                thread.updated_at = averroes_core::work::now();
            }
        }
    }
}

impl Default for AgentThreadRegistry {
    fn default() -> Self {
        Self::new()
    }
}

struct DelegatedRunGuard {
    threads: Arc<AgentThreadRegistry>,
    snapshot: Option<AgentThreadSnapshot>,
    abort_handle: Option<tokio::task::AbortHandle>,
}

impl DelegatedRunGuard {
    fn new(threads: Arc<AgentThreadRegistry>, snapshot: AgentThreadSnapshot) -> Self {
        Self {
            threads,
            snapshot: Some(snapshot),
            abort_handle: None,
        }
    }

    fn set_abort_handle(&mut self, abort_handle: tokio::task::AbortHandle) {
        self.abort_handle = Some(abort_handle);
    }

    fn disarm(&mut self) {
        self.snapshot = None;
        self.abort_handle = None;
    }
}

impl Drop for DelegatedRunGuard {
    fn drop(&mut self) {
        if let Some(abort_handle) = self.abort_handle.take() {
            abort_handle.abort();
        }
        let Some(mut snapshot) = self.snapshot.take() else {
            return;
        };
        snapshot.status = AgentThreadStatus::Interrupted;
        snapshot.output = "Delegated agent interrupted.".into();
        snapshot.updated_at = averroes_core::work::now();
        self.threads.upsert(snapshot);
    }
}

fn delegated_agent_tools(
    _existing: Option<&AgentThreadSnapshot>,
    all_tools: Vec<String>,
) -> Vec<String> {
    all_tools
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
        let existing_thread = self.threads.get(thread_id);
        if let Some(existing) = existing_thread.as_ref() {
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
        let tools = delegated_agent_tools(existing_thread.as_ref(), self.tool_registry.names());
        let running = AgentThreadSnapshot {
            id: thread_id.to_owned(),
            thread_id: thread_id.to_owned(),
            agent_id: request.agent_id.clone(),
            parent_session_id: request.parent_session_id.clone(),
            title,
            model_id: model_id.clone(),
            status: AgentThreadStatus::Running,
            enabled_tools: tools.clone(),
            prompt: request.prompt.clone(),
            output: String::new(),
            created_at: now,
            updated_at: now,
        };
        self.threads.upsert(running.clone());
        let mut run_guard = DelegatedRunGuard::new(self.threads.clone(), running.clone());

        let provider = match self
            .provider_resolver
            .provider_for(&connection_id, &model_id)
            .await
        {
            Ok(provider) => provider,
            Err(error) => {
                run_guard.disarm();
                let failed = AgentThreadSnapshot {
                    status: AgentThreadStatus::Failed,
                    output: error.clone(),
                    updated_at: averroes_core::work::now(),
                    ..running
                };
                self.threads.upsert(failed);
                return Err(error);
            }
        };
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
        let agent = Arc::new(Agent::new(
            AgentConfig {
                name: format!(
                    "delegated-{}",
                    thread_id.chars().take(8).collect::<String>()
                ),
                model: model_id,
                system_prompt: Some(format!(
                    "{}{parent_context}\n\n## Delegation boundary\nYou are a delegated leaf agent. Do not call `list_agents`, `call_agent`, or `call_agents`, and do not start another subagent. Complete the assigned objective yourself and return your result to the parent agent.",
                    self.system_prompt
                )),
                project_instructions_root: Some(request.working_dir.clone()),
                tools,
                max_iterations: 24,
                compaction: self.compaction.clone(),
                reasoning_effort: self.reasoning_effort.clone(),
                tool_approval_policy: request.tool_approval_policy,
                allow_delegation: false,
                work_conversation_id: Some(request.parent_session_id.clone()),
                work_id_prefix: Some(format!("agent:{}:", thread_id)),
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
            run_guard.set_abort_handle(child_task.abort_handle());
            while let Some(event) = child_events.recv().await {
                let _ = parent_events.send(AgentStreamEvent::DelegatedAgentEvent {
                    thread_id: thread_id.to_owned(),
                    event: Box::new(event),
                });
            }
            match child_task.await {
                Ok(result) => result,
                Err(error) => Err(anyhow!("delegated agent task failed: {error}")),
            }
        } else {
            agent.run(&request.prompt).await
        };
        self.threads
            .set_context(thread_id, agent.conversation_history().await);
        let now = averroes_core::work::now();
        let (status, output) = match result {
            Ok(output) => (AgentThreadStatus::Completed, output),
            Err(error) => (AgentThreadStatus::Failed, error.to_string()),
        };
        let finished = AgentThreadSnapshot {
            status,
            output,
            updated_at: now,
            enabled_tools: agent.enabled_tool_names(),
            ..running
        };
        self.threads.upsert(finished.clone());
        run_guard.disarm();
        Ok(finished)
    }
}

#[cfg(test)]
mod delegated_agent_tool_tests {
    use super::{delegated_agent_tools, AgentThreadRegistry, DelegatedRunGuard};
    use averroes_core::agent::orchestration::{AgentThreadSnapshot, AgentThreadStatus};

    fn thread_with_tools(enabled_tools: Vec<String>) -> AgentThreadSnapshot {
        AgentThreadSnapshot {
            id: "thread-1".into(),
            thread_id: "thread-1".into(),
            agent_id: "researcher".into(),
            parent_session_id: "conversation-1".into(),
            title: "Research".into(),
            model_id: "model-1".into(),
            status: AgentThreadStatus::Completed,
            enabled_tools,
            prompt: "First turn".into(),
            output: "Done".into(),
            created_at: 1,
            updated_at: 2,
        }
    }

    #[test]
    fn delegated_threads_use_the_complete_tool_catalog() {
        let existing = thread_with_tools(vec!["shell".into(), "file_read".into()]);

        assert_eq!(
            delegated_agent_tools(Some(&existing), vec!["patch".into(), "web_fetch".into()]),
            vec!["patch", "web_fetch"]
        );
    }

    #[test]
    fn legacy_threads_are_upgraded_to_all_tools() {
        let legacy = thread_with_tools(Vec::new());
        let all_tools = vec!["patch".into(), "web_fetch".into()];

        assert_eq!(
            delegated_agent_tools(Some(&legacy), all_tools.clone()),
            all_tools
        );
    }

    #[test]
    fn restored_threads_keep_tools_and_do_not_replace_newer_live_state() {
        let registry = AgentThreadRegistry::new();
        let mut saved = thread_with_tools(vec!["web_search".into()]);
        saved.status = AgentThreadStatus::Running;
        registry.restore([saved]);

        let restored = registry.get("thread-1").expect("saved thread restored");
        assert_eq!(restored.enabled_tools, vec!["web_search"]);
        assert_eq!(restored.status, AgentThreadStatus::Interrupted);

        let mut live = thread_with_tools(vec!["shell".into()]);
        live.updated_at = 3;
        registry.upsert(live);
        registry.restore([thread_with_tools(vec!["file_read".into()])]);

        assert_eq!(
            registry
                .get("thread-1")
                .expect("live thread retained")
                .enabled_tools,
            vec!["shell"]
        );
    }

    #[tokio::test]
    async fn dropping_a_delegated_run_interrupts_the_snapshot_and_aborts_the_child() {
        let registry = std::sync::Arc::new(AgentThreadRegistry::new());
        let snapshot = thread_with_tools(Vec::new());
        registry.upsert(snapshot.clone());
        let task = tokio::spawn(std::future::pending::<()>());
        let abort_handle = task.abort_handle();

        let mut guard = DelegatedRunGuard::new(registry.clone(), snapshot);
        guard.set_abort_handle(abort_handle);
        drop(guard);

        let restored = registry.get("thread-1").expect("thread remains registered");
        assert_eq!(restored.status, AgentThreadStatus::Interrupted);
        assert!(task.await.unwrap_err().is_cancelled());
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
    #[error("project MCP error: {0}")]
    ProjectMcp(String),
}

#[async_trait]
impl SkillMarketplaceBackend for AppRuntime {
    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> std::result::Result<Vec<SkillMarketplaceEntry>, String> {
        self.search_skill_marketplace(query)
            .await
            .map(|skills| {
                skills
                    .into_iter()
                    .take(limit)
                    .map(|skill| SkillMarketplaceEntry {
                        id: skill.id,
                        name: skill.name,
                        description: skill.description,
                        source: skill.source,
                        slug: skill.slug,
                        installs: skill.installs,
                        url: skill.url,
                    })
                    .collect()
            })
            .map_err(|error| error.to_string())
    }

    async fn install(
        &self,
        workspace_root: &Path,
        skill: &SkillMarketplaceEntry,
    ) -> std::result::Result<String, String> {
        let skill = MarketplaceSkill {
            id: skill.id.clone(),
            name: skill.name.clone(),
            description: skill.description.clone(),
            source: skill.source.clone(),
            slug: skill.slug.clone(),
            installs: skill.installs,
            url: skill.url.clone(),
        };
        self.install_skill_from_marketplace(workspace_root, &skill)
            .await
            .map_err(|error| error.to_string())
    }
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
        let user_questions = Arc::new(AskUserBroker::default());
        let tools = Arc::new(ToolRegistry::with_confirmation_broker(
            user_questions.clone(),
        ));
        builtin::register_all(&tools);
        let database = WorkDatabase::open(&paths)?;
        tools.register(builtin::search_memory::SearchMemoryTool::new(
            database.clone(),
        ));
        tools.register(builtin::checkpoint::CheckpointTool::new(database.clone()));
        tools.register(builtin::task::TaskListTool::new(database.clone()));
        tools.register(builtin::task::AddTaskTool::new(database.clone()));
        tools.register(builtin::task::UpdateTaskTool::new(database.clone()));
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

    pub fn remote_agent(&self) -> RemoteAgentSection {
        self.config.read().remote_agent.clone()
    }

    pub fn has_remote_agent_token(&self) -> bool {
        self.vault
            .contains(&remote_agent_credential_ref())
            .unwrap_or(false)
    }

    pub fn remote_agent_token(&self) -> Result<Zeroizing<String>, RuntimeError> {
        Ok(self.vault.get(&remote_agent_credential_ref())?)
    }

    /// Persist Telegram relay settings while keeping the bot token in the
    /// same encrypted vault used for provider credentials.
    pub fn save_remote_agent(
        &self,
        settings: RemoteAgentSection,
        token: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let credential = remote_agent_credential_ref();
        let previous_token = self.vault.get(&credential).ok();
        let supplied_token = token.filter(|value| !value.trim().is_empty());

        if settings.enabled && supplied_token.is_none() && previous_token.is_none() {
            return Err(RuntimeError::Runtime(
                "a Telegram bot token is required to enable Remote Agent".into(),
            ));
        }

        if let Some(token) = supplied_token {
            self.vault.put(&credential, token)?;
        }

        let mut next = self.config();
        next.remote_agent = settings;
        if let Err(error) = next.save_to(&self.paths) {
            match previous_token {
                Some(previous) => {
                    let _ = self.vault.put(&credential, &previous);
                }
                None if supplied_token.is_some() => {
                    let _ = self.vault.delete(&credential);
                }
                None => {}
            }
            return Err(error.into());
        }

        *self.config.write() = next;
        Ok(())
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

    pub fn interrupt_agent_threads_for(&self, parent_session_id: &str) {
        self.agent_threads.interrupt_for_parent(parent_session_id);
    }

    fn default_agent_tools_for(&self, registry: &ToolRegistry) -> Vec<String> {
        registry.names()
    }

    fn agent_tools(&self, _binding: &SessionBinding, registry: &ToolRegistry) -> Vec<String> {
        self.default_agent_tools_for(registry)
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

    pub fn project_mcp_config(
        &self,
        workspace_root: &Path,
    ) -> Result<ProjectMcpConfig, RuntimeError> {
        ProjectMcpConfig::load(workspace_root)
            .map_err(|error| RuntimeError::ProjectMcp(error.to_string()))
    }

    pub fn save_project_mcp_server(
        &self,
        workspace_root: &Path,
        name: &str,
        mut server: ProjectMcpServer,
        secret: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(RuntimeError::ProjectMcp(
                "MCP server name cannot be empty".into(),
            ));
        }
        let mut config = self.project_mcp_config(workspace_root)?;
        let previous = config.servers.get(name).cloned();
        let mut credential_ref_to_store = None;
        if server.auth.kind != averroes_core::integrations::mcp::McpAuthType::None {
            let credential_ref = server
                .auth
                .credential_ref
                .clone()
                .or_else(|| {
                    previous
                        .as_ref()
                        .and_then(|item| item.auth.credential_ref.clone())
                })
                .unwrap_or_else(|| project_mcp_credential_ref(workspace_root, name).0);
            server.auth.credential_ref = Some(credential_ref.clone());
            credential_ref_to_store = Some(credential_ref);
        }
        let clear_previous_credential =
            server.auth.kind == averroes_core::integrations::mcp::McpAuthType::None;
        let previous_credential = previous
            .as_ref()
            .and_then(|item| item.auth.credential_ref.clone());
        server
            .validate()
            .map_err(|error| RuntimeError::ProjectMcp(error.to_string()))?;
        if let (Some(credential_ref), Some(secret)) = (
            credential_ref_to_store,
            secret.filter(|secret| !secret.trim().is_empty()),
        ) {
            self.vault.put(&CredentialRef(credential_ref), secret)?;
        }
        config.servers.insert(name.to_owned(), server);
        config
            .save(workspace_root)
            .map_err(|error| RuntimeError::ProjectMcp(error.to_string()))?;
        if clear_previous_credential {
            if let Some(credential_ref) = previous_credential {
                let _ = self.vault.delete(&CredentialRef(credential_ref));
            }
        }
        Ok(())
    }

    pub fn delete_project_mcp_server(
        &self,
        workspace_root: &Path,
        name: &str,
    ) -> Result<bool, RuntimeError> {
        let mut config = self.project_mcp_config(workspace_root)?;
        let Some(server) = config.servers.remove(name) else {
            return Ok(false);
        };
        config
            .save(workspace_root)
            .map_err(|error| RuntimeError::ProjectMcp(error.to_string()))?;
        if let Some(credential_ref) = server.auth.credential_ref {
            let _ = self.vault.delete(&CredentialRef(credential_ref));
        }
        Ok(true)
    }

    pub fn project_mcp_file(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join(PROJECT_MCP_FILE)
    }

    /// Returns only skills installed inside the project. Global skills remain
    /// available to agents but are not presented as project-owned files.
    pub fn project_skills(&self, workspace_root: &Path) -> Vec<averroes_core::skill::SkillMeta> {
        let Some(index) = self.workspace_skill_index(workspace_root) else {
            return Vec::new();
        };
        let roots = project_skill_roots(workspace_root);
        index
            .list()
            .into_iter()
            .filter(|skill| roots.iter().any(|root| skill.path.starts_with(root)))
            .cloned()
            .collect()
    }

    pub fn delete_project_skill(
        &self,
        workspace_root: &Path,
        name: &str,
    ) -> Result<bool, RuntimeError> {
        let Some(index) = self.refresh_workspace_skills(workspace_root) else {
            return Ok(false);
        };
        let Some(skill) = index.get(name) else {
            return Ok(false);
        };
        let roots = project_skill_roots(workspace_root);
        if !roots.iter().any(|root| skill.path.starts_with(root)) {
            return Err(RuntimeError::Runtime(
                "refusing to delete a global skill from project settings".into(),
            ));
        }
        let directory = skill.path.parent().unwrap_or(skill.path.as_path());
        std::fs::remove_dir_all(directory).map_err(|error| {
            RuntimeError::Runtime(format!("could not delete skill '{}': {error}", name))
        })?;
        self.refresh_workspace_skills(workspace_root);
        Ok(true)
    }

    pub async fn search_skill_marketplace(
        &self,
        query: &str,
    ) -> Result<Vec<MarketplaceSkill>, RuntimeError> {
        let query = query.trim();
        let client = marketplace_http_client()?;
        if query.is_empty() {
            let response = client
                .get("https://skills.sh/trending")
                .send()
                .await
                .map_err(|error| {
                    RuntimeError::Runtime(format!("skills marketplace request failed: {error}"))
                })?;
            let status = response.status();
            let body = response.text().await.map_err(|error| {
                RuntimeError::Runtime(format!("invalid skills marketplace response: {error}"))
            })?;
            if !status.is_success() {
                return Err(RuntimeError::Runtime(format!(
                    "skills marketplace returned HTTP {status}"
                )));
            }
            let skills = parse_public_trending_skills(&body);
            if skills.is_empty() {
                return Err(RuntimeError::Runtime(
                    "skills marketplace returned no featured skills".into(),
                ));
            }
            return Ok(skills);
        }
        if query.chars().count() < 2 {
            return Ok(Vec::new());
        }

        // The documented /api/v1 endpoints are Vercel OIDC-protected and
        // therefore cannot be called by a standalone desktop application.
        // The public directory uses this endpoint for its own search UI.
        let request = client
            .get("https://skills.sh/api/search")
            .query(&[("q", query), ("limit", "20")]);
        let response = request.send().await.map_err(|error| {
            RuntimeError::Runtime(format!("skills marketplace request failed: {error}"))
        })?;
        let status = response.status();
        let body: serde_json::Value = response.json().await.map_err(|error| {
            RuntimeError::Runtime(format!("invalid skills marketplace response: {error}"))
        })?;
        if !status.is_success() {
            return Err(RuntimeError::Runtime(format!(
                "skills marketplace returned HTTP {status}: {body}"
            )));
        }
        let items = body
            .get("skills")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let skills = items
            .into_iter()
            .filter_map(MarketplaceSkill::from_value)
            .collect::<Vec<_>>();
        if skills.is_empty() && body.get("count").and_then(serde_json::Value::as_u64) > Some(0) {
            return Err(RuntimeError::Runtime(
                "skills marketplace returned results in an unsupported format".into(),
            ));
        }
        Ok(skills)
    }

    pub async fn install_skill_from_marketplace(
        &self,
        workspace_root: &Path,
        skill: &MarketplaceSkill,
    ) -> Result<String, RuntimeError> {
        let source = skill.source.trim();
        let slug = skill.slug.trim();
        let valid_segment = |value: &str| {
            !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\'])
        };
        if !valid_segment(slug) {
            return Err(RuntimeError::Runtime(
                "marketplace skill has an invalid skill name".into(),
            ));
        }
        let source = source.to_owned();
        let slug = slug.to_owned();
        if is_well_known_skill_domain(&source)
            && !IpFilter::block_private().is_hostname_allowed(&source)
        {
            return Err(RuntimeError::Runtime(
                "marketplace skill source resolves to a private or unresolved host".into(),
            ));
        }
        let client = marketplace_http_client()?;
        let files = if source.split('/').count() == 2 && source.split('/').all(valid_segment) {
            fetch_github_skill_files(&client, &source, &slug).await?
        } else if is_well_known_skill_domain(&source) {
            fetch_domain_skill_files(&client, &source, &slug).await?
        } else {
            return Err(RuntimeError::Runtime(
                "marketplace skill has an unsupported source".into(),
            ));
        };
        let destination = workspace_root.join(".averroes").join("skills").join(&slug);
        for file in files {
            let destination = destination.join(file.path);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| RuntimeError::Runtime(error.to_string()))?;
            }
            std::fs::write(destination, file.contents)
                .map_err(|error| RuntimeError::Runtime(error.to_string()))?;
        }
        self.refresh_workspace_skills(workspace_root);
        Ok(slug)
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
        self: &Arc<Self>,
        session_id: &SessionId,
        binding: &SessionBinding,
        working_dir: Option<&Path>,
    ) -> Result<Arc<Agent>, RuntimeError> {
        self.validate_binding(binding)?;
        if let Some(conversation) = self.database.conversation(session_id.as_str())? {
            self.agent_threads.restore(conversation.agent_threads);
        }
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
        if workspace_root.is_some() {
            builtin::register_skill_marketplace_tools(&scoped_tools, self.clone());
        }
        self.register_project_mcp_tools(&scoped_tools, workspace_root.as_deref())
            .await;

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
        let system_prompt = self.prompt.build_system_with_approval_policy(
            &working_dir.display().to_string(),
            None,
            binding.approval_policy,
        );
        let config = self.config();
        let agent_config = AgentConfig {
            name: "averroes".into(),
            model: model.to_owned(),
            system_prompt: Some(system_prompt.clone()),
            project_instructions_root: workspace_root.clone(),
            tools: tools.clone(),
            max_iterations: 50,
            compaction: compaction_config(&config),
            tool_approval_policy: binding.approval_policy,
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

    fn workspace_skill_index(&self, workspace_root: &Path) -> Option<Arc<SkillIndex>> {
        if let Some(index) = self.workspace_skills.read().get(workspace_root).cloned() {
            return Some(index);
        }
        self.load_workspace_tools(workspace_root);
        self.workspace_skills.read().get(workspace_root).cloned()
    }

    fn build_workspace_skill_index(&self, workspace_root: &Path) -> Option<Arc<SkillIndex>> {
        let project_paths = project_skill_roots(workspace_root);
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

    async fn register_project_mcp_tools(
        &self,
        registry: &ToolRegistry,
        workspace_root: Option<&Path>,
    ) {
        let Some(workspace_root) = workspace_root else {
            return;
        };
        let config = match self.project_mcp_config(workspace_root) {
            Ok(config) => config,
            Err(error) => {
                diagnostics::record(DiagnosticLevel::Warning, "mcp.project", error.to_string());
                return;
            }
        };
        for (server_name, server) in config.servers {
            let access_token = server
                .auth
                .credential_ref
                .as_ref()
                .and_then(|reference| self.vault.get(&CredentialRef(reference.clone())).ok())
                .map(|secret| secret.to_string());
            let client =
                match McpClient::from_project_server(server_name.clone(), &server, access_token) {
                    Ok(client) => Arc::new(client),
                    Err(error) => {
                        diagnostics::record(
                            DiagnosticLevel::Warning,
                            "mcp.project",
                            format!("Skipping '{server_name}': {error}"),
                        );
                        continue;
                    }
                };
            match client.list_tools().await {
                Ok(tools) => {
                    for tool in tools {
                        let name = format!(
                            "mcp__{}__{}",
                            mcp_name_part(&server_name),
                            mcp_name_part(&tool.name)
                        );
                        registry.register(DynamicTool::new(DynamicToolConfig {
                            name,
                            description: format!(
                                "{} (MCP server: {server_name})",
                                tool.description
                            ),
                            parameters: tool.input_schema,
                            handler: DynamicToolHandler::MCP {
                                client: client.clone(),
                                tool_name: tool.name,
                            },
                        }));
                    }
                }
                Err(error) => diagnostics::record(
                    DiagnosticLevel::Warning,
                    "mcp.project",
                    format!("Could not discover tools from '{server_name}': {error}"),
                ),
            }
        }
    }

    pub fn spawn_agent_stream(&self, agent: Arc<Agent>, prompt: String) -> AgentStreamHandle {
        self.spawn_agent_stream_with_content(agent, prompt, None)
    }

    pub fn spawn_agent_stream_with_content(
        &self,
        agent: Arc<Agent>,
        prompt: String,
        content: Option<MessageContent>,
    ) -> AgentStreamHandle {
        match self.database.global_memory_prompt() {
            Ok(global_memory_prompt) => agent.set_global_memory_prompt(global_memory_prompt),
            Err(error) => diagnostics::record(
                DiagnosticLevel::Warning,
                "memory.global",
                format!("Could not refresh the global-memory prompt: {error}"),
            ),
        }
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        let handle = self.runtime.spawn(async move {
            match content {
                Some(content) => {
                    agent
                        .run_streaming_with_content(&prompt, content, sender)
                        .await
                }
                None => agent.run_streaming(&prompt, sender).await,
            }
        });
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

    pub fn cancel_user_question(&self, session_id: &SessionId, question_id: Option<&str>) {
        if let Some(question_id) = question_id {
            self.user_questions.cancel(session_id.as_str(), question_id);
        } else {
            self.user_questions.cancel_session(session_id.as_str());
        }
    }

    pub fn spawn_background<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }
}

fn remote_agent_credential_ref() -> CredentialRef {
    CredentialRef(REMOTE_AGENT_CREDENTIAL_REF.into())
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

async fn fetch_github_skill_files(
    client: &reqwest::Client,
    source: &str,
    slug: &str,
) -> Result<Vec<MarketplaceSkillFile>, RuntimeError> {
    let roots = [format!("skills/{slug}"), slug.to_owned()];
    for root in roots {
        let endpoint = format!("https://api.github.com/repos/{source}/contents/{root}");
        let response = client
            .get(&endpoint)
            .header(reqwest::header::USER_AGENT, "Averroes")
            .send()
            .await
            .map_err(|error| {
                RuntimeError::Runtime(format!("skill catalogue request failed: {error}"))
            })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            continue;
        }
        let status = response.status();
        if !status.is_success() {
            return Err(RuntimeError::Runtime(format!(
                "GitHub skill catalogue returned HTTP {status}"
            )));
        }
        let value: serde_json::Value = response.json().await.map_err(|error| {
            RuntimeError::Runtime(format!("invalid GitHub skill catalogue response: {error}"))
        })?;
        let files = collect_github_skill_files(client, &root, value).await?;
        if files.iter().any(|file| is_skill_manifest(&file.path)) {
            return download_github_skill_files(client, files).await;
        }
    }
    Err(RuntimeError::Runtime(format!(
        "SKILL.md was not found in the public repository for '{slug}'"
    )))
}

async fn collect_github_skill_files(
    client: &reqwest::Client,
    root: &str,
    value: serde_json::Value,
) -> Result<Vec<GithubSkillFileRef>, RuntimeError> {
    let mut directories = vec![(root.to_owned(), value)];
    let mut files = Vec::new();
    let mut expected_bytes = 0_u64;
    while let Some((directory, value)) = directories.pop() {
        let entries = value.as_array().ok_or_else(|| {
            RuntimeError::Runtime(format!(
                "GitHub skill path '{directory}' is not a directory"
            ))
        })?;
        for entry in entries {
            let path = entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| RuntimeError::Runtime("GitHub skill entry has no path".into()))?;
            let relative = safe_skill_relative_path(path, root)?;
            match entry.get("type").and_then(serde_json::Value::as_str) {
                Some("dir") => {
                    let endpoint = entry
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .filter(|url| url.starts_with("https://api.github.com/repos/"))
                        .ok_or_else(|| {
                            RuntimeError::Runtime(
                                "GitHub skill directory has an invalid API URL".into(),
                            )
                        })?;
                    let response = client
                        .get(endpoint)
                        .header(reqwest::header::USER_AGENT, "Averroes")
                        .send()
                        .await
                        .map_err(|error| {
                            RuntimeError::Runtime(format!(
                                "GitHub skill directory request failed: {error}"
                            ))
                        })?;
                    let status = response.status();
                    if !status.is_success() {
                        return Err(RuntimeError::Runtime(format!(
                            "GitHub skill directory returned HTTP {status}"
                        )));
                    }
                    let children = response.json().await.map_err(|error| {
                        RuntimeError::Runtime(format!(
                            "invalid GitHub skill directory response: {error}"
                        ))
                    })?;
                    directories.push((path.to_owned(), children));
                }
                Some("file") => {
                    let download_url = entry
                        .get("download_url")
                        .and_then(serde_json::Value::as_str)
                        .filter(|url| url.starts_with("https://raw.githubusercontent.com/"))
                        .ok_or_else(|| {
                            RuntimeError::Runtime(
                                "GitHub skill file has no trusted download URL".into(),
                            )
                        })?;
                    let size = entry
                        .get("size")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default();
                    expected_bytes = expected_bytes.saturating_add(size);
                    if files.len() >= MAX_MARKETPLACE_SKILL_FILES
                        || expected_bytes > MAX_MARKETPLACE_SKILL_BYTES as u64
                    {
                        return Err(RuntimeError::Runtime(
                            "marketplace skill is too large to install safely".into(),
                        ));
                    }
                    files.push(GithubSkillFileRef {
                        path: relative,
                        download_url: download_url.to_owned(),
                    });
                }
                Some("symlink") | Some("submodule") | None => {}
                Some(kind) => {
                    return Err(RuntimeError::Runtime(format!(
                        "unsupported GitHub skill entry type '{kind}'"
                    )));
                }
            }
        }
    }
    Ok(files)
}

async fn download_github_skill_files(
    client: &reqwest::Client,
    files: Vec<GithubSkillFileRef>,
) -> Result<Vec<MarketplaceSkillFile>, RuntimeError> {
    let downloaded = stream::iter(files.into_iter().map(|file| {
        let client = client.clone();
        async move {
            let response = client
                .get(&file.download_url)
                .header(reqwest::header::USER_AGENT, "Averroes")
                .send()
                .await
                .map_err(|error| {
                    RuntimeError::Runtime(format!("skill file download failed: {error}"))
                })?;
            let status = response.status();
            if !status.is_success() {
                return Err(RuntimeError::Runtime(format!(
                    "skill file download returned HTTP {status}"
                )));
            }
            let contents = response.bytes().await.map_err(|error| {
                RuntimeError::Runtime(format!("could not read skill file: {error}"))
            })?;
            if contents.len() > MAX_MARKETPLACE_SKILL_BYTES {
                return Err(RuntimeError::Runtime(
                    "marketplace skill file is too large to install safely".into(),
                ));
            }
            Ok(MarketplaceSkillFile {
                path: file.path,
                contents: contents.to_vec(),
            })
        }
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    let mut result = Vec::with_capacity(downloaded.len());
    let mut total_bytes = 0_usize;
    for file in downloaded {
        let file = file?;
        total_bytes = total_bytes.saturating_add(file.contents.len());
        if total_bytes > MAX_MARKETPLACE_SKILL_BYTES {
            return Err(RuntimeError::Runtime(
                "marketplace skill is too large to install safely".into(),
            ));
        }
        result.push(file);
    }
    Ok(result)
}

async fn fetch_domain_skill_files(
    client: &reqwest::Client,
    source: &str,
    slug: &str,
) -> Result<Vec<MarketplaceSkillFile>, RuntimeError> {
    let index_url = format!("https://{source}/.well-known/agent-skills/index.json");
    let index_bytes = fetch_marketplace_bytes(client, &index_url, "skill domain index").await?;
    let index: serde_json::Value = serde_json::from_slice(&index_bytes)
        .map_err(|error| RuntimeError::Runtime(format!("invalid skill domain index: {error}")))?;
    let entry = index
        .get("skills")
        .and_then(serde_json::Value::as_array)
        .and_then(|skills| {
            skills.iter().find(|entry| {
                entry
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|name| name == slug)
            })
        })
        .ok_or_else(|| {
            RuntimeError::Runtime(format!(
                "skill '{slug}' was not found in {source}'s public skill index"
            ))
        })?;
    let raw_url = entry
        .get("url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| RuntimeError::Runtime("skill domain entry has no download URL".into()))?;
    let url = if raw_url.starts_with('/') {
        format!("https://{source}{raw_url}")
    } else {
        raw_url.to_owned()
    };
    if !url.starts_with(&format!("https://{source}/")) {
        return Err(RuntimeError::Runtime(
            "skill domain entry points outside its source domain".into(),
        ));
    }
    let is_archive = entry.get("type").and_then(serde_json::Value::as_str) == Some("archive")
        || url.to_ascii_lowercase().ends_with(".zip");
    let bytes = fetch_marketplace_bytes(client, &url, "skill files").await?;
    if is_archive {
        decode_skill_archive(bytes)
    } else {
        Ok(vec![MarketplaceSkillFile {
            path: PathBuf::from("SKILL.md"),
            contents: bytes,
        }])
    }
}

async fn fetch_marketplace_bytes(
    client: &reqwest::Client,
    url: &str,
    resource: &str,
) -> Result<Vec<u8>, RuntimeError> {
    let response =
        client.get(url).send().await.map_err(|error| {
            RuntimeError::Runtime(format!("{resource} download failed: {error}"))
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(RuntimeError::Runtime(format!(
            "{resource} download returned HTTP {status}"
        )));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| RuntimeError::Runtime(format!("could not read {resource}: {error}")))?;
    if bytes.len() > MAX_MARKETPLACE_SKILL_BYTES {
        return Err(RuntimeError::Runtime(
            "marketplace skill is too large to install safely".into(),
        ));
    }
    Ok(bytes.to_vec())
}

fn decode_skill_archive(bytes: Vec<u8>) -> Result<Vec<MarketplaceSkillFile>, RuntimeError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| RuntimeError::Runtime(format!("invalid skill archive: {error}")))?;
    if archive.len() > MAX_MARKETPLACE_SKILL_FILES {
        return Err(RuntimeError::Runtime(
            "marketplace skill archive contains too many files".into(),
        ));
    }
    let mut files = Vec::new();
    let mut total_bytes = 0_usize;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            RuntimeError::Runtime(format!("invalid skill archive entry: {error}"))
        })?;
        if entry.is_dir() {
            continue;
        }
        let path = entry.enclosed_name().ok_or_else(|| {
            RuntimeError::Runtime("skill archive contains an unsafe file path".into())
        })?;
        let path = safe_archive_relative_path(&path)?;
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents).map_err(|error| {
            RuntimeError::Runtime(format!("could not read skill archive entry: {error}"))
        })?;
        total_bytes = total_bytes.saturating_add(contents.len());
        if total_bytes > MAX_MARKETPLACE_SKILL_BYTES {
            return Err(RuntimeError::Runtime(
                "marketplace skill archive is too large to install safely".into(),
            ));
        }
        files.push(MarketplaceSkillFile { path, contents });
    }
    let manifest_path = files
        .iter()
        .find(|file| is_skill_manifest(&file.path))
        .map(|file| file.path.clone())
        .ok_or_else(|| RuntimeError::Runtime("skill archive does not contain SKILL.md".into()))?;
    if let Some(parent) = manifest_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        files.retain(|file| file.path.starts_with(parent));
        for file in &mut files {
            file.path = file
                .path
                .strip_prefix(parent)
                .map(Path::to_path_buf)
                .map_err(|_| RuntimeError::Runtime("invalid skill archive layout".into()))?;
        }
    }
    Ok(files)
}

fn safe_skill_relative_path(path: &str, root: &str) -> Result<PathBuf, RuntimeError> {
    let path = Path::new(path);
    let root = Path::new(root);
    let relative = path.strip_prefix(root).map_err(|_| {
        RuntimeError::Runtime("skill source returned a path outside its skill directory".into())
    })?;
    safe_relative_path(relative)
}

fn safe_archive_relative_path(path: &Path) -> Result<PathBuf, RuntimeError> {
    safe_relative_path(path)
}

fn safe_relative_path(path: &Path) -> Result<PathBuf, RuntimeError> {
    if path.as_os_str().is_empty()
        || path.to_string_lossy().contains('\\')
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(RuntimeError::Runtime(
            "skill source returned an unsafe file path".into(),
        ));
    }
    Ok(path.to_path_buf())
}

fn is_skill_manifest(path: &Path) -> bool {
    path.file_name()
        .map(|name| name.to_string_lossy().eq_ignore_ascii_case("SKILL.md"))
        .unwrap_or(false)
}

struct GithubSkillFileRef {
    path: PathBuf,
    download_url: String,
}

fn project_skill_roots(workspace_root: &Path) -> Vec<PathBuf> {
    vec![
        workspace_root.join(".averroes").join("skills"),
        workspace_root.join(".agents").join("skills"),
        workspace_root.join(".codex").join("skills"),
        workspace_root.join(".claude").join("skills"),
        workspace_root.join("skills"),
    ]
}

fn is_well_known_skill_domain(source: &str) -> bool {
    if source.is_empty()
        || source.len() > 253
        || source.parse::<std::net::IpAddr>().is_ok()
        || source.split('.').count() < 2
    {
        return false;
    }
    source.split('.').all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
    })
}

fn marketplace_http_client() -> Result<reqwest::Client, RuntimeError> {
    let redirect_filter = IpFilter::block_private();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if redirect_filter.is_hostname_allowed(attempt.url().host_str().unwrap_or_default()) {
                attempt.follow()
            } else {
                tracing::warn!(
                    url = %attempt.url(),
                    "Blocked marketplace redirect to a private or unresolved host"
                );
                attempt.stop()
            }
        }))
        .build()
        .map_err(|error| {
            RuntimeError::Runtime(format!("could not create marketplace client: {error}"))
        })
}

fn project_mcp_credential_ref(workspace_root: &Path, server_name: &str) -> CredentialRef {
    CredentialRef(format!(
        "mcp:{}:{}",
        workspace_root.to_string_lossy(),
        server_name.trim()
    ))
}

fn mcp_name_part(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if normalized.is_empty() {
        "tool".into()
    } else {
        normalized
    }
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

pub mod orchestration;
pub mod registry;

use crate::compaction::strategies::{HybridStrategy, SummaryStrategy, TrimStrategy};
use crate::compaction::{
    sanitize_tool_history, CompactionConfig, CompactionStrategy, CompactionStrategyType,
};
use crate::provider::types::{
    ContentPart, FunctionCall, MessageContent, Role, TokenUsage, ToolCall,
};
use crate::provider::{
    ChatMessage, ChatRequest, ChatResponse, ChatStream, Provider, ProviderError, StreamEvent,
    ToolDefinition,
};
use crate::runtime::{CallPermit, ResourceGovernor, TokenReservation};
use crate::tool::{ToolContext, ToolRegistry, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub model: String,
    pub system_prompt: Option<String>,
    pub tools: Vec<String>,
    pub max_iterations: usize,
    pub compaction: CompactionConfig,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "default".into(),
            model: "claude-sonnet-4-20250514".into(),
            system_prompt: None,
            tools: Vec::new(),
            max_iterations: 50,
            compaction: CompactionConfig::default(),
            temperature: None,
            max_tokens: None,
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
    TextDelta { text: String },
}

pub struct Agent {
    config: AgentConfig,
    runtime: Arc<std::sync::RwLock<AgentRuntime>>,
    tool_registry: Arc<ToolRegistry>,
    state: Arc<Mutex<AgentState>>,
    run_lock: Arc<tokio::sync::Mutex<()>>,
    messages: Arc<tokio::sync::Mutex<Vec<ChatMessage>>>,
    agent_id: String,
    session_id: String,
    working_dir: PathBuf,
}

#[derive(Clone)]
struct AgentRuntime {
    provider: Arc<dyn Provider>,
    model: String,
    governor: Arc<ResourceGovernor>,
}

fn estimate_request_tokens(request: &ChatRequest) -> u64 {
    let message_chars = request.messages.iter().fold(0u64, |total, message| {
        let content_chars = match &message.content {
            MessageContent::Text(text) => text.chars().count() as u64,
            MessageContent::Parts(parts) => parts.iter().fold(0u64, |total, part| {
                let chars = match part {
                    ContentPart::Text { text } => text.chars().count() as u64,
                    ContentPart::Image { source } => (source.media_type.chars().count() as u64)
                        .saturating_add(source.data.chars().count() as u64),
                    ContentPart::ToolUse { id, name, input } => (id.chars().count() as u64)
                        .saturating_add(name.chars().count() as u64)
                        .saturating_add(json_value_chars(input)),
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                    } => (tool_use_id.chars().count() as u64)
                        .saturating_add(content.chars().count() as u64),
                };
                total.saturating_add(chars)
            }),
        };
        let tool_call_chars = message
            .tool_calls
            .as_ref()
            .map(|tool_calls| {
                tool_calls.iter().fold(0u64, |total, tool_call| {
                    total
                        .saturating_add(tool_call.id.chars().count() as u64)
                        .saturating_add(tool_call.call_type.chars().count() as u64)
                        .saturating_add(tool_call.function.name.chars().count() as u64)
                        .saturating_add(tool_call.function.arguments.chars().count() as u64)
                })
            })
            .unwrap_or(0);
        total
            .saturating_add(content_chars)
            .saturating_add(tool_call_chars)
    });
    let tool_chars = request.tools.iter().fold(0u64, |total, tool| {
        total
            .saturating_add(tool.name.chars().count() as u64)
            .saturating_add(tool.description.chars().count() as u64)
            .saturating_add(json_value_chars(&tool.input_schema))
    });
    let system_chars = request
        .system
        .as_ref()
        .map(|system| system.chars().count() as u64)
        .unwrap_or(0);
    let request_chars = message_chars
        .saturating_add(tool_chars)
        .saturating_add(system_chars);

    (request_chars / 4).saturating_add(request.max_tokens.unwrap_or(4096) as u64)
}

fn json_value_chars(value: &serde_json::Value) -> u64 {
    serde_json::to_string(value)
        .map(|serialized| serialized.chars().count() as u64)
        .unwrap_or(0)
}

fn actual_usage_tokens(usage: &TokenUsage) -> u64 {
    // The governor counts every token category reported by a provider.
    usage
        .input_tokens
        .saturating_add(usage.output_tokens)
        .saturating_add(usage.cache_read_input_tokens.unwrap_or(0))
        .saturating_add(usage.cache_creation_input_tokens.unwrap_or(0))
}

fn message_text(message: &ChatMessage) -> String {
    match &message.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ToolResult { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

struct GovernedProvider {
    provider: Arc<dyn Provider>,
    governor: Arc<ResourceGovernor>,
}

struct GovernedChatStream {
    inner: Option<ChatStream>,
    permit: Option<CallPermit>,
    reservation: Option<TokenReservation>,
    finished: bool,
}

impl Unpin for GovernedChatStream {}

impl Stream for GovernedChatStream {
    type Item = crate::provider::Result<StreamEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        let poll = match this.inner.as_mut() {
            Some(inner) => Pin::new(inner).poll_next(cx),
            None => Poll::Ready(None),
        };

        match poll {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                this.finished = true;
                this.inner.take();
                if let Some(mut reservation) = this.reservation.take() {
                    reservation.disarm();
                }
                this.permit.take();
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(error))) => {
                this.finished = true;
                this.inner.take();
                this.reservation.take();
                this.permit.take();
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(Some(Ok(event))) => match &event {
                StreamEvent::MessageEnd { usage } => {
                    this.inner.take();
                    let reconciled = if let Some(mut reservation) = this.reservation.take() {
                        if let Some(usage) = usage {
                            reservation.reconcile(actual_usage_tokens(usage))
                        } else {
                            reservation.disarm();
                            true
                        }
                    } else {
                        true
                    };
                    this.finished = true;
                    this.permit.take();
                    if reconciled {
                        Poll::Ready(Some(Ok(event)))
                    } else {
                        Poll::Ready(Some(Err(ProviderError::Other(
                            "Token budget exhausted after governed stream response".into(),
                        ))))
                    }
                }
                StreamEvent::Error { .. } => {
                    this.finished = true;
                    this.inner.take();
                    this.reservation.take();
                    this.permit.take();
                    Poll::Ready(Some(Ok(event)))
                }
                _ => Poll::Ready(Some(Ok(event))),
            },
        }
    }
}

struct RunStateGuard {
    state: Arc<Mutex<AgentState>>,
    finished: bool,
}

impl RunStateGuard {
    fn new(state: Arc<Mutex<AgentState>>) -> Self {
        Self {
            state,
            finished: false,
        }
    }

    fn finish(&mut self) {
        self.finished = true;
    }
}

impl Drop for RunStateGuard {
    fn drop(&mut self) {
        if !self.finished {
            *self.state.lock().unwrap() = AgentState::Cancelled;
        }
    }
}

#[async_trait]
impl Provider for GovernedProvider {
    async fn chat(&self, request: ChatRequest) -> crate::provider::Result<ChatResponse> {
        let _permit = self.governor.acquire_call_permit().await;
        let reserved_tokens = estimate_request_tokens(&request);
        let Some(mut reservation) = self.governor.reserve_tokens(reserved_tokens) else {
            return Err(ProviderError::Other(
                "Token budget exhausted before compaction provider request".into(),
            ));
        };

        let response = self.provider.chat(request).await?;
        if let Some(usage) = response.usage.as_ref() {
            if !reservation.reconcile(actual_usage_tokens(usage)) {
                return Err(ProviderError::Other(
                    "Token budget exhausted after compaction provider response".into(),
                ));
            }
        } else {
            reservation.disarm();
        }

        Ok(response)
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> crate::provider::Result<crate::provider::ChatStream> {
        let permit = self.governor.acquire_call_permit().await;
        let reserved_tokens = estimate_request_tokens(&request);
        let Some(reservation) = self.governor.reserve_tokens(reserved_tokens) else {
            return Err(ProviderError::Other(
                "Token budget exhausted before compaction provider request".into(),
            ));
        };

        let stream = self.provider.chat_stream(request).await?;
        Ok(Box::new(GovernedChatStream {
            inner: Some(stream),
            permit: Some(permit),
            reservation: Some(reservation),
            finished: false,
        }))
    }

    fn context_window(&self, model: &str) -> usize {
        self.provider.context_window(model)
    }

    fn supports_tools(&self, model: &str) -> bool {
        self.provider.supports_tools(model)
    }

    fn default_model(&self) -> &str {
        self.provider.default_model()
    }
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

        Self {
            config,
            runtime: Arc::new(std::sync::RwLock::new(AgentRuntime {
                provider,
                model,
                governor,
            })),
            tool_registry,
            state: Arc::new(Mutex::new(AgentState::Idle)),
            run_lock: Arc::new(tokio::sync::Mutex::new(())),
            messages,
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

    fn runtime_snapshot(&self) -> AgentRuntime {
        self.runtime.read().unwrap().clone()
    }

    pub async fn run(&self, user_input: &str) -> Result<String> {
        self.run_inner(user_input, None).await
    }

    pub async fn run_streaming(
        &self,
        user_input: &str,
        events: tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>,
    ) -> Result<String> {
        self.run_inner(user_input, Some(events)).await
    }

    async fn run_inner(
        &self,
        user_input: &str,
        stream_events: Option<tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>>,
    ) -> Result<String> {
        let _run_lock = self.run_lock.lock().await;
        let mut run_state = RunStateGuard::new(self.state.clone());

        {
            let mut msgs = self.messages.lock().await;
            msgs.push(ChatMessage {
                role: Role::User,
                content: MessageContent::Text(user_input.to_string()),
                tool_call_id: None,
                tool_calls: None,
            });
        }

        for _iteration in 0..self.config.max_iterations {
            let runtime = self.runtime_snapshot();
            if self.should_compact_with_runtime(&runtime).await {
                if let Err(error) = self.compact_with_runtime(&runtime).await {
                    self.set_state(AgentState::Errored);
                    run_state.finish();
                    return Err(error);
                }
            }

            let runtime = self.runtime_snapshot();
            let messages = self.messages.lock().await.clone();
            let request = self.build_request(messages, runtime.model.clone());

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

            if response
                .message
                .tool_calls
                .as_ref()
                .map_or(false, |tc| !tc.is_empty())
            {
                self.set_state(AgentState::Acting);

                let tool_messages = match self.execute_tools(&response).await {
                    Ok(messages) => messages,
                    Err(error) => {
                        self.set_state(AgentState::Errored);
                        run_state.finish();
                        return Err(error);
                    }
                };
                {
                    let mut msgs = self.messages.lock().await;
                    msgs.push(response.message.clone());
                    msgs.extend(tool_messages);
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

        self.set_state(AgentState::Errored);
        run_state.finish();
        Err(anyhow::anyhow!("Max iterations reached"))
    }

    async fn chat_with_governor(
        &self,
        runtime: &AgentRuntime,
        request: ChatRequest,
    ) -> Result<ChatResponse> {
        let governor = runtime.governor.clone();
        let _permit = governor.acquire_call_permit().await;
        let reserved_tokens = estimate_request_tokens(&request);
        let Some(mut reservation) = governor.reserve_tokens(reserved_tokens) else {
            return Err(anyhow::anyhow!(
                "Token budget exhausted before provider request"
            ));
        };

        let response = runtime
            .provider
            .chat(request)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;

        if let Some(usage) = response.usage.as_ref() {
            if !reservation.reconcile(actual_usage_tokens(usage)) {
                return Err(anyhow::anyhow!(
                    "Token budget exhausted after provider response"
                ));
            }
        } else {
            reservation.disarm();
        }

        Ok(response)
    }

    async fn chat_stream_with_events(
        &self,
        runtime: &AgentRuntime,
        request: ChatRequest,
        events: &tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>,
    ) -> Result<ChatResponse> {
        let governed = GovernedProvider {
            provider: runtime.provider.clone(),
            governor: runtime.governor.clone(),
        };
        let mut stream = governed
            .chat_stream(request)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = None;

        while let Some(event) = stream.next().await {
            match event.map_err(|error| anyhow::anyhow!(error.to_string()))? {
                StreamEvent::TextDelta { text: delta } => {
                    text.push_str(&delta);
                    let _ = events.send(AgentStreamEvent::TextDelta { text: delta });
                }
                StreamEvent::ToolCallDelta {
                    id,
                    name,
                    arguments_delta,
                } => {
                    if id.is_empty() {
                        continue;
                    }
                    if let Some(tool_call) = tool_calls
                        .iter_mut()
                        .find(|call: &&mut ToolCall| call.id == id)
                    {
                        if !name.is_empty() {
                            tool_call.function.name = name;
                        }
                        tool_call.function.arguments.push_str(&arguments_delta);
                    } else {
                        tool_calls.push(ToolCall {
                            id,
                            call_type: "function".into(),
                            function: FunctionCall {
                                name,
                                arguments: arguments_delta,
                            },
                        });
                    }
                }
                StreamEvent::MessageEnd {
                    usage: stream_usage,
                } => {
                    usage = stream_usage;
                    break;
                }
                StreamEvent::Error { message } => {
                    return Err(anyhow::anyhow!(message));
                }
                StreamEvent::ToolCallEnd { .. } | StreamEvent::MessageStart { .. } => {}
            }
        }

        Ok(ChatResponse {
            message: ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text(text),
                tool_call_id: None,
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            },
            usage,
            stop_reason: None,
        })
    }

    async fn execute_tools(&self, response: &ChatResponse) -> Result<Vec<ChatMessage>> {
        let tool_calls = match &response.message.tool_calls {
            Some(tc) => tc,
            None => return Ok(Vec::new()),
        };

        let ctx = ToolContext {
            working_dir: self.working_dir.clone(),
            session_id: self.session_id.clone(),
            agent_id: self.agent_id.clone(),
        };

        let mut messages = Vec::new();

        for tc in tool_calls {
            let params: serde_json::Value = match serde_json::from_str(&tc.function.arguments) {
                Ok(v) => v,
                Err(e) => {
                    messages.push(ChatMessage {
                        role: Role::Tool,
                        content: MessageContent::Text(format!("invalid arguments: {}", e)),
                        tool_call_id: Some(tc.id.clone()),
                        tool_calls: None,
                    });
                    continue;
                }
            };

            let result = match self
                .tool_registry
                .execute(&tc.function.name, &ctx, &params)
                .await
            {
                Ok(r) => r,
                Err(e) => ToolResult::error(e.to_string()),
            };

            let content = if result.success {
                result.content
            } else {
                result
                    .error
                    .unwrap_or_else(|| String::from("unknown error"))
            };

            messages.push(ChatMessage {
                role: Role::Tool,
                content: MessageContent::Text(content),
                tool_call_id: Some(tc.id.clone()),
                tool_calls: None,
            });
        }

        Ok(messages)
    }

    async fn should_compact_with_runtime(&self, runtime: &AgentRuntime) -> bool {
        let context_limit = runtime.provider.context_window(&runtime.model);
        let msgs = self.messages.lock().await.clone();
        let message_count = msgs.len();
        let request = self.build_request(msgs, runtime.model.clone());
        let minimum_messages = match self.config.compaction.strategy {
            CompactionStrategyType::Trim | CompactionStrategyType::Hybrid => {
                self.config.compaction.keep_last + 2
            }
            CompactionStrategyType::Summary => 2,
        };
        estimate_request_tokens(&request)
            > (self.config.compaction.threshold * context_limit as f64) as u64
            && message_count > minimum_messages
    }

    async fn compact_with_runtime(&self, runtime: &AgentRuntime) -> Result<()> {
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

        let mut compacted = {
            let msgs = self.messages.lock().await.clone();
            strategy
                .compact(
                    &msgs,
                    context_limit,
                    &self.config.compaction,
                    provider_ref,
                    runtime.model.as_str(),
                )
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
        };
        compacted.messages = sanitize_tool_history(compacted.messages);
        compacted.compacted_count = compacted.messages.len();

        let compacted_request =
            self.build_request(compacted.messages.clone(), runtime.model.clone());
        let compacted_tokens = estimate_request_tokens(&compacted_request);
        if compacted_tokens > context_limit as u64 {
            return Err(anyhow::anyhow!(
                "Compacted context still exceeds context window: {}/{} tokens",
                compacted_tokens,
                context_limit
            ));
        }

        {
            let mut msgs = self.messages.lock().await;
            *msgs = compacted.messages;
        }
        Ok(())
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
        self.config
            .tools
            .iter()
            .filter_map(|name| {
                self.tool_registry.get(name).map(|tool| ToolDefinition {
                    name: tool.name().to_string(),
                    description: tool.description().to_string(),
                    input_schema: tool.parameters(),
                })
            })
            .collect()
    }

    fn build_request(&self, messages: Vec<ChatMessage>, model: String) -> ChatRequest {
        ChatRequest {
            model,
            messages,
            tools: self.build_tool_definitions(),
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            system: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{FunctionCall, Role as ProviderRole, TokenUsage, ToolCall};
    use async_trait::async_trait;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct TestProvider {
        responses: Vec<ChatResponse>,
        call_count: std::sync::Mutex<usize>,
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

    struct StreamProvider;

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
            }
        }
    }

    #[async_trait]
    impl Provider for TestProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
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
            Ok(Box::new(futures::stream::iter(vec![
                Ok(StreamEvent::TextDelta {
                    text: "part".into(),
                }),
                Ok(StreamEvent::MessageEnd {
                    usage: Some(TokenUsage {
                        input_tokens: 2,
                        output_tokens: 3,
                        cache_read_input_tokens: None,
                        cache_creation_input_tokens: None,
                    }),
                }),
            ])))
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
            max_tokens: Some(4096),
            ..Default::default()
        };
        assert_eq!(config.name, "custom");
        assert_eq!(config.temperature, Some(0.5));
        assert_eq!(config.max_tokens, Some(4096));
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
            }),
            stop_reason: None,
        }]));
        let agent = Agent::new(
            AgentConfig {
                max_tokens: Some(1),
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
        assert_eq!(new_governor.tokens_available(), 98);
    }

    #[tokio::test]
    async fn test_agent_rejects_request_before_provider_when_reservation_exceeds_budget() {
        let provider = Arc::new(TestProvider::new(vec![]));
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                max_tokens: Some(1),
                ..Default::default()
            },
            provider.clone(),
            test_tool_registry(),
            Arc::new(ResourceGovernor::new(1, 1)),
            "reservation-session".into(),
            PathBuf::from("/tmp"),
        );

        let error = agent.run("request").await.unwrap_err();

        assert!(error
            .to_string()
            .contains("Token budget exhausted before provider request"));
        assert_eq!(*provider.call_count.lock().unwrap(), 0);
        assert_eq!(agent.state().await, AgentState::Errored);
    }

    #[tokio::test]
    async fn test_agent_rejects_response_when_token_budget_is_exhausted() {
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
            }),
            stop_reason: None,
        };
        let provider = Arc::new(TestProvider::new(vec![response.clone(), response]));
        let governor = Arc::new(ResourceGovernor::new(1, 5));
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                max_tokens: Some(4),
                ..Default::default()
            },
            provider,
            test_tool_registry(),
            governor.clone(),
            "token-budget-session".into(),
            PathBuf::from("/tmp"),
        );

        assert_eq!(agent.run("first").await.unwrap(), "budgeted response");
        assert!(agent.run("second").await.is_err());
        assert_eq!(agent.state().await, AgentState::Errored);
        assert_eq!(governor.tokens_available(), 0);
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
                max_tokens: Some(1),
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
        assert!(governor.tokens_available() < 10_000);

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
                max_tokens: Some(1),
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
    async fn cache_tokens_count_toward_budget_usage() {
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
            }),
            stop_reason: None,
        }]));
        let governor = Arc::new(ResourceGovernor::new(1, 100));
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                max_tokens: Some(1),
                ..Default::default()
            },
            provider,
            test_tool_registry(),
            governor.clone(),
            "cache-session".into(),
            PathBuf::from("/tmp"),
        );

        agent.run("x").await.unwrap();

        assert_eq!(governor.tokens_available(), 88);
    }

    fn stream_request() -> ChatRequest {
        ChatRequest {
            model: "test-model".into(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: Some(1),
            temperature: None,
            system: None,
        }
    }

    #[tokio::test]
    async fn governed_stream_holds_permit_and_reconciles_at_message_end() {
        let provider = Arc::new(StreamProvider);
        let governor = Arc::new(ResourceGovernor::new(1, 100));
        let governed = GovernedProvider {
            provider,
            governor: governor.clone(),
        };

        let mut stream = governed.chat_stream(stream_request()).await.unwrap();

        assert_eq!(governor.active_calls(), 1);
        assert!(governor.tokens_available() < 100);
        assert!(stream.next().await.is_some());
        assert_eq!(governor.active_calls(), 1);

        assert!(matches!(
            stream.next().await,
            Some(Ok(StreamEvent::MessageEnd { .. }))
        ));
        assert_eq!(governor.active_calls(), 0);
        assert_eq!(governor.tokens_available(), 95);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn run_streaming_emits_text_deltas_and_returns_complete_response() {
        let agent = Agent::new(
            AgentConfig {
                tools: Vec::new(),
                ..Default::default()
            },
            Arc::new(StreamProvider),
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
        assert!(matches!(
            events.as_slice(),
            [AgentStreamEvent::TextDelta { text }] if text == "part"
        ));
    }

    #[tokio::test]
    async fn governed_stream_commits_estimate_when_usage_is_missing() {
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
        assert_eq!(governor.tokens_available(), 99);
        assert_eq!(governor.active_calls(), 0);
    }

    #[tokio::test]
    async fn governed_stream_commits_estimate_when_stream_ends_without_message_end() {
        let provider = Arc::new(TruncatedStreamProvider);
        let governor = Arc::new(ResourceGovernor::new(1, 100));
        let governed = GovernedProvider {
            provider,
            governor: governor.clone(),
        };

        let mut stream = governed.chat_stream(stream_request()).await.unwrap();

        assert!(stream.next().await.is_some());
        assert!(stream.next().await.is_none());
        assert_eq!(governor.tokens_available(), 99);
        assert_eq!(governor.active_calls(), 0);
    }

    #[tokio::test]
    async fn governed_stream_releases_on_drop_and_provider_error() {
        let governor = Arc::new(ResourceGovernor::new(1, 100));
        let provider = Arc::new(StreamProvider);
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
                max_tokens: Some(1),
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
        assert!(agent.compact_with_runtime(&runtime).await.is_err());
        assert_eq!(*provider.call_count.lock().unwrap(), 0);
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

        assert!(governor.tokens_available() < available_before);
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
                max_tokens: None,
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

        let runtime = agent.runtime_snapshot();
        assert!(agent.should_compact_with_runtime(&runtime).await);
    }

    #[tokio::test]
    async fn compaction_rejects_result_that_still_exceeds_context_window() {
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                max_tokens: None,
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
        let error = agent.compact_with_runtime(&runtime).await.unwrap_err();

        assert!(error.to_string().contains("context window"));
        assert_eq!(*agent.messages.lock().await, original);
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
            stop_reason: None,
        }]));
        let agent = Arc::new(Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: vec!["block".into()],
                max_iterations: 1,
                max_tokens: Some(1),
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
    async fn test_agent_run_max_iterations() {
        let provider = Arc::new(TestProvider::new({
            let mut responses = Vec::new();
            for _ in 0..3 {
                responses.push(ChatResponse {
                    message: ChatMessage {
                        role: ProviderRole::Assistant,
                        content: MessageContent::Text(String::new()),
                        tool_call_id: None,
                        tool_calls: Some(vec![ToolCall {
                            id: "tc_loop".into(),
                            call_type: "function".into(),
                            function: FunctionCall {
                                name: "echo".into(),
                                arguments: r#"{"text": "loop"}"#.into(),
                            },
                        }]),
                    },
                    usage: None,
                    stop_reason: None,
                });
            }
            responses
        }));

        let agent = Agent::new(
            AgentConfig {
                max_iterations: 2,
                ..test_agent_config()
            },
            provider,
            test_tool_registry(),
            test_governor(),
            "session-3".into(),
            PathBuf::from("/tmp"),
        );

        let result = agent.run("loop forever").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Max iterations"));
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
    fn request_estimate_includes_tools_and_rich_content() {
        let basic = ChatRequest {
            model: "test-model".into(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: Some(1),
            temperature: None,
            system: None,
        };
        let rich = ChatRequest {
            model: "test-model".into(),
            messages: vec![ChatMessage {
                role: ProviderRole::User,
                content: MessageContent::Parts(vec![
                    ContentPart::Image {
                        source: crate::provider::types::ImageSource {
                            media_type: "image/png".into(),
                            data: "base64-image-data".into(),
                        },
                    },
                    ContentPart::ToolUse {
                        id: "tool-use-1".into(),
                        name: "search".into(),
                        input: serde_json::json!({"query": "important"}),
                    },
                ]),
                tool_call_id: None,
                tool_calls: None,
            }],
            tools: vec![ToolDefinition {
                name: "search".into(),
                description: "Searches a large indexed corpus".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}}
                }),
            }],
            max_tokens: Some(1),
            temperature: None,
            system: None,
        };

        assert!(estimate_request_tokens(&rich) > estimate_request_tokens(&basic));
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
}

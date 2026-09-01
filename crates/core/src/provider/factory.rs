use super::{
    anthropic::AnthropicProvider, generic::GenericProvider, openai::OpenAiProvider, Provider,
};
use crate::connection::{
    ConnectionKind, ConnectionProfile, OLLAMA_CLOUD_BASE_URL, QDIVZERO_BASE_URL,
};
use crate::github::{
    CopilotEndpoint, COPILOT_API_VERSION, COPILOT_EDITOR_PLUGIN_VERSION, COPILOT_EDITOR_VERSION,
    COPILOT_INTEGRATION_ID, COPILOT_USER_AGENT,
};
use std::sync::Arc;

const COPILOT_API_BASE: &str = "https://api.githubcopilot.com";

pub fn create_direct_provider(
    profile: &ConnectionProfile,
    api_key: &str,
    model: &str,
) -> Result<Arc<dyn Provider>, ProviderFactoryError> {
    if profile.kind.requires_api_key() && api_key.trim().is_empty() {
        return Err(ProviderFactoryError::EmptyCredential);
    }
    if model.trim().is_empty() {
        return Err(ProviderFactoryError::EmptyModel);
    }
    profile
        .validate()
        .map_err(|error| ProviderFactoryError::InvalidProfile(error.to_string()))?;

    match profile.kind {
        ConnectionKind::Copilot => Ok(build_copilot_provider(
            profile,
            api_key,
            model,
            if super::model_uses_responses_api(model) {
                CopilotEndpoint::Responses
            } else {
                CopilotEndpoint::ChatCompletions
            },
        )),
        ConnectionKind::QDivZero => {
            let base_url = profile.base_url.as_deref().unwrap_or(QDIVZERO_BASE_URL);
            Ok(Arc::new(
                GenericProvider::qdivzero(api_key.to_owned(), base_url).with_default_model(model),
            ))
        }
        ConnectionKind::OpenAi => {
            let mut provider = OpenAiProvider::new(api_key.to_owned()).with_default_model(model);
            if let Some(base_url) = profile.base_url.as_deref() {
                provider = provider.with_base_url(base_url);
            }
            Ok(Arc::new(provider))
        }
        ConnectionKind::Anthropic => {
            let mut provider = AnthropicProvider::new(api_key.to_owned()).with_default_model(model);
            if let Some(base_url) = profile.base_url.as_deref() {
                provider = provider.with_base_url(base_url);
            }
            Ok(Arc::new(provider))
        }
        ConnectionKind::DeepSeek => {
            let base_url = profile
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.deepseek.com".into());
            Ok(Arc::new(
                GenericProvider::new(api_key.to_owned(), base_url).with_default_model(model),
            ))
        }
        ConnectionKind::Groq => {
            let base_url = profile
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.groq.com/openai/v1".into());
            Ok(Arc::new(
                GenericProvider::new(api_key.to_owned(), base_url).with_default_model(model),
            ))
        }
        ConnectionKind::Ollama => {
            let base_url = profile
                .base_url
                .as_deref()
                .unwrap_or("http://localhost:11434");
            Ok(Arc::new(
                GenericProvider::ollama(base_url).with_default_model(model),
            ))
        }
        ConnectionKind::OllamaCloud => {
            let base_url = profile.base_url.as_deref().unwrap_or(OLLAMA_CLOUD_BASE_URL);
            Ok(Arc::new(
                GenericProvider::new(api_key.to_owned(), base_url.to_owned())
                    .with_default_model(model),
            ))
        }
        ConnectionKind::Compatible => {
            let base_url = profile
                .base_url
                .clone()
                .ok_or(ProviderFactoryError::MissingBaseUrl)?;
            Ok(Arc::new(
                GenericProvider::new(api_key.to_owned(), base_url).with_default_model(model),
            ))
        }
        ConnectionKind::Codex => Err(ProviderFactoryError::CodexUsesChatGptAuth),
    }
}

/// Constructs a Copilot provider using the endpoint advertised for the chosen
/// model by GitHub's authenticated catalog. This avoids inferring the route
/// from a marketing model name.
pub fn create_copilot_provider(
    profile: &ConnectionProfile,
    api_key: &str,
    model: &str,
    endpoint: CopilotEndpoint,
) -> Result<Arc<dyn Provider>, ProviderFactoryError> {
    if profile.kind != ConnectionKind::Copilot {
        return Err(ProviderFactoryError::NotCopilotConnection);
    }
    if api_key.trim().is_empty() {
        return Err(ProviderFactoryError::EmptyCredential);
    }
    if model.trim().is_empty() {
        return Err(ProviderFactoryError::EmptyModel);
    }
    profile
        .validate()
        .map_err(|error| ProviderFactoryError::InvalidProfile(error.to_string()))?;
    Ok(build_copilot_provider(profile, api_key, model, endpoint))
}

fn build_copilot_provider(
    profile: &ConnectionProfile,
    api_key: &str,
    model: &str,
    endpoint: CopilotEndpoint,
) -> Arc<dyn Provider> {
    let base_url = profile.base_url.as_deref().unwrap_or(COPILOT_API_BASE);

    match endpoint {
        CopilotEndpoint::Messages => Arc::new(
            AnthropicProvider::new(api_key.to_owned())
                .with_base_url(&format!("{base_url}/v1"))
                .with_default_model(model)
                .with_bearer_auth()
                .with_header("User-Agent", COPILOT_USER_AGENT)
                .with_header("Editor-Version", COPILOT_EDITOR_VERSION)
                .with_header("Editor-Plugin-Version", COPILOT_EDITOR_PLUGIN_VERSION)
                .with_header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
                .with_header("X-GitHub-Api-Version", COPILOT_API_VERSION)
                .with_header("Openai-Intent", "conversation-edits")
                .with_header("x-initiator", "agent")
                // Required by the Copilot Anthropic shim when a model exposes
                // interleaved thinking.
                .with_header("anthropic-beta", "interleaved-thinking-2025-05-14"),
        ),
        CopilotEndpoint::ChatCompletions | CopilotEndpoint::Responses => Arc::new(
            OpenAiProvider::new(api_key.to_owned())
                .with_base_url(base_url)
                .with_default_model(model)
                .with_responses_api(endpoint == CopilotEndpoint::Responses)
                .with_header("User-Agent", COPILOT_USER_AGENT)
                .with_header("Editor-Version", COPILOT_EDITOR_VERSION)
                .with_header("Editor-Plugin-Version", COPILOT_EDITOR_PLUGIN_VERSION)
                .with_header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
                .with_header("X-GitHub-Api-Version", COPILOT_API_VERSION)
                .with_header("Openai-Intent", "conversation-edits")
                .with_header("x-initiator", "agent"),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderFactoryError {
    #[error("an API key must be provided directly")]
    EmptyCredential,
    #[error("a model must be selected explicitly")]
    EmptyModel,
    #[error("compatible connections require a base URL")]
    MissingBaseUrl,
    #[error("Codex connections use the integrated ChatGPT sign-in")]
    CodexUsesChatGptAuth,
    #[error("this operation requires a GitHub Copilot connection")]
    NotCopilotConnection,
    #[error("invalid connection profile: {0}")]
    InvalidProfile(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_environment_key_cannot_satisfy_an_empty_explicit_credential() {
        // No environment lookup occurs in create_direct_provider: an empty explicit
        // credential is always rejected, regardless of process configuration.
        let profile = ConnectionProfile::api("openai", "OpenAI", ConnectionKind::OpenAi, None);
        let result = create_direct_provider(&profile, "", "gpt-test");
        assert!(matches!(result, Err(ProviderFactoryError::EmptyCredential)));
    }

    #[test]
    fn codex_is_never_treated_as_an_api_key_provider() {
        let profile = ConnectionProfile::codex("codex", "ChatGPT");
        let result = create_direct_provider(&profile, "not-used", "gpt-test");
        assert!(matches!(
            result,
            Err(ProviderFactoryError::CodexUsesChatGptAuth)
        ));
    }

    #[test]
    fn ollama_can_be_used_without_a_secret() {
        let profile = ConnectionProfile::ollama("local", "Local Ollama", None);

        assert!(create_direct_provider(&profile, "", "qwen3:8b").is_ok());
    }

    #[test]
    fn deepseek_uses_the_direct_api_catalog_endpoint() {
        let profile = ConnectionProfile::deepseek("deepseek", "DeepSeek");

        assert!(create_direct_provider(&profile, "key", "deepseek-v4-pro").is_ok());
    }

    #[test]
    fn groq_uses_the_direct_openai_compatible_transport() {
        let profile = ConnectionProfile::groq("groq", "Groq");

        assert!(create_direct_provider(&profile, "gsk_test", "llama-3.3-70b-versatile").is_ok());
    }

    #[test]
    fn qdivzero_uses_the_authenticated_openai_compatible_transport() {
        let profile = ConnectionProfile::qdivzero("qdivzero", "QDivZero");

        assert!(
            create_direct_provider(&profile, "qdiv_test", "nvidia/Qwen3.6-35B-A3B-NVFP4").is_ok()
        );
    }

    #[test]
    fn copilot_uses_its_direct_api_with_explicit_token_authentication() {
        let profile = ConnectionProfile::copilot("copilot", "GitHub Copilot");

        assert!(create_direct_provider(&profile, "github_pat_token", "gpt-5.4").is_ok());
    }
}

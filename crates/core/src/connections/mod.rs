//! Connection identities, profiles and per-session bindings.

mod binding;
mod ids;
mod profile;

pub use binding::SessionBinding;
pub use ids::{ConnectionId, CredentialRef};
pub use profile::{
    ConnectionKind, ConnectionProfile, ConnectionValidationError, OLLAMA_CLOUD_BASE_URL,
    QDIVZERO_BASE_URL,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolApprovalPolicy;

    #[test]
    fn a_new_session_has_no_implicit_connection_or_model() {
        assert_eq!(
            SessionBinding::default(),
            SessionBinding {
                connection_id: None,
                model_id: None,
                reasoning_effort: None,
                tools: Vec::new(),
                approval_policy: ToolApprovalPolicy::default(),
            }
        );
        assert!(!SessionBinding::default().is_ready());
    }

    #[test]
    fn legacy_session_bindings_default_to_no_explicit_tools() {
        let binding: SessionBinding =
            serde_json::from_str(r#"{"connection_id":"work","model_id":"model-1"}"#).unwrap();

        assert_eq!(binding.connection_id, Some(ConnectionId("work".into())));
        assert_eq!(binding.model_id.as_deref(), Some("model-1"));
        assert!(binding.tools.is_empty());
    }

    #[test]
    fn duplicate_provider_kinds_are_valid_when_ids_differ() {
        let first = ConnectionProfile::api("personal", "Personal", ConnectionKind::OpenAi, None);
        let second = ConnectionProfile::api("work", "Work", ConnectionKind::OpenAi, None);
        assert_ne!(first.id, second.id);
        assert_eq!(first.kind, second.kind);
        assert!(first.validate().is_ok());
        assert!(second.validate().is_ok());
    }

    #[test]
    fn blank_profile_names_are_rejected() {
        let profile = ConnectionProfile::api("id", "   ", ConnectionKind::Anthropic, None);
        assert_eq!(
            profile.validate(),
            Err(ConnectionValidationError::BlankName)
        );
    }

    #[test]
    fn compatible_endpoints_are_limited_to_http() {
        let profile = ConnectionProfile::api(
            "local",
            "Local",
            ConnectionKind::Compatible,
            Some("file:///tmp/provider".into()),
        );
        assert_eq!(
            profile.validate(),
            Err(ConnectionValidationError::InvalidBaseUrl)
        );
    }

    #[test]
    fn codex_has_no_averroes_credential_reference() {
        let profile = ConnectionProfile::codex("codex", "ChatGPT");
        assert_eq!(profile.credential_ref, None);
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn ollama_defaults_to_a_local_server_without_a_credential() {
        let profile = ConnectionProfile::ollama("ollama", "Ollama", None);

        assert_eq!(profile.credential_ref, None);
        assert_eq!(profile.base_url.as_deref(), Some("http://localhost:11434"));
        assert!(profile.validate().is_ok());
        assert!(!ConnectionKind::Ollama.requires_api_key());
    }

    #[test]
    fn deepseek_uses_a_fixed_direct_api_endpoint() {
        let profile = ConnectionProfile::deepseek("deepseek", "DeepSeek");

        assert_eq!(
            profile.base_url.as_deref(),
            Some("https://api.deepseek.com")
        );
        assert!(profile.credential_ref.is_some());
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn groq_uses_the_openai_compatible_api_endpoint() {
        let profile = ConnectionProfile::groq("groq", "Groq");

        assert_eq!(
            profile.base_url.as_deref(),
            Some("https://api.groq.com/openai/v1")
        );
        assert!(profile.credential_ref.is_some());
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn qdivzero_uses_its_fixed_api_endpoint_and_supports_embeddings() {
        let profile = ConnectionProfile::qdivzero("qdivzero", "QDivZero");

        assert_eq!(profile.base_url.as_deref(), Some(QDIVZERO_BASE_URL));
        assert!(profile.credential_ref.is_some());
        assert!(profile.validate().is_ok());
        assert!(ConnectionKind::QDivZero.requires_api_key());
        assert!(ConnectionKind::QDivZero.supports_embeddings());
    }

    #[test]
    fn qdivzero_accepts_legacy_config_names() {
        let profile: ConnectionProfile =
            serde_json::from_str(r#"{"id":"qdivzero","name":"QDivZero","kind":"q_div_zero"}"#)
                .unwrap();

        assert_eq!(profile.kind, ConnectionKind::QDivZero);
    }

    #[test]
    fn ollama_cloud_defaults_to_the_remote_openai_compatible_endpoint() {
        let profile = ConnectionProfile::ollama_cloud("ollama-cloud", "Ollama Cloud", None);

        assert_eq!(profile.base_url.as_deref(), Some(OLLAMA_CLOUD_BASE_URL));
        assert!(profile.credential_ref.is_some());
        assert!(profile.validate().is_ok());
        assert!(ConnectionKind::OllamaCloud.requires_api_key());
        assert!(ConnectionKind::OllamaCloud.supports_embeddings());
    }

    #[test]
    fn ollama_cloud_normalizes_a_host_without_the_v1_suffix() {
        let profile = ConnectionProfile::ollama_cloud(
            "ollama-cloud",
            "Ollama Cloud",
            Some("https://ollama.com/".into()),
        );

        assert_eq!(profile.base_url.as_deref(), Some("https://ollama.com/v1"));
    }

    #[test]
    fn blank_manual_model_ids_are_rejected_before_persistence() {
        let mut profile = ConnectionProfile::ollama("ollama", "Ollama", None);
        profile
            .manual_models
            .push(crate::models::ManualModel::new("  "));

        assert_eq!(
            profile.validate(),
            Err(ConnectionValidationError::BlankManualModelId)
        );
    }
}

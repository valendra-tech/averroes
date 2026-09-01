use super::{ConnectionId, CredentialRef};
use crate::models::ManualModel;
use serde::{Deserialize, Serialize};

/// Default OpenAI-compatible endpoint for direct Ollama Cloud access.
///
/// Ollama local remains a native API integration; Cloud is deliberately a
/// separate connection kind because it requires an API key and exposes its
/// remote catalog through the OpenAI-compatible v1 surface.
pub const OLLAMA_CLOUD_BASE_URL: &str = "https://ollama.com/v1";
/// Root URL for QDivZero's OpenAI-compatible inference API.
pub const QDIVZERO_BASE_URL: &str = "https://api.qdiv0.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    Codex,
    Copilot,
    #[serde(alias = "q_div_zero", alias = "qdivzero")]
    QDivZero,
    OpenAi,
    Anthropic,
    DeepSeek,
    Groq,
    Ollama,
    OllamaCloud,
    Compatible,
}

impl ConnectionKind {
    pub const fn requires_api_key(self) -> bool {
        !matches!(self, Self::Codex | Self::Ollama)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Copilot => "GitHub Copilot",
            Self::QDivZero => "QDivZero",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::DeepSeek => "DeepSeek",
            Self::Groq => "Groq",
            Self::Ollama => "Ollama",
            Self::OllamaCloud => "Ollama Cloud",
            Self::Compatible => "Compatible API",
        }
    }

    pub const fn supports_embeddings(self) -> bool {
        matches!(
            self,
            Self::Codex
                | Self::Copilot
                | Self::QDivZero
                | Self::OpenAi
                | Self::DeepSeek
                | Self::Groq
                | Self::Ollama
                | Self::OllamaCloud
                | Self::Compatible
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: ConnectionId,
    pub name: String,
    pub kind: ConnectionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<CredentialRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified_unix_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manual_models: Vec<ManualModel>,
}

impl ConnectionProfile {
    pub fn api(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: ConnectionKind,
        base_url: Option<String>,
    ) -> Self {
        let id = ConnectionId(id.into());
        Self {
            credential_ref: Some(CredentialRef(format!("credential:{}", id.0))),
            id,
            name: name.into(),
            kind,
            base_url,
            organization: None,
            project: None,
            last_verified_unix_seconds: None,
            manual_models: Vec::new(),
        }
    }

    pub fn codex(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: ConnectionId(id.into()),
            name: name.into(),
            kind: ConnectionKind::Codex,
            base_url: None,
            organization: None,
            project: None,
            credential_ref: None,
            last_verified_unix_seconds: None,
            manual_models: Vec::new(),
        }
    }

    pub fn copilot(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::api(
            id,
            name,
            ConnectionKind::Copilot,
            Some("https://api.githubcopilot.com".into()),
        )
    }

    pub fn qdivzero(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::api(
            id,
            name,
            ConnectionKind::QDivZero,
            Some(QDIVZERO_BASE_URL.into()),
        )
    }

    pub fn deepseek(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::api(
            id,
            name,
            ConnectionKind::DeepSeek,
            Some("https://api.deepseek.com".into()),
        )
    }

    pub fn groq(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::api(
            id,
            name,
            ConnectionKind::Groq,
            Some("https://api.groq.com/openai/v1".into()),
        )
    }

    pub fn ollama(
        id: impl Into<String>,
        name: impl Into<String>,
        base_url: Option<String>,
    ) -> Self {
        Self {
            id: ConnectionId(id.into()),
            name: name.into(),
            kind: ConnectionKind::Ollama,
            base_url: Some(
                base_url
                    .filter(|url| !url.trim().is_empty())
                    .unwrap_or_else(|| "http://localhost:11434".into()),
            ),
            organization: None,
            project: None,
            credential_ref: None,
            last_verified_unix_seconds: None,
            manual_models: Vec::new(),
        }
    }

    pub fn ollama_cloud(
        id: impl Into<String>,
        name: impl Into<String>,
        base_url: Option<String>,
    ) -> Self {
        Self::api(
            id,
            name,
            ConnectionKind::OllamaCloud,
            Some(normalize_ollama_cloud_base_url(base_url)),
        )
    }

    pub fn validate(&self) -> Result<(), ConnectionValidationError> {
        if self.name.trim().is_empty() {
            return Err(ConnectionValidationError::BlankName);
        }
        if self.id.0.trim().is_empty() {
            return Err(ConnectionValidationError::BlankId);
        }
        if matches!(
            self.kind,
            ConnectionKind::Compatible | ConnectionKind::Ollama
        ) {
            let Some(base_url) = self
                .base_url
                .as_deref()
                .filter(|url| !url.trim().is_empty())
            else {
                return Err(ConnectionValidationError::MissingBaseUrl);
            };
            let parsed =
                url::Url::parse(base_url).map_err(|_| ConnectionValidationError::InvalidBaseUrl)?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(ConnectionValidationError::InvalidBaseUrl);
            }
        }
        if self.kind == ConnectionKind::OllamaCloud {
            if let Some(base_url) = self
                .base_url
                .as_deref()
                .filter(|url| !url.trim().is_empty())
            {
                let parsed = url::Url::parse(base_url)
                    .map_err(|_| ConnectionValidationError::InvalidBaseUrl)?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    return Err(ConnectionValidationError::InvalidBaseUrl);
                }
            }
        }
        if self.kind.requires_api_key() && self.credential_ref.is_none() {
            return Err(ConnectionValidationError::MissingCredentialRef);
        }
        if self.kind == ConnectionKind::Codex && self.credential_ref.is_some() {
            return Err(ConnectionValidationError::CodexCannotStoreCredential);
        }
        if self.kind == ConnectionKind::Ollama && self.credential_ref.is_some() {
            return Err(ConnectionValidationError::OllamaCannotStoreCredential);
        }
        if self
            .manual_models
            .iter()
            .any(|model| model.id.trim().is_empty())
        {
            return Err(ConnectionValidationError::BlankManualModelId);
        }
        Ok(())
    }
}

fn normalize_ollama_cloud_base_url(base_url: Option<String>) -> String {
    let base_url = base_url
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| OLLAMA_CLOUD_BASE_URL.into());
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url
        .rsplit('/')
        .next()
        .is_some_and(|segment| segment.eq_ignore_ascii_case("v1"))
    {
        base_url.to_owned()
    } else {
        format!("{base_url}/v1")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionValidationError {
    #[error("connection identifier cannot be blank")]
    BlankId,
    #[error("connection name cannot be blank")]
    BlankName,
    #[error("compatible connections require a base URL")]
    MissingBaseUrl,
    #[error("connection URL must use HTTP or HTTPS")]
    InvalidBaseUrl,
    #[error("API connections require a credential reference")]
    MissingCredentialRef,
    #[error("Codex uses its embedded OAuth vault and cannot use an API-key credential")]
    CodexCannotStoreCredential,
    #[error("Ollama connections do not store an API key")]
    OllamaCannotStoreCredential,
    #[error("manual model identifiers cannot be blank")]
    BlankManualModelId,
}

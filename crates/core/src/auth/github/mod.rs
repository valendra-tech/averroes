use crate::connection::CredentialRef;
use crate::credentials::{CredentialVault, VaultError};
use crate::diagnostics::{self, DiagnosticLevel};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{oneshot, Mutex};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

mod catalog;
mod oauth;

const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_GRAPHQL_URL: &str = "https://api.github.com/graphql";
const GITHUB_COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const GITHUB_COPILOT_API_BASE: &str = "https://api.githubcopilot.com";
// Public device-flow client used by GitHub Copilot itself. Copilot's gateway
// allowlists models by OAuth client ID, so a generic/custom GitHub App token
// only receives the legacy catalog even when the user has a current plan.
// This is a public identifier, never a client secret, and Averroes still
// performs the complete flow directly without invoking a CLI.
const GITHUB_APP_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const USER_AGENT: &str = "averroes";
const SESSION_TYPE: &str = "github_app_device_v1";
const EXPIRY_LEEWAY_SECONDS: u64 = 60;
const COPILOT_SESSION_LEEWAY_SECONDS: u64 = 60;
const COPILOT_SESSION_FALLBACK_TTL_SECONDS: u64 = 15 * 60;
const COPILOT_TOKEN_EXCHANGE_API_VERSION: &str = "2025-04-01";

/// The current GitHub Copilot API revision used for both catalog discovery
/// and inference. Keeping it in one place prevents a model catalog from
/// being fetched under a different compatibility contract than the request
/// that will later run it.
pub const COPILOT_API_VERSION: &str = "2026-06-01";

// The short-lived Copilot session is scoped to the official chat integration.
// These versions meet GitHub's documented minimum for the GPT-5.6 family.
pub const COPILOT_INTEGRATION_ID: &str = "vscode-chat";
pub const COPILOT_EDITOR_VERSION: &str = "vscode/1.128.0";
pub const COPILOT_EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.56.0";
pub const COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.56.0";

/// Details that must be shown while the user completes GitHub's device flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubLogin {
    pub login_id: String,
    pub auth_url: String,
    pub user_code: String,
}

/// The API route advertised by GitHub Copilot for a specific model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopilotEndpoint {
    ChatCompletions,
    Responses,
    Messages,
}

/// A selectable model from GitHub Copilot's authenticated `/models` catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopilotModel {
    pub id: String,
    pub display_name: String,
    /// The account-specific Copilot API endpoint that advertised this model.
    /// GitHub can route Business and Enterprise accounts away from the public
    /// `api.githubcopilot.com` host.
    pub api_base_url: String,
    pub endpoint: CopilotEndpoint,
    pub reasoning_efforts: Vec<String>,
    pub supports_tools: bool,
}

/// Native GitHub App device-flow client for GitHub Copilot connections.
///
/// It only ever uses the app's public client ID. Per-connection access and
/// refresh tokens are encrypted by `CredentialVault`; GitHub CLI credentials,
/// process environment variables, and GitHub App private keys are never read.
#[derive(Clone)]
pub struct GitHubCopilotClient {
    vault: Arc<CredentialVault>,
    http: reqwest::Client,
    pending_logins: Arc<Mutex<HashMap<String, oneshot::Receiver<Result<(), String>>>>>,
    refresh_lock: Arc<Mutex<()>>,
    /// Per-credential Copilot API session tokens. They are short-lived,
    /// fingerprinted against their source credential, and never persisted.
    api_sessions: Arc<Mutex<HashMap<CredentialRef, CopilotApiSession>>>,
}

#[derive(Debug, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct GitHubAppSession {
    #[serde(rename = "type")]
    session_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default = "default_poll_interval")]
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CopilotModelsResponse {
    #[serde(default)]
    data: Vec<Value>,
}

/// The short-lived API credential returned by GitHub after exchanging the
/// user's GitHub OAuth token. Its endpoint is authoritative for Individual,
/// Business, Enterprise, and data-residency accounts.
#[derive(Debug, Deserialize)]
struct CopilotApiTokenResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    expires_at: Option<u64>,
    #[serde(default)]
    refresh_in: Option<u64>,
    #[serde(default)]
    endpoints: Option<CopilotApiEndpoints>,
}

#[derive(Debug, Default, Deserialize)]
struct CopilotApiEndpoints {
    #[serde(default)]
    api: Option<String>,
}

/// Kept only in process memory. The long-lived source credential remains in
/// the encrypted vault; this short-lived token is never written to disk.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
struct CopilotApiSession {
    token: String,
    source_fingerprint: [u8; 32],
    expires_at: u64,
    api_base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CopilotModelResponse {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    supported_endpoints: Option<Vec<String>>,
    #[serde(default)]
    policy: Option<CopilotPolicy>,
    #[serde(default)]
    capabilities: Option<CopilotCapabilities>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct CopilotPolicy {
    #[serde(default)]
    state: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct CopilotCapabilities {
    #[serde(default)]
    supports: Option<CopilotSupports>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct CopilotSupports {
    #[serde(default)]
    reasoning_effort: Option<Vec<String>>,
    #[serde(default)]
    tool_calls: Option<bool>,
}

impl GitHubCopilotClient {
    pub fn connect(vault: Arc<CredentialVault>) -> Result<Arc<Self>, GitHubError> {
        let http = reqwest::Client::builder()
            .user_agent(format!("averroes/{}", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Arc::new(Self {
            vault,
            http,
            pending_logins: Arc::new(Mutex::new(HashMap::new())),
            refresh_lock: Arc::new(Mutex::new(())),
            api_sessions: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    pub async fn start_login(&self, credential: CredentialRef) -> Result<GitHubLogin, GitHubError> {
        diagnostics::record(
            DiagnosticLevel::Info,
            "copilot.oauth",
            "Requesting a new GitHub device authorization.",
        );
        let device = self.request_device_code().await?;
        validate_device_code(&device)?;

        let login_id = uuid::Uuid::new_v4().to_string();
        let auth_url = device
            .verification_uri_complete
            .clone()
            .unwrap_or_else(|| device.verification_uri.clone());
        let user_code = device.user_code.clone();
        let http = self.http.clone();
        let vault = self.vault.clone();
        let (sender, receiver) = oneshot::channel();

        tokio::spawn(async move {
            let result = oauth::poll_for_access_token(http, vault, credential, device)
                .await
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.pending_logins
            .lock()
            .await
            .insert(login_id.clone(), receiver);

        diagnostics::record(
            DiagnosticLevel::Info,
            "copilot.oauth",
            "GitHub device authorization created; waiting for user approval.",
        );

        Ok(GitHubLogin {
            login_id,
            auth_url,
            user_code,
        })
    }

    pub async fn wait_for_login(&self, login_id: &str) -> Result<(), GitHubError> {
        let receiver = self
            .pending_logins
            .lock()
            .await
            .remove(login_id)
            .ok_or_else(|| GitHubError::Login("unknown or expired sign-in request".into()))?;
        let result = receiver
            .await
            .map_err(|_| GitHubError::Login("sign-in task stopped".into()))?
            .map_err(GitHubError::Login);
        match &result {
            Ok(()) => diagnostics::record(
                DiagnosticLevel::Success,
                "copilot.oauth",
                "GitHub authorization completed and the credential was encrypted.",
            ),
            Err(error) => {
                diagnostics::record(DiagnosticLevel::Error, "copilot.oauth", error.to_string())
            }
        }
        result
    }

    /// Returns the currently valid access token. A manually pasted token is
    /// returned unchanged, while a managed GitHub App token is refreshed just
    /// before it expires.
    pub async fn access_token(
        &self,
        credential: &CredentialRef,
    ) -> Result<Zeroizing<String>, GitHubError> {
        let secret = self.vault.get(credential)?;
        let Some(mut session) = parse_session(&secret)? else {
            return Ok(secret);
        };
        drop(secret);

        if session.client_id.as_deref() != Some(GITHUB_APP_CLIENT_ID) {
            diagnostics::record(
                DiagnosticLevel::Warning,
                "copilot.oauth",
                "The saved authorization belongs to the previous custom GitHub App and cannot access the current Copilot catalog.",
            );
            return Err(GitHubError::Login(
                "Reconnect this Copilot connection once to authorize the official GitHub Copilot model catalog"
                    .into(),
            ));
        }

        if session.expires_at.is_some_and(|expires| {
            expires <= unix_timestamp().saturating_add(EXPIRY_LEEWAY_SECONDS)
        }) {
            self.refresh_session(credential, &mut session).await?;
        }
        Ok(Zeroizing::new(session.access_token.clone()))
    }

    /// Resolves the bearer credential used for a Copilot inference request.
    /// Newer Copilot models require the short-lived session token and matching
    /// integration headers. The source credential remains encrypted and the
    /// exchanged token is only cached in memory.
    pub async fn copilot_api_token(
        &self,
        credential: &CredentialRef,
    ) -> Result<Zeroizing<String>, GitHubError> {
        let source_token = self.access_token(credential).await?;
        if let Some(session) = self.copilot_api_session(credential, &source_token).await {
            return Ok(Zeroizing::new(session.token.clone()));
        }
        Err(GitHubError::Login(
            "GitHub did not issue a Copilot session token; reconnect the Copilot connection".into(),
        ))
    }

    /// Exchanges a GitHub OAuth token for a short-lived Copilot API token.
    /// A failure intentionally returns `None`: GitHub does not enable this
    /// endpoint for every legitimate plan/token combination, and callers then
    /// use the direct OAuth path above.
    async fn copilot_api_session(
        &self,
        credential: &CredentialRef,
        source_token: &str,
    ) -> Option<CopilotApiSession> {
        let fingerprint = token_fingerprint(source_token);
        {
            let mut sessions = self.api_sessions.lock().await;
            if let Some(session) = sessions.get(credential) {
                if session.source_fingerprint == fingerprint && session.is_usable() {
                    diagnostics::record(
                        DiagnosticLevel::Info,
                        "copilot.session",
                        "Reusing the valid in-memory Copilot session token.",
                    );
                    return Some(session.clone());
                }
            }
            sessions.remove(credential);
        }

        diagnostics::record(
            DiagnosticLevel::Info,
            "copilot.session",
            "Exchanging the GitHub user credential for a short-lived Copilot session.",
        );
        let mut response = match self
            .copilot_token_exchange_request(source_token, false)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "copilot.session",
                    format!("Copilot session exchange could not connect: {error}"),
                );
                return None;
            }
        };
        // GitHub's Copilot App issues `ghu_` tokens and the exchange endpoint
        // expects the historical `token` scheme. Keep Bearer as a narrow
        // compatibility retry for alternative user-token implementations.
        if matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED
                | reqwest::StatusCode::FORBIDDEN
                | reqwest::StatusCode::NOT_FOUND
        ) {
            diagnostics::record(
                DiagnosticLevel::Warning,
                "copilot.session",
                format!(
                    "Token session exchange returned {}; retrying Bearer authorization.",
                    response.status().as_u16()
                ),
            );
            response = match self
                .copilot_token_exchange_request(source_token, true)
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "copilot.session",
                        format!("Bearer Copilot session exchange could not connect: {error}"),
                    );
                    return None;
                }
            };
        }
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let detail = response.text().await.unwrap_or_default();
            diagnostics::record(
                DiagnosticLevel::Warning,
                "copilot.session",
                format!(
                    "Copilot session exchange returned {status}: {}. The direct user-token route will be tried.",
                    limited_text(&detail)
                ),
            );
            return None;
        }
        let response = match response.json::<CopilotApiTokenResponse>().await {
            Ok(response) => response,
            Err(error) => {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "copilot.session",
                    format!("GitHub returned an unreadable Copilot session: {error}"),
                );
                return None;
            }
        };
        let session = match CopilotApiSession::from_response(response, fingerprint) {
            Some(session) => session,
            None => {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "copilot.session",
                    "GitHub's Copilot session response did not contain a usable token.",
                );
                return None;
            }
        };
        diagnostics::record(
            DiagnosticLevel::Success,
            "copilot.session",
            match session.api_base_url.as_deref() {
                Some(endpoint) => {
                    format!("Copilot session created for endpoint {endpoint}.")
                }
                None => {
                    "Copilot session created; no account-specific endpoint was returned.".into()
                }
            },
        );
        self.api_sessions
            .lock()
            .await
            .insert(credential.clone(), session.clone());
        Some(session)
    }

    fn copilot_token_exchange_request(
        &self,
        source_token: &str,
        use_bearer_auth: bool,
    ) -> reqwest::RequestBuilder {
        let request = self.http.get(GITHUB_COPILOT_TOKEN_URL);
        let request = if use_bearer_auth {
            request.bearer_auth(source_token)
        } else {
            request.header(
                reqwest::header::AUTHORIZATION,
                format!("token {source_token}"),
            )
        };
        self.copilot_request(request, COPILOT_TOKEN_EXCHANGE_API_VERSION)
    }

    fn copilot_request(
        &self,
        request: reqwest::RequestBuilder,
        api_version: &str,
    ) -> reqwest::RequestBuilder {
        request
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, COPILOT_USER_AGENT)
            .header("Editor-Version", COPILOT_EDITOR_VERSION)
            .header("Editor-Plugin-Version", COPILOT_EDITOR_PLUGIN_VERSION)
            .header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
            .header("X-GitHub-Api-Version", api_version)
    }

    async fn discover_api_endpoint(&self, token: &str) -> Option<String> {
        let response = match self
            .http
            .post(GITHUB_GRAPHQL_URL)
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .json(&serde_json::json!({
                "query": "query { viewer { copilotEndpoints { api } } }"
            }))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "copilot.routing",
                    format!("GitHub endpoint discovery could not connect: {error}"),
                );
                return None;
            }
        };
        if !response.status().is_success() {
            diagnostics::record(
                DiagnosticLevel::Warning,
                "copilot.routing",
                format!(
                    "GitHub endpoint discovery returned {}.",
                    response.status().as_u16()
                ),
            );
            return None;
        }
        let response = response.json::<Value>().await.ok()?;
        response
            .pointer("/data/viewer/copilotEndpoints/api")
            .and_then(Value::as_str)
            .and_then(catalog::normalize_copilot_api_base)
    }

    async fn request_device_code(&self) -> Result<DeviceCodeResponse, GitHubError> {
        let response = self
            .http
            .post(GITHUB_DEVICE_CODE_URL)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[("client_id", GITHUB_APP_CLIENT_ID), ("scope", "read:user")])
            .send()
            .await?;
        parse_response(response).await
    }

    async fn refresh_session(
        &self,
        credential: &CredentialRef,
        session: &mut GitHubAppSession,
    ) -> Result<(), GitHubError> {
        let _guard = self.refresh_lock.lock().await;

        // Another request could have refreshed the session while this caller
        // was waiting for the lock, so always reload the encrypted payload.
        let secret = self.vault.get(credential)?;
        let Some(latest) = parse_session(&secret)? else {
            return Err(GitHubError::Login(
                "GitHub sign-in was replaced; reconnect this Copilot connection".into(),
            ));
        };
        drop(secret);
        if latest
            .expires_at
            .is_none_or(|expires| expires > unix_timestamp().saturating_add(EXPIRY_LEEWAY_SECONDS))
        {
            *session = latest;
            return Ok(());
        }

        let refresh_token = latest.refresh_token.as_deref().ok_or_else(|| {
            GitHubError::Login("GitHub token expired; sign in with GitHub again".into())
        })?;
        let response = self
            .http
            .post(GITHUB_ACCESS_TOKEN_URL)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", GITHUB_APP_CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await?;
        let response: AccessTokenResponse = parse_response(response).await?;
        let refreshed = GitHubAppSession::from_access_token_response(response)?;
        self.save_session(credential, &refreshed)?;
        *session = refreshed;
        Ok(())
    }

    fn save_session(
        &self,
        credential: &CredentialRef,
        session: &GitHubAppSession,
    ) -> Result<(), GitHubError> {
        let serialized = Zeroizing::new(
            serde_json::to_string(session)
                .map_err(|error| GitHubError::Login(error.to_string()))?,
        );
        self.vault.put(credential, &serialized)?;
        Ok(())
    }
}

impl GitHubAppSession {
    fn from_access_token_response(response: AccessTokenResponse) -> Result<Self, GitHubError> {
        if let Some(error) = response.error {
            return Err(GitHubError::Login(format_oauth_error(
                &error,
                response.error_description.as_deref(),
            )));
        }
        let access_token = response
            .access_token
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| GitHubError::Login("GitHub did not return an access token".into()))?;
        Ok(Self {
            session_type: SESSION_TYPE.into(),
            client_id: Some(GITHUB_APP_CLIENT_ID.into()),
            access_token,
            refresh_token: response
                .refresh_token
                .filter(|token| !token.trim().is_empty()),
            expires_at: response
                .expires_in
                .map(|ttl| unix_timestamp().saturating_add(ttl)),
        })
    }
}

impl CopilotApiSession {
    fn from_response(
        response: CopilotApiTokenResponse,
        source_fingerprint: [u8; 32],
    ) -> Option<Self> {
        let token = response.token?.trim().to_string();
        if token.is_empty() {
            return None;
        }
        let now = unix_timestamp();
        let expires_at = response
            .expires_at
            .filter(|expires_at| *expires_at > now)
            .or_else(|| {
                response
                    .refresh_in
                    .filter(|refresh_in| *refresh_in > 0)
                    .map(|refresh_in| now.saturating_add(refresh_in))
            })
            // GitHub normally includes an expiry. If a rolling response omits
            // it, cache only briefly so we do not retain a stale session.
            .unwrap_or_else(|| now.saturating_add(COPILOT_SESSION_FALLBACK_TTL_SECONDS));
        let api_base_url = response
            .endpoints
            .and_then(|endpoints| endpoints.api)
            .as_deref()
            .and_then(catalog::normalize_copilot_api_base);
        Some(Self {
            token,
            source_fingerprint,
            expires_at,
            api_base_url,
        })
    }

    fn is_usable(&self) -> bool {
        self.expires_at > unix_timestamp().saturating_add(COPILOT_SESSION_LEEWAY_SECONDS)
    }
}

fn token_fingerprint(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn token_kind(token: &str) -> &'static str {
    if token.starts_with("ghu_") {
        "GitHub App user"
    } else if token.starts_with("gho_") {
        "GitHub OAuth"
    } else if token.starts_with("github_pat_") {
        "fine-grained GitHub PAT"
    } else if token.starts_with("ghp_") {
        "unsupported classic GitHub PAT"
    } else {
        "unrecognized GitHub"
    }
}

async fn parse_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T, GitHubError> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(GitHubError::Login(format!(
            "GitHub returned {}: {}",
            status.as_u16(),
            limited_text(&body)
        )));
    }
    serde_json::from_str(&body).map_err(|error| GitHubError::Login(error.to_string()))
}

fn parse_session(secret: &str) -> Result<Option<GitHubAppSession>, GitHubError> {
    let Ok(value) = serde_json::from_str::<Value>(secret) else {
        return Ok(None);
    };
    if value.get("type").and_then(Value::as_str) != Some(SESSION_TYPE) {
        return Ok(None);
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|error| GitHubError::Login(format!("saved GitHub session is invalid: {error}")))
}

fn validate_device_code(device: &DeviceCodeResponse) -> Result<(), GitHubError> {
    if device.device_code.trim().is_empty()
        || device.user_code.trim().is_empty()
        || device.verification_uri.trim().is_empty()
        || device.expires_in == 0
    {
        return Err(GitHubError::Login(
            "GitHub returned an incomplete device sign-in request".into(),
        ));
    }
    Ok(())
}

fn default_poll_interval() -> u64 {
    5
}

fn format_oauth_error(code: &str, description: Option<&str>) -> String {
    match description.filter(|description| !description.trim().is_empty()) {
        Some(description) => format!("GitHub sign-in failed ({code}): {description}"),
        None => format!("GitHub sign-in failed: {code}"),
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn limited_text(body: &str) -> String {
    body.chars().take(500).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    #[error("GitHub Copilot sign-in failed: {0}")]
    Login(String),
    #[error("GitHub Copilot network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("GitHub Copilot model catalog error: {0}")]
    Catalog(String),
    #[error("GitHub Copilot secure storage error: {0}")]
    Vault(#[from] VaultError),
}

#[cfg(test)]
mod tests {
    use super::catalog::{
        copilot_catalog_endpoints, normalize_copilot_api_base, parse_catalog_models,
    };
    use super::*;

    #[test]
    fn github_app_sessions_are_distinguished_from_manual_tokens() {
        assert!(parse_session("github_pat_example").unwrap().is_none());
        assert!(parse_session(r#"{\"type\":\"something_else\"}"#)
            .unwrap()
            .is_none());
    }

    #[test]
    fn access_token_response_keeps_refresh_metadata() {
        let session = GitHubAppSession::from_access_token_response(AccessTokenResponse {
            access_token: Some("ghu_access".into()),
            refresh_token: Some("ghr_refresh".into()),
            expires_in: Some(28_800),
            error: None,
            error_description: None,
        })
        .unwrap();

        assert_eq!(session.session_type, SESSION_TYPE);
        assert_eq!(session.refresh_token.as_deref(), Some("ghr_refresh"));
        assert!(session.expires_at.is_some());
    }

    #[test]
    fn access_token_response_reports_github_errors_without_tokens() {
        let error = GitHubAppSession::from_access_token_response(AccessTokenResponse {
            access_token: None,
            refresh_token: None,
            expires_in: None,
            error: Some("access_denied".into()),
            error_description: Some("The user cancelled".into()),
        })
        .unwrap_err();

        assert!(error.to_string().contains("access_denied"));
        assert!(!error.to_string().contains("ghu_"));
    }

    #[test]
    fn exchanged_copilot_session_uses_its_account_endpoint() {
        let session = CopilotApiSession::from_response(
            CopilotApiTokenResponse {
                token: Some("tid=short-lived-session".into()),
                expires_at: Some(unix_timestamp().saturating_add(600)),
                refresh_in: None,
                endpoints: Some(CopilotApiEndpoints {
                    api: Some("https://api.individual.githubcopilot.com/".into()),
                }),
            },
            token_fingerprint("github-source-token"),
        )
        .expect("a valid exchange response produces a session");

        assert!(session.is_usable());
        assert_eq!(
            session.api_base_url.as_deref(),
            Some("https://api.individual.githubcopilot.com")
        );
    }

    #[test]
    fn catalog_endpoints_keep_the_session_route_first() {
        assert_eq!(
            copilot_catalog_endpoints(
                Some("https://api.individual.githubcopilot.com/"),
                Some("https://api.githubcopilot.com"),
            ),
            vec![
                "https://api.individual.githubcopilot.com".to_string(),
                "https://api.githubcopilot.com".to_string(),
            ]
        );
    }

    #[test]
    fn copilot_catalog_keeps_usable_models_even_when_picker_flag_is_false() {
        let selectable = CopilotModel::from_response(
            CopilotModelResponse {
                id: "gpt-5.4".into(),
                name: Some("GPT-5.4".into()),
                supported_endpoints: Some(vec!["/responses".into(), "/chat/completions".into()]),
                policy: Some(CopilotPolicy::default()),
                capabilities: Some(CopilotCapabilities {
                    supports: Some(CopilotSupports {
                        reasoning_effort: Some(vec!["low".into(), "high".into()]),
                        tool_calls: Some(true),
                    }),
                }),
            },
            GITHUB_COPILOT_API_BASE,
        )
        .unwrap();
        assert_eq!(selectable.endpoint, CopilotEndpoint::Responses);
        assert_eq!(selectable.api_base_url, GITHUB_COPILOT_API_BASE);
        assert_eq!(selectable.reasoning_efforts, ["low", "high"]);

        let legacy_catalog_gpt = CopilotModel::from_response(
            CopilotModelResponse {
                id: "gpt-5.6-luna".into(),
                name: Some("GPT-5.6 Luna".into()),
                supported_endpoints: Some(vec![]),
                policy: Some(CopilotPolicy::default()),
                capabilities: Some(CopilotCapabilities {
                    supports: Some(CopilotSupports {
                        tool_calls: Some(true),
                        ..Default::default()
                    }),
                }),
            },
            GITHUB_COPILOT_API_BASE,
        )
        .unwrap();
        assert_eq!(legacy_catalog_gpt.endpoint, CopilotEndpoint::Responses);

        let rolling_catalog_model = CopilotModel::from_response(
            CopilotModelResponse {
                id: "mai-code-1.1-flash".into(),
                name: Some("Mai Code 1.1 Flash".into()),
                supported_endpoints: Some(vec![]),
                policy: Some(CopilotPolicy::default()),
                capabilities: Some(CopilotCapabilities {
                    supports: Some(CopilotSupports {
                        tool_calls: Some(true),
                        ..Default::default()
                    }),
                }),
            },
            GITHUB_COPILOT_API_BASE,
        )
        .unwrap();
        assert_eq!(
            rolling_catalog_model.endpoint,
            CopilotEndpoint::ChatCompletions
        );

        // Some valid Copilot chat models do not expose a `supports` object
        // yet. They are still usable; only their tool capability is unknown.
        let chat_only_model = CopilotModel::from_response(
            CopilotModelResponse {
                id: "gemini-2.5-pro".into(),
                name: Some("Gemini 2.5 Pro".into()),
                supported_endpoints: None,
                policy: Some(CopilotPolicy::default()),
                capabilities: Some(CopilotCapabilities { supports: None }),
            },
            GITHUB_COPILOT_API_BASE,
        )
        .unwrap();
        assert_eq!(chat_only_model.endpoint, CopilotEndpoint::ChatCompletions);
        assert!(!chat_only_model.supports_tools);
        assert!(chat_only_model.reasoning_efforts.is_empty());

        let messages_only = CopilotModel::from_response(
            CopilotModelResponse {
                id: "claude-opus".into(),
                name: Some("Claude Opus".into()),
                supported_endpoints: Some(vec!["/v1/messages".into(), "/responses".into()]),
                policy: Some(CopilotPolicy::default()),
                capabilities: Some(CopilotCapabilities {
                    supports: Some(CopilotSupports {
                        tool_calls: Some(true),
                        ..Default::default()
                    }),
                }),
            },
            GITHUB_COPILOT_API_BASE,
        )
        .unwrap();
        assert_eq!(messages_only.endpoint, CopilotEndpoint::Messages);

        // The model catalog is authoritative. GitHub can omit capability
        // metadata for newly rolled-out models, so that must not remove them
        // from the picker.
        let incomplete = CopilotModel::from_response(
            CopilotModelResponse {
                id: "hidden-utility".into(),
                name: Some("Hidden utility".into()),
                supported_endpoints: Some(vec!["/responses".into()]),
                policy: Some(CopilotPolicy::default()),
                capabilities: None,
            },
            GITHUB_COPILOT_API_BASE,
        )
        .unwrap();
        assert_eq!(incomplete.endpoint, CopilotEndpoint::Responses);
        assert!(!incomplete.supports_tools);

        let disabled = CopilotModel::from_response(
            CopilotModelResponse {
                id: "disabled-model".into(),
                name: Some("Disabled model".into()),
                supported_endpoints: None,
                policy: Some(CopilotPolicy {
                    state: Some("disabled".into()),
                }),
                capabilities: None,
            },
            GITHUB_COPILOT_API_BASE,
        );
        assert!(disabled.is_none());

        let nullable_fields = serde_json::from_str::<CopilotModelsResponse>(
            r#"{"data":[{"id":"gpt-4o","name":"GPT-4o","policy":null,"supported_endpoints":null,"capabilities":{"type":"chat","limits":{"max_output_tokens":4096,"max_prompt_tokens":128000},"supports":{"tool_calls":true,"streaming":true,"reasoning_effort":null}}}]}"#,
        )
        .unwrap();
        assert_eq!(nullable_fields.data.len(), 1);
        let (models, malformed) =
            parse_catalog_models(nullable_fields.data, GITHUB_COPILOT_API_BASE);
        assert_eq!(malformed, 0);
        assert_eq!(models.len(), 1);
    }

    #[test]
    fn catalog_keeps_valid_models_when_one_entry_is_malformed() {
        let raw = serde_json::json!([
            {
                "id": "gpt-5.6-luna",
                "name": "GPT-5.6 Luna",
                "supported_endpoints": ["/responses"]
            },
            { "id": null, "name": "broken future catalog entry" }
        ]);
        let (models, malformed) = parse_catalog_models(
            raw.as_array().cloned().unwrap(),
            "https://copilot.example.test/tenant/",
        );

        assert_eq!(malformed, 1);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5.6-luna");
        assert_eq!(
            models[0].api_base_url,
            "https://copilot.example.test/tenant/"
        );
    }

    #[test]
    fn discovered_copilot_endpoint_must_be_an_https_url() {
        assert_eq!(
            normalize_copilot_api_base("https://copilot.example.test/tenant/?unused=true#fragment"),
            Some("https://copilot.example.test/tenant".into())
        );
        assert_eq!(
            normalize_copilot_api_base("http://copilot.example.test"),
            None
        );
        assert_eq!(normalize_copilot_api_base("not a url"), None);
    }
}

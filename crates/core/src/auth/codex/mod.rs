use crate::connection::CredentialRef;
use crate::credentials::{CredentialVault, VaultError};
use crate::diagnostics::{self, DiagnosticLevel};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{oneshot, Mutex};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

mod catalog;
mod oauth;

pub(crate) const CODEX_API_BASE: &str = "https://chatgpt.com/backend-api/codex";
// The ChatGPT Codex catalog is originator-aware.  `codex_cli_rs` is the
// originator accepted by the Codex backend; an app-specific value can make
// the endpoint return a successful response with an empty `models` array.
pub(crate) const CODEX_ORIGINATOR: &str = "codex_cli_rs";
// This is the Codex protocol client version, not Averroes' application
// version. Sending `0.1.0` (the app version) makes the backend advertise no
// usable models because it predates the current Codex catalog contract.
pub(crate) const CODEX_CLIENT_VERSION: &str = "0.149.0";

const OAUTH_ISSUER: &str = "https://auth.openai.com";
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OAUTH_CREDENTIAL: &str = "oauth:chatgpt";
const LOGIN_TIMEOUT_SECONDS: u64 = 300;
const CALLBACK_PORTS: [u16; 2] = [1455, 1457];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAccount {
    pub authenticated: bool,
    pub email: Option<String>,
    pub plan: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexLogin {
    pub login_id: String,
    pub auth_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexModel {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub reasoning_efforts: Vec<String>,
}

#[derive(Clone)]
pub struct CodexClient {
    vault: Arc<CredentialVault>,
    http: reqwest::Client,
    pending_logins: Arc<Mutex<HashMap<String, oneshot::Receiver<Result<(), String>>>>>,
    refresh_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone)]
pub(crate) struct CodexCredentials {
    pub access_token: String,
    pub account_id: String,
}

#[derive(Debug, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct OAuthSession {
    id_token: String,
    access_token: String,
    refresh_token: String,
    account_id: String,
    email: Option<String>,
    plan: Option<String>,
    expires_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

impl CodexClient {
    /// Creates a native ChatGPT client. It neither discovers nor launches a
    /// `codex` executable and it never reads API keys from the environment.
    pub async fn connect(vault: Arc<CredentialVault>) -> Result<Arc<Self>, CodexError> {
        let http = reqwest::Client::builder()
            .user_agent(format!("{CODEX_ORIGINATOR}/{CODEX_CLIENT_VERSION}"))
            .build()?;
        Ok(Arc::new(Self {
            vault,
            http,
            pending_logins: Arc::new(Mutex::new(HashMap::new())),
            refresh_lock: Arc::new(Mutex::new(())),
        }))
    }

    pub async fn account(&self) -> Result<CodexAccount, CodexError> {
        let Some(session) = self.load_session()? else {
            return Ok(CodexAccount {
                authenticated: false,
                email: None,
                plan: None,
            });
        };
        Ok(CodexAccount {
            authenticated: true,
            email: session.email.clone(),
            plan: session.plan.clone(),
        })
    }

    pub async fn start_chatgpt_login(&self) -> Result<CodexLogin, CodexError> {
        let (listener, port) = oauth::bind_callback_server().await?;
        let pkce = oauth::generate_pkce()?;
        let state = oauth::random_urlsafe(32)?;
        let redirect_uri = format!("http://localhost:{port}/auth/callback");
        let auth_url = oauth::build_authorize_url(&redirect_uri, &pkce.challenge, &state);
        let login_id = uuid::Uuid::new_v4().to_string();
        let http = self.http.clone();
        let vault = self.vault.clone();
        let (sender, receiver) = oneshot::channel();

        tokio::spawn(async move {
            let login = oauth::complete_login(
                listener,
                &http,
                &vault,
                &redirect_uri,
                &pkce.verifier,
                &state,
            );
            let result = match tokio::time::timeout(
                std::time::Duration::from_secs(LOGIN_TIMEOUT_SECONDS),
                login,
            )
            .await
            {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(_) => Err("sign-in timed out".to_string()),
            };
            let _ = sender.send(result);
        });
        self.pending_logins
            .lock()
            .await
            .insert(login_id.clone(), receiver);

        Ok(CodexLogin { login_id, auth_url })
    }

    pub async fn wait_for_login(&self, login_id: &str) -> Result<CodexAccount, CodexError> {
        let receiver = self
            .pending_logins
            .lock()
            .await
            .remove(login_id)
            .ok_or_else(|| CodexError::Login("unknown or expired sign-in request".into()))?;
        receiver
            .await
            .map_err(|_| CodexError::Login("sign-in task stopped".into()))?
            .map_err(CodexError::Login)?;
        self.account().await
    }

    pub async fn list_models(&self) -> Result<Vec<CodexModel>, CodexError> {
        diagnostics::record(
            DiagnosticLevel::Info,
            "codex.catalog",
            format!("Requesting {CODEX_API_BASE}/models with originator {CODEX_ORIGINATOR}."),
        );
        let response = self.send_models_request().await?;
        let response = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            diagnostics::record(
                DiagnosticLevel::Warning,
                "codex.catalog",
                "Codex model request returned 401; refreshing the ChatGPT session.",
            );
            self.refresh().await?;
            self.send_models_request().await?
        } else {
            response
        };
        let status = response.status();
        if !status.is_success() {
            diagnostics::record(
                DiagnosticLevel::Error,
                "codex.catalog",
                format!("Codex model request failed with HTTP {status}."),
            );
            return Err(CodexError::Api {
                status: status.as_u16(),
                body: limited_body(response).await,
            });
        }
        let payload = response.json::<Value>().await?;
        let models = catalog::parse_models(&payload);
        diagnostics::record(
            if models.is_empty() {
                DiagnosticLevel::Warning
            } else {
                DiagnosticLevel::Success
            },
            "codex.catalog",
            format!(
                "Codex model catalog parsed {} usable model(s).",
                models.len()
            ),
        );
        if models.is_empty() {
            let detail = match payload.get("models") {
                Some(Value::Array(entries)) => {
                    format!("The response advertised {} model(s).", entries.len())
                }
                Some(value) => format!(
                    "The response contains a `models` field with JSON type {}.",
                    json_type_name(value)
                ),
                None => payload
                    .as_object()
                    .map(|object| {
                        format!(
                            "The response has no `models` field; top-level keys: {}.",
                            object.keys().cloned().collect::<Vec<_>>().join(", ")
                        )
                    })
                    .unwrap_or_else(|| {
                        format!(
                            "The response root has JSON type {}.",
                            json_type_name(&payload)
                        )
                    }),
            };
            diagnostics::record(DiagnosticLevel::Info, "codex.catalog", detail);
        }
        Ok(models)
    }

    pub(crate) async fn credentials(&self) -> Result<CodexCredentials, CodexError> {
        let session = self.load_session()?.ok_or(CodexError::NotAuthenticated)?;
        if session
            .expires_at
            .is_some_and(|expires| expires <= unix_timestamp().saturating_add(60))
        {
            self.refresh().await?;
        }
        let session = self.load_session()?.ok_or(CodexError::NotAuthenticated)?;
        if session.account_id.trim().is_empty() {
            return Err(CodexError::Authentication(
                "ChatGPT account id is missing".into(),
            ));
        }
        Ok(CodexCredentials {
            access_token: session.access_token.clone(),
            account_id: session.account_id.clone(),
        })
    }

    pub(crate) async fn refresh(&self) -> Result<(), CodexError> {
        let _guard = self.refresh_lock.lock().await;
        let mut session = self.load_session()?.ok_or(CodexError::NotAuthenticated)?;
        let response = self
            .http
            .post(format!("{OAUTH_ISSUER}/oauth/token"))
            .json(&serde_json::json!({
                "client_id": OAUTH_CLIENT_ID,
                "grant_type": "refresh_token",
                "refresh_token": session.refresh_token,
            }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(CodexError::Authentication(format!(
                "token refresh returned {}: {}",
                status.as_u16(),
                limited_body(response).await
            )));
        }
        let refreshed: TokenResponse = response.json().await?;
        if let Some(id_token) = refreshed.id_token {
            let claims = parse_token_claims(&id_token)?;
            session.id_token = id_token;
            session.account_id = claims.account_id;
            session.email = claims.email;
            session.plan = claims.plan;
        }
        if let Some(access_token) = refreshed.access_token {
            session.expires_at = jwt_expiration(&access_token).or_else(|| {
                refreshed
                    .expires_in
                    .map(|ttl| unix_timestamp().saturating_add(ttl))
            });
            session.access_token = access_token;
        }
        if let Some(refresh_token) = refreshed.refresh_token {
            session.refresh_token = refresh_token;
        }
        self.save_session(&session)
    }

    async fn send_models_request(&self) -> Result<reqwest::Response, CodexError> {
        let credentials = self.credentials().await?;
        Ok(self
            .http
            .get(format!("{CODEX_API_BASE}/models"))
            .query(&[("client_version", CODEX_CLIENT_VERSION)])
            .bearer_auth(credentials.access_token)
            .header("ChatGPT-Account-ID", credentials.account_id)
            .header("originator", CODEX_ORIGINATOR)
            .send()
            .await?)
    }

    fn load_session(&self) -> Result<Option<OAuthSession>, CodexError> {
        let credential = CredentialRef(OAUTH_CREDENTIAL.into());
        match self.vault.get(&credential) {
            Ok(secret) => serde_json::from_str(&secret)
                .map(Some)
                .map_err(|error| CodexError::Authentication(error.to_string())),
            Err(VaultError::CredentialNotFound(_)) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn save_session(&self, session: &OAuthSession) -> Result<(), CodexError> {
        let serialized = Zeroizing::new(
            serde_json::to_string(session)
                .map_err(|error| CodexError::Authentication(error.to_string()))?,
        );
        self.vault
            .put(&CredentialRef(OAUTH_CREDENTIAL.into()), &serialized)?;
        Ok(())
    }
}

struct TokenClaims {
    account_id: String,
    email: Option<String>,
    plan: Option<String>,
}

fn parse_token_claims(token: &str) -> Result<TokenClaims, CodexError> {
    let payload = jwt_payload(token)?;
    let auth = payload
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object);
    let profile = payload
        .get("https://api.openai.com/profile")
        .and_then(Value::as_object);
    let account_id = auth
        .and_then(|claims| claims.get("chatgpt_account_id"))
        .or_else(|| payload.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| CodexError::Authentication("ChatGPT account id is missing".into()))?
        .to_string();
    let email = payload
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| {
            profile
                .and_then(|claims| claims.get("email"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned);
    let plan = auth
        .and_then(|claims| claims.get("chatgpt_plan_type"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(TokenClaims {
        account_id,
        email,
        plan,
    })
}

fn jwt_expiration(token: &str) -> Option<u64> {
    jwt_payload(token).ok()?.get("exp")?.as_u64()
}

fn jwt_payload(token: &str) -> Result<Value, CodexError> {
    let payload = token
        .split('.')
        .nth(1)
        .filter(|part| !part.is_empty())
        .ok_or_else(|| CodexError::Authentication("token has an invalid JWT format".into()))?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|error| CodexError::Authentication(error.to_string()))?;
    serde_json::from_slice(&decoded).map_err(|error| CodexError::Authentication(error.to_string()))
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn limited_body(response: reqwest::Response) -> String {
    response
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(2_000)
        .collect()
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodexError {
    #[error("ChatGPT is not connected")]
    NotAuthenticated,
    #[error("ChatGPT sign-in failed: {0}")]
    Login(String),
    #[error("ChatGPT authentication failed: {0}")]
    Authentication(String),
    #[error("Codex API error: status={status}, body={body}")]
    Api { status: u16, body: String },
    #[error("Codex network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Codex secure storage error: {0}")]
    Vault(#[from] VaultError),
    #[error("Codex callback server error: {0}")]
    Io(#[from] std::io::Error),
    #[error("secure random generation failed: {0}")]
    Random(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: Value) -> String {
        let encode = |value: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value);
        format!(
            "{}.{}.signature",
            encode(br#"{"alg":"none"}"#),
            encode(&serde_json::to_vec(&payload).unwrap())
        )
    }

    #[test]
    fn parses_chatgpt_identity_without_exposing_tokens() {
        let claims = parse_token_claims(&jwt(serde_json::json!({
            "email": "person@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-1",
                "chatgpt_plan_type": "plus"
            }
        })))
        .unwrap();
        assert_eq!(claims.account_id, "account-1");
        assert_eq!(claims.email.as_deref(), Some("person@example.com"));
        assert_eq!(claims.plan.as_deref(), Some("plus"));
    }

    #[test]
    fn parses_live_chatgpt_models() {
        let models = catalog::parse_models(&serde_json::json!({
            "models": [{
                "slug": "gpt-5-codex",
                "display_name": "GPT-5 Codex",
                "description": "Coding model",
                "supported_in_api": true,
                "supported_reasoning_levels": [
                    { "effort": "medium" },
                    { "effort": "high" }
                ]
            }]
        }));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5-codex");
        assert_eq!(models[0].reasoning_efforts, ["medium", "high"]);
    }

    #[test]
    fn uses_the_codex_catalog_originator() {
        assert_eq!(CODEX_ORIGINATOR, "codex_cli_rs");
        assert_eq!(CODEX_CLIENT_VERSION, "0.149.0");
    }

    #[test]
    fn parses_alternate_model_catalog_shapes_and_effort_names() {
        let models = catalog::parse_models(&serde_json::json!({
            "data": {
                "gpt-5-mini": {
                    "id": "gpt-5-mini",
                    "displayName": "GPT-5 mini",
                    "reasoningEfforts": "high"
                },
                "hidden": {
                    "id": "hidden",
                    "supported_in_api": false
                }
            }
        }));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].display_name, "GPT-5 mini");
        assert_eq!(models[0].reasoning_efforts, ["high"]);
    }

    #[test]
    fn parses_nested_catalogs_and_deduplicates_model_ids() {
        let models = catalog::parse_models(&serde_json::json!({
            "data": {
                "available": [
                    { "model_id": "gpt-5.6-codex", "name": "GPT-5.6 Codex" },
                    { "model_id": "gpt-5.6-codex", "name": "duplicate" }
                ],
                "metadata": { "request_id": "safe-to-ignore" }
            }
        }));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5.6-codex");
        assert_eq!(models[0].display_name, "GPT-5.6 Codex");
    }

    #[test]
    fn parses_model_catalogs_indexed_by_model_id() {
        let models = catalog::parse_models(&serde_json::json!({
            "models": {
                "gpt-5.6-codex": {
                    "display_name": "GPT-5.6 Codex",
                    "description": "Coding model"
                }
            }
        }));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5.6-codex");
        assert_eq!(models[0].display_name, "GPT-5.6 Codex");
    }

    #[test]
    fn parses_catalogs_that_advertise_model_ids_as_strings() {
        let models = catalog::parse_models(&serde_json::json!({
            "models": ["gpt-5.6-codex", "gpt-5.4-mini"]
        }));

        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5.6-codex", "gpt-5.4-mini"]
        );
    }

    #[test]
    fn authorize_url_uses_pkce_and_local_callback() {
        let url =
            oauth::build_authorize_url("http://localhost:1455/auth/callback", "challenge", "state");
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("localhost%3A1455"));
    }
}

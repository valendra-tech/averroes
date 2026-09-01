use super::{
    CopilotEndpoint, CopilotModel, CopilotModelResponse, CopilotModelsResponse,
    GitHubCopilotClient, GitHubError, COPILOT_API_VERSION, GITHUB_COPILOT_API_BASE,
};
use crate::connection::CredentialRef;
use crate::diagnostics::{self, DiagnosticLevel};
use serde_json::Value;
use std::collections::HashSet;

impl GitHubCopilotClient {
    /// Fetches the account-scoped Copilot catalog. Authentication and model
    /// transport stay here; parsing and endpoint normalization below remain
    /// pure and independently testable.
    pub async fn list_models(
        &self,
        credential: &CredentialRef,
    ) -> Result<Vec<CopilotModel>, GitHubError> {
        let source_token = self.access_token(credential).await?;
        diagnostics::record(
            DiagnosticLevel::Info,
            "copilot.catalog",
            format!(
                "Refreshing the authenticated model catalog with a {} credential.",
                super::token_kind(&source_token)
            ),
        );
        let discovered_api_base = self.discover_api_endpoint(&source_token).await;
        diagnostics::record(
            DiagnosticLevel::Info,
            "copilot.routing",
            match discovered_api_base.as_deref() {
                Some(endpoint) => format!("GitHub advertised account endpoint {endpoint}."),
                None => "GitHub did not advertise an account endpoint; the public Copilot endpoint remains available.".into(),
            },
        );

        if let Some(session) = self.copilot_api_session(credential, &source_token).await {
            let endpoints = copilot_catalog_endpoints(
                session.api_base_url.as_deref(),
                discovered_api_base.as_deref(),
            );
            match self.fetch_catalogs(&session.token, endpoints).await {
                Ok(models) => {
                    diagnostics::record(
                        DiagnosticLevel::Success,
                        "copilot.catalog",
                        format!("Loaded {} models with the Copilot session token.", models.len()),
                    );
                    return Ok(models);
                }
                Err(error) => diagnostics::record(
                    DiagnosticLevel::Warning,
                    "copilot.catalog",
                    format!(
                        "The Copilot session-token catalog route failed; trying the GitHub user token. {error}"
                    ),
                ),
            }
        }

        let endpoints = copilot_catalog_endpoints(None, discovered_api_base.as_deref());
        let result = self.fetch_catalogs(&source_token, endpoints).await;
        match &result {
            Ok(models) => diagnostics::record(
                DiagnosticLevel::Success,
                "copilot.catalog",
                format!("Loaded {} models with the GitHub user token.", models.len()),
            ),
            Err(error) => {
                diagnostics::record(DiagnosticLevel::Error, "copilot.catalog", error.to_string())
            }
        }
        result
    }

    async fn fetch_catalogs(
        &self,
        token: &str,
        endpoints: Vec<String>,
    ) -> Result<Vec<CopilotModel>, GitHubError> {
        let mut models = Vec::new();
        let mut seen_ids = HashSet::new();
        let mut advertised_count = 0;
        let mut unreadable_count = 0;
        let mut last_error = None;

        for api_base_url in endpoints {
            diagnostics::record(
                DiagnosticLevel::Info,
                "copilot.catalog",
                format!("Requesting {api_base_url}/models."),
            );
            match self.fetch_catalog(token, &api_base_url).await {
                Ok((mut catalog, advertised, unreadable)) => {
                    diagnostics::record(
                        if catalog.is_empty() {
                            DiagnosticLevel::Warning
                        } else {
                            DiagnosticLevel::Success
                        },
                        "copilot.catalog",
                        format!(
                            "{api_base_url}/models advertised {advertised} entries; {} usable, {unreadable} malformed.",
                            catalog.len()
                        ),
                    );
                    advertised_count += advertised;
                    unreadable_count += unreadable;
                    for model in catalog.drain(..) {
                        if seen_ids.insert(model.id.clone()) {
                            models.push(model);
                        }
                    }
                }
                Err(error) => {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "copilot.catalog",
                        error.to_string(),
                    );
                    last_error = Some(error);
                }
            }
        }

        if !models.is_empty() {
            diagnostics::record(
                DiagnosticLevel::Info,
                "copilot.catalog",
                format!(
                    "Resolved model IDs: {}.",
                    models
                        .iter()
                        .map(|model| model.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            return Ok(models);
        }

        if advertised_count > 0 {
            return Err(GitHubError::Catalog(format!(
                "GitHub returned {advertised_count} Copilot catalog entries, but none were usable ({unreadable_count} malformed entries)"
            )));
        }
        Err(last_error.unwrap_or_else(|| {
            GitHubError::Catalog(
                "GitHub returned no Copilot models for this account and integration.".into(),
            )
        }))
    }

    async fn fetch_catalog(
        &self,
        token: &str,
        api_base_url: &str,
    ) -> Result<(Vec<CopilotModel>, usize, usize), GitHubError> {
        let response = self
            .copilot_request(
                self.http
                    .get(format!("{api_base_url}/models"))
                    .bearer_auth(token),
                COPILOT_API_VERSION,
            )
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(GitHubError::Catalog(format!(
                "{api_base_url}/models returned {}: {}",
                status.as_u16(),
                super::limited_text(&body)
            )));
        }
        let response: CopilotModelsResponse = serde_json::from_str(&body).map_err(|error| {
            GitHubError::Catalog(format!(
                "GitHub returned an invalid Copilot catalog: {error}"
            ))
        })?;
        let advertised_count = response.data.len();
        let (models, unreadable_count) = parse_catalog_models(response.data, api_base_url);
        Ok((models, advertised_count, unreadable_count))
    }
}

impl CopilotModel {
    pub(super) fn from_response(model: CopilotModelResponse, api_base_url: &str) -> Option<Self> {
        let supports = model
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.supports.as_ref());
        let endpoints = model.supported_endpoints.as_deref().unwrap_or_default();
        let policy_state = model
            .policy
            .as_ref()
            .and_then(|policy| policy.state.as_deref());
        if model.id.trim().is_empty() || policy_state == Some("disabled") {
            return None;
        }
        // Follow the endpoint precedence used by GitHub's own Copilot
        // integrations. Some Claude entries advertise more than one route,
        // but are only callable through the Anthropic-compatible endpoint.
        let endpoint = if endpoints.iter().any(|endpoint| endpoint == "/v1/messages") {
            CopilotEndpoint::Messages
        } else if endpoints.iter().any(|endpoint| endpoint == "/responses") {
            CopilotEndpoint::Responses
        } else if endpoints
            .iter()
            .any(|endpoint| endpoint == "/chat/completions")
        {
            CopilotEndpoint::ChatCompletions
        } else {
            // Older and rolling Copilot catalogs can omit
            // `supported_endpoints` entirely. GitHub's Copilot integrations
            // use these same compatibility rules until the catalog begins
            // advertising a concrete route; do not turn those live models
            // into unusable picker entries.
            fallback_copilot_endpoint(&model.id).unwrap_or(CopilotEndpoint::ChatCompletions)
        };
        let id = model.id;
        let display_name = model.name.as_deref().unwrap_or("").trim();
        let display_name = (!display_name.is_empty())
            .then_some(display_name.to_string())
            .unwrap_or_else(|| id.clone());
        Some(Self {
            id,
            display_name,
            api_base_url: api_base_url.into(),
            endpoint,
            reasoning_efforts: supports
                .and_then(|supports| supports.reasoning_effort.clone())
                .unwrap_or_default(),
            supports_tools: supports
                .and_then(|supports| supports.tool_calls)
                .unwrap_or(false),
        })
    }
}

pub(super) fn parse_catalog_models(
    values: Vec<Value>,
    api_base_url: &str,
) -> (Vec<CopilotModel>, usize) {
    let mut models = Vec::with_capacity(values.len());
    let mut unreadable_count = 0;
    for value in values {
        let Ok(model) = serde_json::from_value::<CopilotModelResponse>(value) else {
            // GitHub rolls catalog metadata independently from the model API.
            // A single future/unknown entry must not make the whole account's
            // catalog disappear from the picker.
            unreadable_count += 1;
            continue;
        };
        if let Some(model) = CopilotModel::from_response(model, api_base_url) {
            models.push(model);
        }
    }
    (models, unreadable_count)
}

/// Produces the full set of hosts that GitHub can legitimately advertise for
/// one account. The session endpoint remains first so duplicate model IDs keep
/// its authoritative route; the public host is a compatibility fallback.
pub(super) fn copilot_catalog_endpoints(
    session_api_base: Option<&str>,
    discovered_api_base: Option<&str>,
) -> Vec<String> {
    let mut endpoints = Vec::with_capacity(3);
    for endpoint in [
        session_api_base,
        discovered_api_base,
        Some(GITHUB_COPILOT_API_BASE),
    ] {
        let Some(endpoint) = endpoint.and_then(normalize_copilot_api_base) else {
            continue;
        };
        if !endpoints.iter().any(|candidate| candidate == &endpoint) {
            endpoints.push(endpoint);
        }
    }
    endpoints
}

pub(super) fn normalize_copilot_api_base(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value.trim()).ok()?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url.as_str().trim_end_matches('/').into())
}

fn fallback_copilot_endpoint(model_id: &str) -> Option<CopilotEndpoint> {
    let normalized = model_id.trim().to_ascii_lowercase();
    if normalized.starts_with("claude") {
        return Some(CopilotEndpoint::Messages);
    }

    let Some(version) = normalized
        .strip_prefix("gpt-")
        .and_then(|rest| {
            rest.split(|character: char| !character.is_ascii_digit())
                .next()
        })
        .and_then(|major| major.parse::<u32>().ok())
    else {
        // GitHub's rolling catalog omits `supported_endpoints` for ordinary
        // OpenAI-compatible chat models. Use chat completions as the
        // compatibility default rather than hiding a newly published entry.
        return Some(CopilotEndpoint::ChatCompletions);
    };

    if version >= 5 && !normalized.starts_with("gpt-5-mini") {
        Some(CopilotEndpoint::Responses)
    } else {
        Some(CopilotEndpoint::ChatCompletions)
    }
}

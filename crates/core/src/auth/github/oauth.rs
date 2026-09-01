use super::{
    AccessTokenResponse, DeviceCodeResponse, GitHubAppSession, GitHubError,
    GITHUB_ACCESS_TOKEN_URL, GITHUB_APP_CLIENT_ID,
};
use crate::connection::CredentialRef;
use crate::credentials::CredentialVault;
use crate::diagnostics::{self, DiagnosticLevel};
use std::sync::Arc;
use std::time::Duration;
use zeroize::Zeroizing;

pub(super) async fn poll_for_access_token(
    http: reqwest::Client,
    vault: Arc<CredentialVault>,
    credential: CredentialRef,
    device: DeviceCodeResponse,
) -> Result<(), GitHubError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(device.expires_in);
    let mut interval = Duration::from_secs(device.interval.max(1));

    loop {
        tokio::time::sleep(interval).await;
        if tokio::time::Instant::now() > deadline {
            return Err(GitHubError::Login(
                "GitHub sign-in code expired; start again".into(),
            ));
        }
        let response = http
            .post(GITHUB_ACCESS_TOKEN_URL)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", GITHUB_APP_CLIENT_ID),
                ("device_code", device.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?;
        let response: AccessTokenResponse = super::parse_response(response).await?;
        match response.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval = interval.saturating_add(Duration::from_secs(5));
                continue;
            }
            Some(_) => {
                return Err(GitHubError::Login(super::format_oauth_error(
                    response.error.as_deref().unwrap_or_default(),
                    response.error_description.as_deref(),
                )));
            }
            None => {
                let session = GitHubAppSession::from_access_token_response(response)?;
                diagnostics::record(
                    DiagnosticLevel::Success,
                    "copilot.oauth",
                    format!(
                        "GitHub issued a {} credential; storing it in the encrypted vault.",
                        super::token_kind(&session.access_token)
                    ),
                );
                let serialized = Zeroizing::new(
                    serde_json::to_string(&session)
                        .map_err(|error| GitHubError::Login(error.to_string()))?,
                );
                vault.put(&credential, &serialized)?;
                return Ok(());
            }
        }
    }
}

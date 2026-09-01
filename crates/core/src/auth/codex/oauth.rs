use super::{
    jwt_expiration, parse_token_claims, CodexError, OAuthSession, TokenResponse, CALLBACK_PORTS,
    CODEX_ORIGINATOR, OAUTH_CLIENT_ID, OAUTH_CREDENTIAL, OAUTH_ISSUER,
};
use crate::connection::CredentialRef;
use crate::credentials::CredentialVault;
use base64::Engine as _;
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use zeroize::Zeroizing;

pub(super) struct Pkce {
    pub(super) verifier: String,
    pub(super) challenge: String,
}

pub(super) fn generate_pkce() -> Result<Pkce, CodexError> {
    let verifier = random_urlsafe(64)?;
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    Ok(Pkce {
        verifier,
        challenge,
    })
}

pub(super) fn random_urlsafe(size: usize) -> Result<String, CodexError> {
    let mut bytes = vec![0_u8; size];
    getrandom::fill(&mut bytes).map_err(|error| CodexError::Random(error.to_string()))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

pub(super) fn build_authorize_url(redirect_uri: &str, challenge: &str, state: &str) -> String {
    let mut url = url::Url::parse(&format!("{OAUTH_ISSUER}/oauth/authorize"))
        .expect("static OpenAI OAuth URL");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", OAUTH_CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair(
            "scope",
            "openid profile email offline_access api.connectors.read api.connectors.invoke",
        )
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", state)
        .append_pair("originator", CODEX_ORIGINATOR);
    url.into()
}

pub(super) async fn bind_callback_server() -> Result<(TcpListener, u16), CodexError> {
    let mut last_error = None;
    for port in CALLBACK_PORTS {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return Ok((listener, port)),
            Err(error) => last_error = Some(error),
        }
    }
    Err(CodexError::Io(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "no OAuth callback port",
        )
    })))
}

pub(super) async fn complete_login(
    listener: TcpListener,
    http: &reqwest::Client,
    vault: &CredentialVault,
    redirect_uri: &str,
    verifier: &str,
    expected_state: &str,
) -> Result<(), CodexError> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let target = match read_request_target(&mut stream).await {
            Ok(target) => target,
            Err(error) => {
                write_callback_response(&mut stream, 400, "Invalid sign-in request").await?;
                return Err(error);
            }
        };
        let callback = url::Url::parse("http://localhost")
            .expect("static callback base")
            .join(&target)
            .map_err(|error| CodexError::Login(error.to_string()))?;
        if callback.path() != "/auth/callback" {
            write_callback_response(&mut stream, 404, "Not found").await?;
            continue;
        }
        let params = callback
            .query_pairs()
            .into_owned()
            .collect::<HashMap<String, String>>();
        if params.get("state").map(String::as_str) != Some(expected_state) {
            write_callback_response(&mut stream, 400, "Sign-in state did not match").await?;
            return Err(CodexError::Login("OAuth state did not match".into()));
        }
        if let Some(error) = params.get("error") {
            write_callback_response(&mut stream, 400, "ChatGPT sign-in was cancelled").await?;
            return Err(CodexError::Login(error.clone()));
        }
        let code = params
            .get("code")
            .filter(|code| !code.is_empty())
            .ok_or_else(|| CodexError::Login("authorization code is missing".into()))?;
        let session = exchange_code(http, code, redirect_uri, verifier).await?;
        let serialized = Zeroizing::new(
            serde_json::to_string(&session)
                .map_err(|error| CodexError::Authentication(error.to_string()))?,
        );
        vault.put(&CredentialRef(OAUTH_CREDENTIAL.into()), &serialized)?;
        write_callback_response(
            &mut stream,
            200,
            "ChatGPT is connected. You can return to Averroes.",
        )
        .await?;
        return Ok(());
    }
}

async fn exchange_code(
    http: &reqwest::Client,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<OAuthSession, CodexError> {
    let response = http
        .post(format!("{OAUTH_ISSUER}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", OAUTH_CLIENT_ID),
            ("code_verifier", verifier),
        ])
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(CodexError::Authentication(format!(
            "token exchange returned {}: {}",
            status.as_u16(),
            super::limited_body(response).await
        )));
    }
    let tokens: TokenResponse = response.json().await?;
    let id_token = tokens
        .id_token
        .ok_or_else(|| CodexError::Authentication("ID token is missing".into()))?;
    let access_token = tokens
        .access_token
        .ok_or_else(|| CodexError::Authentication("access token is missing".into()))?;
    let refresh_token = tokens
        .refresh_token
        .ok_or_else(|| CodexError::Authentication("refresh token is missing".into()))?;
    let claims = parse_token_claims(&id_token)?;
    let expires_at = jwt_expiration(&access_token).or_else(|| {
        tokens
            .expires_in
            .map(|ttl| super::unix_timestamp().saturating_add(ttl))
    });
    Ok(OAuthSession {
        id_token,
        access_token,
        refresh_token,
        account_id: claims.account_id,
        email: claims.email,
        plan: claims.plan,
        expires_at,
    })
}

async fn read_request_target(stream: &mut TcpStream) -> Result<String, CodexError> {
    let mut request = Vec::with_capacity(2_048);
    let mut chunk = [0_u8; 1_024];
    while request.len() < 16_384 {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request =
        std::str::from_utf8(&request).map_err(|error| CodexError::Login(error.to_string()))?;
    let mut first_line = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    match (first_line.next(), first_line.next()) {
        (Some("GET"), Some(target)) => Ok(target.to_string()),
        _ => Err(CodexError::Login("invalid OAuth callback request".into())),
    }
}

async fn write_callback_response(
    stream: &mut TcpStream,
    status: u16,
    message: &str,
) -> Result<(), CodexError> {
    let reason = if status == 200 { "OK" } else { "Error" };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Averroes</title></head><body style=\"font-family:-apple-system;padding:48px;background:#171717;color:#f5f5f5\"><h1>{message}</h1><p>This window can now be closed.</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

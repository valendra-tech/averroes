//! Project-scoped Model Context Protocol integrations.
//!
//! Transport and authentication are separate on purpose. WebMCP is a
//! browser document exposing JavaScript tools, while OAuth authenticates an
//! HTTP MCP server; neither is represented as an opaque URL plus token.

use anyhow::{anyhow, Context};
use base64::Engine as _;
use oxibrowser_core::{Browser, BrowserConfig};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, OnceCell};
use url::Url;

use crate::tool::ToolResult;

pub const PROJECT_MCP_FILE: &str = "mcp.yaml";
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const MAX_TOOL_RESULT_IMAGES: usize = 20;
const MAX_TOOL_RESULT_IMAGE_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectMcpConfig {
    #[serde(rename = "mcpServers", default)]
    pub servers: BTreeMap<String, ProjectMcpServer>,
}

impl ProjectMcpConfig {
    pub fn load(workspace_root: &Path) -> anyhow::Result<Self> {
        let path = workspace_root.join(PROJECT_MCP_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        serde_yaml::from_str(&contents)
            .with_context(|| format!("could not parse {}", path.display()))
    }

    pub fn save(&self, workspace_root: &Path) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(workspace_root)
            .with_context(|| format!("could not create {}", workspace_root.display()))?;
        let path = workspace_root.join(PROJECT_MCP_FILE);
        let contents = serde_yaml::to_string(self).context("could not serialize mcp.yaml")?;
        let temporary = path.with_extension("yaml.tmp");
        std::fs::write(&temporary, contents)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    Stdio,
    #[serde(rename = "streamable_http", alias = "http", alias = "streamable-http")]
    StreamableHttp,
    Sse,
    #[serde(rename = "webmcp", alias = "web_mcp")]
    WebMcp,
}

impl Default for McpTransport {
    fn default() -> Self {
        Self::Stdio
    }
}

impl McpTransport {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::StreamableHttp => "streamable HTTP",
            Self::Sse => "HTTP + SSE",
            Self::WebMcp => "WebMCP",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpAuthType {
    None,
    Bearer,
    #[serde(rename = "oauth")]
    OAuth,
}

impl Default for McpAuthType {
    fn default() -> Self {
        Self::None
    }
}

/// Authentication metadata is safe to keep in the project. Access and
/// refresh tokens are never fields of this structure; the runtime resolves
/// `credential_ref` through CredentialVault/Keychain.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpAuth {
    #[serde(rename = "type", default)]
    pub kind: McpAuthType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectMcpServer {
    #[serde(default, alias = "type")]
    pub transport: McpTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub auth: McpAuth,
}

impl ProjectMcpServer {
    pub fn validate(&self) -> anyhow::Result<()> {
        match self.transport {
            McpTransport::Stdio => {
                if self
                    .command
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    return Err(anyhow!("stdio MCP servers require a command"));
                }
            }
            McpTransport::StreamableHttp | McpTransport::Sse | McpTransport::WebMcp => {
                let parsed = Url::parse(self.url.as_deref().unwrap_or_default().trim())
                    .context("MCP URL is invalid")?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err(anyhow!("MCP URL must be an http(s) URL with a host"));
                }
            }
        }
        if self.auth.kind != McpAuthType::None && self.auth.credential_ref.is_none() {
            return Err(anyhow!(
                "authenticated MCP servers require a Keychain credential reference"
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct McpClient {
    pub server_name: String,
    pub transport: McpTransport,
    pub endpoint: Option<String>,
    command: Option<String>,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
    access_token: Option<String>,
    session_id: Arc<Mutex<Option<String>>>,
    webmcp_browser: Arc<OnceCell<Arc<Browser>>>,
}

impl McpClient {
    pub fn new(server_name: String, endpoint: String) -> Self {
        Self {
            server_name,
            transport: McpTransport::StreamableHttp,
            endpoint: Some(endpoint),
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            headers: BTreeMap::new(),
            access_token: None,
            session_id: Arc::new(Mutex::new(None)),
            webmcp_browser: Arc::new(OnceCell::new()),
        }
    }

    pub fn from_project_server(
        server_name: String,
        server: &ProjectMcpServer,
        access_token: Option<String>,
    ) -> anyhow::Result<Self> {
        server.validate()?;
        Ok(Self {
            server_name,
            transport: server.transport.clone(),
            endpoint: server.url.clone(),
            command: server.command.clone(),
            args: server.args.clone(),
            env: server.env.clone(),
            headers: server.headers.clone(),
            access_token,
            session_id: Arc::new(Mutex::new(None)),
            webmcp_browser: Arc::new(OnceCell::new()),
        })
    }

    pub async fn list_tools(&self) -> anyhow::Result<Vec<McpToolDef>> {
        match self.transport {
            McpTransport::Stdio => parse_tools(self.stdio_request("tools/list", json!({})).await?),
            McpTransport::StreamableHttp | McpTransport::Sse => {
                self.ensure_http_initialized().await?;
                parse_tools(self.http_request("tools/list", json!({})).await?)
            }
            McpTransport::WebMcp => self.webmcp_list_tools().await,
        }
    }

    pub async fn call_tool(&self, name: &str, params: Value) -> anyhow::Result<ToolResult> {
        let value = match self.transport {
            McpTransport::Stdio => {
                self.stdio_request("tools/call", json!({ "name": name, "arguments": params }))
                    .await?
            }
            McpTransport::StreamableHttp | McpTransport::Sse => {
                self.ensure_http_initialized().await?;
                self.http_request("tools/call", json!({ "name": name, "arguments": params }))
                    .await?
            }
            McpTransport::WebMcp => return self.webmcp_call_tool(name, params).await,
        };
        parse_tool_result(value)
    }

    async fn stdio_request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let command = self
            .command
            .as_deref()
            .ok_or_else(|| anyhow!("MCP server '{}' has no command", self.server_name))?;
        let mut child = tokio::process::Command::new(command)
            .args(&self.args)
            .envs(&self.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("could not start MCP server '{}'", self.server_name))?;
        let mut stdin = child.stdin.take().context("MCP stdin unavailable")?;
        let stdout = child.stdout.take().context("MCP stdout unavailable")?;
        let mut lines = BufReader::new(stdout).lines();
        write_json_line(
            &mut stdin,
            &json_rpc_request(
                1,
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "Averroes", "version": env!("CARGO_PKG_VERSION") }
                }),
            ),
        )
        .await?;
        read_json_response(&mut lines, 1).await?;
        write_json_line(
            &mut stdin,
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }),
        )
        .await?;
        write_json_line(&mut stdin, &json_rpc_request(2, method, params)).await?;
        let result = read_json_response(&mut lines, 2).await;
        let _ = child.kill().await;
        result
    }

    async fn ensure_http_initialized(&self) -> anyhow::Result<()> {
        if self.session_id.lock().await.is_some() {
            return Ok(());
        }
        let result = self
            .http_request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "Averroes", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        if result.is_null() {
            return Err(anyhow!(
                "MCP server '{}' returned an empty initialize response",
                self.server_name
            ));
        }
        let _ = self
            .http_notification("notifications/initialized", json!({}))
            .await;
        Ok(())
    }

    async fn http_request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let endpoint = self
            .endpoint
            .as_deref()
            .ok_or_else(|| anyhow!("MCP server '{}' has no URL", self.server_name))?;
        let client = reqwest::Client::new();
        let mut request = client
            .post(endpoint)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
            .json(&json_rpc_request(1, method, params));
        for (key, value) in &self.headers {
            request = request.header(key, value);
        }
        if let Some(session_id) = self.session_id.lock().await.clone() {
            request = request.header("mcp-session-id", session_id);
        }
        if let Some(token) = self.access_token.as_deref() {
            request = request.bearer_auth(token);
        }
        let response = tokio::time::timeout(MCP_REQUEST_TIMEOUT, request.send())
            .await
            .context("MCP HTTP request timed out")??;
        if let Some(session_id) = response.headers().get("mcp-session-id") {
            *self.session_id.lock().await = Some(session_id.to_str()?.to_owned());
        }
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "MCP server '{}' returned HTTP {}: {}",
                self.server_name,
                status,
                body
            ));
        }
        parse_http_json(&body)
    }

    async fn http_notification(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let endpoint = self
            .endpoint
            .as_deref()
            .ok_or_else(|| anyhow!("MCP server '{}' has no URL", self.server_name))?;
        let client = reqwest::Client::new();
        let mut request = client
            .post(endpoint)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
            .json(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
        if let Some(session_id) = self.session_id.lock().await.clone() {
            request = request.header("mcp-session-id", session_id);
        }
        if let Some(token) = self.access_token.as_deref() {
            request = request.bearer_auth(token);
        }
        let _ = tokio::time::timeout(MCP_REQUEST_TIMEOUT, request.send()).await??;
        Ok(())
    }

    async fn webmcp_browser(&self) -> anyhow::Result<Arc<Browser>> {
        self.webmcp_browser
            .get_or_try_init(|| async {
                Browser::new(BrowserConfig::headless())
                    .await
                    .map(Arc::new)
                    .map_err(|error| anyhow!("could not start WebMCP browser: {error}"))
            })
            .await
            .cloned()
    }

    async fn webmcp_list_tools(&self) -> anyhow::Result<Vec<McpToolDef>> {
        let browser = self.webmcp_browser().await?;
        let endpoint = self.endpoint.as_deref().unwrap_or_default();
        let session_handle = browser.new_session().await?;
        let mut session = session_handle.write().await;
        if let Err(error) = session.navigate(endpoint).await {
            drop(session);
            close_webmcp_session(&browser, &session_handle).await;
            return Err(error.into());
        }
        let result = session
            .evaluate_js_with_await(
                "(async function(){if(!document.modelContext){throw new Error('WebMCP is not exposed by this page')}const tools=await document.modelContext.getTools();return tools.map(function(t){return {name:t.name,description:t.description,input_schema:t.inputSchema};});})()",
                true,
            )
            .await;
        drop(session);
        // The session is closed below through the browser handle. Keeping
        // this cleanup on both success and JS errors prevents a failed page
        // from consuming one of the browser's finite session slots.
        close_webmcp_session(&browser, &session_handle).await;
        let result = result?;
        if let Some(error) = result.exception {
            return Err(anyhow!("WebMCP discovery failed: {error}"));
        }
        parse_tools(result.value.unwrap_or(Value::Null))
    }

    async fn webmcp_call_tool(&self, name: &str, params: Value) -> anyhow::Result<ToolResult> {
        let browser = self.webmcp_browser().await?;
        let endpoint = self.endpoint.as_deref().unwrap_or_default();
        let session_handle = browser.new_session().await?;
        let name = serde_json::to_string(name)?;
        let params = serde_json::to_string(&params)?;
        let expression = format!(
            "(async function(){{if(!document.modelContext){{throw new Error('WebMCP is not exposed by this page')}}return await document.modelContext.executeTool({name}, {params});}})()"
        );
        let mut session = session_handle.write().await;
        if let Err(error) = session.navigate(endpoint).await {
            drop(session);
            close_webmcp_session(&browser, &session_handle).await;
            return Err(error.into());
        }
        let result = session.evaluate_js_with_await(&expression, true).await;
        drop(session);
        close_webmcp_session(&browser, &session_handle).await;
        let result = result?;
        if let Some(error) = result.exception {
            return Err(anyhow!("WebMCP tool execution failed: {error}"));
        }
        parse_tool_result(result.value.unwrap_or(Value::Null))
    }
}

pub struct McpToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

fn json_rpc_request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

async fn close_webmcp_session(
    browser: &Browser,
    session: &Arc<tokio::sync::RwLock<oxibrowser_core::Session>>,
) {
    let mut session = session.write().await;
    let _ = session.close().await;
    browser.cleanup_closed_sessions();
}

async fn write_json_line<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    value: &Value,
) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_json_response<R: AsyncRead + Unpin>(
    lines: &mut tokio::io::Lines<BufReader<R>>,
    expected_id: u64,
) -> anyhow::Result<Value> {
    let read = async {
        while let Some(line) = lines.next_line().await? {
            let value: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value.get("id").and_then(Value::as_u64) != Some(expected_id) {
                continue;
            }
            return json_rpc_result(value);
        }
        Err(anyhow!("MCP server closed stdout without a response"))
    };
    tokio::time::timeout(MCP_REQUEST_TIMEOUT, read)
        .await
        .context("MCP stdio request timed out")?
}

fn json_rpc_result(value: Value) -> anyhow::Result<Value> {
    if let Some(error) = value.get("error") {
        return Err(anyhow!("MCP JSON-RPC error: {error}"));
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

fn parse_http_json(body: &str) -> anyhow::Result<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        return json_rpc_result(value);
    }
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(value) = serde_json::from_str::<Value>(data.trim()) {
                return json_rpc_result(value);
            }
        }
    }
    Err(anyhow!(
        "MCP response was neither JSON-RPC JSON nor an SSE data event"
    ))
}

fn parse_tools(value: Value) -> anyhow::Result<Vec<McpToolDef>> {
    let tools = value
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| value.as_array().cloned())
        .ok_or_else(|| anyhow!("MCP tools/list response has no tools array"))?;
    tools
        .into_iter()
        .map(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| anyhow!("MCP tool has no name"))?
                .to_owned();
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("MCP tool")
                .to_owned();
            let input_schema = tool
                .get("inputSchema")
                .or_else(|| tool.get("input_schema"))
                .cloned()
                .map(|schema| {
                    if let Value::String(schema) = &schema {
                        serde_json::from_str(schema).unwrap_or_else(|_| json!({ "type": "object" }))
                    } else {
                        schema
                    }
                })
                .unwrap_or_else(|| json!({ "type": "object" }));
            Ok(McpToolDef {
                name,
                description,
                input_schema,
            })
        })
        .collect()
}

fn parse_tool_result(value: Value) -> anyhow::Result<ToolResult> {
    let is_error = value
        .get("isError")
        .or_else(|| value.get("is_error"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let structured_content = value
        .get("structuredContent")
        .or_else(|| value.get("structured_content"))
        .cloned();
    let content = value.get("content").unwrap_or(&value);
    let mut text = Vec::new();
    let mut images = Vec::new();

    if let Some(items) = content.as_array() {
        for item in items {
            parse_tool_content_item(item, &mut text, &mut images);
        }
    } else if let Some(value) = content.as_str() {
        text.push(value.to_owned());
    } else if !content.is_null() {
        text.push(content.to_string());
    }

    if text.is_empty() {
        if let Some(structured_content) = &structured_content {
            text.push(structured_content.to_string());
        } else if !images.is_empty() {
            text.push(format!(
                "Tool returned {} image{}.",
                images.len(),
                if images.len() == 1 { "" } else { "s" }
            ));
        } else if !content.is_null() {
            text.push(content.to_string());
        }
    }

    let message = text.join("\n");
    let mut result = if is_error {
        ToolResult::error(message)
    } else {
        ToolResult::ok(message)
    };
    result.images = images;
    if let Some(structured_content) = structured_content {
        result = result.with_metadata(json!({ "structured_content": structured_content }));
    }
    Ok(result)
}

fn parse_tool_content_item(
    item: &Value,
    text: &mut Vec<String>,
    images: &mut Vec<crate::provider::types::ImageSource>,
) {
    match item.get("type").and_then(Value::as_str) {
        Some("text") => {
            if let Some(value) = item.get("text").and_then(Value::as_str) {
                text.push(value.to_owned());
            }
        }
        Some("image") => push_image(item, text, images),
        Some("resource") => parse_embedded_resource(item, text, images),
        Some("resource_link") => {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("resource");
            let uri = item.get("uri").and_then(Value::as_str).unwrap_or_default();
            text.push(format!("Resource: {name} ({uri})"));
        }
        Some("audio") => {
            let media_type = item
                .get("mimeType")
                .or_else(|| item.get("mime_type"))
                .and_then(Value::as_str)
                .unwrap_or("audio");
            text.push(format!("Audio output returned ({media_type})."));
        }
        _ => {
            if let Some(value) = item.get("text").and_then(Value::as_str) {
                text.push(value.to_owned());
            }
        }
    }
}

fn parse_embedded_resource(
    item: &Value,
    text: &mut Vec<String>,
    images: &mut Vec<crate::provider::types::ImageSource>,
) {
    let resource = item.get("resource").unwrap_or(item);
    if let Some(value) = resource.get("text").and_then(Value::as_str) {
        text.push(value.to_owned());
        return;
    }
    if resource.get("blob").is_some() {
        let image_count = images.len();
        push_image(resource, text, images);
        if images.len() == image_count {
            let uri = resource
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or("embedded resource");
            text.push(format!("Binary resource returned: {uri}"));
        }
    }
}

fn push_image(
    item: &Value,
    text: &mut Vec<String>,
    images: &mut Vec<crate::provider::types::ImageSource>,
) {
    let media_type = item
        .get("mimeType")
        .or_else(|| item.get("mime_type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let data = item
        .get("data")
        .or_else(|| item.get("blob"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let decoded_image = (!data.is_empty())
        .then(|| base64::engine::general_purpose::STANDARD.decode(data))
        .transpose()
        .ok()
        .flatten();
    if is_supported_image_type(media_type)
        && images.len() < MAX_TOOL_RESULT_IMAGES
        && decoded_image
            .as_ref()
            .is_some_and(|bytes| bytes.len() <= MAX_TOOL_RESULT_IMAGE_BYTES)
    {
        images.push(crate::provider::types::ImageSource {
            media_type: normalize_image_type(media_type),
            data: data.to_owned(),
        });
    } else {
        text.push(format!(
            "Image output omitted because it was invalid, too large, or unsupported ({}).",
            if media_type.is_empty() {
                "missing media type"
            } else {
                media_type
            }
        ));
    }
}

fn is_supported_image_type(media_type: &str) -> bool {
    matches!(
        media_type.to_ascii_lowercase().as_str(),
        "image/jpeg" | "image/jpg" | "image/png" | "image/gif" | "image/webp"
    )
}

fn normalize_image_type(media_type: &str) -> String {
    if media_type.eq_ignore_ascii_case("image/jpg") {
        "image/jpeg".into()
    } else {
        media_type.to_ascii_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_mcp_yaml_keeps_transport_and_auth_separate() {
        let config: ProjectMcpConfig = serde_yaml::from_str(
            r#"
mcpServers:
  browser:
    transport: web_mcp
    url: https://example.test
    auth:
      type: none
  private-api:
    transport: streamable_http
    url: https://mcp.example.test
    auth:
      type: oauth
      credential_ref: mcp:private-api
      authorization_server: https://auth.example.test
      scopes: [read]
"#,
        )
        .unwrap();

        assert_eq!(config.servers["browser"].transport, McpTransport::WebMcp);
        assert_eq!(config.servers["private-api"].auth.kind, McpAuthType::OAuth);
        assert_eq!(
            config.servers["private-api"].auth.credential_ref.as_deref(),
            Some("mcp:private-api")
        );
    }

    #[test]
    fn project_mcp_server_rejects_missing_transport_fields() {
        let server = ProjectMcpServer {
            transport: McpTransport::WebMcp,
            ..Default::default()
        };
        assert!(server.validate().is_err());
    }

    #[test]
    fn mcp_tool_results_preserve_text_images_and_structured_content() {
        let result = parse_tool_result(json!({
            "content": [
                { "type": "text", "text": "Rendered chart" },
                { "type": "image", "mimeType": "image/png", "data": "aW1hZ2U=" }
            ],
            "structuredContent": { "width": 640, "height": 480 }
        }))
        .unwrap();

        assert!(result.success);
        assert_eq!(result.content, "Rendered chart");
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].media_type, "image/png");
        assert_eq!(result.images[0].data, "aW1hZ2U=");
        assert_eq!(result.metadata.unwrap()["structured_content"]["width"], 640);
    }

    #[test]
    fn mcp_embedded_image_resources_are_multimodal() {
        let result = parse_tool_result(json!({
            "content": [{
                "type": "resource",
                "resource": {
                    "uri": "file:///chart.webp",
                    "mimeType": "image/webp",
                    "blob": "aW1hZ2U="
                }
            }]
        }))
        .unwrap();

        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].media_type, "image/webp");
        assert_eq!(result.content, "Tool returned 1 image.");
    }
}

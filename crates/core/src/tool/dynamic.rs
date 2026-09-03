use crate::integrations::mcp::McpClient;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::redirect::Policy;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::tool::builtin::web_browser::validate_url;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_COMMAND_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 2 * 1024 * 1024;
const OUTPUT_TRUNCATION_MARKER: &str = "\n[output truncated]";

pub struct DynamicToolConfig {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub handler: DynamicToolHandler,
}

pub enum DynamicToolHandler {
    ShellCommand {
        command_template: String,
    },
    HttpRequest {
        url: String,
        method: String,
        headers: HashMap<String, String>,
    },
    Inline(Arc<dyn Fn(&Value) -> String + Send + Sync>),
    MCP {
        client: Arc<McpClient>,
        tool_name: String,
    },
}

pub struct DynamicTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub handler: DynamicToolHandler,
}

impl DynamicTool {
    pub fn new(config: DynamicToolConfig) -> Self {
        Self {
            name: config.name,
            description: config.description,
            parameters: config.parameters,
            handler: config.handler,
        }
    }
}

#[async_trait]
impl Tool for DynamicTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        match &self.handler {
            DynamicToolHandler::ShellCommand { command_template } => {
                let command = interpolate_shell(command_template, params);
                let output =
                    run_shell_command(&command, &ctx.current_dir(), self.name.as_str()).await?;
                let stdout = output.stdout;
                let stderr = output.stderr;
                let combined = if stderr.is_empty() {
                    stdout.clone()
                } else if stdout.is_empty() {
                    stderr.clone()
                } else {
                    format!("{stdout}\n{stderr}")
                };
                let (combined, truncated) = bound_output(combined, MAX_COMMAND_OUTPUT_BYTES);

                let exit_code = output.exit_code;
                if exit_code != 0 {
                    Ok(
                        ToolResult::error(combined).with_metadata(serde_json::json!({
                            "exit_code": exit_code,
                            "timed_out": output.timed_out,
                            "output_truncated": truncated,
                        })),
                    )
                } else {
                    Ok(ToolResult::ok(combined).with_metadata(serde_json::json!({
                        "exit_code": exit_code,
                        "timed_out": output.timed_out,
                        "output_truncated": truncated,
                    })))
                }
            }
            DynamicToolHandler::HttpRequest {
                url,
                method,
                headers,
            } => {
                let interpolated_url = interpolate(url, params);
                let url = validate_url(self.name.as_str(), &interpolated_url)?;
                let redirect_filter = oxibrowser_core::network::IpFilter::block_private();
                let client = reqwest::Client::builder()
                    .redirect(Policy::custom(move |attempt| {
                        if redirect_filter
                            .is_hostname_allowed(attempt.url().host_str().unwrap_or_default())
                        {
                            attempt.follow()
                        } else {
                            attempt.stop()
                        }
                    }))
                    .connect_timeout(HTTP_CONNECT_TIMEOUT)
                    .timeout(HTTP_TIMEOUT)
                    .build()
                    .map_err(|error| ToolError::Execution {
                        tool: self.name.clone(),
                        message: format!("Could not configure HTTP client: {error}"),
                    })?;
                let mut request = match method.to_ascii_uppercase().as_str() {
                    "GET" => client.get(&url),
                    "HEAD" => client.head(&url),
                    "OPTIONS" => client.request(reqwest::Method::OPTIONS, &url),
                    "POST" => client.post(&url),
                    "PUT" => client.put(&url),
                    "DELETE" => client.delete(&url),
                    "PATCH" => client.patch(&url),
                    _ => {
                        return Err(ToolError::InvalidParams {
                            tool: self.name.clone(),
                            message: format!("Unsupported HTTP method: {method}"),
                        });
                    }
                };

                for (key, value) in headers {
                    request = request.header(key, value);
                }

                let response = request.send().await.map_err(|e| ToolError::Execution {
                    tool: self.name.clone(),
                    message: format!("HTTP request failed: {e}"),
                })?;

                let status = response.status();
                let (body, truncated) =
                    read_bounded_body(response)
                        .await
                        .map_err(|error| ToolError::Execution {
                            tool: self.name.clone(),
                            message: format!("Failed reading HTTP response: {error}"),
                        })?;

                if status.is_success() {
                    Ok(ToolResult::ok(body).with_metadata(serde_json::json!({
                        "status": status.as_u16(),
                        "output_truncated": truncated,
                    })))
                } else {
                    Ok(ToolResult::error(body).with_metadata(serde_json::json!({
                        "status": status.as_u16(),
                        "output_truncated": truncated,
                    })))
                }
            }
            DynamicToolHandler::Inline(handler) => {
                let (result, truncated) = bound_output(handler(params), MAX_COMMAND_OUTPUT_BYTES);
                Ok(ToolResult::ok(result).with_metadata(serde_json::json!({
                    "output_truncated": truncated,
                })))
            }
            DynamicToolHandler::MCP { client, tool_name } => client
                .call_tool(tool_name, params.clone())
                .await
                .map_err(|error| ToolError::Execution {
                    tool: self.name.clone(),
                    message: format!("MCP tool call failed: {error}"),
                }),
        }
    }

    fn requires_confirmation_for(&self, _params: &Value) -> bool {
        match &self.handler {
            DynamicToolHandler::ShellCommand { .. } | DynamicToolHandler::MCP { .. } => true,
            DynamicToolHandler::HttpRequest { method, .. } => !matches!(
                method.to_ascii_uppercase().as_str(),
                "GET" | "HEAD" | "OPTIONS"
            ),
            DynamicToolHandler::Inline(_) => false,
        }
    }
}

struct ShellOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
}

async fn run_shell_command(
    command: &str,
    current_dir: &std::path::Path,
    tool: &str,
) -> Result<ShellOutput> {
    let mut child = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(current_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| ToolError::Execution {
            tool: tool.into(),
            message: format!("Failed to execute command: {error}"),
        })?;
    let stdout = child.stdout.take().ok_or_else(|| ToolError::Execution {
        tool: tool.into(),
        message: "Command stdout is unavailable".into(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| ToolError::Execution {
        tool: tool.into(),
        message: "Command stderr is unavailable".into(),
    })?;
    let stdout_task = tokio::spawn(read_command_stream(stdout));
    let stderr_task = tokio::spawn(read_command_stream(stderr));
    let status = match tokio::time::timeout(COMMAND_TIMEOUT, child.wait()).await {
        Ok(result) => result.map_err(|error| ToolError::Execution {
            tool: tool.into(),
            message: format!("Failed waiting for command: {error}"),
        })?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Ok(ShellOutput {
                stdout: String::new(),
                stderr: format!(
                    "Command timed out after {} seconds.",
                    COMMAND_TIMEOUT.as_secs()
                ),
                exit_code: -1,
                timed_out: true,
            });
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|error| ToolError::Execution {
            tool: tool.into(),
            message: format!("Failed reading command stdout: {error}"),
        })?
        .map_err(|error| ToolError::Execution {
            tool: tool.into(),
            message: format!("Failed reading command stdout: {error}"),
        })?;
    let stderr = stderr_task
        .await
        .map_err(|error| ToolError::Execution {
            tool: tool.into(),
            message: format!("Failed reading command stderr: {error}"),
        })?
        .map_err(|error| ToolError::Execution {
            tool: tool.into(),
            message: format!("Failed reading command stderr: {error}"),
        })?;
    Ok(ShellOutput {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code: status.code().unwrap_or(-1),
        timed_out: false,
    })
}

async fn read_command_stream<R>(mut stream: R) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut output = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(output);
        }
        output.extend_from_slice(&chunk[..read]);
        if output.len() > MAX_COMMAND_OUTPUT_BYTES {
            output.truncate(MAX_COMMAND_OUTPUT_BYTES + 1);
        }
    }
}

async fn read_bounded_body(response: reqwest::Response) -> std::io::Result<(String, bool)> {
    let mut body = Vec::new();
    let mut truncated = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(std::io::Error::other)?;
        let remaining = MAX_HTTP_BODY_BYTES.saturating_sub(body.len());
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    Ok((
        if truncated {
            format!("{body}{OUTPUT_TRUNCATION_MARKER}")
        } else {
            body
        },
        truncated,
    ))
}

fn bound_output(mut output: String, max_bytes: usize) -> (String, bool) {
    if output.len() <= max_bytes {
        return (output, false);
    }
    let marker = OUTPUT_TRUNCATION_MARKER;
    if max_bytes <= marker.len() {
        let mut marker = marker.to_owned();
        truncate_to_utf8_boundary(&mut marker, max_bytes);
        return (marker, true);
    }
    let max_content = max_bytes.saturating_sub(marker.len());
    truncate_to_utf8_boundary(&mut output, max_content);
    output.push_str(marker);
    (output, true)
}

fn truncate_to_utf8_boundary(value: &mut String, max_bytes: usize) {
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

pub fn interpolate(template: &str, params: &Value) -> String {
    let mut result = template.to_string();
    if let Value::Object(map) = params {
        for (key, value) in map {
            let placeholder = format!("${{{}}}", key);
            let replacement = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }
    }
    result
}

fn interpolate_shell(template: &str, params: &Value) -> String {
    let mut result = template.to_string();
    if let Value::Object(map) = params {
        for (key, value) in map {
            let placeholder = format!("${{{}}}", key);
            let value = match value {
                Value::String(value) => value.clone(),
                other => other.to_string(),
            };
            let replacement = format!("'{}'", value.replace('\'', "'\\''"));
            result = result.replace(&placeholder, &replacement);
        }
    }
    result
}

pub fn register_dynamic_tools(
    registry: &crate::tool::ToolRegistry,
    configs: Vec<DynamicToolConfig>,
) {
    for config in configs {
        registry.register(DynamicTool::new(config));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolActivation;
    use serde_json::json;
    use std::{path::Path, sync::Arc};
    use tokio::io::AsyncReadExt;

    fn context(root: &Path) -> ToolContext {
        ToolContext {
            working_dir: root.to_path_buf(),
            session_id: "session".into(),
            agent_id: "agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    #[test]
    fn test_interpolate_simple() {
        let template = "echo ${name}";
        let params = json!({"name": "hello"});
        let result = interpolate(template, &params);
        assert_eq!(result, "echo hello");
    }

    #[test]
    fn test_interpolate_multiple() {
        let template = "${greeting} ${name}!";
        let params = json!({"greeting": "Hello", "name": "World"});
        let result = interpolate(template, &params);
        assert_eq!(result, "Hello World!");
    }

    #[test]
    fn test_interpolate_no_match() {
        let template = "echo hello";
        let params = json!({"other": "value"});
        let result = interpolate(template, &params);
        assert_eq!(result, "echo hello");
    }

    #[test]
    fn test_interpolate_empty_params() {
        let template = "${name}";
        let params = json!({});
        let result = interpolate(template, &params);
        assert_eq!(result, "${name}");
    }

    #[test]
    fn shell_interpolation_quotes_command_metacharacters() {
        let template = "printf '%s' ${value}";
        let params = json!({"value": "$(touch /tmp/pwned); echo unsafe"});

        assert_eq!(
            interpolate_shell(template, &params),
            "printf '%s' '$(touch /tmp/pwned); echo unsafe'"
        );
    }

    #[tokio::test]
    async fn shell_commands_use_the_conversation_current_directory() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let context = context(directory.path());
        context.set_current_dir(nested.canonicalize().unwrap());
        let tool = DynamicTool::new(DynamicToolConfig {
            name: "pwd".into(),
            description: "Print the current directory".into(),
            parameters: json!({ "type": "object" }),
            handler: DynamicToolHandler::ShellCommand {
                command_template: "pwd".into(),
            },
        });

        let result = tool.execute(&context, &json!({})).await.unwrap();

        assert_eq!(
            result.content.trim(),
            nested.canonicalize().unwrap().display().to_string()
        );
    }

    #[test]
    fn mutating_dynamic_handlers_require_confirmation() {
        let shell = DynamicTool::new(DynamicToolConfig {
            name: "shell".into(),
            description: String::new(),
            parameters: json!({ "type": "object" }),
            handler: DynamicToolHandler::ShellCommand {
                command_template: "echo ok".into(),
            },
        });
        let get = DynamicTool::new(DynamicToolConfig {
            name: "get".into(),
            description: String::new(),
            parameters: json!({ "type": "object" }),
            handler: DynamicToolHandler::HttpRequest {
                url: "https://example.com".into(),
                method: "GET".into(),
                headers: HashMap::new(),
            },
        });
        let post = DynamicTool::new(DynamicToolConfig {
            name: "post".into(),
            description: String::new(),
            parameters: json!({ "type": "object" }),
            handler: DynamicToolHandler::HttpRequest {
                url: "https://example.com".into(),
                method: "POST".into(),
                headers: HashMap::new(),
            },
        });

        assert!(shell.requires_confirmation_for(&json!({})));
        assert!(!get.requires_confirmation_for(&json!({})));
        assert!(post.requires_confirmation_for(&json!({})));

        let head = DynamicTool::new(DynamicToolConfig {
            name: "head".into(),
            description: String::new(),
            parameters: json!({ "type": "object" }),
            handler: DynamicToolHandler::HttpRequest {
                url: "https://example.com".into(),
                method: "HEAD".into(),
                headers: HashMap::new(),
            },
        });
        let options = DynamicTool::new(DynamicToolConfig {
            name: "options".into(),
            description: String::new(),
            parameters: json!({ "type": "object" }),
            handler: DynamicToolHandler::HttpRequest {
                url: "https://example.com".into(),
                method: "OPTIONS".into(),
                headers: HashMap::new(),
            },
        });

        assert!(!head.requires_confirmation_for(&json!({})));
        assert!(!options.requires_confirmation_for(&json!({})));
    }

    #[tokio::test]
    async fn http_handlers_reject_private_hosts_before_connecting() {
        let tool = DynamicTool::new(DynamicToolConfig {
            name: "private_http".into(),
            description: String::new(),
            parameters: json!({ "type": "object" }),
            handler: DynamicToolHandler::HttpRequest {
                url: "http://127.0.0.1:9".into(),
                method: "GET".into(),
                headers: HashMap::new(),
            },
        });

        assert!(tool
            .execute(&context(Path::new("/tmp")), &json!({}))
            .await
            .is_err());
    }

    #[test]
    fn output_bound_preserves_utf8_and_marks_truncation() {
        let (output, truncated) = bound_output("é".repeat(100), 32);

        assert!(truncated);
        assert!(output.is_char_boundary(output.len()));
        assert!(output.ends_with(OUTPUT_TRUNCATION_MARKER));
        assert!(output.len() <= 32);
    }

    #[tokio::test]
    async fn command_stream_keeps_an_extra_byte_to_detect_truncation() {
        let stream = tokio::io::repeat(b'x').take((MAX_COMMAND_OUTPUT_BYTES + 1) as u64);
        let output = read_command_stream(stream).await.unwrap();

        assert_eq!(output.len(), MAX_COMMAND_OUTPUT_BYTES + 1);
    }
}

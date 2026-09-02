use crate::integrations::mcp::McpClient;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

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
                let command = interpolate(command_template, params);
                let output = tokio::process::Command::new("bash")
                    .arg("-c")
                    .arg(&command)
                    .current_dir(ctx.current_dir())
                    .output()
                    .await
                    .map_err(|e| ToolError::Execution {
                        tool: self.name.clone(),
                        message: format!("Failed to execute command: {e}"),
                    })?;

                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let combined = if stderr.is_empty() {
                    stdout.clone()
                } else if stdout.is_empty() {
                    stderr.clone()
                } else {
                    format!("{stdout}\n{stderr}")
                };

                let exit_code = output.status.code().unwrap_or(-1);
                if exit_code != 0 {
                    Ok(ToolResult::error(combined)
                        .with_metadata(serde_json::json!({ "exit_code": exit_code })))
                } else {
                    Ok(ToolResult::ok(combined)
                        .with_metadata(serde_json::json!({ "exit_code": exit_code })))
                }
            }
            DynamicToolHandler::HttpRequest {
                url,
                method,
                headers,
            } => {
                let interpolated_url = interpolate(url, params);
                let client = reqwest::Client::new();
                let mut request = match method.to_uppercase().as_str() {
                    "GET" => client.get(&interpolated_url),
                    "POST" => client.post(&interpolated_url),
                    "PUT" => client.put(&interpolated_url),
                    "DELETE" => client.delete(&interpolated_url),
                    "PATCH" => client.patch(&interpolated_url),
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
                let body = response.text().await.unwrap_or_default();

                if status.is_success() {
                    Ok(ToolResult::ok(body)
                        .with_metadata(serde_json::json!({ "status": status.as_u16() })))
                } else {
                    Ok(ToolResult::error(body)
                        .with_metadata(serde_json::json!({ "status": status.as_u16() })))
                }
            }
            DynamicToolHandler::Inline(handler) => {
                let result = handler(params);
                Ok(ToolResult::ok(result))
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

        assert_eq!(result.content.trim(), nested.display().to_string());
    }
}

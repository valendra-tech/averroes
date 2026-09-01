use super::shell_session::{default_input_wait, ShellSessionManager};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

#[derive(Default)]
pub struct BashTool {
    sessions: ShellSessionManager,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run commands in a persistent non-interactive bash session backed by pipes. State such as cd and exports is retained between calls. Terminal-dependent programs, pagers, SSH shells, and interactive REPLs are not supported. Use detach:true only for commands that can run without a terminal."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Command to run. Omit this when only sending input or reading an existing session."
                },
                "input": {
                    "type": "string",
                    "description": "Raw input for a process waiting on stdin. A newline is added for ordinary text; terminal interaction is not supported."
                },
                "session": {
                    "type": "string",
                    "description": "Optional session name. Sessions are isolated by conversation and workspace."
                },
                "detach": {
                    "type": "boolean",
                    "description": "Start the command and return while it remains attached to the persistent shell pipes."
                },
                "wait_ms": {
                    "type": "integer",
                    "description": "How long to collect output for detached commands or input-only calls, in milliseconds."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Optional timeout in milliseconds"
                },
                "close": {
                    "type": "boolean",
                    "description": "Close this persistent shell session."
                }
            },
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let session_name = params
            .get("session")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("default");

        if params
            .get("close")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let closed = self
                .sessions
                .close(&ctx.session_id, session_name, &ctx.working_dir)
                .map_err(|error| ToolError::Execution {
                    tool: self.name().into(),
                    message: error.to_string(),
                })?;
            return Ok(ToolResult::ok(if closed {
                format!("Closed shell session '{session_name}'.")
            } else {
                format!("Shell session '{session_name}' was not open.")
            })
            .with_metadata(json!({
                "session": session_name,
                "closed": closed,
                "interactive": false,
                "transport": "pipes"
            })));
        }

        let command = params.get("command").and_then(Value::as_str);
        let input = params.get("input").and_then(Value::as_str);
        if command.is_none_or(|value| value.trim().is_empty()) && input.is_none() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "Provide command, input, or close".into(),
            });
        }

        let session = self
            .sessions
            .get_or_create(&ctx.session_id, session_name, &ctx.working_dir)
            .await
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let output = if let Some(command) = command.filter(|value| !value.trim().is_empty()) {
            if params
                .get("detach")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                session
                    .start_detached(
                        command,
                        default_input_wait(params.get("wait_ms").and_then(Value::as_u64)),
                    )
                    .await
            } else {
                let timeout = params
                    .get("timeout")
                    .and_then(Value::as_u64)
                    .map(Duration::from_millis)
                    .unwrap_or_else(|| Duration::from_secs(120));
                session.run_command(command, timeout).await
            }
        } else if let Some(input) = input {
            session
                .send_input(
                    input,
                    default_input_wait(params.get("wait_ms").and_then(Value::as_u64)),
                )
                .await
        } else {
            session
                .read_output(default_input_wait(
                    params.get("wait_ms").and_then(Value::as_u64),
                ))
                .await
        }
        .map_err(|error| ToolError::Execution {
            tool: self.name().into(),
            message: error.to_string(),
        })?;

        let metadata = json!({
            "session": session_name,
            "persistent": true,
            "interactive": false,
            "transport": "pipes",
            "pid": session.process_id(),
            "exit_code": output.exit_code,
            "timed_out": output.timed_out,
            "running": output.running,
            "working_dir": ctx.working_dir,
        });
        let content = if output.content.is_empty() {
            if output.running {
                format!("Shell session '{session_name}' is still running.")
            } else {
                String::from("Command completed with no output.")
            }
        } else {
            output.content
        };

        if output.exit_code.is_some_and(|code| code != 0) || output.timed_out {
            Ok(ToolResult::error(content).with_metadata(metadata))
        } else {
            Ok(ToolResult::ok(content).with_metadata(metadata))
        }
    }
}

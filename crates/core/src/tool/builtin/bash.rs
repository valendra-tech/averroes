use super::shell_session::{default_input_wait, ShellSessionManager};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const MIN_COMMAND_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Default)]
pub struct BashTool {
    sessions: ShellSessionManager,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BashParams {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    detach: bool,
    #[serde(default)]
    wait_ms: Option<u64>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    close: bool,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run commands in a persistent non-interactive session using the operating system user's shell and environment. New sessions start in the conversation's current directory; shell-local state such as cd and exports is retained between calls. Commands may access paths outside the active workspace. Terminal-dependent programs, pagers, SSH shells, and interactive REPLs are not supported. Use detach:true only for commands that can run without a terminal."
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
                    "minimum": 100,
                    "maximum": 120000,
                    "description": "How long to collect output for detached commands or input-only calls, in milliseconds."
                },
                "timeout": {
                    "type": "integer",
                    "minimum": 100,
                    "maximum": 600000,
                    "default": 120000,
                    "description": "Optional timeout in milliseconds for an attached command."
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
        let params: BashParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let current_dir = ctx.current_dir();
        let session_name = params
            .session
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("default");

        if params.close {
            let closed = self
                .sessions
                .close(&ctx.session_id, session_name, &current_dir)
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

        let command = params.command.as_deref();
        let input = params.input.as_deref();
        if command.is_none_or(|value| value.trim().is_empty()) && input.is_none() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "Provide command, input, or close".into(),
            });
        }

        let session = self
            .sessions
            .get_or_create(&ctx.session_id, session_name, &current_dir)
            .await
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let output = if let Some(command) = command.filter(|value| !value.trim().is_empty()) {
            if params.detach {
                session
                    .start_detached(command, default_input_wait(params.wait_ms))
                    .await
            } else {
                session
                    .run_command(command, command_timeout(params.timeout))
                    .await
            }
        } else if let Some(input) = input {
            session
                .send_input(input, default_input_wait(params.wait_ms))
                .await
        } else {
            session
                .read_output(default_input_wait(params.wait_ms))
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
            "working_dir": current_dir,
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

fn command_timeout(value: Option<u64>) -> Duration {
    value
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_COMMAND_TIMEOUT)
        .clamp(MIN_COMMAND_TIMEOUT, MAX_COMMAND_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_timeout_is_bounded() {
        assert_eq!(command_timeout(None), DEFAULT_COMMAND_TIMEOUT);
        assert_eq!(command_timeout(Some(0)), MIN_COMMAND_TIMEOUT);
        assert_eq!(command_timeout(Some(u64::MAX)), MAX_COMMAND_TIMEOUT);
    }

    #[test]
    fn parameters_reject_unknown_fields() {
        let result = serde_json::from_value::<BashParams>(json!({
            "command": "pwd",
            "unexpected": true
        }));

        assert!(result.is_err());
    }
}

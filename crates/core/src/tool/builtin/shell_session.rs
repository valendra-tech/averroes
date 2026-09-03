//! Persistent non-interactive shell sessions used by the `bash` tool.
//!
//! A persistent shell keeps useful state such as `cd` and exports between
//! calls without allocating a terminal. Interactive terminal programs,
//! pagers, and SSH shells are intentionally out of scope for this transport.
//! The process lives only for the lifetime of Averroes and is cleaned up when
//! it has been idle for a while or when its session is explicitly closed.

use anyhow::{anyhow, Context as _};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex as AsyncMutex, OnceCell};

const READY_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_INPUT_WAIT: Duration = Duration::from_millis(800);
const OUTPUT_QUIET_PERIOD: Duration = Duration::from_millis(250);
const MAX_IDLE: Duration = Duration::from_secs(30 * 60);
const MAX_SHELL_BUFFER_BYTES: usize = 128 * 1024;
const OUTPUT_TRUNCATION_MARKER: &[u8] = b"\n...[shell output truncated]...\n";

fn spawn_output_reader<R>(mut reader: R, sender: mpsc::Sender<Vec<u8>>, stream: &'static str)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(bytes) => {
                    if sender.send(buffer[..bytes].to_vec()).await.is_err() {
                        break;
                    }
                }
                Err(error) => {
                    tracing::debug!(error = %error, stream, "persistent shell pipe reader stopped");
                    break;
                }
            }
        }
    });
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct ShellSessionKey {
    conversation_id: String,
    session_name: String,
    working_dir: PathBuf,
}

impl ShellSessionKey {
    pub(crate) fn new(conversation_id: &str, session_name: &str, working_dir: &Path) -> Self {
        Self {
            conversation_id: conversation_id.to_owned(),
            session_name: session_name.to_owned(),
            working_dir: working_dir.to_path_buf(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct CommandOutput {
    pub(crate) content: String,
    pub(crate) exit_code: Option<i32>,
    pub(crate) timed_out: bool,
    pub(crate) running: bool,
}

pub(crate) struct ShellSessionManager {
    sessions: Mutex<HashMap<ShellSessionKey, Arc<ShellSession>>>,
}

impl Default for ShellSessionManager {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

impl ShellSessionManager {
    pub(crate) async fn get_or_create(
        &self,
        conversation_id: &str,
        session_name: &str,
        working_dir: &Path,
    ) -> anyhow::Result<Arc<ShellSession>> {
        self.reap_idle();
        let key = ShellSessionKey::new(conversation_id, session_name, working_dir);

        let existing = {
            let sessions = self
                .sessions
                .lock()
                .map_err(|_| anyhow!("shell session registry is poisoned"))?;
            sessions.get(&key).cloned()
        };
        if let Some(session) = existing {
            if session.is_alive() {
                session.ensure_ready().await?;
                return Ok(session);
            }
            self.sessions
                .lock()
                .map_err(|_| anyhow!("shell session registry is poisoned"))?
                .remove(&key);
        }

        let session = Arc::new(ShellSession::spawn(working_dir)?);
        session.ensure_ready().await?;

        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| anyhow!("shell session registry is poisoned"))?;
        if let Some(existing) = sessions.get(&key).cloned() {
            if existing.is_alive() {
                return Ok(existing);
            }
        }
        sessions.insert(key, session.clone());
        Ok(session)
    }

    pub(crate) fn close(
        &self,
        conversation_id: &str,
        session_name: &str,
        working_dir: &Path,
    ) -> anyhow::Result<bool> {
        let key = ShellSessionKey::new(conversation_id, session_name, working_dir);
        Ok(self
            .sessions
            .lock()
            .map_err(|_| anyhow!("shell session registry is poisoned"))?
            .remove(&key)
            .is_some())
    }

    fn reap_idle(&self) {
        let stale = self
            .sessions
            .lock()
            .ok()
            .map(|sessions| {
                sessions
                    .iter()
                    .filter_map(|(key, session)| {
                        (session.is_idle(MAX_IDLE) && !session.is_busy()).then(|| key.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if let Ok(mut sessions) = self.sessions.lock() {
            for key in stale {
                sessions.remove(&key);
            }
        }
    }
}

pub(crate) struct ShellSession {
    writer: AsyncMutex<ChildStdin>,
    receiver: AsyncMutex<mpsc::Receiver<Vec<u8>>>,
    pending: AsyncMutex<Vec<u8>>,
    command_lock: AsyncMutex<()>,
    child: Mutex<Child>,
    ready: OnceCell<()>,
    ready_marker: String,
    alive: AtomicBool,
    last_used: Mutex<Instant>,
}

impl ShellSession {
    fn spawn(working_dir: &Path) -> anyhow::Result<Self> {
        let mut command = Command::new("bash");
        command.arg("--noprofile");
        command.arg("--norc");
        // `-s` keeps bash reading commands from stdin while preserving shell
        // state between calls. Pipes deliberately prevent child processes
        // from believing they have a terminal.
        command.arg("-s");
        command.current_dir(working_dir);
        // Keep agent output deterministic and prevent CLI programs from
        // opening a pager or asking an interactive prompt on the pipe.
        command.env("TERM", "dumb");
        command.env("COLORTERM", "");
        command.env("CLICOLOR", "0");
        command.env("PAGER", "cat");
        command.env("GH_PAGER", "cat");
        command.env("GIT_PAGER", "cat");
        command.env("GH_PROMPT_DISABLED", "1");
        command.env("CI", "1");
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.kill_on_drop(true);

        let mut child = command
            .spawn()
            .context("failed to start non-interactive bash session")?;
        let writer = child
            .stdin
            .take()
            .context("failed to open shell stdin pipe")?;
        let stdout = child
            .stdout
            .take()
            .context("failed to open shell stdout pipe")?;
        let stderr = child
            .stderr
            .take()
            .context("failed to open shell stderr pipe")?;

        let (sender, receiver) = mpsc::channel(128);
        spawn_output_reader(stdout, sender.clone(), "stdout");
        spawn_output_reader(stderr, sender, "stderr");

        Ok(Self {
            writer: AsyncMutex::new(writer),
            receiver: AsyncMutex::new(receiver),
            pending: AsyncMutex::new(Vec::new()),
            command_lock: AsyncMutex::new(()),
            child: Mutex::new(child),
            ready: OnceCell::const_new(),
            ready_marker: format!("__AVERROES_SHELL_READY_{}__", uuid::Uuid::new_v4()),
            alive: AtomicBool::new(true),
            last_used: Mutex::new(Instant::now()),
        })
    }

    pub(crate) async fn run_command(
        &self,
        command: &str,
        timeout: Duration,
    ) -> anyhow::Result<CommandOutput> {
        self.ensure_ready().await?;
        let _lock = self.command_lock.lock().await;
        self.touch();

        let marker = format!("__AVERROES_SHELL_DONE_{}__:", uuid::Uuid::new_v4());
        let request = format!("{command}\nprintf '\\n{marker}%d\\n' \"$?\"\n");
        self.write(request.as_bytes()).await?;

        let marker_result = tokio::time::timeout(timeout, self.wait_for_exit_marker(&marker)).await;
        match marker_result {
            Ok(result) => {
                let (output, exit_code) = result?;
                Ok(CommandOutput {
                    content: clean_shell_output(&output),
                    exit_code: Some(exit_code),
                    timed_out: false,
                    running: false,
                })
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = timeout.as_millis(),
                    "non-interactive shell command timed out; terminating session"
                );
                self.terminate();
                Err(anyhow!(
                    "shell command timed out; non-interactive shell session terminated"
                ))
            }
        }
    }

    pub(crate) async fn start_detached(
        &self,
        command: &str,
        wait: Duration,
    ) -> anyhow::Result<CommandOutput> {
        self.ensure_ready().await?;
        let _lock = self.command_lock.lock().await;
        self.touch();
        self.write(format!("{command}\n").as_bytes()).await?;
        let content = self.read_until_quiet(wait).await?;
        Ok(CommandOutput {
            content: clean_shell_output(&content),
            exit_code: None,
            timed_out: false,
            running: self.is_alive(),
        })
    }

    pub(crate) async fn send_input(
        &self,
        input: &str,
        wait: Duration,
    ) -> anyhow::Result<CommandOutput> {
        self.ensure_ready().await?;
        let _lock = self.command_lock.lock().await;
        self.touch();

        let mut data = input.as_bytes().to_vec();
        if !data.ends_with(b"\n")
            && !data.ends_with(b"\r")
            && !data.iter().any(|byte| matches!(byte, 0x03 | 0x04))
        {
            data.push(b'\n');
        }
        self.write(&data).await?;
        let content = self.read_until_quiet(wait).await?;
        Ok(CommandOutput {
            content: clean_shell_output(&content),
            exit_code: None,
            timed_out: false,
            running: self.is_alive(),
        })
    }

    pub(crate) async fn read_output(&self, wait: Duration) -> anyhow::Result<CommandOutput> {
        self.ensure_ready().await?;
        let _lock = self.command_lock.lock().await;
        self.touch();
        let content = self.read_until_quiet(wait).await?;
        Ok(CommandOutput {
            content: clean_shell_output(&content),
            exit_code: None,
            timed_out: false,
            running: self.is_alive(),
        })
    }

    pub(crate) fn process_id(&self) -> Option<u32> {
        self.child.lock().ok()?.id()
    }

    async fn ensure_ready(&self) -> anyhow::Result<()> {
        self.ready
            .get_or_try_init(|| async {
                let _lock = self.command_lock.lock().await;
                self.write(format!("printf '\\n{}\\n'\n", self.ready_marker).as_bytes())
                    .await?;
                tokio::time::timeout(READY_TIMEOUT, self.wait_for_text_marker(&self.ready_marker))
                    .await
                    .context("shell process did not become ready")??;
                self.pending.lock().await.clear();
                Ok::<(), anyhow::Error>(())
            })
            .await
            .map(|_| ())
    }

    async fn write(&self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut writer = self.writer.lock().await;
        writer.write_all(bytes).await?;
        writer.flush().await?;
        Ok(())
    }

    async fn wait_for_text_marker(&self, marker: &str) -> anyhow::Result<String> {
        loop {
            if let Some(output) = self.take_text_marker(marker).await {
                return Ok(output);
            }
            let chunk = self.next_chunk().await?;
            append_bounded(&mut *self.pending.lock().await, &chunk);
        }
    }

    async fn wait_for_exit_marker(&self, marker: &str) -> anyhow::Result<(String, i32)> {
        loop {
            if let Some(result) = self.take_exit_marker(marker).await? {
                return Ok(result);
            }
            let chunk = self.next_chunk().await?;
            append_bounded(&mut *self.pending.lock().await, &chunk);
        }
    }

    async fn take_text_marker(&self, marker: &str) -> Option<String> {
        let mut pending = self.pending.lock().await;
        let position = find_subslice(&pending, marker.as_bytes())?;
        let after = position + marker.len();
        let end = pending[after..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| after + offset + 1)
            .unwrap_or(after);
        let output = pending[..position].to_vec();
        pending.drain(..end);
        Some(String::from_utf8_lossy(&output).into_owned())
    }

    async fn take_exit_marker(&self, marker: &str) -> anyhow::Result<Option<(String, i32)>> {
        let mut pending = self.pending.lock().await;
        let Some(position) = find_subslice(&pending, marker.as_bytes()) else {
            return Ok(None);
        };
        let mut search_from = position;
        loop {
            let Some(position) = find_subslice(&pending[search_from..], marker.as_bytes())
                .map(|offset| search_from + offset)
            else {
                return Ok(None);
            };
            let after = position + marker.len();
            let Some(offset) = pending[after..].iter().position(|byte| *byte == b'\n') else {
                return Ok(None);
            };
            let end = after + offset + 1;
            let status = String::from_utf8_lossy(&pending[after..after + offset])
                .trim()
                .parse::<i32>();
            if let Ok(status) = status {
                let output = pending[..position].to_vec();
                pending.drain(..end);
                return Ok(Some((
                    String::from_utf8_lossy(&output).into_owned(),
                    status,
                )));
            }
            // A shell may echo the printf protocol line before executing it.
            // Ignore that false marker and keep looking for the real one.
            search_from = end;
        }
    }

    async fn next_chunk(&self) -> anyhow::Result<Vec<u8>> {
        self.receiver.lock().await.recv().await.ok_or_else(|| {
            self.alive.store(false, Ordering::Release);
            anyhow!("shell process closed unexpectedly")
        })
    }

    async fn read_until_quiet(&self, wait: Duration) -> anyhow::Result<String> {
        let mut output = {
            let mut pending = self.pending.lock().await;
            std::mem::take(&mut *pending)
        };
        let started = Instant::now();
        let mut received_output = !output.is_empty();

        loop {
            let elapsed = started.elapsed();
            if elapsed >= wait {
                break;
            }
            let remaining = wait.saturating_sub(elapsed);
            let wait_for = if received_output {
                remaining.min(OUTPUT_QUIET_PERIOD)
            } else {
                remaining
            };

            match tokio::time::timeout(wait_for, self.receiver.lock().await.recv()).await {
                Ok(Some(chunk)) => {
                    received_output = true;
                    append_bounded(&mut output, &chunk);
                }
                Ok(None) => {
                    self.alive.store(false, Ordering::Release);
                    break;
                }
                Err(_) => break,
            }
        }

        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    fn touch(&self) {
        if let Ok(mut last_used) = self.last_used.lock() {
            *last_used = Instant::now();
        }
    }

    fn is_idle(&self, max_idle: Duration) -> bool {
        self.last_used
            .lock()
            .map(|last_used| last_used.elapsed() >= max_idle)
            .unwrap_or(false)
    }

    fn is_busy(&self) -> bool {
        self.command_lock.try_lock().is_err()
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}

fn append_bounded(output: &mut Vec<u8>, chunk: &[u8]) {
    output.extend_from_slice(chunk);
    if output.len() <= MAX_SHELL_BUFFER_BYTES {
        return;
    }

    let tail_bytes = MAX_SHELL_BUFFER_BYTES.saturating_sub(OUTPUT_TRUNCATION_MARKER.len());
    let head_bytes = tail_bytes / 2;
    let tail_start = output.len().saturating_sub(tail_bytes - head_bytes);
    let tail = output[tail_start..].to_vec();
    output.truncate(head_bytes);
    output.extend_from_slice(OUTPUT_TRUNCATION_MARKER);
    output.extend_from_slice(&tail);
}

impl Drop for ShellSession {
    fn drop(&mut self) {
        self.terminate();
    }
}

impl ShellSession {
    fn terminate(&self) {
        self.alive.store(false, Ordering::Release);
        if let Ok(mut child) = self.child.lock() {
            if let Err(error) = child.start_kill() {
                tracing::debug!(error = %error, "could not stop persistent shell child");
            }
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn clean_shell_output(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut escape = false;
    let mut csi = false;
    let mut osc = false;

    for character in raw.chars() {
        if osc {
            if character == '\x07' {
                osc = false;
            } else if character == '\x1b' {
                escape = true;
                osc = false;
            }
            continue;
        }
        if csi {
            if ('@'..='~').contains(&character) {
                csi = false;
            }
            continue;
        }
        if escape {
            if character == '[' {
                csi = true;
                escape = false;
            } else if character == ']' {
                osc = true;
                escape = false;
            } else if ('@'..='~').contains(&character) {
                escape = false;
            }
            continue;
        }
        match character {
            '\x1b' => escape = true,
            '\r' => {}
            '\0' => {}
            _ => output.push(character),
        }
    }

    output.trim_matches('\n').to_owned()
}

pub(crate) fn default_input_wait(value: Option<u64>) -> Duration {
    value
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_INPUT_WAIT)
        .clamp(Duration::from_millis(100), Duration::from_secs(120))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_terminal_control_sequences_without_removing_newlines() {
        assert_eq!(
            clean_shell_output("\x1b[32mhello\x1b[0m\r\nworld"),
            "hello\nworld"
        );
    }

    #[test]
    fn session_keys_isolate_conversations_and_directories() {
        let first = ShellSessionKey::new("one", "default", Path::new("/tmp/a"));
        let second = ShellSessionKey::new("two", "default", Path::new("/tmp/a"));
        let third = ShellSessionKey::new("one", "default", Path::new("/tmp/b"));
        assert_ne!(first, second);
        assert_ne!(first, third);
    }

    #[test]
    fn input_gets_a_newline_but_control_keys_do_not() {
        let mut normal = b"pwd".to_vec();
        if !normal.ends_with(b"\n")
            && !normal.ends_with(b"\r")
            && !normal.iter().any(|byte| matches!(byte, 0x03 | 0x04))
        {
            normal.push(b'\n');
        }
        assert_eq!(normal, b"pwd\n");
        assert_eq!(b"\x03".to_vec(), vec![0x03]);
    }

    #[test]
    fn shell_output_is_bounded_with_a_truncation_marker() {
        let mut output = Vec::new();
        append_bounded(&mut output, &vec![b'x'; MAX_SHELL_BUFFER_BYTES + 1]);

        assert_eq!(output.len(), MAX_SHELL_BUFFER_BYTES);
        assert!(output
            .windows(OUTPUT_TRUNCATION_MARKER.len())
            .any(|window| window == OUTPUT_TRUNCATION_MARKER));
    }

    #[tokio::test]
    async fn shell_commands_do_not_receive_a_terminal() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let manager = ShellSessionManager::default();
        let session = manager
            .get_or_create("conversation", "non-interactive", directory.path())
            .await
            .expect("non-interactive shell");

        let output = session
            .run_command(
                "if [ -t 0 ] || [ -t 1 ] || [ -t 2 ]; then printf 'tty'; else printf 'pipe'; fi",
                Duration::from_secs(5),
            )
            .await
            .expect("command completes");

        assert_eq!(output.content.trim(), "pipe");
    }

    #[tokio::test]
    async fn shell_commands_disable_interactive_pagers() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let manager = ShellSessionManager::default();
        let session = manager
            .get_or_create("conversation", "non-interactive", directory.path())
            .await
            .expect("non-interactive shell");

        let output = session
            .run_command(
                "printf '%s|%s|%s|%s' \"$PAGER\" \"$GH_PAGER\" \"$GIT_PAGER\" \"$GH_PROMPT_DISABLED\"",
                Duration::from_secs(5),
            )
            .await
            .expect("command completes");

        assert_eq!(output.content.trim(), "cat|cat|cat|1");
    }

    #[tokio::test]
    async fn timeout_terminates_the_session_before_the_next_command() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let manager = ShellSessionManager::default();
        let session = manager
            .get_or_create("conversation", "timeout", directory.path())
            .await
            .expect("non-interactive shell");

        let error = session
            .run_command("read value", Duration::from_millis(50))
            .await
            .expect_err("stdin wait should time out");

        assert!(error.to_string().contains("session terminated"));
        assert!(!session.is_alive());

        let replacement = manager
            .get_or_create("conversation", "timeout", directory.path())
            .await
            .expect("replacement shell");
        let output = replacement
            .run_command("printf recovered", Duration::from_secs(5))
            .await
            .expect("replacement command completes");

        assert_eq!(output.content.trim(), "recovered");
    }

    #[tokio::test]
    async fn reuses_shell_state_between_commands() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let manager = ShellSessionManager::default();
        let session = manager
            .get_or_create("conversation", "default", directory.path())
            .await
            .expect("persistent shell");

        let parent = directory
            .path()
            .canonicalize()
            .expect("canonical temporary directory")
            .parent()
            .expect("temporary directory parent")
            .to_string_lossy()
            .to_string();
        session
            .run_command("cd ..", Duration::from_secs(5))
            .await
            .expect("change directory");
        let output = session
            .run_command("pwd", Duration::from_secs(5))
            .await
            .expect("print directory");

        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.content.trim(), parent);
    }

    #[tokio::test]
    async fn accepts_input_for_an_attached_process() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let manager = ShellSessionManager::default();
        let session = manager
            .get_or_create("conversation", "interactive", directory.path())
            .await
            .expect("persistent shell");

        session
            .start_detached(
                "read -r value; printf 'received:%s\\n' \"$value\"",
                Duration::from_millis(150),
            )
            .await
            .expect("start attached process");
        let output = session
            .send_input("hello", Duration::from_secs(2))
            .await
            .expect("send input");

        assert!(output.content.contains("received:hello"));
    }
}

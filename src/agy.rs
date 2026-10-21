//! Process transport for the official Antigravity CLI's documented headless
//! streaming protocol. This module deliberately treats the CLI as the owner of
//! model execution and tool dispatch; it does not call private model endpoints.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex as AsyncMutex, mpsc, watch},
    task::JoinHandle,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

/// The fields understood by Discodex from one AGY NDJSON event.
///
/// Unknown event/step fields are preserved in `Other`/`raw` so an upstream CLI
/// extension does not silently lose data at this adapter boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Init {
        conversation_id: Option<String>,
        init: Value,
    },
    TextDelta {
        conversation_id: Option<String>,
        step_index: Option<u64>,
        state: Option<String>,
        text: String,
        raw: Value,
    },
    ToolStep {
        conversation_id: Option<String>,
        step_index: Option<u64>,
        state: Option<String>,
        tool_name: Option<String>,
        tool_info: Option<Value>,
        raw: Value,
    },
    SubagentStep {
        conversation_id: Option<String>,
        step_index: Option<u64>,
        state: Option<String>,
        subagent_info: Option<Value>,
        raw: Value,
    },
    Result {
        conversation_id: Option<String>,
        status: Option<String>,
        response: Option<String>,
        error: Option<String>,
        result: Value,
    },
    Other(Value),
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str()
}

/// Parse one documented `stream-json` line without discarding unknown fields.
pub fn parse_event(value: Value) -> Event {
    match value.get("event").and_then(Value::as_str) {
        Some("init") => Event::Init {
            conversation_id: string_at(&value, &["conversation_id"]).map(str::to_owned),
            init: value.get("init").cloned().unwrap_or(Value::Null),
        },
        Some("result") => {
            let result = value.get("result").cloned().unwrap_or(Value::Null);
            Event::Result {
                conversation_id: string_at(&result, &["conversation_id"]).map(str::to_owned),
                status: string_at(&result, &["status"]).map(str::to_owned),
                response: string_at(&result, &["response"]).map(str::to_owned),
                error: string_at(&result, &["error"]).map(str::to_owned),
                result,
            }
        }
        Some("step_update") => {
            let step = value.get("step_update").cloned().unwrap_or(Value::Null);
            let conversation_id = string_at(&step, &["conversation_id"]).map(str::to_owned);
            let step_index = step.get("step_index").and_then(Value::as_u64);
            let state = string_at(&step, &["state"]).map(str::to_owned);
            // The docs describe subagent metadata on a step but do not
            // guarantee a stable step_type value for it.
            if let Some(subagent_info) = step.get("subagent_info").cloned() {
                return Event::SubagentStep {
                    conversation_id,
                    step_index,
                    state,
                    subagent_info: Some(subagent_info),
                    raw: step,
                };
            }
            match string_at(&step, &["step_type"]) {
                Some("agent_response") => Event::TextDelta {
                    conversation_id,
                    step_index,
                    state,
                    text: string_at(&step, &["text_delta"])
                        .unwrap_or_default()
                        .to_owned(),
                    raw: step,
                },
                Some("tool") => Event::ToolStep {
                    conversation_id,
                    step_index,
                    state,
                    tool_name: string_at(&step, &["tool_name"]).map(str::to_owned),
                    tool_info: step.get("tool_info").cloned(),
                    raw: step,
                },
                _ => Event::Other(value),
            }
        }
        _ => Event::Other(value),
    }
}

/// A long-running `agy --input-format stream-json` process.
///
/// The headless protocol accepts user text events only. Tool calls are executed
/// by AGY itself (including configured MCP tools); it does not accept external
/// control/tool-result events on stdin.
pub struct Session {
    child: Child,
    owned_children: OwnedChildren,
    stdin: Option<ChildStdin>,
    events: mpsc::UnboundedReceiver<Result<Event>>,
    reader_cancel: watch::Sender<bool>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    stderr_output: Arc<AsyncMutex<String>>,
    turn_in_flight: bool,
}

#[derive(Clone, Debug)]
pub struct McpSessionContext {
    pub endpoint: PathBuf,
    pub binding: crate::agy_mcp::NativeRunBinding,
}

#[cfg(unix)]
struct OwnedChildren {
    process_group: Option<i32>,
}

#[cfg(unix)]
fn termination_group(process_group: Option<i32>) -> Option<nix::unistd::Pid> {
    process_group.map(nix::unistd::Pid::from_raw)
}

#[cfg(windows)]
struct OwnedChildren {
    job: Option<WindowsChildJob>,
}

#[cfg(windows)]
struct WindowsChildJob {
    _handle: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl WindowsChildJob {
    fn assign(child: &Child) -> Result<Self> {
        use std::{
            mem::{size_of, zeroed},
            os::windows::io::{FromRawHandle, RawHandle},
            ptr::null,
        };
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
            System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
        };

        let pid = child.id().context("AGY child has no process ID")?;
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return Err(std::io::Error::last_os_error()).context("create AGY child job object");
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(error).context("configure AGY child job lifetime");
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(error).context("open AGY process for job assignment");
            }
            let assigned = AssignProcessToJobObject(job, process);
            CloseHandle(process);
            if assigned == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(error).context("assign AGY process to owned job");
            }
            Ok(Self {
                _handle: std::os::windows::io::OwnedHandle::from_raw_handle(job as RawHandle),
            })
        }
    }
}

impl OwnedChildren {
    fn terminate(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            use nix::{
                errno::Errno,
                sys::signal::{Signal, killpg},
            };
            if let Some(group) = termination_group(self.process_group) {
                for signal in [Signal::SIGTERM, Signal::SIGKILL] {
                    match killpg(group, signal) {
                        Ok(()) | Err(Errno::ESRCH) => {}
                        Err(error) => {
                            return Err(error).context("terminate owned AGY process group");
                        }
                    }
                }
            }
        }
        #[cfg(windows)]
        {
            self.job.take();
        }
        Ok(())
    }
}

impl Drop for OwnedChildren {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use nix::{
                errno::Errno,
                sys::signal::{Signal, killpg},
            };
            if let Some(group) = termination_group(self.process_group)
                && let Err(error) = killpg(group, Signal::SIGKILL)
                && error != Errno::ESRCH
            {
                tracing::warn!(target: "discodex::agy", %error, "failed to stop owned AGY process group");
            }
        }
    }
}

fn encode_prompt(text: &str) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(&json!({
        "event": "user",
        "message": {"content": text}
    }))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn format_premature_exit(exit_code: Option<i32>, stderr: &str) -> String {
    let status = exit_code
        .map(|code| format!("exit code {code}"))
        .unwrap_or_else(|| "termination status unavailable".to_owned());
    let diagnostic = stderr.trim_end();
    if diagnostic.is_empty() {
        format!("AGY exited before terminal result ({status})")
    } else {
        format!("AGY exited before terminal result ({status}):\n{diagnostic}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    pub models: Vec<ModelOption>,
    pub efforts: Vec<String>,
}

/// Parse tab-separated model rows emitted by `agy models`; status lines are
/// ignored so they cannot be mistaken for model choices.
pub fn parse_models(output: &str) -> Vec<ModelOption> {
    output
        .lines()
        .filter_map(|line| {
            let (id, label) = line.split_once('\t')?;
            let id = id.trim();
            let label = label.trim();
            if id.is_empty() || label.is_empty() {
                return None;
            }
            Some(ModelOption {
                id: id.to_owned(),
                label: label.to_owned(),
            })
        })
        .collect()
}

/// Parse the alternatives in the CLI's `--effort` help row. This intentionally
/// follows the installed CLI instead of deriving choices from model names.
pub fn parse_efforts(help: &str) -> Vec<String> {
    help.lines()
        .find(|line| line.contains("--effort"))
        .and_then(|line| line.split_once('(').map(|(_, tail)| tail))
        .and_then(|tail| tail.split_once(')').map(|(choices, _)| choices))
        .map(|choices| {
            choices
                .split(['|', ','])
                .map(str::trim)
                .filter(|choice| !choice.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

async fn cli_output(executable: &Path, args: &[&str]) -> Result<std::process::Output> {
    let mut command = Command::new(executable);
    command.args(args);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .output()
        .await
        .with_context(|| format!("run AGY CLI command {}", args.join(" ")))
}

/// Discover the installed CLI's available model IDs and supported effort
/// alternatives. Both commands are metadata-only; no model prompt is sent.
pub async fn discover_catalog(executable: &Path) -> Result<Catalog> {
    let (models_output, help_output) = tokio::join!(
        cli_output(executable, &["models"]),
        cli_output(executable, &["--help"]),
    );
    let models_output = models_output?;
    let help_output = help_output?;
    let models_stdout = String::from_utf8_lossy(&models_output.stdout);
    let models_stderr = String::from_utf8_lossy(&models_output.stderr);
    let help_stdout = String::from_utf8_lossy(&help_output.stdout);
    let help_stderr = String::from_utf8_lossy(&help_output.stderr);
    if !models_output.status.success() {
        bail!("AGY model listing failed: {}", models_stderr.trim());
    }
    if !help_output.status.success() {
        bail!("AGY CLI help failed: {}", help_stderr.trim());
    }
    let mut models = parse_models(&models_stdout);
    if models.is_empty() {
        models = parse_models(&models_stderr);
    }
    if models.is_empty() {
        bail!("AGY model listing returned no parseable model rows");
    }
    let mut efforts = parse_efforts(&help_stdout);
    if efforts.is_empty() {
        efforts = parse_efforts(&help_stderr);
    }
    if efforts.is_empty() {
        bail!("AGY CLI help returned no parseable --effort choices");
    }
    Ok(Catalog { models, efforts })
}

fn session_command(
    executable: &Path,
    cwd: &Path,
    model: Option<&str>,
    effort: Option<&str>,
    mcp_context: Option<&McpSessionContext>,
) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(cwd)
        .arg("--input-format")
        .arg("stream-json")
        .arg("--output-format")
        .arg("stream-json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .env_remove(crate::agy_mcp::NATIVE_ENDPOINT_ENV)
        .env_remove(crate::agy_mcp::NATIVE_RUN_ENV);
    #[cfg(unix)]
    command.process_group(0);
    if let Some(context) = mcp_context {
        command
            .env(crate::agy_mcp::NATIVE_ENDPOINT_ENV, &context.endpoint)
            .env(
                crate::agy_mcp::NATIVE_RUN_ENV,
                json!(context.binding).to_string(),
            );
    }
    if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
        command.arg("--model").arg(model);
    }
    if let Some(effort) = effort.filter(|value| !value.trim().is_empty()) {
        command.arg("--effort").arg(effort);
    }
    command
}

impl Session {
    /// Start one process for a conversation. `cwd` selects the CLI workspace;
    /// AGY resolves its existing cached login and MCP configuration itself.
    pub async fn spawn(
        executable: &Path,
        cwd: &Path,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Self> {
        Self::spawn_with_context(executable, cwd, model, effort, None).await
    }

    /// Start AGY with an optional per-process Discodex native MCP binding.
    /// The MCP project config remains stable; these values are inherited by
    /// the CLI's MCP child and cannot race with another AGY session.
    pub async fn spawn_with_context(
        executable: &Path,
        cwd: &Path,
        model: Option<&str>,
        effort: Option<&str>,
        mcp_context: Option<&McpSessionContext>,
    ) -> Result<Self> {
        let mut child = session_command(executable, cwd, model, effort, mcp_context)
            .spawn()
            .with_context(|| format!("start Antigravity CLI at {}", executable.display()))?;
        #[cfg(unix)]
        let owned_children = OwnedChildren {
            process_group: child.id().map(|pid| pid as i32),
        };
        #[cfg(windows)]
        let owned_children = match WindowsChildJob::assign(&child) {
            Ok(job) => OwnedChildren { job: Some(job) },
            Err(error) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(error);
            }
        };
        let stdin = child
            .stdin
            .take()
            .context("AGY process has no stdin pipe")?;
        let stdout = child
            .stdout
            .take()
            .context("AGY process has no stdout pipe")?;
        let stderr = child
            .stderr
            .take()
            .context("AGY process has no stderr pipe")?;

        let (sender, events) = mpsc::unbounded_channel();
        let (reader_cancel, _) = watch::channel(false);
        let mut stdout_cancel = reader_cancel.subscribe();
        let stdout_reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                let next_line = tokio::select! {
                    biased;
                    line = lines.next_line() => line,
                    _ = stdout_cancel.changed() => return,
                };
                match next_line {
                    Ok(Some(line)) => {
                        let parsed = serde_json::from_str::<Value>(&line)
                            .map(parse_event)
                            .context("parse AGY stream-json event");
                        if sender.send(parsed).is_err() {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(error) => {
                        let _ = sender.send(Err(error).context("read AGY stdout"));
                        return;
                    }
                }
            }
        });
        let stderr_output = Arc::new(AsyncMutex::new(String::new()));
        let stderr_capture = stderr_output.clone();
        let mut stderr_cancel = reader_cancel.subscribe();
        let stderr_reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            loop {
                let next_line = tokio::select! {
                    biased;
                    line = lines.next_line() => line,
                    _ = stderr_cancel.changed() => return,
                };
                match next_line {
                    Ok(Some(line)) => {
                        tracing::debug!(target: "discodex::agy", "{line}");
                        let mut captured = stderr_capture.lock().await;
                        captured.push_str(&line);
                        captured.push('\n');
                    }
                    Ok(None) => return,
                    Err(error) => {
                        let mut captured = stderr_capture.lock().await;
                        captured.push_str(&format!("[could not read AGY stderr: {error}]\n"));
                        return;
                    }
                }
            }
        });

        Ok(Self {
            child,
            owned_children,
            stdin: Some(stdin),
            events,
            reader_cancel,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
            stderr_output,
            turn_in_flight: false,
        })
    }

    /// Submit one text-only user turn using the official NDJSON envelope.
    pub async fn send_text(&mut self, text: &str) -> Result<()> {
        if self.turn_in_flight {
            bail!("wait for the current AGY result before sending another turn");
        }
        let bytes = encode_prompt(text)?;
        let stdin = self.stdin.as_mut().context("AGY input stream is closed")?;
        stdin.write_all(&bytes).await.context("write AGY prompt")?;
        stdin.flush().await.context("flush AGY prompt")?;
        self.turn_in_flight = true;
        Ok(())
    }

    /// Receive the next parsed stdout event. `None` means stdout closed.
    pub async fn next_event(&mut self) -> Option<Result<Event>> {
        let event = self.events.recv().await;
        if matches!(event, Some(Ok(Event::Result { .. }))) {
            self.turn_in_flight = false;
        }
        if event.is_some() {
            return event;
        }
        let status = match self.child.wait().await {
            Ok(status) => status,
            Err(error) => {
                let _ = self.stop_readers().await;
                return Some(Err(error).context("wait for AGY after stdout closed"));
            }
        };
        if let Err(error) = self.owned_children.terminate() {
            let _ = self.stop_readers().await;
            return Some(Err(error).context("stop AGY children after stdout closed"));
        }
        if let Err(error) = self.stop_readers().await {
            return Some(Err(error).context("stop AGY output readers after stdout closed"));
        }
        let stderr = self.stderr_output.lock().await.clone();
        if !std::mem::replace(&mut self.turn_in_flight, false) {
            return None;
        }
        Some(Err(anyhow::anyhow!(format_premature_exit(
            status.code(),
            &stderr
        ))))
    }

    async fn stop_readers(&mut self) -> Result<()> {
        let _ = self.reader_cancel.send(true);
        if let Some(reader) = self.stdout_reader.take() {
            reader.await.context("join AGY stdout reader")?;
        }
        if let Some(reader) = self.stderr_reader.take() {
            reader.await.context("join AGY stderr reader")?;
        }
        Ok(())
    }

    /// Close stdin and let the current turn finish before collecting status.
    pub async fn finish(mut self) -> Result<std::process::ExitStatus> {
        self.stdin.take();
        let status = self.child.wait().await.context("wait for AGY process");
        let cleanup = self.owned_children.terminate();
        let readers = self.stop_readers().await;
        let status = status?;
        cleanup?;
        readers?;
        Ok(status)
    }

    /// Stop the owned CLI process when the surrounding conversation is canceled.
    pub async fn terminate(&mut self) -> Result<std::process::ExitStatus> {
        self.stdin.take();
        let cleanup = self.owned_children.terminate();
        let status = self
            .child
            .wait()
            .await
            .context("wait for terminated AGY process");
        let readers = self.stop_readers().await;
        let status = status?;
        cleanup?;
        readers?;
        Ok(status)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.reader_cancel.send(true);
        if let Some(reader) = self.stdout_reader.take() {
            reader.abort();
        }
        if let Some(reader) = self.stderr_reader.take() {
            reader.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    #[test]
    fn session_command_preserves_executable_arguments_and_native_binding() {
        let executable = Path::new("example tools").join("agy-cli");
        let cwd = Path::new("example workspace");
        let context = McpSessionContext {
            endpoint: "example-native-endpoint".into(),
            binding: crate::agy_mcp::NativeRunBinding {
                thread_id: "example-thread".into(),
                turn_id: "example-turn".into(),
                run_id: "example-response".into(),
                generation: 3,
            },
        };
        let command = session_command(
            &executable,
            cwd,
            Some("exact model id"),
            Some("precise-effort"),
            Some(&context),
        );
        let command = command.as_std();
        assert_eq!(command.get_program(), executable.as_os_str());
        assert_eq!(command.get_current_dir(), Some(cwd));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--model",
                "exact model id",
                "--effort",
                "precise-effort",
            ]
            .map(OsStr::new)
        );
        assert_eq!(
            command.get_envs().collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                (
                    OsStr::new(crate::agy_mcp::NATIVE_ENDPOINT_ENV),
                    Some(OsStr::new(&context.endpoint))
                ),
                (
                    OsStr::new(crate::agy_mcp::NATIVE_RUN_ENV),
                    Some(OsStr::new(&json!({"threadId":"example-thread","turnId":"example-turn","runId":"example-response","generation":3}).to_string()))
                ),
            ])
        );
    }

    #[test]
    fn unbound_session_command_removes_inherited_native_binding() {
        for (model, effort) in [(None, None), (Some("  "), Some(""))] {
            let command = session_command(Path::new("agy"), Path::new("."), model, effort, None);
            let command = command.as_std();
            assert_eq!(command.get_program(), OsStr::new("agy"));
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                [
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json"
                ]
                .map(OsStr::new)
            );
            assert_eq!(
                command.get_envs().collect::<BTreeMap<_, _>>(),
                BTreeMap::from([
                    (OsStr::new(crate::agy_mcp::NATIVE_ENDPOINT_ENV), None),
                    (OsStr::new(crate::agy_mcp::NATIVE_RUN_ENV), None),
                ])
            );
        }
    }

    #[test]
    fn sends_only_documented_text_user_event() {
        let bytes = encode_prompt("hello").unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let message: Value = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(message["event"], "user");
        assert_eq!(message["message"]["content"], "hello");
        assert!(message["message"].get("attachments").is_none());
        let empty_bytes = encode_prompt("").unwrap();
        let empty: Value = serde_json::from_slice(&empty_bytes[..empty_bytes.len() - 1]).unwrap();
        assert_eq!(empty["message"]["content"], "");
    }

    #[test]
    fn premature_exit_diagnostic_preserves_exit_code_and_stderr() {
        assert_eq!(
            format_premature_exit(Some(1), "invalid effort\n"),
            "AGY exited before terminal result (exit code 1):\ninvalid effort"
        );
        assert_eq!(
            format_premature_exit(None, ""),
            "AGY exited before terminal result (termination status unavailable)"
        );
    }

    #[test]
    fn parses_model_rows_without_interpreting_model_names() {
        let models = parse_models(
            "Fetching available models...\nmodel-alpha\tModel Alpha (Low)\ncustom-model-z\tModel Z\n",
        );
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "model-alpha");
        assert_eq!(models[0].label, "Model Alpha (Low)");
        assert_eq!(models[1].id, "custom-model-z");
        assert!(parse_models("Fetching available models...\n").is_empty());
    }

    #[test]
    fn parses_effort_alternatives_from_cli_help_row() {
        let help = "Usage: agy\n  --effort Set reasoning effort (brief|careful|deep)\n";
        assert_eq!(parse_efforts(help), ["brief", "careful", "deep"]);
        assert!(parse_efforts("Usage: agy\n  --effort Set reasoning effort\n").is_empty());
    }

    #[test]
    fn parses_deltas_tool_steps_results_and_preserves_unknowns() {
        let delta = parse_event(
            json!({"event":"step_update","step_update":{"conversation_id":"c","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"hi"}}),
        );
        assert_eq!(
            delta,
            Event::TextDelta {
                conversation_id: Some("c".into()),
                step_index: Some(2),
                state: Some("ACTIVE".into()),
                text: "hi".into(),
                raw: json!({"conversation_id":"c","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"hi"})
            }
        );

        let tool = parse_event(
            json!({"event":"step_update","step_update":{"conversation_id":"tool-conversation","step_index":7,"state":"DONE","step_type":"tool","tool_name":"run_command","tool_info":{"call_id":"tool-call-7","parameters":{"command":"printf ok"},"output":"ok"}}}),
        );
        assert_eq!(
            tool,
            Event::ToolStep {
                conversation_id: Some("tool-conversation".into()),
                step_index: Some(7),
                state: Some("DONE".into()),
                tool_name: Some("run_command".into()),
                tool_info: Some(json!({
                    "call_id":"tool-call-7",
                    "parameters":{"command":"printf ok"},
                    "output":"ok"
                })),
                raw: json!({
                    "conversation_id":"tool-conversation",
                    "step_index":7,
                    "state":"DONE",
                    "step_type":"tool",
                    "tool_name":"run_command",
                    "tool_info":{
                        "call_id":"tool-call-7",
                        "parameters":{"command":"printf ok"},
                        "output":"ok"
                    }
                }),
            }
        );

        let result = parse_event(
            json!({"event":"result","result":{"conversation_id":"c","status":"ERROR","error":"unsupported"}}),
        );
        assert!(
            matches!(result, Event::Result { status: Some(status), error: Some(error), .. } if status == "ERROR" && error == "unsupported")
        );

        let subagent = parse_event(
            json!({"event":"step_update","step_update":{"step_type":"tool","subagent_info":{"subagents":[{"conversation_id":"child"}]}}}),
        );
        assert!(
            matches!(subagent, Event::SubagentStep { subagent_info: Some(info), .. } if info.pointer("/subagents/0/conversation_id").and_then(Value::as_str) == Some("child"))
        );

        let future = json!({"event":"future_event","payload":1});
        assert_eq!(parse_event(future.clone()), Event::Other(future));
    }

    #[cfg(unix)]
    #[test]
    fn termination_target_tracks_owned_process_group() {
        assert_eq!(termination_group(None), None);
        assert_eq!(
            termination_group(Some(42)).map(|pid| pid.as_raw()),
            Some(42)
        );
    }
}

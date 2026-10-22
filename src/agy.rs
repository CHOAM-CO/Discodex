//! Process transport for the official Antigravity CLI's documented headless
//! streaming protocol. This module deliberately treats the CLI as the owner of
//! model execution and tool dispatch; it does not call private model endpoints.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write as _,
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

/// Canonical tokens for the documented AGY CLI protocol and argv contract.
pub(crate) mod protocol {
    pub const EVENT: &str = "event";
    pub const INIT: &str = "init";
    pub const RESULT: &str = "result";
    pub const STEP_UPDATE: &str = "step_update";
    pub const CONVERSATION_ID: &str = "conversation_id";
    pub const STATUS: &str = "status";
    pub const RESPONSE: &str = "response";
    pub const ERROR: &str = "error";
    pub const STEP_INDEX: &str = "step_index";
    pub const STATE: &str = "state";
    pub const SUBAGENT_INFO: &str = "subagent_info";
    pub const STEP_TYPE: &str = "step_type";
    pub const AGENT_RESPONSE: &str = "agent_response";
    pub const TEXT_DELTA: &str = "text_delta";
    pub const TOOL: &str = "tool";
    pub const TOOL_NAME: &str = "tool_name";
    pub const TOOL_INFO: &str = "tool_info";
    pub const INPUT_FORMAT_ARG: &str = "--input-format";
    pub const OUTPUT_FORMAT_ARG: &str = "--output-format";
    pub const MODEL_ARG: &str = "--model";
    pub const EFFORT_ARG: &str = "--effort";
    pub const AGENT_ARG: &str = "--agent";
    pub const STREAM_JSON: &str = "stream-json";
    pub const MODEL_ROW_SEPARATOR: char = '\t';
    pub const EFFORT_CHOICES_OPEN: char = '(';
    pub const EFFORT_CHOICES_CLOSE: char = ')';
    pub const EFFORT_CHOICES_SEPARATORS: [char; 2] = ['|', ','];
    pub const MODELS_COMMAND: &str = "models";
    pub const HELP_ARG: &str = "--help";
    pub const EFFORT_NAME: &str = "effort";
    pub const LOGIN_REQUIRED_DIAGNOSTIC: &str =
        "Please sign in to view available models. Launch the CLI without arguments to sign in.";
    pub const AUTH_BROWSER_URL_PROMPT: &str = "Open the URL below in your browser:";
    pub const AUTH_BROWSER_URL_PROMPT_FALLBACK: &str =
        "Your browser should open automatically. If not:";
    pub const AUTH_CODE_PROMPT: &str =
        "After authenticating, copy the code displayed in the browser and paste it below:";
    pub const AUTH_CODE_PROMPT_FALLBACK: &str =
        "If you aren't automatically redirected, paste the authorization code below:";
    pub const AUTH_ERROR_PREFIX: &str = "Got an error:";
    pub const AUTH_RETURN_PROMPT: &str = "Press any key to go back.";
}

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
pub fn parse_event(value: Value) -> Result<Event> {
    match value.get(protocol::EVENT).and_then(Value::as_str) {
        Some(protocol::INIT) => match value.get(protocol::INIT) {
            Some(init) => Ok(Event::Init {
                conversation_id: string_at(&value, &[protocol::CONVERSATION_ID]).map(str::to_owned),
                init: init.clone(),
            }),
            None => bail!("AGY stream-json init event omitted its required payload"),
        },
        Some(protocol::RESULT) => {
            let Some(result) = value.get(protocol::RESULT).cloned() else {
                bail!("AGY stream-json result event omitted its required payload");
            };
            let Some(status) = string_at(&result, &[protocol::STATUS]) else {
                bail!("AGY stream-json result payload omitted its required status");
            };
            Ok(Event::Result {
                conversation_id: string_at(&result, &[protocol::CONVERSATION_ID])
                    .map(str::to_owned),
                status: Some(status.to_owned()),
                response: string_at(&result, &[protocol::RESPONSE]).map(str::to_owned),
                error: string_at(&result, &[protocol::ERROR]).map(str::to_owned),
                result,
            })
        }
        Some(protocol::STEP_UPDATE) => {
            let Some(step) = value.get(protocol::STEP_UPDATE).cloned() else {
                bail!("AGY stream-json step_update event omitted its required payload");
            };
            let conversation_id = string_at(&step, &[protocol::CONVERSATION_ID]).map(str::to_owned);
            let step_index = step.get(protocol::STEP_INDEX).and_then(Value::as_u64);
            let state = string_at(&step, &[protocol::STATE]).map(str::to_owned);
            // The docs describe subagent metadata on a step but do not
            // guarantee a stable step_type value for it.
            if let Some(subagent_info) = step.get(protocol::SUBAGENT_INFO).cloned() {
                return Ok(Event::SubagentStep {
                    conversation_id,
                    step_index,
                    state,
                    subagent_info: Some(subagent_info),
                    raw: step,
                });
            }
            let Some(step_type) = string_at(&step, &[protocol::STEP_TYPE]) else {
                bail!("AGY stream-json step_update payload omitted its required step_type");
            };
            match step_type {
                protocol::AGENT_RESPONSE => match string_at(&step, &[protocol::TEXT_DELTA]) {
                    Some(text) => Ok(Event::TextDelta {
                        conversation_id,
                        step_index,
                        state,
                        text: text.to_owned(),
                        raw: step,
                    }),
                    None => bail!("AGY agent_response step omitted its required text_delta"),
                },
                protocol::TOOL => Ok(Event::ToolStep {
                    conversation_id,
                    step_index,
                    state,
                    tool_name: string_at(&step, &[protocol::TOOL_NAME]).map(str::to_owned),
                    tool_info: step.get(protocol::TOOL_INFO).cloned(),
                    raw: step,
                }),
                _ => Ok(Event::Other(value)),
            }
        }
        _ => Ok(Event::Other(value)),
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
    _agent: Option<TemporaryAgent>,
}

#[derive(Clone, Debug)]
pub struct McpSessionContext {
    pub endpoint: PathBuf,
    pub binding: crate::agy_mcp::NativeRunBinding,
}

/// Per-run custom agent definition discovered through AGY's documented
/// workspace `.agents/agents/<name>/agent.md` location. The session owns this
/// value so the definition remains available until the CLI process exits.
pub(crate) struct TemporaryAgent {
    directory: tempfile::TempDir,
    name: String,
}

impl TemporaryAgent {
    pub(crate) fn create(workspace: &Path, instructions: &str) -> Result<Self> {
        let agents_root = workspace.join(".agents");
        let definitions_root = agents_root.join("agents");
        for directory in [&agents_root, &definitions_root] {
            fs::create_dir_all(directory)
                .with_context(|| format!("create AGY agent directory {}", directory.display()))?;
        }

        let directory = match tempfile::Builder::new()
            .prefix("discodex-run-")
            .tempdir_in(&definitions_root)
        {
            Ok(directory) => directory,
            Err(error) => return Err(error).context("create temporary AGY agent directory"),
        };
        let Some(name) = directory.path().file_name().and_then(|name| name.to_str()) else {
            bail!("temporary AGY agent name is not UTF-8");
        };
        let name = name.to_owned();
        let file_path = directory.path().join("agent.md");
        let definition = temporary_agent_definition(&name, instructions);
        let write_result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file_path)
                .with_context(|| format!("create AGY agent definition {}", file_path.display()))?;
            file.write_all(definition.as_bytes())?;
            file.flush()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            return Err(error);
        }
        Ok(Self { directory, name })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

fn temporary_agent_definition(name: &str, instructions: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: Per-run Codex execution guidance.\n---\n\n{instructions}"
    )
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

pub(crate) fn encode_prompt(text: &str) -> Result<Vec<u8>> {
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
    let diagnostic = sanitize_agy_diagnostic(stderr);
    let diagnostic = diagnostic.trim_end();
    if diagnostic.is_empty() {
        format!("AGY exited before terminal result ({status})")
    } else {
        format!("AGY exited before terminal result ({status}):\n{diagnostic}")
    }
}

fn format_prompt_io_error(
    operation: &str,
    error: &std::io::Error,
    status: &str,
    stderr: &str,
) -> anyhow::Error {
    let stderr = sanitize_agy_diagnostic(stderr);
    let stderr = stderr.trim_end();
    let stderr = if stderr.is_empty() {
        "<empty>".to_owned()
    } else {
        let tail = stderr.chars().rev().take(4096).collect::<String>();
        tail.chars().rev().collect::<String>()
    };
    anyhow::anyhow!("{operation}: {error} (AGY {status}; stderr: {stderr})")
}

fn sanitize_agy_diagnostic(detail: &str) -> String {
    crate::discord::sanitize_discord_diagnostic(detail)
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
            let (id, label) = line.split_once(protocol::MODEL_ROW_SEPARATOR)?;
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
        .find(|line| line.contains(protocol::EFFORT_ARG))
        .and_then(|line| {
            line.split_once(protocol::EFFORT_CHOICES_OPEN)
                .map(|(_, tail)| tail)
        })
        .and_then(|tail| {
            tail.split_once(protocol::EFFORT_CHOICES_CLOSE)
                .map(|(choices, _)| choices)
        })
        .map(|choices| {
            choices
                .split(protocol::EFFORT_CHOICES_SEPARATORS)
                .map(str::trim)
                .filter(|choice| !choice.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// One managed CLI and settings scope, shared by setup, discovery, and turns.
/// AGY's native keyring remains owned by AGY even with separate settings.
#[derive(Clone, Debug)]
pub struct Runtime {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: HashMap<String, String>,
}

fn profile_data_root(state_dir: &Path) -> Result<PathBuf> {
    let state_dir =
        std::path::absolute(state_dir).context("resolve AGY profile state directory")?;
    Ok(state_dir
        .parent()
        .context("AGY profile state directory has no parent")?
        .to_owned())
}

pub fn managed_root(state_dir: &Path) -> Result<PathBuf> {
    Ok(profile_data_root(state_dir)?.join("runtime").join("agy"))
}

pub fn managed_executable(state_dir: &Path) -> Result<PathBuf> {
    Ok(managed_root(state_dir)?.join(if cfg!(windows) { "agy.exe" } else { "agy" }))
}

impl Runtime {
    pub fn for_config(config: &crate::config::Config, share_settings: bool) -> Result<Self> {
        let mut environment = HashMap::new();
        // The managed package is updated only through Discodex's verified
        // manifest installer, so disable the CLI's self-replacing updater.
        environment.insert("AGY_CLI_DISABLE_AUTO_UPDATE".to_owned(), "true".to_owned());
        Self::with_environment(
            &config.state_dir,
            share_settings,
            cfg!(windows),
            environment,
        )
    }

    pub fn for_preferences(
        config: &crate::config::Config,
        preferences: &crate::native::NativeFeaturePreferences,
    ) -> Result<Self> {
        Self::for_config(
            config,
            preferences
                .antigravity_share_settings
                .context("choose whether to share existing AGY settings in Discodex settings")?,
        )
    }

    fn with_environment(
        state_dir: &Path,
        share_settings: bool,
        windows: bool,
        mut environment: HashMap<String, String>,
    ) -> Result<Self> {
        environment.insert("AGY_CLI_DISABLE_AUTO_UPDATE".to_owned(), "true".to_owned());
        let executable = managed_root(state_dir)?.join(if windows { "agy.exe" } else { "agy" });
        let arguments = if share_settings {
            Vec::new()
        } else {
            let settings_root = profile_data_root(state_dir)?.join("agy-settings");
            vec![
                format!("--gemini_dir={}", settings_root.display()),
                "--app_data_dir=antigravity-cli".to_owned(),
            ]
        };
        Ok(Self {
            executable,
            arguments,
            environment,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.args(&self.arguments).envs(&self.environment);
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
}

fn cli_command(runtime: &Runtime, args: &[&str]) -> Command {
    let mut command = runtime.command();
    command.args(args);
    command
}

async fn cli_output(runtime: &Runtime, args: &[&str]) -> Result<std::process::Output> {
    cli_command(runtime, args).output().await.with_context(|| {
        format!(
            "run AGY CLI at {} with command {}",
            runtime.executable.display(),
            args.join(" ")
        )
    })
}

/// Discover the installed CLI's available model IDs and supported effort
/// alternatives. Both commands are metadata-only; no model prompt is sent.
pub async fn discover_catalog(runtime: &Runtime) -> Result<Catalog> {
    let (models_output, help_output) = tokio::join!(
        cli_output(runtime, &[protocol::MODELS_COMMAND]),
        cli_output(runtime, &[protocol::HELP_ARG]),
    );
    let models_output = models_output?;
    let help_output = help_output?;
    let models_stdout = String::from_utf8_lossy(&models_output.stdout);
    let models_stderr = sanitize_agy_diagnostic(&String::from_utf8_lossy(&models_output.stderr));
    let help_stdout = String::from_utf8_lossy(&help_output.stdout);
    let help_stderr = sanitize_agy_diagnostic(&String::from_utf8_lossy(&help_output.stderr));
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

pub(crate) fn session_command(
    runtime: &Runtime,
    cwd: &Path,
    model: Option<&str>,
    effort: Option<&str>,
    mcp_context: Option<&McpSessionContext>,
) -> Command {
    session_command_with_agent(runtime, cwd, model, effort, mcp_context, None)
}

fn session_command_with_agent(
    runtime: &Runtime,
    cwd: &Path,
    model: Option<&str>,
    effort: Option<&str>,
    mcp_context: Option<&McpSessionContext>,
    agent: Option<&str>,
) -> Command {
    let mut command = runtime.command();
    command
        .current_dir(cwd)
        .arg(protocol::INPUT_FORMAT_ARG)
        .arg(protocol::STREAM_JSON)
        .arg(protocol::OUTPUT_FORMAT_ARG)
        .arg(protocol::STREAM_JSON)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
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
        command.arg(protocol::MODEL_ARG).arg(model);
    }
    if let Some(effort) = effort.and_then(agy_cli_effort) {
        command.arg(protocol::EFFORT_ARG).arg(effort);
    }
    if let Some(agent) = agent.filter(|value| !value.trim().is_empty()) {
        command.arg(protocol::AGENT_ARG).arg(agent);
    }
    command
}

/// Codex Ultra selects proactive multi-agent orchestration; it is not an AGY
/// model effort. Keep that mode in Codex's history/instructions and let AGY's
/// selected model ID choose its own variant by omitting only this CLI argument.
fn agy_cli_effort(effort: &str) -> Option<&str> {
    (!effort.trim().is_empty() && effort != "ultra").then_some(effort)
}

impl Session {
    /// Start one process for a conversation. `cwd` selects the CLI workspace;
    /// AGY resolves its existing cached login and MCP configuration itself.
    pub async fn spawn(
        runtime: &Runtime,
        cwd: &Path,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Self> {
        Self::spawn_with_context(runtime, cwd, model, effort, None).await
    }

    /// Start AGY with an optional per-process Discodex native MCP binding.
    /// The MCP project config remains stable; these values are inherited by
    /// the CLI's MCP child and cannot race with another AGY session.
    pub async fn spawn_with_context(
        runtime: &Runtime,
        cwd: &Path,
        model: Option<&str>,
        effort: Option<&str>,
        mcp_context: Option<&McpSessionContext>,
    ) -> Result<Self> {
        Self::spawn_internal(runtime, cwd, model, effort, mcp_context, None).await
    }

    /// Start AGY with a per-run custom agent whose definition is owned by the
    /// returned session and removed when that session is dropped.
    pub(crate) async fn spawn_with_agent_context(
        runtime: &Runtime,
        cwd: &Path,
        model: Option<&str>,
        effort: Option<&str>,
        mcp_context: Option<&McpSessionContext>,
        agent: TemporaryAgent,
    ) -> Result<Self> {
        Self::spawn_internal(runtime, cwd, model, effort, mcp_context, Some(agent)).await
    }

    async fn spawn_internal(
        runtime: &Runtime,
        cwd: &Path,
        model: Option<&str>,
        effort: Option<&str>,
        mcp_context: Option<&McpSessionContext>,
        agent: Option<TemporaryAgent>,
    ) -> Result<Self> {
        let mut child = session_command_with_agent(
            runtime,
            cwd,
            model,
            effort,
            mcp_context,
            agent.as_ref().map(TemporaryAgent::name),
        )
        .spawn()
        .with_context(|| format!("start Antigravity CLI at {}", runtime.executable.display()))?;
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
                            .context("decode AGY stream-json event")
                            .and_then(parse_event)
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
                        let line = sanitize_agy_diagnostic(&line);
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
            _agent: agent,
        })
    }

    /// Submit one text-only user turn using the official NDJSON envelope.
    pub async fn send_text(&mut self, text: &str) -> Result<()> {
        if self.turn_in_flight {
            bail!("wait for the current AGY result before sending another turn");
        }
        let bytes = encode_prompt(text)?;
        let Some(stdin) = self.stdin.as_mut() else {
            bail!("AGY input stream is closed before prompt write");
        };
        if let Err(error) = stdin.write_all(&bytes).await {
            self.stdin.take();
            return Err(self.prompt_io_error("write AGY prompt", error).await);
        }
        let flush_result = self
            .stdin
            .as_mut()
            .context("AGY input stream closed before prompt flush")?
            .flush()
            .await;
        if let Err(error) = flush_result {
            self.stdin.take();
            return Err(self.prompt_io_error("flush AGY prompt", error).await);
        }
        self.turn_in_flight = true;
        Ok(())
    }

    async fn prompt_io_error(&mut self, operation: &str, error: std::io::Error) -> anyhow::Error {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => {
                let child_cleanup = self
                    .owned_children
                    .terminate()
                    .err()
                    .map(|error| format!("; child cleanup failed: {error:#}"))
                    .unwrap_or_default();
                if let Some(reader) = self.stderr_reader.take() {
                    let _ = reader.await;
                }
                let status = status
                    .code()
                    .map(|code| format!("exited with status {code}"))
                    .unwrap_or_else(|| format!("terminated ({status:?})"));
                format!("{status}{child_cleanup}")
            }
            Ok(None) => "still running".to_owned(),
            Err(status_error) => format!("status unavailable ({status_error})"),
        };
        let stderr = self.stderr_output.lock().await.clone();
        format_prompt_io_error(operation, &error, &status, &stderr)
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
    fn managed_runtime_uses_each_profile_binary_without_searching_global_path() {
        let directory = tempfile::tempdir().unwrap();
        let mut environment = HashMap::from([
            ("PATH".to_owned(), "global tools".to_owned()),
            ("HOME".to_owned(), "shared home".to_owned()),
        ]);
        environment.insert("AGY_CLI_DISABLE_AUTO_UPDATE".to_owned(), "true".to_owned());
        for profile in ["example-one", "example-two"] {
            let data_root = directory.path().join(profile);
            for windows in [false, true] {
                let runtime = Runtime::with_environment(
                    &data_root.join("state"),
                    true,
                    windows,
                    environment.clone(),
                )
                .unwrap();
                assert_eq!(
                    runtime.executable,
                    data_root
                        .join("runtime/agy")
                        .join(if windows { "agy.exe" } else { "agy" })
                );
                assert!(runtime.arguments.is_empty());
                assert_eq!(runtime.environment, environment);
            }
        }
    }

    #[test]
    fn separate_settings_use_native_flags_and_preserve_home_and_environment() {
        let directory = tempfile::tempdir().unwrap();
        let mut environment = HashMap::from([
            ("HOME".to_owned(), "original home".to_owned()),
            ("USERPROFILE".to_owned(), "original user profile".to_owned()),
            ("PATH".to_owned(), "original path".to_owned()),
        ]);
        environment.insert("AGY_CLI_DISABLE_AUTO_UPDATE".to_owned(), "true".to_owned());
        for windows in [false, true] {
            let runtime = Runtime::with_environment(
                &directory.path().join("state"),
                false,
                windows,
                environment.clone(),
            )
            .unwrap();
            assert_eq!(
                runtime.arguments,
                [
                    format!(
                        "--gemini_dir={}",
                        directory.path().join("agy-settings").display()
                    ),
                    "--app_data_dir=antigravity-cli".to_owned(),
                ]
            );
            assert_eq!(runtime.environment, environment);
            for args in [&["models"][..], &["--help"][..]] {
                let command = cli_command(&runtime, args);
                let expected = runtime
                    .arguments
                    .iter()
                    .map(String::as_str)
                    .chain(args.iter().copied())
                    .map(OsStr::new)
                    .collect::<Vec<_>>();
                assert_eq!(
                    command.as_std().get_program(),
                    runtime.executable.as_os_str()
                );
                assert_eq!(command.as_std().get_args().collect::<Vec<_>>(), expected);
                assert_eq!(
                    command.as_std().get_envs().collect::<BTreeMap<_, _>>(),
                    environment
                        .iter()
                        .map(|(key, value)| (OsStr::new(key), Some(OsStr::new(value))))
                        .collect()
                );
            }
        }
    }

    #[test]
    fn preference_runtime_requires_settings_choice_and_ignores_legacy_executable() {
        let directory = tempfile::tempdir().unwrap();
        let config: crate::config::Config = serde_json::from_value(json!({
            "profile":"example", "name":"Example", "tokenEnv":"EXAMPLE_TOKEN",
            "guildId":1, "homeChannelId":2, "transcriptChannelId":3,
            "codexPath":"example-core", "projectDir":directory.path().join("project"),
            "stateDir":directory.path().join("state"),
        }))
        .unwrap();
        let mut preferences = crate::native::NativeFeaturePreferences {
            antigravity_enabled: true,
            antigravity_path: Some("global-tools/custom-agy".to_owned()),
            ..Default::default()
        };
        assert!(
            Runtime::for_preferences(&config, &preferences)
                .unwrap_err()
                .to_string()
                .contains("choose whether to share")
        );
        for share_settings in [false, true] {
            preferences.antigravity_share_settings = Some(share_settings);
            let runtime = Runtime::for_preferences(&config, &preferences).unwrap();
            assert_eq!(
                runtime.executable,
                managed_executable(&config.state_dir).unwrap()
            );
            assert_eq!(runtime.arguments.is_empty(), share_settings);
        }
    }

    #[test]
    fn session_command_preserves_executable_arguments_and_native_binding() {
        let executable = Path::new("example tools").join("agy-cli");
        let runtime = Runtime {
            executable: executable.clone(),
            arguments: vec![
                "--gemini_dir=example settings".to_owned(),
                "--app_data_dir=antigravity-cli".to_owned(),
            ],
            environment: HashMap::from([("HOME".to_owned(), "shared home".to_owned())]),
        };
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
            &runtime,
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
                "--gemini_dir=example settings",
                "--app_data_dir=antigravity-cli",
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
                (OsStr::new("HOME"), Some(OsStr::new("shared home"))),
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
    fn session_command_omits_codex_ultra_but_preserves_model_and_native_binding() {
        let runtime = Runtime {
            executable: "example-runtime/agy".into(),
            arguments: vec!["--settings=example".to_owned()],
            environment: HashMap::new(),
        };
        let cwd = Path::new("example workspace");
        let context = McpSessionContext {
            endpoint: "example-native-endpoint".into(),
            binding: crate::agy_mcp::NativeRunBinding {
                thread_id: "example-thread".into(),
                turn_id: "example-turn".into(),
                run_id: "example-run".into(),
                generation: 7,
            },
        };
        let command = session_command(
            &runtime,
            cwd,
            Some("example-argon-high"),
            Some("ultra"),
            Some(&context),
        );
        let command = command.as_std();
        assert_eq!(command.get_current_dir(), Some(cwd));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--settings=example",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--model",
                "example-argon-high",
            ]
            .map(OsStr::new)
        );
        let envs = command.get_envs().collect::<BTreeMap<_, _>>();
        assert_eq!(
            envs.get(OsStr::new(crate::agy_mcp::NATIVE_ENDPOINT_ENV)),
            Some(&Some(OsStr::new(&context.endpoint)))
        );
        assert_eq!(
            envs.get(OsStr::new(crate::agy_mcp::NATIVE_RUN_ENV)),
            Some(&Some(OsStr::new(
                &json!({"threadId":"example-thread","turnId":"example-turn","runId":"example-run","generation":7}).to_string()
            )))
        );
    }

    #[test]
    fn session_command_preserves_agy_effort_values() {
        let runtime = Runtime {
            executable: "example-runtime/agy".into(),
            arguments: Vec::new(),
            environment: HashMap::new(),
        };
        for effort in ["low", "high", "xhigh", "max", "precise-effort"] {
            let command = session_command(
                &runtime,
                Path::new("."),
                Some("exact model id"),
                Some(effort),
                None,
            );
            let args = command
                .as_std()
                .get_args()
                .map(OsStr::to_owned)
                .collect::<Vec<_>>();
            assert_eq!(args[args.len() - 4], OsStr::new("--model"), "{effort}");
            assert_eq!(
                args[args.len() - 3],
                OsStr::new("exact model id"),
                "{effort}"
            );
            assert_eq!(args[args.len() - 1], OsStr::new(effort), "{effort}");
            assert!(
                args.iter().any(|arg| arg == OsStr::new("--effort")),
                "{effort}"
            );
        }
    }

    #[test]
    fn temporary_agent_definition_and_cli_selection_preserve_guidance_and_model() {
        let name = "discodex-run-fixture";
        let instructions = "Native base instructions\n\nproactive mode";
        assert_eq!(
            temporary_agent_definition(name, instructions),
            "---\nname: discodex-run-fixture\ndescription: Per-run Codex execution guidance.\n---\n\nNative base instructions\n\nproactive mode"
        );
        let runtime = Runtime {
            executable: "example-runtime/agy".into(),
            arguments: vec!["--settings=example".to_owned()],
            environment: HashMap::new(),
        };
        let command = session_command_with_agent(
            &runtime,
            Path::new("fixture-workspace"),
            Some("example-argon-high"),
            Some("ultra"),
            None,
            Some(name),
        );
        let args = command
            .as_std()
            .get_args()
            .map(OsStr::to_owned)
            .collect::<Vec<_>>();
        assert!(
            args.windows(2)
                .any(|args| args == [OsStr::new("--agent"), OsStr::new(name)])
        );
        assert!(
            args.windows(2)
                .any(|args| args == [OsStr::new("--model"), OsStr::new("example-argon-high")])
        );
        assert!(!args.iter().any(|arg| arg == OsStr::new("--effort")));
    }

    #[test]
    fn unbound_session_command_removes_inherited_native_binding() {
        let runtime = Runtime {
            executable: "example-runtime/agy".into(),
            arguments: Vec::new(),
            environment: HashMap::from([
                (
                    crate::agy_mcp::NATIVE_ENDPOINT_ENV.to_owned(),
                    "inherited endpoint".to_owned(),
                ),
                (
                    crate::agy_mcp::NATIVE_RUN_ENV.to_owned(),
                    "inherited run".to_owned(),
                ),
                ("HOME".to_owned(), "preserved home".to_owned()),
            ]),
        };
        for (model, effort) in [(None, None), (Some("  "), Some(""))] {
            let command = session_command(&runtime, Path::new("."), model, effort, None);
            let command = command.as_std();
            assert_eq!(command.get_program(), runtime.executable.as_os_str());
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
                    (OsStr::new(crate::agy_mcp::NATIVE_ENDPOINT_ENV), None,),
                    (OsStr::new(crate::agy_mcp::NATIVE_RUN_ENV), None),
                    (OsStr::new("HOME"), Some(OsStr::new("preserved home"))),
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
    fn prompt_io_diagnostic_reports_status_and_bounded_stderr_tail() {
        let stderr = format!("discarded-prefix{}", "x".repeat(4096));
        let error = format_prompt_io_error(
            "write AGY prompt",
            &std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed"),
            "exited with status 23",
            &stderr,
        );
        let message = format!("{error:#}");
        assert!(message.contains("write AGY prompt"));
        assert!(message.contains("exited with status 23"));
        assert!(!message.contains("discarded-prefix"));
        assert!(message.ends_with(&format!("stderr: {})", "x".repeat(4096))));

        let empty = format_prompt_io_error(
            "flush AGY prompt",
            &std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed"),
            "still running",
            "\n",
        );
        assert!(format!("{empty:#}").ends_with("stderr: <empty>)"));
    }

    #[test]
    fn safe_stderr_retains_model_ids_and_public_urls_while_redacting_credentials() {
        let detail = sanitize_agy_diagnostic(
            "model=synthetic-model-high api_key=synthetic-secret https://docs.example/cli?view=help",
        );
        assert!(detail.contains("synthetic-model-high"));
        assert!(detail.contains("https://docs.example/cli?view=help"));
        assert!(!detail.contains("synthetic-secret"));

        let rendered = format_premature_exit(Some(2), "api_key=synthetic-secret");
        assert!(!rendered.contains("synthetic-secret"));
        assert!(rendered.contains("exit code 2"));
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
        ).unwrap();
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
        ).unwrap();
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
        ).unwrap();
        assert!(
            matches!(result, Event::Result { status: Some(status), error: Some(error), .. } if status == "ERROR" && error == "unsupported")
        );

        let subagent = parse_event(
            json!({"event":"step_update","step_update":{"step_type":"tool","subagent_info":{"subagents":[{"conversation_id":"child"}]}}}),
        ).unwrap();
        assert!(
            matches!(subagent, Event::SubagentStep { subagent_info: Some(info), .. } if info.pointer("/subagents/0/conversation_id").and_then(Value::as_str) == Some("child"))
        );

        let future = json!({"event":"future_event","payload":1});
        assert_eq!(parse_event(future.clone()).unwrap(), Event::Other(future));
        let future_step = json!({"event":"step_update","step_update":{"step_type":"future_step"}});
        assert_eq!(
            parse_event(future_step.clone()).unwrap(),
            Event::Other(future_step)
        );

        let malformed_delta = json!({
            "event":"step_update",
            "step_update":{"step_type":"agent_response","conversation_id":"c"}
        });
        let delta_error = parse_event(malformed_delta).unwrap_err();
        assert_eq!(
            delta_error.to_string(),
            "AGY agent_response step omitted its required text_delta"
        );
        let malformed_result = json!({"event":"result"});
        let result_error = parse_event(malformed_result).unwrap_err();
        assert_eq!(
            result_error.to_string(),
            "AGY stream-json result event omitted its required payload"
        );
        let missing_status =
            parse_event(json!({"event":"result","result":{"response":"done"}})).unwrap_err();
        assert_eq!(
            missing_status.to_string(),
            "AGY stream-json result payload omitted its required status"
        );
        let missing_step_type = parse_event(json!({
            "event":"step_update",
            "step_update":{"conversation_id":"c"}
        }))
        .unwrap_err();
        assert_eq!(
            missing_step_type.to_string(),
            "AGY stream-json step_update payload omitted its required step_type"
        );
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

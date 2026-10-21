//! Cancellable setup for the Discodex-managed official Antigravity CLI.
//!
//! Authentication remains owned by the CLI. No prompt is submitted to a model
//! and no account credential store is inspected by this adapter.

use anyhow::{Context, Result, bail};
use codex_utils_pty::{ChildFds, SpawnedProcess, TerminalSize};
use serde::Deserialize;
use sha2::{Digest, Sha512};
use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, oneshot, watch},
};

const RELEASE_BASE: &str = "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app";
pub const OWNERSHIP_FILE: &str = "discodex-managed.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupProgress {
    Checking,
    Installing,
    CheckingAccount,
    Authentication {
        output: String,
        url: Option<String>,
        accepts_code: bool,
        accepts_navigation: bool,
    },
    Verifying,
}

/// The caller selects the managed install location and shared/isolated CLI
/// environment. The setup backend never substitutes a global PATH executable.
#[derive(Clone, Debug)]
pub struct SetupConfig {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
    pub environment: HashMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct SetupReady {
    pub executable: PathBuf,
    pub catalog: crate::agy::Catalog,
}

pub struct SetupInput {
    pub data: String,
    pub acknowledgement: oneshot::Sender<InputDisposition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputDisposition {
    Delivered,
    NoLongerNeeded,
    NotAccepting,
}

#[derive(Debug)]
pub struct SetupCancelled;

impl fmt::Display for SetupCancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Antigravity setup cancelled")
    }
}

impl std::error::Error for SetupCancelled {}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow_and_update() || cancel.changed().await.is_err() {
            return;
        }
    }
}

fn check_cancelled(cancel: &watch::Receiver<bool>) -> Result<()> {
    if *cancel.borrow() || cancel.has_changed().is_err() {
        return Err(SetupCancelled.into());
    }
    Ok(())
}

fn emit(events: &mpsc::UnboundedSender<SetupProgress>, event: SetupProgress) -> Result<()> {
    events.send(event).map_err(|_| SetupCancelled.into())
}

#[derive(Debug)]
struct CommandOutput {
    code: i32,
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }

    fn require_success(&self, command: &str) -> Result<()> {
        if self.code != 0 {
            bail!(
                "{command} exited with status {}: {}",
                self.code,
                self.combined().trim()
            );
        }
        Ok(())
    }
}

async fn capture_process(
    mut process: SpawnedProcess,
    cancel: &mut watch::Receiver<bool>,
) -> Result<CommandOutput> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut exit = None;
    while stdout_open || stderr_open || exit.is_none() {
        tokio::select! {
            biased;
            _ = cancelled(cancel) => return Err(SetupCancelled.into()),
            chunk = process.stdout_rx.recv(), if stdout_open => match chunk {
                Some(chunk) => stdout.extend(chunk),
                None => stdout_open = false,
            },
            chunk = process.stderr_rx.recv(), if stderr_open => match chunk {
                Some(chunk) => stderr.extend(chunk),
                None => stderr_open = false,
            },
            code = &mut process.exit_rx, if exit.is_none() => {
                exit = Some(code.context("wait for Antigravity CLI process")?);
            },
        }
    }
    Ok(CommandOutput {
        code: exit.context("Antigravity CLI process returned no exit status")?,
        stdout: plain_terminal_text(&stdout),
        stderr: plain_terminal_text(&stderr),
    })
}

async fn command(
    config: &SetupConfig,
    args: &[&str],
    cancel: &mut watch::Receiver<bool>,
) -> Result<CommandOutput> {
    check_cancelled(cancel)?;
    let mut command_args = config.arguments.clone();
    command_args.extend(args.iter().map(|value| (*value).to_owned()));
    let process = codex_utils_pty::spawn_pipe_process_no_stdin(
        &config.executable,
        &command_args,
        &config.cwd,
        &config.environment,
        &None,
        &[],
    )
    .await
    .with_context(|| {
        format!(
            "run managed Antigravity CLI {}",
            config.executable.display()
        )
    })?;
    capture_process(process, cancel).await
}

/// Only the CLI's explicit missing-login diagnostic selects the interactive
/// login flow. Network, server and other failures retain their actual meaning.
fn requires_authentication(output: &CommandOutput) -> bool {
    output.code != 0
        && output.combined().contains(
            "Please sign in to view available models. Launch the CLI without arguments to sign in.",
        )
}

fn catalog(models: &CommandOutput, help: &CommandOutput) -> Result<crate::agy::Catalog> {
    models.require_success("agy models")?;
    help.require_success("agy --help")?;
    let mut parsed_models = crate::agy::parse_models(&models.stdout);
    if parsed_models.is_empty() {
        parsed_models = crate::agy::parse_models(&models.stderr);
    }
    let mut efforts = crate::agy::parse_efforts(&help.stdout);
    if efforts.is_empty() {
        efforts = crate::agy::parse_efforts(&help.stderr);
    }
    if parsed_models.is_empty() {
        bail!("Antigravity CLI returned no model rows");
    }
    if efforts.is_empty() {
        bail!("Antigravity CLI help returned no reasoning effort choices");
    }
    Ok(crate::agy::Catalog {
        models: parsed_models,
        efforts,
    })
}

/// Strip terminal styling/control sequences while keeping ordinary text and
/// tabs intact, including prompts that do not end in a newline.
fn plain_terminal_text(bytes: &[u8]) -> String {
    #[derive(Clone, Copy)]
    enum Escape {
        Text,
        Start,
        Csi,
        Osc,
        OscEscape,
    }
    let mut state = Escape::Text;
    let mut output = Vec::new();
    for &byte in bytes {
        state = match state {
            Escape::Text => match byte {
                0x1b => Escape::Start,
                b'\r' => Escape::Text,
                b'\n' | b'\t' | 0x20..=0xff => {
                    output.push(byte);
                    Escape::Text
                }
                _ => Escape::Text,
            },
            Escape::Start => match byte {
                b'[' => Escape::Csi,
                b']' => Escape::Osc,
                _ => Escape::Text,
            },
            Escape::Csi if (0x40..=0x7e).contains(&byte) => Escape::Text,
            Escape::Csi => Escape::Csi,
            Escape::Osc => match byte {
                7 => Escape::Text,
                0x1b => Escape::OscEscape,
                _ => Escape::Osc,
            },
            Escape::OscEscape if byte == b'\\' => Escape::Text,
            Escape::OscEscape => Escape::Osc,
        };
    }
    String::from_utf8_lossy(&output).into_owned()
}

/// Rejoin terminal rows that the emulator marked as soft-wrapped. The screen
/// API's `contents()` inserts line breaks at those points, which can split an
/// OAuth URL or a CLI prompt into separate tokens.
fn terminal_screen_text(screen: &vt100::Screen) -> String {
    let (_, columns) = screen.size();
    let mut output = String::new();
    for (index, row) in screen.rows(0, columns).enumerate() {
        if index > 0 && !screen.row_wrapped(index as u16 - 1) {
            output.push('\n');
        }
        output.push_str(&row);
    }
    output.trim_end().to_owned()
}

fn authentication_url(output: &str) -> Option<String> {
    let start = [
        "Open the URL below in your browser:",
        "Your browser should open automatically. If not:",
    ]
    .iter()
    .filter_map(|marker| output.rfind(marker))
    .max()?;
    output[start..].split_whitespace().find_map(|word| {
        let word = word.trim_matches(['<', '>', '"', '\'', '(', ')', ',', '.', ';']);
        let url = url::Url::parse(word).ok()?;
        matches!(url.scheme(), "https" | "http").then(|| word.to_owned())
    })
}

/// PTYs may echo stdin. Redact complete inputs and incomplete echoed suffixes
/// before publishing any CLI output, including when an echo is split by reads.
fn redact_inputs(output: &str, inputs: &[String]) -> String {
    let mut output = output.to_owned();
    for input in inputs.iter().filter(|input| !input.is_empty()) {
        output = output.replace(input, "[submitted]");
        for (index, _) in input.char_indices().rev() {
            if index > 0 && output.ends_with(&input[..index]) {
                output.truncate(output.len() - index);
                output.push_str("[submitted]");
                break;
            }
        }
    }
    output
}

#[derive(Deserialize)]
struct ReleaseManifest {
    version: String,
    url: String,
    sha512: String,
}

fn release_platform() -> Result<String> {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        bail!("Antigravity CLI has no official installer for this operating system")
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        bail!("Antigravity CLI has no official build for this CPU architecture")
    };
    let suffix = if cfg!(all(target_os = "linux", target_env = "musl")) {
        "_musl"
    } else {
        ""
    };
    Ok(format!("{os}_{arch}{suffix}"))
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    cancel: &mut watch::Receiver<bool>,
) -> Result<String> {
    let operation = async {
        let mut response = client.get(url).send().await?.error_for_status()?;
        let mut file = tokio::fs::File::create(destination).await?;
        let mut digest = Sha512::new();
        while let Some(chunk) = response.chunk().await? {
            digest.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok::<_, anyhow::Error>(format!("{:x}", digest.finalize()))
    };
    tokio::select! {
        biased;
        _ = cancelled(cancel) => Err(SetupCancelled.into()),
        result = operation => result.context("download official Antigravity CLI release"),
    }
}

pub fn is_owned_install(executable: &Path) -> bool {
    let Some(directory) = executable.parent() else {
        return false;
    };
    let Some(filename) = executable.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Ok(marker) = std::fs::read(directory.join(OWNERSHIP_FILE)) else {
        return false;
    };
    let Ok(marker) = serde_json::from_slice::<serde_json::Value>(&marker) else {
        return false;
    };
    marker["owner"] == "discodex"
        && marker["component"] == "antigravity-cli"
        && marker["executable"].as_str() == Some(filename)
}

pub async fn ensure_installed(
    config: &SetupConfig,
    cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
    check_cancelled(cancel)?;
    if config.executable.is_file() {
        return Ok(());
    }
    let manifest_url = format!("{RELEASE_BASE}/manifests/{}.json", release_platform()?);
    install_from_manifest(config, cancel, &manifest_url).await
}

async fn install_from_manifest(
    config: &SetupConfig,
    cancel: &mut watch::Receiver<bool>,
    manifest_url: &str,
) -> Result<()> {
    let directory = config
        .executable
        .parent()
        .context("managed AGY executable has no parent directory")?;
    tokio::fs::create_dir_all(directory)
        .await
        .context("create managed AGY install directory")?;
    let staging = tempfile::Builder::new()
        .prefix(".agy-setup-")
        .tempdir_in(directory)?;
    let client = reqwest::Client::new();
    let manifest: ReleaseManifest = tokio::select! {
        biased;
        _ = cancelled(cancel) => return Err(SetupCancelled.into()),
        result = async { client.get(manifest_url).send().await?.error_for_status()?.json().await } => {
            result.context("read official Antigravity CLI release manifest")?
        },
    };
    let payload = staging.path().join("payload");
    let digest = download(&client, &manifest.url, &payload, cancel).await?;
    if !digest.eq_ignore_ascii_case(&manifest.sha512) {
        bail!("Official Antigravity CLI download checksum does not match its release manifest");
    }
    check_cancelled(cancel)?;
    let binary = staging.path().join("binary");
    if url::Url::parse(&manifest.url)?.path().ends_with(".tar.gz") {
        let source = payload.clone();
        let destination = binary.clone();
        let extraction_cancel = cancel.clone();
        // The official archive contains a single flat native executable. Keep
        // the download's staging directory alive until extraction has stopped.
        let extracted = tokio::task::spawn_blocking(move || -> Result<()> {
            let archive = CancelledReader {
                reader: std::fs::File::open(source)?,
                cancel: extraction_cancel,
            };
            let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive));
            for entry in archive.entries()? {
                let mut entry = entry?;
                if entry.path()?.as_ref() == Path::new("antigravity") {
                    entry.unpack(destination)?;
                    return Ok(());
                }
            }
            bail!("Official Antigravity CLI archive contains no antigravity executable")
        })
        .await
        .context("extract official Antigravity CLI archive")?;
        check_cancelled(cancel)?;
        extracted?;
    } else {
        tokio::fs::rename(&payload, &binary).await?;
    }
    check_cancelled(cancel)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).await?;
    }
    let marker = serde_json::json!({
        "owner": "discodex",
        "component": "antigravity-cli",
        "version": manifest.version,
        "executable": config.executable.file_name().and_then(|name| name.to_str())
            .context("managed AGY filename is not UTF-8")?,
    });
    let staged_marker = staging.path().join(OWNERSHIP_FILE);
    tokio::fs::write(&staged_marker, serde_json::to_vec(&marker)?).await?;
    tokio::fs::rename(&staged_marker, directory.join(OWNERSHIP_FILE)).await?;
    check_cancelled(cancel)?;
    tokio::fs::rename(&binary, &config.executable)
        .await
        .context("place managed Antigravity CLI executable")?;
    Ok(())
}

struct CancelledReader<R> {
    reader: R,
    cancel: watch::Receiver<bool>,
}

impl<R: std::io::Read> std::io::Read for CancelledReader<R> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if check_cancelled(&self.cancel).is_err() {
            return Err(std::io::Error::other(SetupCancelled));
        }
        self.reader.read(output)
    }
}

/// None has one meaning: the official CLI explicitly requires sign-in.
pub async fn account_catalog(
    config: &SetupConfig,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Option<crate::agy::Catalog>> {
    let models = command(config, &["models"], cancel).await?;
    if requires_authentication(&models) {
        return Ok(None);
    }
    models.require_success("agy models")?;
    let help = command(config, &["--help"], cancel).await?;
    Ok(Some(catalog(&models, &help)?))
}

fn code_prompt(output: &str) -> bool {
    let output = output.split_whitespace().collect::<Vec<_>>().join(" ");
    output.contains(
        "After authenticating, copy the code displayed in the browser and paste it below:",
    ) || output
        .contains("If you aren't automatically redirected, paste the authorization code below:")
}

fn navigation_input(input: &str) -> bool {
    matches!(
        input,
        "\x1b[A" | "\x1b[B" | "\x1b[C" | "\x1b[D" | "\r" | "\n" | "\t" | "\x1b"
    )
}

async fn deliver_input(
    writer: &mpsc::Sender<Vec<u8>>,
    input: String,
    acknowledgement: oneshot::Sender<InputDisposition>,
    cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
    tokio::select! {
        biased;
        _ = cancelled(cancel) => return Err(SetupCancelled.into()),
        result = writer.send(input.into_bytes()) => result.context("write Antigravity sign-in response")?,
    }
    let _ = acknowledgement.send(InputDisposition::Delivered);
    Ok(())
}

/// Native auth is part of the ordinary CLI onboarding TUI, not an invented
/// login command. The metadata command determines when authentication is ready.
async fn authenticate(
    config: &SetupConfig,
    events: &mpsc::UnboundedSender<SetupProgress>,
    inputs: &mut mpsc::Receiver<SetupInput>,
    cancel: &mut watch::Receiver<bool>,
) -> Result<crate::agy::Catalog> {
    check_cancelled(cancel)?;
    let mut process = codex_utils_pty::spawn_pty_process(
        config
            .executable
            .to_str()
            .context("managed AGY executable path is not UTF-8")?,
        &config.arguments,
        &config.cwd,
        &config.environment,
        &None,
        TerminalSize::default(),
        ChildFds::Inherited(&[]),
    )
    .await
    .context("start Antigravity CLI sign-in")?;
    let writer = process.session.writer_sender();
    let mut terminal = vt100::Parser::default();
    let mut submitted = Vec::new();
    let mut login_url = None;
    let mut can_write_code = false;
    let mut awaiting_verification = false;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut inputs_open = true;
    let mut verification = tokio::task::JoinSet::<Result<Option<crate::agy::Catalog>>>::new();
    let mut poll = tokio::time::interval(Duration::from_secs(2));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = cancelled(cancel) => return Err(SetupCancelled.into()),
            _ = events.closed() => return Err(SetupCancelled.into()),
            code = &mut process.exit_rx => {
                process.session.close_stdin();
                if let Some(catalog) = account_catalog(config, cancel).await? {
                    return Ok(catalog);
                }
                bail!("Antigravity sign-in closed before an account was available (status {}): {}",
                    code.context("wait for Antigravity sign-in")?,
                    redact_inputs(&terminal_screen_text(terminal.screen()), &submitted).trim());
            },
            result = verification.join_next(), if !verification.is_empty() => {
                if let Some(catalog) = result.context("Antigravity verification task disappeared")??.context("check Antigravity account")? {
                    process.session.close_stdin();
                    process.session.terminate();
                    return Ok(catalog);
                }
            },
            _ = poll.tick(), if verification.is_empty() => {
                let config = config.clone();
                let mut cancel = cancel.clone();
                verification.spawn(async move { account_catalog(&config, &mut cancel).await });
            },
            chunk = process.stdout_rx.recv(), if stdout_open => match chunk {
                Some(chunk) => {
                    terminal.process(&chunk);
                    publish_auth_output(events, &terminal_screen_text(terminal.screen()), &submitted,
                        &mut login_url, &mut can_write_code, &mut awaiting_verification)?;
                },
                None => stdout_open = false,
            },
            chunk = process.stderr_rx.recv(), if stderr_open => match chunk {
                Some(chunk) => {
                    terminal.process(&chunk);
                    publish_auth_output(events, &terminal_screen_text(terminal.screen()), &submitted,
                        &mut login_url, &mut can_write_code, &mut awaiting_verification)?;
                },
                None => stderr_open = false,
            },
            input = inputs.recv(), if inputs_open => match input {
                Some(SetupInput { data: input, acknowledgement }) => {
                    let navigation = navigation_input(&input);
                    if awaiting_verification || (!navigation && !can_write_code) {
                        let _ = acknowledgement.send(InputDisposition::NotAccepting);
                        continue;
                    }
                    // Account status may have changed in the browser after the
                    // displayed prompt. Do not pass a late code to a chat prompt.
                    if let Some(catalog) = account_catalog(config, cancel).await? {
                        process.session.close_stdin();
                        process.session.terminate();
                        let _ = acknowledgement.send(InputDisposition::NoLongerNeeded);
                        return Ok(catalog);
                    }
                    if !navigation {
                        submitted.push(input.trim_end_matches(['\r', '\n']).to_owned());
                        can_write_code = false;
                        awaiting_verification = true;
                        emit(events, SetupProgress::Verifying)?;
                    }
                    deliver_input(&writer, input, acknowledgement, cancel).await?;
                },
                None => inputs_open = false,
            },
        }
    }
}

fn publish_auth_output(
    events: &mpsc::UnboundedSender<SetupProgress>,
    screen: &str,
    submitted: &[String],
    login_url: &mut Option<String>,
    can_write_code: &mut bool,
    awaiting_verification: &mut bool,
) -> Result<()> {
    if *awaiting_verification {
        // An unsuccessful code returns to the native error screen. Waiting
        // through ordinary redraws prevents a duplicate submission entering chat.
        if screen.contains("Got an error:") && screen.contains("Press any key to go back.") {
            *awaiting_verification = false;
        } else {
            return Ok(());
        }
    }
    *can_write_code = code_prompt(screen);
    let output = redact_inputs(screen, submitted);
    if let Some(url) = authentication_url(&output) {
        *login_url = Some(url);
    }
    emit(
        events,
        SetupProgress::Authentication {
            url: login_url.clone(),
            output,
            accepts_code: *can_write_code,
            accepts_navigation: !*awaiting_verification,
        },
    )
}

pub async fn prepare(
    config: SetupConfig,
    events: mpsc::UnboundedSender<SetupProgress>,
    mut inputs: mpsc::Receiver<SetupInput>,
    mut cancel: watch::Receiver<bool>,
) -> Result<SetupReady> {
    check_cancelled(&cancel)?;
    emit(&events, SetupProgress::Checking)?;
    if !config.executable.is_file() {
        emit(&events, SetupProgress::Installing)?;
        ensure_installed(&config, &mut cancel).await?;
    }
    emit(&events, SetupProgress::CheckingAccount)?;
    let catalog = match account_catalog(&config, &mut cancel).await? {
        Some(catalog) => catalog,
        None => authenticate(&config, &events, &mut inputs, &mut cancel).await?,
    };
    check_cancelled(&cancel)?;
    emit(&events, SetupProgress::Verifying)?;
    Ok(SetupReady {
        executable: config.executable,
        catalog,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::{broadcast, oneshot};

    struct FakeProcess {
        process: SpawnedProcess,
        output: broadcast::Sender<Vec<u8>>,
        exit: oneshot::Sender<i32>,
        terminated: Arc<AtomicUsize>,
    }

    fn fake_process() -> FakeProcess {
        let (writer_tx, _writer_rx) = mpsc::channel(1);
        let (output, stdout_rx) = broadcast::channel(16);
        let (exit, exit_rx) = oneshot::channel();
        let terminated = Arc::new(AtomicUsize::new(0));
        let terminated_for_driver = Arc::clone(&terminated);
        let process = codex_utils_pty::spawn_from_driver(codex_utils_pty::ProcessDriver {
            writer_tx,
            stdout_rx,
            stderr_rx: None,
            exit_rx,
            terminator: Some(Box::new(move || {
                terminated_for_driver.fetch_add(1, Ordering::SeqCst);
            })),
            writer_handle: None,
            resizer: None,
            #[cfg(windows)]
            tty: false,
        });
        FakeProcess {
            process,
            output,
            exit,
            terminated,
        }
    }

    #[tokio::test]
    async fn capture_drains_final_output_after_exit() -> Result<()> {
        let FakeProcess {
            process,
            output,
            exit,
            ..
        } = fake_process();
        let (_cancel, mut cancellation) = watch::channel(false);
        let task = tokio::spawn(async move { capture_process(process, &mut cancellation).await });
        output.send(b"\x1b[32mfirst".to_vec())?;
        exit.send(0).unwrap();
        tokio::task::yield_now().await;
        output.send(b" final\x1b[0m\r\n".to_vec())?;
        drop(output);
        let result = task.await??;
        assert_eq!(result.code, 0);
        assert_eq!(result.stdout, "first final\n");
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_terminates_owned_process() -> Result<()> {
        let FakeProcess {
            process,
            output: _output,
            exit: _exit,
            terminated,
        } = fake_process();
        let (cancel, mut cancellation) = watch::channel(false);
        let task = tokio::spawn(async move { capture_process(process, &mut cancellation).await });
        cancel.send(true)?;
        let error = task.await?.unwrap_err();
        assert!(error.downcast_ref::<SetupCancelled>().is_some());
        assert_eq!(terminated.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn acknowledged_input_is_the_exact_input_sent_to_native_writer() -> Result<()> {
        let (writer, mut received) = mpsc::channel(1);
        let (acknowledgement, acknowledged) = oneshot::channel();
        let (_cancel, mut cancellation) = watch::channel(false);
        deliver_input(
            &writer,
            "fictional-code\r".into(),
            acknowledgement,
            &mut cancellation,
        )
        .await?;
        assert_eq!(
            received.recv().await.as_deref(),
            Some(b"fictional-code\r".as_slice())
        );
        assert_eq!(acknowledged.await?, InputDisposition::Delivered);
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_a_full_native_input_channel_never_claims_delivery() -> Result<()> {
        let (writer, mut received) = mpsc::channel(1);
        writer.send(b"existing input".to_vec()).await?;
        let (acknowledgement, acknowledged) = oneshot::channel();
        let (cancel, mut cancellation) = watch::channel(false);
        let task = tokio::spawn(async move {
            deliver_input(
                &writer,
                "fictional-code\r".into(),
                acknowledgement,
                &mut cancellation,
            )
            .await
        });
        cancel.send(true)?;
        assert!(
            task.await?
                .unwrap_err()
                .downcast_ref::<SetupCancelled>()
                .is_some()
        );
        assert!(acknowledged.await.is_err());
        assert_eq!(
            received.recv().await.as_deref(),
            Some(b"existing input".as_slice())
        );
        assert!(received.recv().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn closed_native_input_channel_never_claims_delivery() {
        let (writer, received) = mpsc::channel(1);
        drop(received);
        let (acknowledgement, acknowledged) = oneshot::channel();
        let (_cancel, mut cancellation) = watch::channel(false);
        assert!(
            deliver_input(
                &writer,
                "fictional-code\r".into(),
                acknowledgement,
                &mut cancellation
            )
            .await
            .is_err()
        );
        assert!(acknowledged.await.is_err());
    }

    #[test]
    fn only_native_missing_login_diagnostic_starts_authentication() {
        let mut output = CommandOutput {
            code: 1, stdout: String::new(),
            stderr: "Error: Please sign in to view available models. Launch the CLI without arguments to sign in.".into(),
        };
        assert!(requires_authentication(&output));
        output.stderr = "Server unavailable (503); authentication service unreachable".into();
        assert!(!requires_authentication(&output));
        output.stderr = "AGY model discovery failed: No such file or directory".into();
        assert!(!requires_authentication(&output));
    }

    #[test]
    fn terminal_output_keeps_unicode_tabs_and_unterminated_prompt() {
        let output =
            b"\x1b]0;AGY\x07\x1b[2J\x1b[H\xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e\tEnter code: ";
        assert_eq!(plain_terminal_text(output), "日本語\tEnter code: ");
    }

    #[test]
    fn authentication_url_uses_native_login_instructions() {
        let output = "Documentation https://docs.example.test\nOpen the URL below in your browser:\nhttps://login.example.test/start?nonce=sample\n";
        assert_eq!(
            authentication_url(output).as_deref(),
            Some("https://login.example.test/start?nonce=sample")
        );
        assert_eq!(
            authentication_url("Server unavailable: https://status.example.test"),
            None
        );
    }

    #[test]
    fn submitted_code_is_hidden_in_split_echo_and_errors() {
        let submitted = vec!["fictional-auth-code".into()];
        assert_eq!(
            redact_inputs("Code: fictional-au", &submitted),
            "Code: [submitted]"
        );
        assert_eq!(
            redact_inputs("Code: fictional-auth-code\nTry again", &submitted),
            "Code: [submitted]\nTry again"
        );
    }

    #[test]
    fn native_code_prompt_allows_one_submission_and_error_can_retry() -> Result<()> {
        let (events, mut updates) = mpsc::unbounded_channel();
        let prompt = "Open the URL below in your browser:\nhttps://login.example.test\nAfter authenticating, copy the code displayed in the browser and paste it below:\n\n";
        let mut login_url = None;
        let mut accepts_code = false;
        let mut awaiting = false;
        publish_auth_output(
            &events,
            prompt,
            &[],
            &mut login_url,
            &mut accepts_code,
            &mut awaiting,
        )?;
        assert!(accepts_code);
        assert!(matches!(
            updates.try_recv()?,
            SetupProgress::Authentication {
                accepts_code: true,
                accepts_navigation: true,
                ..
            }
        ));

        awaiting = true;
        accepts_code = false;
        // Native TUI redraw while processing the submitted code is not a new
        // invitation to type arbitrary text.
        publish_auth_output(
            &events,
            prompt,
            &[],
            &mut login_url,
            &mut accepts_code,
            &mut awaiting,
        )?;
        assert!(updates.try_recv().is_err());
        assert!(!accepts_code);

        let error_output = "Got an error: code expired\nPress any key to go back.";
        publish_auth_output(
            &events,
            error_output,
            &[],
            &mut login_url,
            &mut accepts_code,
            &mut awaiting,
        )?;
        assert!(!awaiting);
        assert!(!accepts_code);
        assert!(matches!(
            updates.try_recv()?,
            SetupProgress::Authentication {
                accepts_code: false,
                accepts_navigation: true,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn terminal_redraw_replaces_old_code_prompt_and_preserves_wrapped_url() -> Result<()> {
        let (events, mut updates) = mpsc::unbounded_channel();
        let mut terminal = vt100::Parser::default();
        let mut login_url = None;
        let mut accepts_code = false;
        let mut awaiting = false;
        let url = format!("https://login.example.test/start?nonce={}", "a".repeat(120));
        let screen = format!(
            "Open the URL below in your browser:\r\n{url}\r\nAfter authenticating, copy the code displayed in the browser and paste it below:\r\n"
        );
        for chunk in screen.as_bytes().chunks(11) {
            terminal.process(chunk);
        }
        publish_auth_output(
            &events,
            &terminal_screen_text(terminal.screen()),
            &[],
            &mut login_url,
            &mut accepts_code,
            &mut awaiting,
        )?;
        assert!(accepts_code);
        assert_eq!(login_url.as_deref(), Some(url.as_str()));
        updates.try_recv()?;
        terminal.process(b"\x1b[2J\x1b[HSelect login method:\r\nBrowser\r\nRemote browser");
        publish_auth_output(
            &events,
            &terminal_screen_text(terminal.screen()),
            &[],
            &mut login_url,
            &mut accepts_code,
            &mut awaiting,
        )?;
        assert!(!accepts_code);
        let SetupProgress::Authentication { output, .. } = updates.try_recv()? else {
            panic!("expected native screen")
        };
        assert_eq!(output, "Select login method:\nBrowser\nRemote browser");
        Ok(())
    }

    #[test]
    fn catalog_preserves_official_model_and_effort_values() -> Result<()> {
        let models = CommandOutput {
            code: 0,
            stdout: "Fetching models...\nsample-model\tSample Model\n".into(),
            stderr: String::new(),
        };
        let help = CommandOutput {
            code: 0,
            stdout: "--effort Effort (quick|careful)".into(),
            stderr: String::new(),
        };
        let catalog = catalog(&models, &help)?;
        assert_eq!(catalog.models[0].id, "sample-model");
        assert_eq!(catalog.efforts, ["quick", "careful"]);
        Ok(())
    }

    #[test]
    fn ownership_requires_matching_managed_component_and_filename() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("agy");
        assert!(!is_owned_install(&executable));
        let marker = directory.path().join(OWNERSHIP_FILE);
        std::fs::write(
            &marker,
            br#"{"owner":"discodex","component":"antigravity-cli","executable":"agy"}"#,
        )?;
        assert!(is_owned_install(&executable));
        assert!(!is_owned_install(&directory.path().join("other-program")));
        std::fs::write(
            marker,
            br#"{"owner":"other-product","component":"antigravity-cli","executable":"agy"}"#,
        )?;
        assert!(!is_owned_install(&executable));
        Ok(())
    }

    #[test]
    fn cancellation_interrupts_archive_reads() -> Result<()> {
        use std::io::Read;
        let (cancel, receiver) = watch::channel(false);
        let mut reader = CancelledReader {
            reader: std::io::Cursor::new(b"binary"),
            cancel: receiver,
        };
        let mut output = [0; 1];
        assert_eq!(reader.read(&mut output)?, 1);
        cancel.send(true)?;
        assert!(reader.read(&mut output).is_err());
        Ok(())
    }

    async fn release_server(
        payload: Vec<u8>,
        checksum: String,
    ) -> Result<(String, tokio::task::JoinHandle<Result<()>>)> {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let base = format!("http://{}", listener.local_addr()?);
        let manifest = serde_json::to_vec(
            &serde_json::json!({"version":"1.2.3-test","url":format!("{base}/binary"),"sha512":checksum}),
        )?;
        let task = tokio::spawn(async move {
            for response in [manifest, payload] {
                let (mut socket, _) = listener.accept().await?;
                let mut request = Vec::new();
                loop {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).await?;
                    request.push(byte[0]);
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", response.len()).as_bytes()).await?;
                socket.write_all(&response).await?;
            }
            Ok(())
        });
        Ok((format!("{base}/manifest"), task))
    }

    fn setup_config(directory: &Path) -> SetupConfig {
        SetupConfig {
            executable: directory
                .join("runtime")
                .join("agy")
                .join(if cfg!(windows) { "agy.exe" } else { "agy" }),
            arguments: vec![],
            cwd: directory.to_owned(),
            environment: std::env::vars().collect(),
        }
    }

    #[tokio::test]
    async fn install_publishes_verified_prebuilt_and_ownership_without_running_it() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let config = setup_config(directory.path());
        let payload = b"test-only inert prebuilt bytes".to_vec();
        let (url, server) =
            release_server(payload.clone(), format!("{:x}", Sha512::digest(&payload))).await?;
        let (_cancel, mut cancellation) = watch::channel(false);
        install_from_manifest(&config, &mut cancellation, &url).await?;
        server.await??;
        assert_eq!(std::fs::read(&config.executable)?, payload);
        assert!(is_owned_install(&config.executable));
        assert_eq!(
            std::fs::read_dir(config.executable.parent().unwrap())?.count(),
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn checksum_failure_leaves_no_binary_or_temporary_download() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let config = setup_config(directory.path());
        let (url, server) =
            release_server(b"incorrect bytes".to_vec(), "bad-checksum".into()).await?;
        let (_cancel, mut cancellation) = watch::channel(false);
        let error = install_from_manifest(&config, &mut cancellation, &url)
            .await
            .unwrap_err();
        server.await??;
        assert!(error.to_string().contains("checksum"));
        assert!(!config.executable.exists());
        assert_eq!(
            std::fs::read_dir(config.executable.parent().unwrap())?.count(),
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_release_download_cleans_its_temporary_directory() -> Result<()> {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/manifest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "version":"1.2.3-test", "url": format!("{}/binary", server.uri()), "sha512": "unused-before-cancel",
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/binary"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(30))
                    .set_body_bytes(b"unused binary"),
            )
            .mount(&server)
            .await;
        let directory = tempfile::tempdir()?;
        let config = setup_config(directory.path());
        let config_for_task = config.clone();
        let (cancel, mut cancellation) = watch::channel(false);
        let manifest = format!("{}/manifest", server.uri());
        let task = tokio::spawn(async move {
            install_from_manifest(&config_for_task, &mut cancellation, &manifest).await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while server.received_requests().await.unwrap_or_default().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        cancel.send(true)?;
        let error = tokio::time::timeout(Duration::from_secs(2), task)
            .await??
            .unwrap_err();
        assert!(error.downcast_ref::<SetupCancelled>().is_some());
        assert!(!config.executable.exists());
        assert_eq!(
            std::fs::read_dir(config.executable.parent().unwrap())?.count(),
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn tar_release_installs_only_the_official_executable() -> Result<()> {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};
        let binary = b"inert executable bytes";
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(binary.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive.append_data(&mut header, "antigravity", binary.as_slice())?;
        let payload = archive.into_inner()?.finish()?;
        let server = MockServer::start().await;
        Mock::given(path("/manifest")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version":"1.2.3-test", "url": format!("{}/binary.tar.gz", server.uri()), "sha512": format!("{:x}", Sha512::digest(&payload)),
        }))).mount(&server).await;
        Mock::given(path("/binary.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(payload))
            .mount(&server)
            .await;
        let directory = tempfile::tempdir()?;
        let config = setup_config(directory.path());
        let (_cancel, mut cancellation) = watch::channel(false);
        install_from_manifest(
            &config,
            &mut cancellation,
            &format!("{}/manifest", server.uri()),
        )
        .await?;
        assert_eq!(std::fs::read(&config.executable)?, binary);
        assert!(is_owned_install(&config.executable));
        assert_eq!(
            std::fs::read_dir(config.executable.parent().unwrap())?.count(),
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn pre_cancelled_prepare_never_creates_installation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let config = setup_config(directory.path());
        let (events, _updates) = mpsc::unbounded_channel();
        let (_inputs, input) = mpsc::channel(1);
        let (_cancel, cancellation) = watch::channel(true);
        let error = prepare(config.clone(), events, input, cancellation)
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<SetupCancelled>().is_some());
        assert!(!config.executable.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prepared_account_uses_managed_executable_and_configuration_prefix() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let mut config = setup_config(directory.path());
        std::fs::create_dir_all(config.executable.parent().unwrap())?;
        std::fs::write(
            &config.executable,
            "#!/bin/sh\n[ \"$1\" = '--gemini_dir=/invented/settings' ] || exit 5\ncase \"$2\" in\n models) printf 'sample-model\\tSample Model\\n';;\n --help) printf '%s\\n' '--effort Effort (quick|careful)';;\n *) exit 6;;\nesac\n",
        )?;
        std::fs::set_permissions(&config.executable, std::fs::Permissions::from_mode(0o755))?;
        config
            .arguments
            .push("--gemini_dir=/invented/settings".into());
        let (events, mut updates) = mpsc::unbounded_channel();
        let (_input, input) = mpsc::channel(1);
        let (_cancel, cancel) = watch::channel(false);
        let ready = prepare(config.clone(), events, input, cancel).await?;
        assert_eq!(ready.executable, config.executable);
        assert_eq!(ready.catalog.models[0].id, "sample-model");
        assert_eq!(updates.try_recv()?, SetupProgress::Checking);
        assert_eq!(updates.try_recv()?, SetupProgress::CheckingAccount);
        assert_eq!(updates.try_recv()?, SetupProgress::Verifying);
        assert!(updates.try_recv().is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_onboarding_rejects_text_outside_code_prompt_then_verifies_code() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let mut config = setup_config(directory.path());
        std::fs::create_dir_all(config.executable.parent().unwrap())?;
        let account = directory.path().join("fake-account-ready");
        let received = directory.path().join("fake-received-code");
        config.environment.insert(
            "SETUP_TEST_ACCOUNT".into(),
            account.to_string_lossy().into_owned(),
        );
        config.environment.insert(
            "SETUP_TEST_RECEIVED".into(),
            received.to_string_lossy().into_owned(),
        );
        std::fs::write(
            &config.executable,
            r#"#!/bin/sh
case "$1" in
 models)
  if test -f "$SETUP_TEST_ACCOUNT"; then printf 'sample-model\tSample Model\n'; exit 0; fi
  printf '%s\n' 'Error: Please sign in to view available models. Launch the CLI without arguments to sign in.' >&2
  exit 1;;
 --help) printf '%s\n' '--effort Effort (quick|careful)'; exit 0;;
esac
printf 'Select login method:\r\nRemote browser\r\n'
IFS= read -r selection
printf '\033[2J\033[HOpen the URL below in your browser:\r\nhttps://login.example.test\r\nAfter authenticating, copy the code displayed in the browser and paste it below:\r\n'
IFS= read -r code
printf '%s' "$code" > "$SETUP_TEST_RECEIVED"
touch "$SETUP_TEST_ACCOUNT"
printf '\033[2J\033[HReady for a conversation\r\n'
while IFS= read -r forbidden; do printf '%s' "$forbidden" >> "$SETUP_TEST_RECEIVED"; done
"#,
        )?;
        std::fs::set_permissions(&config.executable, std::fs::Permissions::from_mode(0o755))?;
        let (events, mut progress) = mpsc::unbounded_channel();
        let (input, mut inputs) = mpsc::channel(4);
        let (_cancel, mut cancellation) = watch::channel(false);
        let task = tokio::spawn(async move {
            authenticate(&config, &events, &mut inputs, &mut cancellation).await
        });
        let interaction = async {
            loop {
                if let Some(SetupProgress::Authentication {
                    accepts_navigation: true,
                    accepts_code: false,
                    ..
                }) = progress.recv().await
                {
                    break;
                }
            }
            let (acknowledgement, acknowledged) = oneshot::channel();
            input
                .send(SetupInput {
                    data: "not-an-auth-code\r".into(),
                    acknowledgement,
                })
                .await?;
            assert_eq!(acknowledged.await?, InputDisposition::NotAccepting);
            let (acknowledgement, acknowledged) = oneshot::channel();
            input
                .send(SetupInput {
                    data: "\r".into(),
                    acknowledgement,
                })
                .await?;
            assert_eq!(acknowledged.await?, InputDisposition::Delivered);
            loop {
                if let Some(SetupProgress::Authentication {
                    accepts_code: true, ..
                }) = progress.recv().await
                {
                    break;
                }
            }
            let (acknowledgement, acknowledged) = oneshot::channel();
            input
                .send(SetupInput {
                    data: "fictional-code\r".into(),
                    acknowledgement,
                })
                .await?;
            assert_eq!(acknowledged.await?, InputDisposition::Delivered);
            let catalog = task.await??;
            assert_eq!(catalog.models[0].id, "sample-model");
            assert_eq!(std::fs::read_to_string(received)?, "fictional-code");
            while let Ok(SetupProgress::Authentication { output, .. }) = progress.try_recv() {
                assert!(!output.contains("fictional-code"));
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(10), interaction).await??;
        Ok(())
    }
}

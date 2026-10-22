#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use discodex::{config::Config, native::Native};
use serde_json::json;
#[cfg(windows)]
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    #[cfg(windows)]
    attach_parent_console_for_cli(command.as_deref());
    let Some(command) = command else {
        print_help();
        return Ok(());
    };
    let rest: Vec<String> = args.collect();

    // The MCP child uses stdout as its JSON-RPC transport. Dispatch before
    // initializing the normal application logger so no startup text can leak there.
    if command == "agy-hook" {
        return discodex::agy_hook::run(&rest);
    }
    if command == "harness-tools" {
        let endpoint = voice_tools_endpoint(&rest)?;
        if rest.iter().any(|arg| arg == "--native") {
            let binding = std::env::var(discodex::agy_mcp::NATIVE_RUN_ENV)
                .context("native MCP server requires a per-process Discodex response binding")?;
            let binding =
                serde_json::from_str(&binding).context("parse native MCP response binding")?;
            return discodex::harness::run_native(&endpoint, binding).await;
        }
        return discodex::harness::run(&endpoint).await;
    }
    if command == "voice-tools" {
        let endpoint = voice_tools_endpoint(&rest)?;
        return discodex::voice_tools::run(&endpoint, voice_tools_language(&rest)?).await;
    }

    if command == "run" {
        return run_command(&rest).await;
    }

    // MCP children returned above so their stdout remains a clean JSON-RPC stream.
    discodex::logging::init_stderr_logging()?;

    match command.as_str() {
        "run" => unreachable!("run command was handled before CLI logger setup"),
        "config" => {
            let config = Config::load(config_path(&rest)?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "language": config.language,
                    "profile": config.profile,
                    "name": config.name,
                    "guildId": config.guild_id,
                    "homeChannelId": config.home_channel_id,
                    "transcriptChannelId": config.transcript_channel_id,
                    "operatorIds": config.operator_ids,
                    "voiceChannelIds": config.voice_channel_ids,
                    "codexPath": config.codex_path,
                    "codexHome": config.codex_home,
                    "projectDir": config.project_dir,
                    "stateDir": config.state_dir,
                    "model": config.model,
                    "permissions": config.permissions,
                    "inputMode": config.input_mode,
                    "tokenConfigured": !config.token_env.is_empty() || config.credentials_file.is_some()
                }))?
            );
        }
        "native-probe" => {
            let config = Config::load(config_path(&rest)?)?;
            let native = Native::start(&config).await?;
            let probe = async {
                native
                    .request("account/read", json!({}))
                    .await
                    .context("query native account state")?;
                let profiles = native
                    .permission_profiles()
                    .await
                    .context("query native permission profile catalog")?;
                let snapshot = native
                    .request(
                        "thread/read",
                        json!({
                            "threadId": native.thread_id(),
                            "includeTurns": false
                        }),
                    )
                    .await
                    .context("read native thread settings")?;
                Ok::<_, anyhow::Error>((profiles, snapshot))
            }
            .await;
            let thread_id = native.thread_id();
            let shutdown = native.shutdown().await;
            let (profiles, snapshot) = probe?;
            shutdown?;
            let full_access_available = profiles
                .get("data")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("id").and_then(serde_json::Value::as_str)
                            == Some(":danger-full-access")
                            && item.get("allowed").and_then(serde_json::Value::as_bool)
                                == Some(true)
                    })
                });
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "native": "connected",
                    "threadId": thread_id,
                    "accountRead": "ok",
                    "fullAccessAvailable": full_access_available,
                    "threadStartOrResumeState": native.thread_state(),
                    "threadState": snapshot,
                    "permissionProfiles": profiles
                }))?
            );
        }
        // Installers use synchronous HTTP and service APIs. Leave the async
        // context before constructing or dropping their blocking clients.
        "install" => tokio::task::block_in_place(discodex::install::install)?,
        "migrate" => tokio::task::block_in_place(|| {
            discodex::install::migrate_installation(&config_path(&rest)?)
        })?,
        "update" => tokio::task::block_in_place(|| discodex::install::update(&rest))?,
        "update-helper" => {
            tokio::task::block_in_place(|| discodex::install::updates::run_helper(&rest))?
        }
        "uninstall" => tokio::task::block_in_place(discodex::install::uninstall)?,
        "install-plan" => tokio::task::block_in_place(discodex::install::print_plan)?,
        #[cfg(target_os = "macos")]
        "mac-permissions" => discodex::install::open_mac_permissions(&rest)?,
        #[cfg(target_os = "macos")]
        "mac-sign" => discodex::mac_signing::sign(&rest)?,
        #[cfg(target_os = "macos")]
        "mac-sign-in-login-session" => discodex::mac_signing::sign_in_login_session(&rest)?,
        #[cfg(target_os = "macos")]
        "mac-update" => tokio::task::block_in_place(|| discodex::install::update_mac(&rest))?,
        "help" | "--help" | "-h" => print_help(),
        other => bail!("unknown command `{other}`; run `discodex help`"),
    }
    Ok(())
}

async fn run_command(args: &[String]) -> Result<()> {
    let selected_config_path = config_path(args)?;
    let config = Config::load(&selected_config_path)?;
    let diagnostic_config = config.clone();
    discodex::logging::init_file_logging(&config.state_dir)?;

    let result = async {
        let _token = config.token()?;
        let shutdown = start_shutdown_listener();
        let native = Native::start_for_bridge(&config).await?;
        let result =
            discodex::discord::run(config, native.clone(), shutdown, selected_config_path).await;
        let shutdown = native.shutdown().await;
        result?;
        shutdown?;
        Ok(())
    }
    .await;

    if let Err(error) = &result {
        let detail =
            discodex::discord::sanitize_diagnostic(&diagnostic_config, &format!("{error:#}"));
        tracing::error!(error = %detail, "bridge process failed");
    }
    result
}

fn start_shutdown_listener() -> tokio::sync::oneshot::Receiver<std::io::Result<()>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = sender.send(shutdown_signal().await);
    });
    receiver
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(windows)]
    {
        tokio::signal::ctrl_c().await
    }
}

#[cfg(windows)]
fn attach_parent_console_for_cli(command: Option<&str>) {
    if matches!(
        command,
        Some("run" | "voice-tools" | "harness-tools" | "agy-hook")
    ) {
        return;
    }

    // Attach only when the operator launched a CLI command from an existing
    // console. AttachConsole preserves handles supplied with STARTF_USESTDHANDLES,
    // so PowerShell setup capture remains connected. Runtime and MCP modes skip
    // attachment entirely and keep their inherited stdio untouched.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn config_path(args: &[String]) -> Result<PathBuf> {
    if let Some(index) = args.iter().position(|arg| arg == "--config") {
        return args
            .get(index + 1)
            .map(PathBuf::from)
            .context("--config requires a JSON configuration path");
    }
    args.iter()
        .find(|arg| !arg.starts_with('-'))
        .map(PathBuf::from)
        .context("provide a JSON configuration path or --config <path>")
}

fn voice_tools_endpoint(args: &[String]) -> Result<PathBuf> {
    if let Some(index) = args.iter().position(|arg| arg == "--endpoint") {
        return args
            .get(index + 1)
            .map(PathBuf::from)
            .context("voice-tools --endpoint requires a filesystem path");
    }
    std::env::var_os(discodex::agy_mcp::NATIVE_ENDPOINT_ENV)
        .map(PathBuf::from)
        .context("native MCP server requires a per-process Discodex endpoint binding")
}

fn voice_tools_language(args: &[String]) -> Result<discodex::i18n::Language> {
    match args.iter().position(|arg| arg == "--language") {
        Some(index) => args
            .get(index + 1)
            .context("--language requires en, ko, zh, or ja")?
            .parse(),
        None => Ok(discodex::i18n::Language::default()),
    }
}

fn print_help() {
    println!(
        "discodex <command> [--config <path>]\n\
         Commands:\n\
           run            Start the Discord bridge and Codex app-server\n\
           config         Show non-secret profile configuration\n\
           native-probe   Connect to Codex and read native account state\n\
           install        Install a user-scoped bridge profile\n\
           update         Update an owned profile while preserving settings and sessions\n\
           migrate        Migrate an owned legacy profile to Discodex\n\
           uninstall      Remove bridge-owned installation items\n\
           install-plan   Show install and preserve/remove scope"
    );
    #[cfg(target_os = "macos")]
    println!(
        "           mac-permissions [--config <path> | --ownership <path>]\n\
         Reveal the owned worker in Finder and open Full Disk Access settings.\n\
           mac-sign --binary <staged-binary>\n\
         Sign the staged binary using this Mac's persistent signing identity.\n\
           mac-update --source <new-binary> --config <path> [--migrate-adhoc]\n\
         Update the owned executable while preserving settings and conversations."
    );
}

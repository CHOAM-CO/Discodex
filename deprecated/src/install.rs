use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, IsTerminal, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};

#[cfg(windows)]
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use crate::config::{Config, InputMode};
use crate::i18n::{Language, MessageKey};
use crate::message_access::{MessageAccess, MessagePolicy};
use crate::native_features::{NativeFeaturePreferences, NativeFeatureStore};

mod product;
mod runtime;
pub mod updates;

fn binary_directory_name() -> &'static str {
    &product::resources().product.binary_directory
}

const APP_NAME: &str = "discodex";
const SERVICE_NAME: &str = "discodex";
const LEGACY_APP_NAME: &str = "codex-worker";
const LEGACY_SERVICE_NAME: &str = "codex-worker-renewal";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallationList {
    format_version: u32,
    #[serde(default)]
    language: Language,
    profile: String,
    native_source: NativeSource,
    native_path: Option<PathBuf>,
    #[serde(default)]
    codex_home: Option<PathBuf>,
    /// A newly chosen installer action, consumed after applying it. Reinstalls
    /// otherwise preserve the current Discord settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    antigravity: Option<AntigravityInstallChoice>,
    discord_app: String,
    guild_id: u64,
    home_channel_id: u64,
    transcript_channel_id: u64,
    operator_ids: Vec<u64>,
    #[serde(default)]
    message_access: Option<MessageAccess>,
    /// Set only by a newly generated plan; consumed after the selected policy is
    /// written to durable state so subsequent reinstalls preserve runtime choices.
    #[serde(default)]
    message_access_plan_pending: bool,
    voice_channel_ids: Vec<u64>,
    project_dir: PathBuf,
    model: Option<String>,
    requested_permission_profile: String,
    created_at: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum AntigravityInstallChoice {
    Disabled,
    SharedSettings,
    SeparateSettings,
}

impl AntigravityInstallChoice {
    fn share_settings(self) -> Option<bool> {
        match self {
            Self::Disabled => None,
            Self::SharedSettings => Some(true),
            Self::SeparateSettings => Some(false),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum NativeSource {
    DesktopApp,
    ExistingCli,
    OfficialCore,
    ManagedCore,
}

fn merge_install_config(
    previous: Option<Config>,
    list: &InstallationList,
    credentials_path: &Path,
    codex_path: &Path,
    worker_path: Option<&Path>,
    state_dir: &Path,
    selected_codex_home: &Path,
) -> Result<Config> {
    let (
        name,
        permissions,
        voice,
        input_mode,
        effort,
        service_tier,
        memory_enabled,
        language,
        model,
        message_access,
    ) = match previous {
        Some(config) => (
            config.name,
            config.permissions,
            config.voice,
            config.input_mode,
            config.effort,
            config.service_tier,
            config.memory_enabled,
            configured_language(config.language)?,
            config.model.or_else(|| list.model.clone()),
            if list.message_access_plan_pending {
                list.message_access.clone().or(config.message_access)
            } else {
                config
                    .message_access
                    .or_else(|| list.message_access.clone())
            },
        ),
        None => (
            list.profile.clone(),
            None,
            None,
            InputMode::Steering,
            None,
            None,
            None,
            configured_language(list.language)?,
            list.model.clone(),
            list.message_access.clone(),
        ),
    };
    Ok(Config {
        language: env_language()?.unwrap_or(language),
        profile: list.profile.clone(),
        name,
        token_env: "DISCODEX_DISCORD_TOKEN".to_owned(),
        credentials_file: Some(credentials_path.to_owned()),
        guild_id: list.guild_id,
        home_channel_id: list.home_channel_id,
        transcript_channel_id: list.transcript_channel_id,
        operator_ids: list.operator_ids.clone(),
        message_access,
        voice_channel_ids: list.voice_channel_ids.clone(),
        codex_path: codex_path.to_owned(),
        worker_path: worker_path.map(Path::to_owned),
        codex_home: Some(selected_codex_home.to_owned()),
        project_dir: list.project_dir.clone(),
        state_dir: state_dir.to_owned(),
        model,
        effort,
        service_tier,
        memory_enabled,
        permissions,
        voice,
        input_mode,
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum WindowsTaskRunLevel {
    #[default]
    Limited,
    Highest,
}

#[cfg(windows)]
impl WindowsTaskRunLevel {
    fn powershell(self) -> &'static str {
        match self {
            Self::Limited => "Limited",
            Self::Highest => "Highest",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Ownership {
    format_version: u32,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    runtime_owned: bool,
    config_file: PathBuf,
    credentials_file: PathBuf,
    install_list_file: PathBuf,
    runtime_dir: PathBuf,
    runtime_executable: Option<PathBuf>,
    #[serde(default)]
    worker_executable: Option<PathBuf>,
    service_file: Option<PathBuf>,
    service_kind: Option<String>,
    #[serde(default)]
    windows_task_run_level: WindowsTaskRunLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    windows_task_pending_run_level: Option<WindowsTaskRunLevel>,
    codex_source: String,
    codex_path: PathBuf,
    codex_version: Option<String>,
    state_dir: PathBuf,
    project_dir: PathBuf,
    #[serde(default)]
    mac_signing_requirement: Option<String>,
}

#[cfg(windows)]
fn windows_task_may_be_replaced(state: &str, previous_record_owns_task: bool) -> bool {
    match state {
        "absent" => true,
        "matching" => previous_record_owns_task,
        _ => false,
    }
}

#[cfg(windows)]
fn windows_task_executable_path(path: &Path, language: Language) -> Result<PathBuf> {
    let mut value = path.to_str().context(
        language.message(MessageKey::InstallDiagnosticsWindowsTaskExecutablePathNotUtf8),
    )?;
    if let Some(unprefixed) = value.strip_prefix(r"\\?\") {
        value = unprefixed;
    }
    let bytes = value.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        bail!(
            "{}",
            language.message(MessageKey::InstallDiagnosticsWindowsTaskExecutablePathInvalid)
        )
    }
    Ok(PathBuf::from(value))
}

fn prepare_project_directory(path: &Path, language: Language) -> Result<PathBuf> {
    fs::create_dir_all(path).with_context(|| {
        format!(
            "{}",
            language
                .message(MessageKey::InstallDiagnosticsCreateProjectDirectory)
                .replace("{0}", &path.display().to_string())
        )
    })?;
    let canonical = path.canonicalize().with_context(|| {
        format!(
            "{}",
            language
                .message(MessageKey::InstallDiagnosticsResolveProjectDirectory)
                .replace("{0}", &path.display().to_string())
        )
    })?;
    ensure!(
        canonical.is_dir(),
        "{}",
        language.message(MessageKey::InstallDiagnosticsProjectPathNotDirectory)
    );
    Ok(canonical)
}

pub fn print_plan() -> Result<()> {
    let language = choose_language(None)?;
    print_plan_for_language(language)
}

fn print_plan_for_language(language: Language) -> Result<()> {
    let roots = roots()?;
    fs::create_dir_all(&roots.config).with_context(|| {
        format!(
            "{}",
            language
                .message(MessageKey::InstallDiagnosticsCreateConfigDirectory)
                .replace("{0}", &roots.config.display().to_string())
        )
    })?;
    set_private_dir(&roots.config)?;

    let profile = ask_text(language, MessageKey::InstallDiscordProfileName, "default")?;
    let native_source = NativeSource::ManagedCore;
    let native_path = None;
    let codex_home = Some(choose_codex_home(&roots, language)?);
    let antigravity = Some(choose_antigravity_install(language)?);
    let discord_app = ask_text(
        language,
        MessageKey::InstallDiscordApplicationProfileExistingApp,
        &profile,
    )?;
    let guild_id = ask_u64(language, MessageKey::InstallHomeGuildServerId)?;
    let home_channel_id = ask_u64(language, MessageKey::InstallHomeTextChannelId)?;
    let transcript_channel_id = ask_u64(language, MessageKey::InstallVoiceTranscriptChannelId)?;
    let voice_channel_ids = ask_ids(
        language,
        MessageKey::InstallAllowedVoiceChannelIdsCommaSeparated,
    )?;
    let operator_ids = ask_ids(language, MessageKey::InstallOperatorUserIdsCommaSeparated)?;
    let message_access = ask_install_message_access(
        language,
        &operator_ids,
        &[home_channel_id, transcript_channel_id],
    )?;
    let project_default = env::current_dir()
        .context(language.message(MessageKey::InstallDiagnosticsReadCurrentDirectory))?;
    let project_dir = prepare_project_directory(
        &PathBuf::from(ask_text(
            language,
            MessageKey::InstallCodexProjectWorkDirectory,
            &project_default.display().to_string(),
        )?),
        language,
    )?;
    let model = optional_value(
        language,
        MessageKey::InstallCodexModelLeaveEmptyForNativeDefault,
    )?;

    let list = InstallationList {
        format_version: 1,
        language,
        profile,
        native_source,
        native_path,
        codex_home,
        antigravity,
        discord_app,
        guild_id,
        home_channel_id,
        transcript_channel_id,
        operator_ids,
        message_access: Some(message_access),
        message_access_plan_pending: true,
        voice_channel_ids,
        project_dir,
        model,
        requested_permission_profile: "Allowed profile (Full Access preferred)".into(),
        created_at: unix_time_string(),
    };
    validate_existing_installation_plan(
        &roots.config.join("ownership.json"),
        &roots.config.join("config.json"),
        &list.profile,
    )?;
    let path = roots.config.join("installation-list.json");
    write_json_private(&path, &list)?;
    println!("Saved installation list: {}", path.display());
    println!(
        "The next `discodex install` step will ask for the Discord token privately and verify the selected services."
    );
    Ok(())
}

#[cfg(windows)]
fn checked_recorded_config_path(recorded: &Path, selected: &Path) -> Result<PathBuf> {
    ensure!(
        recorded.canonicalize().context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsResolveRecordedConfig,)
        )? == selected.canonicalize().context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsResolveSelectedConfig,)
        )?,
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsSelectedConfigNotOwned)
    );
    Ok(recorded.to_owned())
}

pub fn migrate_installation(config_path: &Path) -> Result<()> {
    let source_config = config_path.canonicalize().with_context(|| {
        format!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsResolveLegacyConfig)
                .replace("{0}", &config_path.display().to_string())
        )
    })?;
    let legacy_config_root = source_config
        .parent()
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsLegacyConfigParentMissing),
        )?
        .to_owned();
    ensure!(
        legacy_config_root
            .file_name()
            .and_then(|name| name.to_str())
            == Some(LEGACY_APP_NAME),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsMigrationConfigMustBeLegacy)
            .replace("{0}", LEGACY_APP_NAME)
    );
    let legacy_owner_path = legacy_config_root.join("ownership.json");
    let legacy: Ownership = read_json(&legacy_owner_path).with_context(|| {
        format!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsReadLegacyOwnership)
                .replace("{0}", &legacy_owner_path.display().to_string())
        )
    })?;
    // Windows canonical paths add a verbatim prefix. Prove file identity, then
    // retain the ownership record's spelling for sibling paths and task arguments.
    #[cfg(windows)]
    let source_config = checked_recorded_config_path(&legacy.config_file, &source_config)?;
    #[cfg(windows)]
    let legacy_config_root = source_config
        .parent()
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsRecordedLegacyConfigParentMissing),
        )?
        .to_owned();
    let old_data_root = legacy
        .state_dir
        .parent()
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsLegacyStateDataRootMissing),
        )?
        .to_owned();
    ensure!(
        legacy.state_dir == old_data_root.join("state")
            && old_data_root.file_name().and_then(|name| name.to_str()) == Some(LEGACY_APP_NAME),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsLegacyOwnershipLayoutInvalid)
    );

    let new_config = rename_legacy_path(&source_config);
    let new_data = rename_legacy_path(&old_data_root);
    let roots = Roots {
        config: new_config
            .parent()
            .context(
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsDiscodexConfigParentMissing),
            )?
            .to_owned(),
        data: new_data,
    };
    let migration_config_language = validate_legacy_migration_source(
        &legacy,
        &legacy_config_root,
        &old_data_root,
        &source_config,
    )?;
    ensure!(
        !roots.config.exists() && !roots.data.exists(),
        "{}",
        migration_config_language.message(MessageKey::InstallDiagnosticsMigrationDestinationExists)
    );

    let service_backup = validate_legacy_service(&legacy, migration_config_language)?;
    let migration_language = choose_language(Some(Config::load(&source_config)?.language))?;
    #[cfg(target_os = "macos")]
    if let Some(requirement) = legacy.mac_signing_requirement.as_deref() {
        let executable = legacy
            .worker_executable
            .as_deref()
            .context(
                migration_language.message(MessageKey::InstallDiagnosticsLegacySignedWorkerMissing),
            )?
            .canonicalize()
            .context(
                migration_language.message(MessageKey::InstallDiagnosticsResolveLegacySignedWorker),
            )?;
        crate::mac_signing::adopt_owned_binary_identifier(&executable, requirement).context(
            migration_language.message(MessageKey::InstallDiagnosticsPreserveLegacySigningIdentity),
        )?;
    }
    stage_legacy_profile(&legacy_config_root, &roots)?;
    let staged_worker = match stage_worker_file(&env::current_exe()?, &roots.data) {
        Ok(path) => path,
        Err(error) => {
            remove_migration_staging(&roots, &legacy_config_root).context(
                migration_language
                    .message(MessageKey::InstallDiagnosticsWorkerStageAndCleanupFailed),
            )?;
            return Err(error).context(
                migration_language
                    .message(MessageKey::InstallDiagnosticsStageWorkerBeforeStopFailed),
            );
        }
    };
    #[cfg(target_os = "macos")]
    if let Err(error) = sign_staged_migration_worker(
        &staged_worker,
        migration_language,
        legacy.mac_signing_requirement.as_deref(),
    ) {
        remove_migration_staging(&roots, &legacy_config_root).context(
            migration_language.message(MessageKey::InstallDiagnosticsWorkerSignAndCleanupFailed),
        )?;
        return Err(error).context(
            migration_language
                .message(MessageKey::InstallDiagnosticsPrepareSignedWorkerBeforeStopFailed),
        );
    }
    let _ = staged_worker;
    let mut stopped_ownership = legacy.clone();
    if let Err(error) = uninstall_user_service(
        &mut stopped_ownership,
        &legacy_owner_path,
        migration_language,
    ) {
        let staged_cleanup = remove_migration_staging(&roots, &legacy_config_root);
        let service_restore =
            restore_legacy_service(&service_backup, &legacy_owner_path, migration_language);
        if let Err(restore_error) = service_restore {
            return Err(anyhow!(
                "{}",
                migration_language
                    .message(MessageKey::InstallMigrationStopAndRestoreFailed)
                    .replace("{0}", &format!("{error:#}"))
                    .replace("{1}", &format!("{restore_error:#}"))
            ));
        }
        if let Err(cleanup_error) = staged_cleanup {
            return Err(anyhow!(
                "{}",
                migration_language
                    .message(MessageKey::InstallMigrationStopCleanupIncomplete)
                    .replace("{0}", &format!("{error:#}"))
                    .replace("{1}", &format!("{cleanup_error:#}"))
            ));
        }
        return Err(error).context(
            migration_language
                .message(MessageKey::InstallDiagnosticsMigrationStopLegacyServiceFailed),
        );
    }
    if let Err(error) =
        copy_private_directory(&old_data_root.join("state"), &roots.data.join("state"))
    {
        let staged_cleanup = remove_migration_staging(&roots, &legacy_config_root);
        let service_restore =
            restore_legacy_service(&service_backup, &legacy_owner_path, migration_language);
        if let Err(restore_error) = service_restore {
            return Err(anyhow!(
                "{}",
                migration_language
                    .message(MessageKey::InstallMigrationCopyAndRestoreFailed)
                    .replace("{0}", &format!("{error:#}"))
                    .replace("{1}", &format!("{restore_error:#}"))
            ));
        }
        if let Err(cleanup_error) = staged_cleanup {
            return Err(anyhow!(
                "{}",
                migration_language
                    .message(MessageKey::InstallMigrationCopyCleanupIncomplete)
                    .replace("{0}", &format!("{error:#}"))
                    .replace("{1}", &format!("{cleanup_error:#}"))
            ));
        }
        return Err(error).context(
            migration_language
                .message(MessageKey::InstallDiagnosticsMigrationCopyLegacyStateFailed),
        );
    }
    match install_at(
        roots.clone(),
        migration_language,
        legacy.worker_executable.as_deref(),
    ) {
        Ok(()) => {
            if let Err(error) = remove_migration_marker(&roots) {
                eprintln!(
                    "{}",
                    migration_language
                        .message(MessageKey::InstallDiagnosticsMigrationMarkerRemoveFailed)
                        .replace("{0}", &format!("{error:#}"))
                );
            }
            println!(
                "{}",
                migration_language
                    .message(MessageKey::InstallDiagnosticsMigrationCompleted)
                    .replace("{0}", legacy.profile.as_deref().unwrap_or("unknown"))
                    .replace("{1}", &legacy_config_root.display().to_string())
            );
            Ok(())
        }
        Err(error) => {
            let cleanup = rollback_new_migration_service(&roots, migration_language);
            if let Err(cleanup_error) = cleanup {
                return Err(anyhow!(
                    "{}",
                    migration_language
                        .message(MessageKey::InstallMigrationNewServiceCleanupFailed)
                        .replace("{0}", &format!("{error:#}"))
                        .replace("{1}", &format!("{cleanup_error:#}"))
                ));
            }
            let files_cleanup = remove_migration_staging(&roots, &legacy_config_root);
            restore_legacy_service(&service_backup, &legacy_owner_path, migration_language)
                .context(
                    migration_language.message(MessageKey::InstallMigrationRestorePreviousService),
                )?;
            if let Err(cleanup_error) = files_cleanup {
                Err(anyhow!(
                    "{}",
                    migration_language
                        .message(MessageKey::InstallMigrationFilesCleanupIncomplete)
                        .replace("{0}", &format!("{error:#}"))
                        .replace("{1}", &format!("{cleanup_error:#}"))
                ))
            } else {
                Err(error)
                    .context(migration_language.message(MessageKey::InstallMigrationFailedRestored))
            }
        }
    }
}

fn rename_legacy_path(path: &Path) -> PathBuf {
    let mut renamed = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name)
                if name == OsString::from(LEGACY_APP_NAME)
                    || name == OsString::from(LEGACY_SERVICE_NAME) =>
            {
                renamed.push(APP_NAME);
            }
            other => renamed.push(other.as_os_str()),
        }
    }
    renamed
}

struct LegacyServiceBackup {
    ownership: Ownership,
    service_contents: Option<String>,
    was_enabled: bool,
    was_active: bool,
}

#[cfg(target_os = "linux")]
fn systemctl_user_state(operation: &str, unit: &str, language: Language) -> Result<String> {
    let output = command_without_console("systemctl")
        .args(["--user", operation, unit])
        .stdin(Stdio::null())
        .output()
        .context(language.message(MessageKey::InstallDiagnosticsInspectLegacyServiceState))?;
    let state = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let known = match operation {
        "is-active" => [
            "active",
            "inactive",
            "failed",
            "activating",
            "deactivating",
            "reloading",
        ]
        .contains(&state.as_str()),
        "is-enabled" => [
            "enabled",
            "enabled-runtime",
            "linked",
            "linked-runtime",
            "alias",
            "disabled",
            "static",
            "indirect",
            "generated",
            "transient",
            "masked",
            "masked-runtime",
        ]
        .contains(&state.as_str()),
        _ => false,
    };
    ensure!(
        known,
        "{}",
        language
            .message(MessageKey::InstallDiagnosticsUnexpectedSystemctlState)
            .replace("{0}", operation)
            .replace("{1}", unit)
            .replace("{2}", &state)
    );
    Ok(state)
}

#[cfg(windows)]
fn inspect_windows_task_state(task_name: &str, language: Language) -> Result<(bool, bool)> {
    let task_name = ps_single_quote(task_name);
    let script = format!(
        r#"$ErrorActionPreference='Stop'; $task=Get-ScheduledTask -TaskPath '\' -TaskName '{task_name}'; Write-Output ($task.Settings.Enabled.ToString() + '|' + $task.State.ToString())"#
    );
    let output = command_without_console("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .output()
        .context(language.message(MessageKey::InstallDiagnosticsInspectWindowsTaskState))?;
    ensure!(
        output.status.success(),
        "{}",
        language.message(MessageKey::InstallDiagnosticsReadWindowsTaskStateFailed)
    );
    let value = String::from_utf8(output.stdout)?.trim().to_owned();
    let (enabled, state) = value
        .split_once('|')
        .context(language.message(MessageKey::InstallDiagnosticsWindowsTaskStateInvalid))?;
    let enabled = enabled.eq_ignore_ascii_case("true");
    ensure!(
        state == "Running" || state == "Ready" || state == "Disabled",
        "{}",
        language.message(MessageKey::InstallDiagnosticsWindowsTaskTransitional)
    );
    ensure!(
        (enabled && state != "Disabled") || (!enabled && state == "Disabled"),
        "{}",
        language.message(MessageKey::InstallDiagnosticsWindowsTaskStateDisagrees)
    );
    Ok((enabled, state == "Running"))
}

fn validate_legacy_service(
    ownership: &Ownership,
    language: Language,
) -> Result<LegacyServiceBackup> {
    let executable = ownership
        .worker_executable
        .as_deref()
        .context(language.message(MessageKey::InstallDiagnosticsLegacyWorkerMissing))?
        .canonicalize()
        .context(language.message(MessageKey::InstallDiagnosticsResolveLegacyOwnedWorker))?;
    let config = ownership
        .config_file
        .canonicalize()
        .context(language.message(MessageKey::InstallDiagnosticsResolveLegacyOwnedConfig))?;
    #[cfg(target_os = "linux")]
    {
        let path = ownership
            .service_file
            .as_deref()
            .context(language.message(MessageKey::InstallDiagnosticsLegacySystemdUnitMissing))?;
        let expected_name = format!("{LEGACY_SERVICE_NAME}.service");
        ensure!(
            ownership.service_kind.as_deref() == Some("systemd-user")
                && path.file_name().and_then(|name| name.to_str()) == Some(expected_name.as_str())
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    == Some("user")
                && path
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    == Some("systemd"),
            "{}",
            language.message(MessageKey::InstallDiagnosticsLegacySystemdOwnershipInvalid)
        );
        let expected_unit = format!(
            "[Unit]\nDescription=Codex Worker Discord bridge\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={} run --config {}\nRestart=on-failure\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n",
            systemd_quote(&executable),
            systemd_quote(&config)
        );
        let actual = fs::read_to_string(path)
            .context(language.message(MessageKey::InstallDiagnosticsReadLegacySystemdUnit))?;
        ensure!(
            actual == expected_unit,
            "{}",
            language.message(MessageKey::InstallDiagnosticsLegacySystemdUnitMismatch)
        );
        let was_enabled =
            systemctl_user_state("is-enabled", &expected_name, language)? == "enabled";
        let was_active = systemctl_user_state("is-active", &expected_name, language)? == "active";
        Ok(LegacyServiceBackup {
            ownership: ownership.clone(),
            service_contents: Some(actual),
            was_enabled,
            was_active,
        })
    }
    #[cfg(target_os = "macos")]
    {
        let (label, expected_path) = launchd_identity_for_service(&config, LEGACY_SERVICE_NAME)?;
        let path = ownership.service_file.as_deref().context(
            language.message(MessageKey::InstallDiagnosticsLegacyLaunchAgentPlistMissing),
        )?;
        ensure!(
            ownership.service_kind.as_deref() == Some("launchd-user") && path == expected_path,
            "{}",
            language.message(MessageKey::InstallDiagnosticsLegacyLaunchAgentOwnershipInvalid)
        );
        let expected = launchd_plist(&label, &executable, &config);
        ensure!(
            existing_launchd_plist_matches(path, &expected, true)?,
            "{}",
            language.message(MessageKey::InstallDiagnosticsLegacyLaunchAgentMissing)
        );
        let contents = fs::read_to_string(path)?;
        let was_active = launchd_is_loaded(&label, &executable, &config)?;
        Ok(LegacyServiceBackup {
            ownership: ownership.clone(),
            service_contents: Some(contents),
            was_enabled: true,
            was_active,
        })
    }
    #[cfg(windows)]
    {
        ensure!(
            ownership.service_kind.as_deref() == Some("scheduled-task-user")
                && ownership.service_file.is_none(),
            "{}",
            language.message(MessageKey::InstallDiagnosticsLegacyWindowsTaskOwnershipInvalid)
        );
        let task_executable = windows_task_executable_path(&executable, language)?;
        let expected_executable = ps_single_quote(&task_executable.display().to_string());
        let expected_arguments = ps_single_quote(&format!(
            "run --config \"{}\"",
            ownership.config_file.display()
        ));
        let (state, _) = inspect_owned_windows_task(
            &expected_executable,
            &expected_arguments,
            ownership,
            language,
        )?;
        ensure!(
            state == "matching",
            "{}",
            language.message(MessageKey::InstallDiagnosticsLegacyWindowsTaskMismatch)
        );
        let (was_enabled, was_active) = inspect_windows_task_state(
            windows_task_name_for_config(&ownership.config_file),
            language,
        )?;
        Ok(LegacyServiceBackup {
            ownership: ownership.clone(),
            service_contents: None,
            was_enabled,
            was_active,
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (executable, config);
        bail!(
            "{}",
            language.message(MessageKey::InstallDiagnosticsLegacyMigrationUnsupportedPlatform)
        )
    }
}

fn restore_legacy_service(
    backup: &LegacyServiceBackup,
    owner_file: &Path,
    language: Language,
) -> Result<()> {
    let mut ownership = backup.ownership.clone();
    #[cfg(target_os = "linux")]
    {
        let path = ownership
            .service_file
            .as_deref()
            .context(language.message(MessageKey::InstallDiagnosticsLegacySystemdUnitMissing))?;
        fs::write(
            path,
            backup.service_contents.as_deref().context(
                language.message(MessageKey::InstallDiagnosticsLegacySystemdSnapshotMissing),
            )?,
        )?;
        set_private_file(path)?;
        write_json_private(owner_file, &ownership)?;
        let name = path.file_name().and_then(|value| value.to_str()).context(
            language.message(MessageKey::InstallDiagnosticsLegacySystemdUnitFilenameInvalid),
        )?;
        run_checked("systemctl", &["--user", "daemon-reload"])?;
        if backup.was_enabled {
            run_checked("systemctl", &["--user", "enable", name])?;
        }
        if backup.was_active {
            run_checked("systemctl", &["--user", "start", name])?;
        }
    }
    #[cfg(target_os = "macos")]
    {
        let path = ownership.service_file.as_deref().context(
            language.message(MessageKey::InstallDiagnosticsLegacyLaunchAgentPlistMissing),
        )?;
        let contents = backup.service_contents.as_deref().context(
            language.message(MessageKey::InstallDiagnosticsLegacyLaunchAgentSnapshotMissing),
        )?;
        let config = ownership.config_file.canonicalize()?;
        let (label, _) = launchd_identity_for_service(&config, LEGACY_SERVICE_NAME)?;
        if path.exists() {
            ensure!(
                existing_launchd_plist_matches(
                    path,
                    &launchd_plist(
                        &label,
                        ownership.worker_executable.as_deref().context(
                            language.message(MessageKey::InstallDiagnosticsLegacyWorkerMissing,)
                        )?,
                        &config
                    ),
                    true
                )?,
                "{}",
                language.message(MessageKey::InstallDiagnosticsExistingLegacyLaunchAgentMismatch)
            );
        } else {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .with_context(|| {
                    format!(
                        "{}",
                        language
                            .message(MessageKey::InstallDiagnosticsRestoreLegacyLaunchAgent)
                            .replace("{0}", &path.display().to_string())
                    )
                })?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            set_private_file(path)?;
        }
        write_json_private(owner_file, &ownership)?;
        let domain = launchd_domain()?;
        if backup.was_active
            && !launchd_is_loaded(
                &label,
                ownership
                    .worker_executable
                    .as_deref()
                    .context(language.message(MessageKey::InstallDiagnosticsLegacyWorkerMissing))?,
                &config,
            )?
        {
            run_checked(
                "launchctl",
                &[
                    "bootstrap",
                    &domain,
                    path.to_str().context(
                        language
                            .message(MessageKey::InstallDiagnosticsLegacyLaunchAgentPathNotUtf8),
                    )?,
                ],
            )?;
            run_checked("launchctl", &["kickstart", &format!("{domain}/{label}")])?;
        }
    }
    #[cfg(windows)]
    {
        let executable = ownership
            .worker_executable
            .as_deref()
            .context(language.message(MessageKey::InstallDiagnosticsLegacyWorkerMissing))?;
        let task_executable = windows_task_executable_path(executable, language)?;
        let expected_executable = ps_single_quote(&task_executable.display().to_string());
        let expected_arguments = ps_single_quote(&format!(
            "run --config \"{}\"",
            ownership.config_file.display()
        ));
        write_json_private(owner_file, &ownership)?;
        let level = ownership.windows_task_run_level;
        let registration = windows_task_registration_script_named(
            &expected_executable,
            &expected_arguments,
            false,
            level,
            windows_task_name_for_config(&ownership.config_file),
        );
        run_elevated_powershell(&registration)?;
        if backup.was_active {
            let start = windows_task_start_script_named(
                &expected_executable,
                &expected_arguments,
                level,
                windows_task_name_for_config(&ownership.config_file),
            );
            run_powershell(&start)?;
        }
        if !backup.was_enabled {
            run_powershell(&format!(
                "Disable-ScheduledTask -TaskPath '\\' -TaskName '{}' | Out-Null",
                ps_single_quote(windows_task_name_for_config(&ownership.config_file))
            ))?;
        }
    }
    Ok(())
}

fn validate_legacy_migration_source(
    ownership: &Ownership,
    config_root: &Path,
    data_root: &Path,
    config_path: &Path,
) -> Result<Language> {
    ensure!(
        ownership.config_file == config_path
            && ownership.credentials_file == config_root.join("discord-token")
            && ownership.install_list_file == config_root.join("installation-list.json")
            && ownership.state_dir == data_root.join("state")
            && ownership.project_dir.is_absolute(),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsLegacyOwnershipPathsMismatch)
    );
    let config = Config::load(config_path).context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsReadLegacyProfileSettings),
    )?;
    let language = config.language;
    ensure!(
        !config.profile.is_empty()
            && ownership
                .profile
                .as_deref()
                .is_none_or(|profile| profile == config.profile)
            && config.credentials_file.as_deref() == Some(ownership.credentials_file.as_path())
            && config.state_dir == ownership.state_dir
            && config.project_dir == ownership.project_dir
            && config.codex_path == ownership.codex_path,
        "{}",
        language.message(MessageKey::InstallDiagnosticsLegacyConfigOwnershipMismatch)
    );
    let list: InstallationList = read_json(&ownership.install_list_file)
        .context(language.message(MessageKey::InstallDiagnosticsReadLegacyInstallationList))?;
    ensure!(
        list.profile == config.profile && list.project_dir == ownership.project_dir,
        "{}",
        language.message(MessageKey::InstallDiagnosticsLegacyInstallationListMismatch)
    );
    let credentials_metadata = fs::symlink_metadata(&ownership.credentials_file)
        .context(language.message(MessageKey::InstallDiagnosticsInspectLegacyCredentials))?;
    ensure!(
        credentials_metadata.file_type().is_file(),
        "{}",
        language.message(MessageKey::InstallDiagnosticsLegacyCredentialsNotRegular)
    );
    let executable = ownership
        .worker_executable
        .as_deref()
        .context(language.message(MessageKey::InstallDiagnosticsLegacyWorkerMissing))?
        .canonicalize()
        .context(language.message(MessageKey::InstallDiagnosticsResolveLegacyOwnedWorker))?;
    ensure!(
        executable.is_file() && is_owned_worker_executable(&executable, data_root),
        "{}",
        language.message(MessageKey::InstallDiagnosticsLegacyWorkerOutsideOwnedBin)
    );
    ensure!(
        ownership.runtime_dir
            == data_root.join(if ownership.codex_source == "official-core-owned" {
                "official-codex"
            } else {
                "codex-runtime"
            }),
        "{}",
        language.message(MessageKey::InstallDiagnosticsLegacyRuntimeDataRootMismatch)
    );
    Ok(language)
}

fn worker_executable_name() -> OsString {
    #[cfg(windows)]
    {
        OsString::from("discodex.exe")
    }
    #[cfg(not(windows))]
    {
        OsString::from("discodex")
    }
}

fn stage_worker_file(source: &Path, data_root: &Path) -> Result<PathBuf> {
    let source = source.canonicalize().context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsResolveRunningDiscodex),
    )?;
    ensure!(
        fs::symlink_metadata(&source)?.file_type().is_file(),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsRunningDiscodexNotRegular)
    );
    let bin = data_root.join("bin");
    create_private_dir(&bin)?;
    let destination = bin.join(worker_executable_name());
    copy_private_regular_file(&source, &destination)?;
    set_executable(&destination)?;
    destination.canonicalize().context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsResolveStagedDiscodexWorker),
    )
}

#[cfg(target_os = "macos")]
fn sign_staged_migration_worker(
    path: &Path,
    language: Language,
    expected: Option<&str>,
) -> Result<()> {
    use crate::mac_signing::{sign_binary_with_language, verify_binary};
    let requirement = sign_binary_with_language(path, language)
        .context(language.message(MessageKey::InstallDiagnosticsSignStagedDiscodexWorker))?;
    if let Some(expected) = expected {
        ensure!(
            requirement == expected,
            "{}",
            language.message(MessageKey::InstallDiagnosticsStagedDiscodexSigningIdentityMismatch)
        );
    }
    verify_binary(path, &requirement)
        .context(language.message(MessageKey::InstallDiagnosticsVerifyStagedDiscodexSignature))
}

fn recorded_service_executable(ownership: &Ownership) -> Result<Option<PathBuf>> {
    let Some(recorded) = ownership.worker_executable.as_deref() else {
        return Ok(None);
    };
    let data_root = ownership.state_dir.parent().context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsOwnedStateDataRootMissing),
    )?;
    ensure!(
        ownership.state_dir == data_root.join("state"),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsOwnedWorkerDataLayoutMismatch)
    );
    let executable = recorded.canonicalize().with_context(|| {
        format!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsResolveRecordedWorker)
                .replace("{0}", &recorded.display().to_string())
        )
    })?;
    ensure!(
        recorded == executable
            && executable.is_file()
            && is_owned_worker_executable(&executable, data_root),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsRecordedWorkerOutsideOwnedBin)
    );
    Ok(Some(executable))
}

fn staged_migration_worker(roots: &Roots) -> Result<Option<PathBuf>> {
    let marker = roots.config.join("migration-in-progress");
    let metadata = match fs::symlink_metadata(&marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context(
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsInspectMigrationMarker),
            );
        }
    };
    ensure!(
        metadata.file_type().is_file(),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsMigrationMarkerNotRegular)
    );
    let old_config_root: PathBuf = serde_json::from_slice(&fs::read(&marker).context(
        crate::i18n::Language::English.message(MessageKey::InstallDiagnosticsReadMigrationMarker),
    )?)
    .context(
        crate::i18n::Language::English.message(MessageKey::InstallDiagnosticsParseMigrationMarker),
    )?;
    ensure!(
        old_config_root.file_name().and_then(|name| name.to_str()) == Some(LEGACY_APP_NAME)
            && rename_legacy_path(&old_config_root) == roots.config,
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsMigrationMarkerProfileMismatch)
    );
    let candidate = roots.data.join("bin").join(worker_executable_name());
    if !candidate.is_file() {
        return Ok(None);
    }
    let candidate = candidate.canonicalize().context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsResolveStagedMigrationWorker),
    )?;
    let data_root = roots.data.canonicalize().context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsResolveMigrationDataRoot),
    )?;
    ensure!(
        candidate.starts_with(data_root.join("bin"))
            && is_owned_worker_executable(&candidate, &data_root),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsStagedMigrationWorkerOutsideOwnedBin)
    );
    Ok(Some(candidate))
}

fn stage_legacy_profile(old_config_root: &Path, roots: &Roots) -> Result<()> {
    let config_parent = roots.config.parent().context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsDiscodexConfigParentMissing),
    )?;
    let data_parent = roots.data.parent().context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsDiscodexDataParentMissing),
    )?;
    fs::create_dir_all(config_parent)?;
    fs::create_dir_all(data_parent)?;
    let config_stage = tempfile::tempdir_in(config_parent)?;
    let data_stage = tempfile::tempdir_in(data_parent)?;
    let staged_config = config_stage.path().join(APP_NAME);
    let staged_data = data_stage.path().join(APP_NAME);
    create_private_dir(&staged_config)?;
    create_private_dir(&staged_data)?;
    for name in ["config.json", "discord-token", "installation-list.json"] {
        copy_private_regular_file(&old_config_root.join(name), &staged_config.join(name))?;
    }
    fs::rename(&staged_config, &roots.config).with_context(|| {
        format!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsPublishStagedConfig)
                .replace("{0}", &roots.config.display().to_string())
        )
    })?;
    if let Err(error) = fs::rename(&staged_data, &roots.data) {
        let cleanup = fs::remove_dir_all(&roots.config);
        return match cleanup {
            Ok(()) => Err(error).with_context(|| {
                format!(
                    "{}",
                    crate::i18n::Language::English
                        .message(MessageKey::InstallDiagnosticsPublishStagedData)
                        .replace("{0}", &roots.data.display().to_string())
                )
            }),
            Err(cleanup_error) => Err(anyhow!(
                "{}",
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsPublishStagedDataAndCleanupFailed)
                    .replace("{0}", &roots.data.display().to_string())
                    .replace("{1}", &format!("{error}"))
                    .replace("{2}", &roots.config.display().to_string())
                    .replace("{3}", &format!("{cleanup_error}"))
            )),
        };
    }
    let marker = roots.config.join("migration-in-progress");
    if let Err(error) = fs::write(&marker, serde_json::to_vec(old_config_root)?)
        .and_then(|()| set_private_file(&marker).map_err(io::Error::other))
    {
        let config_cleanup = fs::remove_dir_all(&roots.config);
        let data_cleanup = fs::remove_dir_all(&roots.data);
        return Err(anyhow!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsWriteMigrationMarkerAndCleanup)
                .replace("{0}", &format!("{error}"))
                .replace("{1}", &format!("{config_cleanup:?}"))
                .replace("{2}", &format!("{data_cleanup:?}"))
        ));
    }
    Ok(())
}

fn remove_migration_marker(roots: &Roots) -> Result<()> {
    let marker = roots.config.join("migration-in-progress");
    match fs::symlink_metadata(&marker) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_file(),
                "{}",
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsMigrationMarkerNotRegular)
            );
            fs::remove_file(&marker).context(
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsRemoveCompletedMigrationMarker),
            )
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsInspectMigrationMarker),
        ),
    }
}

fn remove_migration_staging(roots: &Roots, old_config_root: &Path) -> Result<()> {
    let marker = roots.config.join("migration-in-progress");
    let metadata = fs::symlink_metadata(&marker).context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsVerifyStagedMigrationOwnership),
    )?;
    ensure!(
        metadata.file_type().is_file(),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsMigrationMarkerNotRegular)
    );
    ensure!(
        fs::read(&marker)? == serde_json::to_vec(old_config_root)?,
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsMigrationMarkerProfileMismatch)
    );
    for path in [&roots.config, &roots.data] {
        let metadata = fs::symlink_metadata(path).with_context(|| {
            format!(
                "{}",
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsInspectStagedMigrationRoot)
                    .replace("{0}", &path.display().to_string())
            )
        })?;
        ensure!(
            metadata.file_type().is_dir(),
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsStagedMigrationRootNotDirectory)
                .replace("{0}", &path.display().to_string())
        );
    }
    fs::remove_dir_all(&roots.data).with_context(|| {
        format!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsRemoveStagedData)
                .replace("{0}", &roots.data.display().to_string())
        )
    })?;
    fs::remove_dir_all(&roots.config).with_context(|| {
        format!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsRemoveStagedConfig)
                .replace("{0}", &roots.config.display().to_string())
        )
    })?;
    Ok(())
}

fn copy_private_regular_file(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).with_context(|| {
        format!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsInspectMigrationSource)
                .replace("{0}", &source.display().to_string())
        )
    })?;
    ensure!(
        metadata.file_type().is_file(),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsMigrationSourceNotRegular)
            .replace("{0}", &source.display().to_string())
    );
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| {
            format!(
                "{}",
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsCreateStagedMigrationFile)
                    .replace("{0}", &destination.display().to_string())
            )
        })?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    set_private_file(destination)?;
    Ok(())
}

fn copy_private_directory(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).with_context(|| {
        format!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsInspectMigrationStateDirectory)
                .replace("{0}", &source.display().to_string())
        )
    })?;
    ensure!(
        metadata.file_type().is_dir(),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsMigrationStateNotDirectory)
            .replace("{0}", &source.display().to_string())
    );
    create_private_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_dir() {
            copy_private_directory(&source_path, &destination_path)?;
        } else {
            copy_private_regular_file(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn rollback_new_migration_service(roots: &Roots, language: Language) -> Result<()> {
    let owner_path = roots.config.join("ownership.json");
    if !owner_path.is_file() {
        return Ok(());
    }
    let mut ownership: Ownership = read_json(&owner_path)?;
    uninstall_user_service(&mut ownership, &owner_path, language)
}

pub fn install() -> Result<()> {
    let roots = roots()?;
    let language = choose_language(saved_install_language(&roots)?)?;
    install_at(roots, language, None)
}

fn install_at(
    roots: Roots,
    language: Language,
    previous_worker_path_hint: Option<&Path>,
) -> Result<()> {
    fs::create_dir_all(&roots.config).with_context(|| {
        format!(
            "{}",
            language
                .message(MessageKey::InstallDiagnosticsCreateConfigDirectory)
                .replace("{0}", &roots.config.display().to_string())
        )
    })?;
    fs::create_dir_all(&roots.data).with_context(|| {
        format!(
            "{}",
            language
                .message(MessageKey::InstallDiagnosticsCreateDataDirectory)
                .replace("{0}", &roots.data.display().to_string())
        )
    })?;
    set_private_dir(&roots.config)?;
    set_private_dir(&roots.data)?;

    let list_path = roots.config.join("installation-list.json");
    if !list_path.exists() {
        println!(
            "{}",
            language.message(MessageKey::InstallNoInstallationListIsSavedCreateItBeforeS)
        );
        print_plan_for_language(language)?;
    }
    let list: InstallationList = read_json(&list_path)?;
    let config_path = roots.config.join("config.json");
    let credentials_path = roots.config.join("discord-token");
    let state_dir = roots.data.join("state");
    create_private_dir(&state_dir)?;
    let ownership_path = roots.config.join("ownership.json");
    let previous_ownership = if ownership_path.exists() {
        let previous: Ownership = read_json(&ownership_path)?;
        ensure!(
            same_profile_installation_paths(
                &previous,
                &config_path,
                &credentials_path,
                &list_path,
                &state_dir,
                &list.project_dir,
            ),
            "{}",
            language.message(MessageKey::InstallDiagnosticsOwnershipOtherProfile)
        );
        Some(previous)
    } else {
        None
    };
    let previous_config = if config_path.is_file() {
        Some(Config::load(&config_path).with_context(|| {
            format!(
                "{}",
                language
                    .message(MessageKey::InstallDiagnosticsReadExistingProfileConfig)
                    .replace("{0}", &config_path.display().to_string())
            )
        })?)
    } else {
        None
    };
    validate_existing_profile_identity(
        previous_ownership.is_some(),
        &list.profile,
        previous_ownership
            .as_ref()
            .and_then(|ownership| ownership.profile.as_deref()),
        previous_config
            .as_ref()
            .map(|config| config.profile.as_str()),
    )?;
    let worker_executable = owned_bridge_executable(&roots.data)
        .or(staged_migration_worker(&roots)?)
        .or_else(|| {
            let previous = previous_ownership.as_ref()?;
            let executable = previous.worker_executable.as_ref()?.canonicalize().ok()?;
            is_owned_worker_executable(&executable, &roots.data).then_some(executable)
        });
    let previous_worker_path = previous_worker_path_hint.or_else(|| {
        previous_ownership
            .as_ref()
            .and_then(|ownership| ownership.worker_executable.as_deref())
    });
    #[cfg(target_os = "macos")]
    let mac_worker_path_unchanged =
        mac_worker_path_unchanged(previous_worker_path, worker_executable.as_deref());
    #[cfg(not(target_os = "macos"))]
    let _ = previous_worker_path;
    // The selected release is installed before any profile/service changes.
    let managed_runtime = runtime::prepare(&roots.data)?;
    let codex_path = managed_runtime.executable.clone();
    let codex_version = Some(managed_runtime.manifest.codex_commit.clone());
    let mut ownership = Ownership {
        format_version: 1,
        profile: Some(list.profile.clone()),
        runtime_owned: true,
        config_file: config_path.clone(),
        credentials_file: credentials_path.clone(),
        install_list_file: list_path.clone(),
        runtime_dir: managed_runtime.directory.clone(),
        runtime_executable: Some(codex_path.clone()),
        worker_executable: worker_executable.clone(),
        service_file: previous_ownership
            .as_ref()
            .and_then(|previous| previous.service_file.clone()),
        service_kind: previous_ownership
            .as_ref()
            .and_then(|previous| previous.service_kind.clone()),
        windows_task_run_level: previous_ownership
            .as_ref()
            .map(|previous| previous.windows_task_run_level)
            .unwrap_or_default(),
        windows_task_pending_run_level: previous_ownership
            .as_ref()
            .and_then(|previous| previous.windows_task_pending_run_level),
        codex_source: "discodex-core-owned".into(),
        codex_path: codex_path.clone(),
        codex_version: codex_version.clone(),
        state_dir: state_dir.clone(),
        project_dir: list.project_dir.clone(),
        mac_signing_requirement: previous_ownership
            .as_ref()
            .and_then(|previous| previous.mac_signing_requirement.clone()),
    };
    let selected_env_home = list
        .codex_home
        .clone()
        .or_else(|| env::var_os("CODEX_HOME").map(PathBuf::from));
    let previous_home = previous_config
        .as_ref()
        .and_then(|config| config.codex_home.as_deref());
    if selected_env_home
        .as_ref()
        .is_some_and(|path| previous_home.is_none() && path.as_os_str().is_empty())
    {
        bail!(
            "{}",
            language.message(MessageKey::InstallDiagnosticsCodexHomeEmpty)
        );
    }
    let default_home = if previous_home.is_none() && selected_env_home.is_none() {
        Some(default_codex_home()?)
    } else {
        None
    };
    let selected_codex_home = resolve_codex_home(
        previous_home,
        selected_env_home.as_deref(),
        &list.project_dir,
        default_home.as_deref(),
    )?;
    let saved_token = if credentials_path.exists() {
        let metadata = fs::symlink_metadata(&credentials_path)
            .context(language.message(MessageKey::InstallDiagnosticsInspectSavedCredentials))?;
        ensure!(
            metadata.file_type().is_file(),
            "{}",
            language.message(MessageKey::InstallDiagnosticsSavedCredentialsNotRegular)
        );
        Some(
            fs::read_to_string(&credentials_path)
                .context(language.message(MessageKey::InstallDiagnosticsReadSavedCredentials))?,
        )
    } else {
        None
    };
    let token = select_token_environment(
        explicit_token_environment_value(
            "DISCODEX_DISCORD_TOKEN",
            env::var_os("DISCODEX_DISCORD_TOKEN"),
        ),
        || {
            explicit_token_environment_value(
                "CODEX_WORKER_DISCORD_TOKEN",
                env::var_os("CODEX_WORKER_DISCORD_TOKEN"),
            )
        },
        saved_token,
    )?;
    let token = if token.trim().is_empty() {
        rpassword::prompt_password(language.message(MessageKey::InstallDiscordBotTokenInputHidden))
            .context(language.message(MessageKey::InstallDiagnosticsReadDiscordTokenPrivately))?
    } else {
        token
    };
    if token.trim().is_empty() {
        bail!(
            "{}",
            language.message(MessageKey::InstallDiagnosticsDiscordTokenEmpty)
        );
    }
    write_secret(&credentials_path, token.trim())?;

    let mut config = merge_install_config(
        previous_config,
        &list,
        &credentials_path,
        &codex_path,
        worker_executable.as_deref(),
        &state_dir,
        &selected_codex_home,
    )?;
    write_json_private(&config_path, &config)?;
    if list.message_access_plan_pending {
        let access = list
            .message_access
            .as_ref()
            .or(config.message_access.as_ref())
            .context(language.message(MessageKey::InstallDiagnosticsMessagePolicyMissing))?;
        access.save(&state_dir.join("message-access.json"))?;
        let path = state_dir.join("message-access.json");
        set_private_file(&path)?;
    }

    println!("Config saved at {}", config_path.display());
    println!(
        "Selected Codex: {} ({})",
        codex_path.display(),
        codex_version.as_deref().unwrap_or("version not reported")
    );

    write_json_private(&ownership_path, &ownership)?;
    #[cfg(target_os = "macos")]
    if !mac_worker_path_unchanged {
        request_mac_fda_guidance(&ownership_path, language)?;
    }
    let discord = run_async(crate::commands::validate_discord(&config))
        .context(language.message(MessageKey::InstallDiagnosticsDiscordProfileCheckFailed))?;
    for id in [list.home_channel_id, list.transcript_channel_id] {
        let channel = discord
            .channels
            .iter()
            .find(|channel| channel.id == id)
            .ok_or_else(|| {
                anyhow!(
                    "{}",
                    language
                        .message(MessageKey::InstallDiagnosticsDiscordTextChannelOmitted)
                        .replace("{0}", &id.to_string())
                )
            })?;
        if channel.kind != "text" {
            bail!(
                "{}",
                language
                    .message(MessageKey::InstallDiagnosticsDiscordTextChannelKind)
                    .replace("{0}", &id.to_string())
                    .replace("{1}", &channel.kind)
            );
        }
    }
    for id in &list.voice_channel_ids {
        let channel = discord
            .channels
            .iter()
            .find(|channel| channel.id == *id)
            .ok_or_else(|| {
                anyhow!(
                    "{}",
                    language
                        .message(MessageKey::InstallDiagnosticsDiscordVoiceChannelOmitted)
                        .replace("{0}", &id.to_string())
                )
            })?;
        if channel.kind != "voice" && channel.kind != "stage" {
            bail!(
                "{}",
                language
                    .message(MessageKey::InstallDiagnosticsDiscordVoiceChannelKind)
                    .replace("{0}", &id.to_string())
                    .replace("{1}", &channel.kind)
            );
        }
    }
    for channel in &discord.channels {
        for warning in &channel.warnings {
            eprintln!(
                "{}",
                format!(
                    "{}: {warning}",
                    language.message(MessageKey::InstallDiscordChannelPermissionCheck)
                )
            );
        }
    }
    println!(
        "{}",
        language.message(MessageKey::InstallVerifiedTheBotSCurrentRoleAndChannelOver)
    );
    println!(
        "Discord API authenticated bot {} and can read guild {} ({}).",
        discord.bot_id, discord.guild_id, discord.guild_name
    );
    run_async(crate::commands::register_commands(&config)).context(
        language.message(MessageKey::InstallDiagnosticsDiscordCommandRegistrationFailed),
    )?;
    configure_antigravity_install(&config, list.antigravity)?;
    run_async(configure_native_profile(&mut config, &config_path)).context(
        language.message(MessageKey::InstallDiagnosticsCodexAccountOrPermissionCheckFailed),
    )?;
    let previous_run = crate::installation_check::read(&config)
        .ok()
        .flatten()
        .map(|report| report.run_id);
    install_user_service(
        &config_path,
        &mut ownership,
        previous_ownership.as_ref(),
        language,
    )?;
    write_json_private(&ownership_path, &ownership)?;
    let mut installed_list = list.clone();
    installed_list.message_access_plan_pending = false;
    installed_list.native_source = NativeSource::ManagedCore;
    installed_list.native_path = None;
    installed_list.codex_home = Some(selected_codex_home);
    installed_list.antigravity = None;
    write_json_private(&list_path, &installed_list)?;
    if let Some(previous) = &previous_ownership {
        retire_previous_runtime(previous, &managed_runtime);
    }
    println!(
        "{}",
        language.message(MessageKey::InstallCodexProviderAuthenticationSettingsWereChec)
    );
    confirm_installed_features(&config, previous_run)?;
    Ok(())
}

fn confirm_installed_features(config: &Config, previous_run: Option<String>) -> Result<()> {
    let language = configured_language(config.language)?;
    println!(
        "{}",
        language.message(MessageKey::InstallSendARealTextMessageInDiscordAndTrySett)
    );
    loop {
        let report = match crate::installation_check::read(config) {
            Ok(Some(report)) if Some(report.run_id.as_str()) != previous_run.as_deref() => {
                Some(report)
            }
            Ok(_) => None,
            Err(error) => {
                eprintln!(
                    "{}",
                    format!(
                        "{}: {error:#}",
                        language.message(
                            MessageKey::InstallCouldNotReadInstallationFeatureObservations
                        )
                    )
                );
                None
            }
        };
        if let Some(report) = report {
            println!("{}", report.summary(language));
            for (name, result) in report.features(language) {
                match result {
                    crate::installation_check::Observation::Failed { error } => eprintln!(
                        "{name}: {} — {error}",
                        language.message(MessageKey::InstallFailed)
                    ),
                    crate::installation_check::Observation::Incomplete { reason } => println!(
                        "{name}: {} — {reason}",
                        language.message(MessageKey::InstallIncomplete)
                    ),
                    _ => {}
                }
            }
            let durable = crate::state::StateStore::new(&config.state_dir).load();
            if report.ready()
                && durable
                    .as_ref()
                    .ok()
                    .and_then(|state| state.thread_id.as_deref())
                    == Some(report.thread_id.as_str())
            {
                println!(
                    "{}",
                    language.message(MessageKey::InstallReadyThisInstallSuccessfullyExercisedTextS)
                );
                return Ok(());
            }
        } else {
            println!(
                "{}",
                language.message(MessageKey::InstallNoFeatureResultFromTheCurrentInstallHasB)
            );
        }
        if !io::stdin().is_terminal() {
            println!(
                "{}",
                language.message(MessageKey::InstallSettingsAndConversationHistoryWerePreserved)
            );
            return Ok(());
        }
        print!(
            "{}",
            language.message(MessageKey::InstallPressEnterToReadResultsAgainOrQToExitW)
        );
        io::stdout().flush()?;
        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 || input.trim().eq_ignore_ascii_case("q") {
            println!(
                "{}",
                language.message(MessageKey::InstallSettingsAndConversationHistoryWerePreserved)
            );
            return Ok(());
        }
    }
}

pub fn uninstall() -> Result<()> {
    let roots = roots()?;
    let ownership_path = roots.config.join("ownership.json");
    if !ownership_path.exists() {
        println!(
            "No Discodex ownership record exists at {}",
            ownership_path.display()
        );
        return Ok(());
    }
    let ownership: Ownership = read_json(&ownership_path)?;

    println!(
        "Owned service: {}",
        ownership.service_kind.as_deref().unwrap_or("none")
    );
    println!("Owned config: {}", ownership.config_file.display());
    println!(
        "Owned credentials: {}",
        ownership.credentials_file.display()
    );
    println!(
        "Native thread state will be preserved at {}",
        ownership.state_dir.display()
    );
    println!(
        "External Codex app/CLI and its account, threads, projects, and conversation files will be preserved."
    );
    println!(
        "Discord application and server history will be preserved; registered commands will be removed when the token remains valid."
    );

    if let Some(executable) = &ownership.worker_executable {
        if !is_owned_worker_executable(executable, &roots.data) {
            bail!(
                "{}",
                Language::English
                    .message(MessageKey::InstallDiagnosticsUninstallWorkerOutsideOwnedBin)
                    .replace("{0}", &executable.display().to_string())
                    .replace("{1}", &roots.data.join("bin").display().to_string())
            )
        }
    }

    let config = if matches!(
        ownership.codex_source.as_str(),
        "official-core-owned" | "discodex-core-owned"
    ) && ownership.config_file.exists()
    {
        Some(Config::load(&ownership.config_file).context(
            Language::English.message(MessageKey::InstallDiagnosticsReadProfileBeforeUninstall),
        )?)
    } else {
        None
    };
    let mut ownership = ownership;
    let runtime_dir = prepare_owned_core_uninstall(&mut ownership, &roots.data, config.as_ref())?;
    uninstall_user_service(
        &mut ownership,
        &ownership_path,
        config
            .as_ref()
            .map_or(Language::English, |config| config.language),
    )?;
    remove_registered_commands(
        &ownership.config_file,
        &ownership.credentials_file,
        &ownership.state_dir,
        config
            .as_ref()
            .map_or(Language::English, |config| config.language),
    )
    .context(Language::English.message(MessageKey::InstallDiagnosticsRemoveDiscordCommandFailed))?;

    if let Some(runtime_dir) = &runtime_dir {
        remove_owned_core_runtime(runtime_dir)?;
        ownership.runtime_owned = false;
        write_json_private(&ownership_path, &ownership)?;
    }
    remove_owned_antigravity(&ownership.state_dir)?;

    #[cfg(windows)]
    if let Some(executable) = &ownership.worker_executable {
        if is_current_executable(executable) {
            defer_windows_self_cleanup(&ownership, &ownership_path, None, config.as_ref())?;
            println!(
                "Windows accepted the owned-file cleanup request. The helper will wait for this process to exit, then remove the recorded files and report the actual result."
            );
            return Ok(());
        }
    }

    if let Some(executable) = &ownership.worker_executable {
        remove_if_file(executable)?;
    }
    remove_if_file(&ownership.config_file)?;
    remove_if_file(&ownership.credentials_file)?;
    remove_if_file(&ownership.install_list_file)?;
    remove_if_file(&ownership_path)?;
    println!(
        "Uninstall finished. Native thread state and Codex user data remain at {}.",
        ownership.state_dir.display()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn open_mac_permissions(args: &[String]) -> Result<()> {
    let roots = roots()?;
    let ownership_path =
        mac_permissions_ownership_path(args, &roots.config.join("ownership.json"))?;
    let (ownership, executable) = owned_mac_worker(&ownership_path)?;
    let saved_language = ownership
        .config_file
        .is_file()
        .then(|| {
            Config::load(&ownership.config_file)
                .ok()
                .map(|config| config.language)
        })
        .flatten();
    let language = configured_language(saved_language.unwrap_or_default())?;
    open_mac_permissions_for_executable(&executable)?;
    println!(
        "{}",
        language
            .message(MessageKey::InstallInFinderAddTheSelectedExecutableToSystem)
            .replace("{}", &executable.display().to_string())
    );
    println!(
        "{}",
        language.message(MessageKey::InstallThisCommandDoesNotAutomaticallyCheckOrCha)
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn owned_mac_worker(ownership_path: &Path) -> Result<(Ownership, PathBuf)> {
    let ownership: Ownership = read_json(ownership_path).with_context(|| {
        format!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsReadOwnedInstallationRecord)
                .replace("{0}", &ownership_path.display().to_string())
        )
    })?;
    let data_root = ownership_data_root(&ownership)?;
    let recorded_executable = ownership.worker_executable.as_ref().context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsOwnedWorkerNotRecorded),
    )?;
    let executable = recorded_executable.canonicalize().with_context(|| {
        format!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsResolveOwnedWorker)
                .replace("{0}", &recorded_executable.display().to_string())
        )
    })?;
    ensure!(
        recorded_executable == &executable,
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsRecordedWorkerPathNotCanonical)
            .replace("{0}", &recorded_executable.display().to_string())
    );
    if !executable.is_file() || !is_owned_worker_executable(&executable, &data_root) {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedWorkerNotRegular)
                .replace("{0}", &executable.display().to_string())
        );
    }
    Ok((ownership, executable))
}

#[cfg(target_os = "macos")]
fn open_mac_permissions_for_executable(executable: &Path) -> Result<()> {
    let executable_text = executable.to_str().context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsOwnedWorkerPathNotUtf8),
    )?;
    run_checked("/usr/bin/open", &["-R", executable_text])?;
    run_checked(
        "/usr/bin/open",
        &["x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"],
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_mac_fda_interactive_terminal_state(
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
    executable: &Path,
    operation: &str,
) -> Result<()> {
    ensure!(
        stdin_is_terminal && stdout_is_terminal,
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsMacFdaNeedsTerminal)
            .replace("{0}", operation)
            .replace("{1}", &executable.display().to_string())
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_mac_fda_interactive_terminal(executable: &Path, operation: &str) -> Result<()> {
    require_mac_fda_interactive_terminal_state(
        io::stdin().is_terminal(),
        io::stdout().is_terminal(),
        executable,
        operation,
    )
}

#[cfg(target_os = "macos")]
fn mac_worker_path_unchanged(previous: Option<&Path>, selected: Option<&Path>) -> bool {
    let Some(previous) = previous.and_then(|path| path.canonicalize().ok()) else {
        return false;
    };
    let Some(selected) = selected.and_then(|path| path.canonicalize().ok()) else {
        return false;
    };
    previous == selected && fs::metadata(&selected).is_ok_and(|metadata| metadata.is_file())
}

#[cfg(target_os = "macos")]
fn mac_fda_guidance_answer(read_count: usize, answer: &str) -> Result<bool> {
    ensure!(
        read_count != 0,
        "{}",
        crate::i18n::Language::English.message(MessageKey::InstallDiagnosticsMacFdaNoResponse)
    );
    match answer.trim() {
        "" => Ok(true),
        "q" | "Q" => Ok(false),
        _ => bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacFdaInvalidAnswer)
        ),
    }
}

#[cfg(target_os = "macos")]
fn request_mac_fda_guidance(ownership_path: &Path, language: Language) -> Result<()> {
    use crate::mac_signing::{expected_requirement, verify_binary};

    let (ownership, executable) = owned_mac_worker(ownership_path)?;
    let requirement = expected_requirement()
        .context(language.message(MessageKey::InstallDiagnosticsReadMacSigningIdentity))?;
    if let Some(pin) = ownership.mac_signing_requirement.as_deref() {
        ensure!(
            pin == requirement,
            "{}",
            language.message(MessageKey::InstallDiagnosticsOwnedSigningIdentityMismatch)
        );
    }
    verify_binary(&executable, &requirement)
        .context(language.message(MessageKey::InstallDiagnosticsVerifyOwnedWorkerBeforeFda))?;
    require_mac_fda_interactive_terminal(
        &executable,
        language.message(MessageKey::InstallDiagnosticsFdaSetupOperationReason),
    )?;
    open_mac_permissions_for_executable(&executable).with_context(|| {
        format!(
            "{}",
            language
                .message(MessageKey::InstallDiagnosticsOpenFdaGuidance)
                .replace("{0}", &executable.display().to_string())
        )
    })?;
    println!(
        "{}",
        language
            .message(MessageKey::InstallSelectedSignedWorkerExecutable)
            .replace("{}", &executable.display().to_string())
    );
    println!(
        "{}",
        language.message(MessageKey::InstallMacosMayAskThisExecutableForProtectedFold)
    );
    println!(
        "{}",
        language.message(MessageKey::InstallIfYouGrantFullDiskAccessSelectTheExactE)
    );
    print!(
        "{}",
        language.message(MessageKey::InstallContinueAfterFdaGuidanceEnterCancelQ)
    );
    io::stdout().flush()?;
    let mut answer = String::new();
    let read = io::stdin().read_line(&mut answer)?;
    if mac_fda_guidance_answer(read, &answer)? {
        Ok(())
    } else {
        bail!(
            "{}",
            language
                .message(MessageKey::InstallDiagnosticsMacFdaCancelled)
                .replace("{0}", &executable.display().to_string())
        )
    }
}

#[cfg(target_os = "macos")]
fn ownership_data_root(ownership: &Ownership) -> Result<PathBuf> {
    let state_dir = ownership.state_dir.canonicalize().with_context(|| {
        format!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsResolveOwnedStateDirectory)
                .replace("{0}", &ownership.state_dir.display().to_string())
        )
    })?;
    if state_dir.file_name().and_then(|name| name.to_str()) != Some("state") {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedStateLayoutExpected)
                .replace("{0}", &state_dir.display().to_string())
        );
    }
    let data_root = state_dir
        .parent()
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedStateProfileDataRootMissing),
        )?
        .to_owned();
    if !ownership.runtime_dir.is_absolute() || !ownership.runtime_dir.starts_with(&data_root) {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedRuntimeOutsideDataRoot)
                .replace("{0}", &ownership.runtime_dir.display().to_string())
        );
    }
    Ok(data_root)
}

#[cfg(target_os = "macos")]
fn mac_permissions_ownership_path(args: &[String], default: &Path) -> Result<PathBuf> {
    let mut config = None;
    let mut ownership = None;
    let mut index = 0;
    while index < args.len() {
        let option = args[index].as_str();
        if matches!(option, "--config" | "--ownership") {
            let value = args.get(index + 1).context(
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsMacPermissionsPathRequired),
            )?;
            if value.starts_with('-') {
                bail!(
                    "{}",
                    crate::i18n::Language::English
                        .message(MessageKey::InstallDiagnosticsMacPermissionsPathRequired)
                );
            }
            if option == "--config" {
                if config.replace(PathBuf::from(value)).is_some() {
                    bail!(
                        "{}",
                        crate::i18n::Language::English
                            .message(MessageKey::InstallDiagnosticsMacPermissionsConfigRepeated)
                    );
                }
            } else if ownership.replace(PathBuf::from(value)).is_some() {
                bail!(
                    "{}",
                    crate::i18n::Language::English
                        .message(MessageKey::InstallDiagnosticsMacPermissionsOwnershipRepeated)
                );
            }
            index += 2;
        } else {
            bail!(
                "{}",
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsMacPermissionsUnknownOption)
                    .replace("{0}", option)
            );
        }
    }
    if config.is_some() && ownership.is_some() {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacPermissionsConfigOwnershipExclusive)
        );
    }
    Ok(ownership.unwrap_or_else(|| {
        config.map_or_else(
            || default.to_owned(),
            |path| {
                path.parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("ownership.json")
            },
        )
    }))
}

struct UpdateArgs {
    source: PathBuf,
    config: PathBuf,
}

fn parse_update_args(args: &[String]) -> Result<UpdateArgs> {
    let (mut source, mut config) = (None, None);
    let mut i = 0;
    while i < args.len() {
        let option = args[i].as_str();
        ensure!(
            option == "--source" || option == "--config",
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsUpdateUnknownOption)
                .replace("{0}", option)
        );
        let value = args.get(i + 1).context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsUpdateOptionRequiresPath),
        )?;
        ensure!(
            !value.starts_with('-'),
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsUpdateOptionRequiresPath)
        );
        let slot = if option == "--source" {
            &mut source
        } else {
            &mut config
        };
        ensure!(
            slot.replace(PathBuf::from(value)).is_none(),
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsUpdateOptionRepeated)
                .replace("{0}", option)
        );
        i += 2;
    }
    Ok(UpdateArgs {
        source: source.context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsUpdateSourceRequired),
        )?,
        config: config.context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsUpdateConfigRequired),
        )?,
    })
}

pub fn update(args: &[String]) -> Result<()> {
    // Let the incoming executable select its own runtime, including when a user
    // invokes `update --source` through the currently installed executable.
    #[cfg(target_os = "macos")]
    let source = parse_mac_update_args(args)?.source;
    #[cfg(not(target_os = "macos"))]
    let source = parse_update_args(args)?.source;
    if source.canonicalize()? != env::current_exe()?.canonicalize()? {
        let status = Command::new(&source)
            .arg("update")
            .args(args)
            .status()
            .context(
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsRunIncomingUpdateInstaller),
            )?;
        ensure!(
            status.success(),
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsIncomingWorkerUpdateFailed)
                .replace("{0}", &status.to_string())
        );
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        update_mac(args)
    }
    #[cfg(any(target_os = "linux", windows))]
    {
        update_owned_binary(parse_update_args(args)?)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = args;
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsBinaryUpdatesUnsupportedPlatform)
        )
    }
}

fn resolve_profile_ownership(config_path: &Path) -> Result<Ownership> {
    ensure!(
        config_path.file_name().and_then(|name| name.to_str()) == Some("config.json")
            && config_path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some(APP_NAME),
        "selected config is not inside a Discodex profile directory"
    );
    let owner_path = config_path
        .parent()
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsConfigPathParentMissing),
        )?
        .join("ownership.json");
    let owner: Ownership = read_json(&owner_path)
        .with_context(|| format!("read profile ownership record {}", owner_path.display()))?;
    ensure!(
        owner.config_file.canonicalize()? == config_path,
        "ownership record points at another profile config"
    );
    Ok(owner)
}

#[cfg(any(target_os = "linux", windows))]
fn update_owned_binary(args: UpdateArgs) -> Result<()> {
    let source = args.source.canonicalize().context(
        crate::i18n::Language::English.message(MessageKey::InstallDiagnosticsResolveUpdateSource),
    )?;
    ensure!(
        fs::symlink_metadata(&source)?.file_type().is_file(),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsUpdateSourceNotRegularFile)
    );
    let config_path = args.config.canonicalize().context(
        crate::i18n::Language::English.message(MessageKey::InstallDiagnosticsResolveSelectedConfig),
    )?;
    let mut owner = resolve_profile_ownership(&config_path)?;
    let previous_owner = owner.clone();
    ensure!(
        owner.config_file.canonicalize()? == config_path,
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsSelectedConfigOwnershipMismatch)
    );
    let data_root = owner
        .state_dir
        .parent()
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedStateDataRootMissing),
        )?
        .to_owned();
    ensure!(
        owner.state_dir == data_root.join("state"),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsOwnedStateLayoutInvalid)
    );
    let installed = recorded_service_executable(&owner)?.context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsOwnershipWorkerMissing),
    )?;
    ensure!(
        source != installed,
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsUpdateSourceIsInstalledWorker)
    );
    ensure!(
        installed.file_name() == Some(worker_executable_name().as_os_str()),
        "{}",
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsOwnedWorkerNameInvalid)
    );
    let mut config = Config::load(&config_path)?;
    let update_language = config.language;
    ensure!(
        config.state_dir == owner.state_dir
            && config.credentials_file.as_deref() == Some(owner.credentials_file.as_path()),
        "{}",
        update_language.message(MessageKey::InstallDiagnosticsProfileOwnershipMismatch)
    );
    #[cfg(target_os = "linux")]
    let service =
        validate_owned_linux_update_service(&owner, &installed, &config_path, update_language)?;
    #[cfg(windows)]
    let service =
        validate_owned_windows_update_service(&owner, &installed, &config_path, update_language)?;
    let profile_snapshot = ProfileRuntimeSnapshot::read(&owner)?;
    let runtime = runtime::prepare(&data_root)?;
    let previous = fs::read(&installed)
        .context(update_language.message(MessageKey::InstallDiagnosticsSnapshotOwnedWorker))?;
    let permissions = fs::metadata(&installed)?.permissions();
    let parent = installed
        .parent()
        .context(update_language.message(MessageKey::InstallDiagnosticsWorkerParentMissing))?;
    let staged = stage_update_binary(&source, parent)?;
    let mut stopped = false;
    let mut replaced = false;
    let result = (|| -> Result<()> {
        #[cfg(target_os = "linux")]
        if service.was_active {
            run_checked("systemctl", &["--user", "stop", &service.unit])?;
            stopped = true;
        }
        #[cfg(windows)]
        if service.was_active || service.was_enabled {
            stopped = true;
            set_windows_update_task(&service.task_name, false, false)?;
        }
        staged.persist(&installed).map_err(|error| {
            anyhow!(
                "{}: {}",
                update_language.message(MessageKey::InstallDiagnosticsReplaceOwnedWorker),
                error.error
            )
        })?;
        replaced = true;
        set_executable(&installed)?;
        ensure!(
            fs::metadata(&installed)?.len() > 0,
            "{}",
            update_language.message(MessageKey::InstallDiagnosticsUpdatedWorkerEmpty)
        );
        activate_runtime(&mut owner, &mut config, &runtime)?;
        #[cfg(target_os = "linux")]
        {
            if service.was_active {
                run_checked("systemctl", &["--user", "start", &service.unit])?;
                ensure!(
                    systemctl_user_state("is-active", &service.unit, update_language)? == "active",
                    "{}",
                    update_language.message(MessageKey::InstallDiagnosticsUpdatedWorkerInactive)
                );
            }
            let enabled_state = systemctl_user_state("is-enabled", &service.unit, update_language)?;
            let remains_enabled = matches!(enabled_state.as_str(), "enabled" | "enabled-runtime");
            ensure!(
                remains_enabled == service.was_enabled,
                "{}",
                update_language.message(MessageKey::InstallDiagnosticsUpdatedServiceEnableState)
            );
            stopped = false;
        }
        #[cfg(windows)]
        {
            set_windows_update_task(&service.task_name, service.was_enabled, service.was_active)?;
            let (enabled, active) =
                inspect_windows_task_state(&service.task_name, update_language)?;
            ensure!(
                enabled == service.was_enabled && active == service.was_active,
                "{}",
                update_language.message(MessageKey::InstallDiagnosticsUpdatedTaskState)
            );
            stopped = false;
        }
        Ok(())
    })();
    if let Err(error) = result {
        #[cfg(target_os = "linux")]
        let stop_before_restore = replaced && stopped;
        #[cfg(windows)]
        let stop_before_restore = replaced;
        let rollback = recover_update(stop_before_restore, replaced, stopped || replaced, |step| {
            match step {
                UpdateRecoveryStep::EnsureServiceStopped => {
                    #[cfg(target_os = "linux")]
                    run_checked("systemctl", &["--user", "stop", &service.unit]).context(
                        update_language
                            .message(MessageKey::InstallDiagnosticsStopUpdatedServiceForRestore),
                    )?;
                    #[cfg(windows)]
                    {
                        set_windows_update_task(&service.task_name, false, false)
                            .context(update_language.message(
                                MessageKey::InstallDiagnosticsStopUpdatedTaskForRestore,
                            ))?;
                        let (enabled, active) =
                            inspect_windows_task_state(&service.task_name, update_language)?;
                        ensure!(
                            !enabled && !active,
                            "{}",
                            update_language
                                .message(MessageKey::InstallDiagnosticsUpdatedTaskStillRunning,)
                        );
                    }
                }
                UpdateRecoveryStep::RestoreWorker => {
                    restore_worker_bytes(&installed, &previous, permissions.clone()).context(
                        update_language.message(MessageKey::InstallDiagnosticsWorkerRestoreFailed),
                    )?;
                }
                UpdateRecoveryStep::RestoreProfile => {
                    profile_snapshot.restore(replaced).context(
                        update_language
                            .message(MessageKey::InstallDiagnosticsProfileRuntimeRestoreFailed),
                    )?;
                }
                UpdateRecoveryStep::RestoreService => {
                    #[cfg(target_os = "linux")]
                    {
                        if service.was_active {
                            run_checked("systemctl", &["--user", "start", &service.unit]).context(
                                update_language.message(
                                    MessageKey::InstallDiagnosticsPreviousServiceRestartFailed,
                                ),
                            )?;
                        }
                        ensure!(
                            (systemctl_user_state("is-active", &service.unit, update_language)?
                                == "active")
                                == service.was_active,
                            "{}",
                            update_language
                                .message(MessageKey::InstallDiagnosticsPreviousServiceActiveState,)
                        );
                        let enabled =
                            systemctl_user_state("is-enabled", &service.unit, update_language)?;
                        ensure!(
                            matches!(enabled.as_str(), "enabled" | "enabled-runtime")
                                == service.was_enabled,
                            "{}",
                            update_language.message(
                                MessageKey::InstallDiagnosticsPreviousServiceEnabledState,
                            )
                        );
                    }
                    #[cfg(windows)]
                    {
                        set_windows_update_task(
                            &service.task_name,
                            service.was_enabled,
                            service.was_active,
                        )
                        .context(
                            update_language
                                .message(MessageKey::InstallDiagnosticsPreviousTaskRestoreFailed),
                        )?;
                        let (enabled, active) =
                            inspect_windows_task_state(&service.task_name, update_language)?;
                        ensure!(
                            enabled == service.was_enabled && active == service.was_active,
                            "{}",
                            update_language
                                .message(MessageKey::InstallDiagnosticsPreviousTaskState,)
                        );
                    }
                }
                UpdateRecoveryStep::DiscardPreparedRuntime => runtime.discard().context(
                    update_language
                        .message(MessageKey::InstallDiagnosticsStagedRuntimeCleanupFailed),
                )?,
            }
            Ok(())
        });
        return match rollback {
            Ok(()) => Err(error).context(if replaced {
                update_language.message(MessageKey::InstallUpdateRollbackRestored)
            } else {
                update_language.message(MessageKey::InstallUpdateRollbackUnchanged)
            }),
            Err(restore) => Err(anyhow!(
                "{}",
                update_language
                    .message(MessageKey::InstallUpdateRollbackIncomplete)
                    .replace("{0}", &format!("{error:#}"))
                    .replace("{1}", &format!("{restore:#}"))
            )),
        };
    }
    retire_previous_runtime(&previous_owner, &runtime);
    println!(
        "{}",
        config
            .language
            .message(MessageKey::InstallUpdatedDiscodexWorkerAt)
            .replace("{}", &installed.display().to_string())
    );
    Ok(())
}

struct ProfileRuntimeSnapshot {
    config_path: PathBuf,
    config: Vec<u8>,
    ownership_path: PathBuf,
    ownership: Vec<u8>,
    managed_settings: Vec<ManagedSettingsSnapshot>,
    language: Language,
}

struct ManagedSettingsSnapshot {
    path: PathBuf,
    previous: Option<(Vec<u8>, fs::Permissions)>,
}

impl ProfileRuntimeSnapshot {
    fn read(owner: &Ownership) -> Result<Self> {
        let ownership_path = owner
            .config_file
            .parent()
            .context(
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsProfileDirectoryMissing),
            )?
            .join("ownership.json");
        let config = Config::load(&owner.config_file)?;
        let mut managed_settings = Vec::new();
        if NativeFeatureStore::new(&config.state_dir)
            .read()?
            .antigravity_share_settings
            .is_some()
        {
            for path in crate::agy_settings::managed_update_paths(&config)? {
                let previous = match fs::read(&path) {
                    Ok(bytes) => Some((bytes, fs::metadata(&path)?.permissions())),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => {
                        return Err(error).with_context(|| {
                            config
                                .language
                                .message(MessageKey::AgySettingsRead)
                                .replace("{path}", &path.display().to_string())
                        });
                    }
                };
                managed_settings.push(ManagedSettingsSnapshot { path, previous });
            }
        }
        Ok(Self {
            config_path: owner.config_file.clone(),
            config: fs::read(&owner.config_file)?,
            ownership: fs::read(&ownership_path)?,
            ownership_path,
            managed_settings,
            language: config.language,
        })
    }

    fn restore(&self, replaced: bool) -> Result<()> {
        for (path, bytes) in self.restore_inputs(replaced).into_iter().flatten() {
            write_bytes_private(path, bytes)?;
        }
        if replaced {
            for file in &self.managed_settings {
                let restored = if let Some((bytes, permissions)) = &file.previous {
                    atomic_restore(&file.path, bytes, permissions)
                } else {
                    match fs::remove_file(&file.path) {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                        Err(error) => Err(error.into()),
                    }
                };
                restored.with_context(|| {
                    self.language
                        .message(MessageKey::AgySettingsRestore)
                        .replace("{path}", &file.path.display().to_string())
                })?;
            }
        }
        Ok(())
    }

    fn restore_inputs(&self, replaced: bool) -> Option<[(&Path, &[u8]); 2]> {
        replaced.then_some([
            (&self.config_path, &self.config),
            (&self.ownership_path, &self.ownership),
        ])
    }
}

fn write_bytes_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut staged = tempfile::NamedTempFile::new_in(
        path.parent().context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsProfileDirectoryMissing),
        )?,
    )?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    set_private_file(staged.path())?;
    staged.persist(path).map_err(|error| {
        anyhow!(
            "{}: {}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsReplaceProfileState),
            error.error
        )
    })?;
    Ok(())
}

fn activate_runtime(
    owner: &mut Ownership,
    config: &mut Config,
    runtime: &runtime::Prepared,
) -> Result<()> {
    apply_runtime_selection(owner, config, runtime);
    crate::bundled_skill::sync_for_profile(config);
    write_json_private(&owner.config_file, config)?;
    write_json_private(
        &owner
            .config_file
            .parent()
            .context(
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsProfileDirectoryMissing),
            )?
            .join("ownership.json"),
        owner,
    )?;
    // The incoming installer may run from a temporary download. Older profiles
    // without workerPath must register the installed worker, not that installer.
    let mut managed_config = config.clone();
    if managed_config.worker_path.is_none() {
        managed_config.worker_path = owner.worker_executable.clone();
    }
    crate::agy_settings::reconcile(&managed_config)
}

fn apply_runtime_selection(
    owner: &mut Ownership,
    config: &mut Config,
    runtime: &runtime::Prepared,
) {
    config.codex_path = runtime.executable.clone();
    owner.codex_path = runtime.executable.clone();
    owner.runtime_executable = Some(runtime.executable.clone());
    owner.runtime_dir = runtime.directory.clone();
    owner.runtime_owned = true;
    owner.codex_source = "discodex-core-owned".into();
    owner.codex_version = Some(runtime.manifest.codex_commit.clone());
}

fn retire_previous_runtime(previous: &Ownership, current: &runtime::Prepared) {
    if previous.codex_source == "discodex-core-owned"
        && previous.runtime_owned
        && previous.runtime_dir != current.directory
        && previous.runtime_dir.parent() == current.directory.parent()
        && let Err(error) = runtime::remove_owned(&previous.runtime_dir)
    {
        eprintln!("Updated runtime is active; previous package was preserved: {error:#}");
    }
}

fn stage_update_binary(source: &Path, parent: &Path) -> Result<tempfile::NamedTempFile> {
    let staged = tempfile::NamedTempFile::new_in(parent).context("stage update beside worker")?;
    fs::copy(source, staged.path()).context("copy update binary to staging file")?;
    set_executable(staged.path())?;
    staged.as_file().sync_all()?;
    ensure!(
        fs::metadata(staged.path())?.len() > 0,
        "update source is empty"
    );
    Ok(staged)
}

fn restore_worker_bytes(path: &Path, bytes: &[u8], permissions: fs::Permissions) -> Result<()> {
    let parent = path.parent().context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic652dcc39d538),
    )?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.as_file().set_permissions(permissions)?;
    staged
        .persist(path)
        .map_err(|e| anyhow!("restore previous worker: {}", e.error))?;
    Ok(())
}

#[cfg(target_os = "linux")]
struct OwnedLinuxUpdateService {
    unit: String,
    was_enabled: bool,
    was_active: bool,
}

#[cfg(target_os = "linux")]
fn validate_owned_linux_update_service(
    owner: &Ownership,
    executable: &Path,
    config: &Path,
    language: Language,
) -> Result<OwnedLinuxUpdateService> {
    let unit = format!("{SERVICE_NAME}.service");
    let path = owner
        .service_file
        .as_deref()
        .context(language.message(MessageKey::InstallDiagnostic3a80d25b789d))?;
    ensure!(
        owner.service_kind.as_deref() == Some("systemd-user")
            && path.file_name().and_then(|n| n.to_str()) == Some(unit.as_str()),
        "{}",
        language.message(MessageKey::InstallDiagnostic4377bddeb298)
    );
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context(language.message(crate::i18n::MessageKey::InstallDiagnostic5469a622b9bd))?;
    let dir = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("systemd/user");
    ensure!(
        path == &dir.join(&unit),
        "{}",
        language.message(MessageKey::InstallDiagnostic9ef0af33e93c)
    );
    let expected = format!(
        "[Unit]\nDescription=Discodex Discord bridge\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={} run --config {}\nRestart=on-failure\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(executable),
        systemd_quote(config)
    );
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file() && fs::read_to_string(path)? == expected,
        language.message(MessageKey::InstallDiagnosticAcb4f60f2f54)
    );
    let active_state = systemctl_user_state("is-active", &unit, language)?;
    ensure!(
        active_state == "active" || active_state == "inactive",
        "{}",
        language.message(MessageKey::InstallDiagnostic84f0109c4116)
    );
    let enabled_state = systemctl_user_state("is-enabled", &unit, language)?;
    Ok(OwnedLinuxUpdateService {
        unit: unit.clone(),
        was_enabled: matches!(enabled_state.as_str(), "enabled" | "enabled-runtime"),
        was_active: active_state == "active",
    })
}

#[cfg(windows)]
struct OwnedWindowsUpdateService {
    task_name: String,
    was_enabled: bool,
    was_active: bool,
}

#[cfg(windows)]
fn validate_owned_windows_update_service(
    owner: &Ownership,
    executable: &Path,
    config: &Path,
    language: Language,
) -> Result<OwnedWindowsUpdateService> {
    ensure!(
        owner.service_kind.as_deref() == Some("scheduled-task-user")
            && owner.service_file.is_none(),
        "{}",
        language.message(MessageKey::InstallDiagnostic6de983621253)
    );
    let task_name = windows_task_name_for_config(config).to_owned();
    let task_exe = windows_task_executable_path(executable, language)?;
    let expected_exe = ps_single_quote(&task_exe.display().to_string());
    let expected_args = ps_single_quote(&format!(
        r#"run --config "{}""#,
        owner.config_file.display()
    ));
    let (state, _) = inspect_owned_windows_task(&expected_exe, &expected_args, owner, language)?;
    ensure!(
        state == "matching",
        "{}",
        language.message(MessageKey::InstallDiagnostic3721e34e3d6c)
    );
    let (was_enabled, was_active) = inspect_windows_task_state(&task_name, language)?;
    Ok(OwnedWindowsUpdateService {
        task_name,
        was_enabled,
        was_active,
    })
}

#[cfg(windows)]
fn set_windows_update_task(name: &str, enabled: bool, active: bool) -> Result<()> {
    let name = ps_single_quote(name);
    let change_enabled = if enabled {
        format!("Enable-ScheduledTask -TaskPath '\\' -TaskName '{name}' | Out-Null")
    } else {
        format!("Disable-ScheduledTask -TaskPath '\\' -TaskName '{name}' | Out-Null")
    };
    let change_active = if active {
        format!("Start-ScheduledTask -TaskPath '\\' -TaskName '{name}'")
    } else {
        format!(
            "Stop-ScheduledTask -TaskPath '\\' -TaskName '{name}' -ErrorAction SilentlyContinue"
        )
    };
    let expected_enabled = enabled.to_string();
    let expected_active = active.to_string();
    run_powershell(&format!(
        "$ErrorActionPreference='Stop'; $task=Get-ScheduledTask -TaskPath '\\' -TaskName '{name}'; if ($task.State -eq 'Running') {{ Stop-ScheduledTask -InputObject $task; $limit=(Get-Date).AddSeconds(20); while ((Get-ScheduledTask -TaskPath '\\' -TaskName '{name}').State -eq 'Running' -and (Get-Date) -lt $limit) {{ Start-Sleep -Milliseconds 100 }}; if ((Get-ScheduledTask -TaskPath '\\' -TaskName '{name}').State -eq 'Running') {{ throw 'task did not stop' }} }}; {change_enabled}; {change_active}; $expectedEnabled=[bool]::Parse('{expected_enabled}'); $expectedActive=[bool]::Parse('{expected_active}'); $limit=(Get-Date).AddSeconds(15); do {{ $task=Get-ScheduledTask -TaskPath '\\' -TaskName '{name}'; $enabledMatches=($task.Settings.Enabled -eq $expectedEnabled); $activeMatches=(($task.State -eq 'Running') -eq $expectedActive); if ($enabledMatches -and $activeMatches) {{ break }}; Start-Sleep -Milliseconds 100 }} while ((Get-Date) -lt $limit); if (-not $enabledMatches -or -not $activeMatches) {{ throw 'task did not reach its expected enabled/running state' }}"
    ))?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
struct MacUpdateArgs {
    source: PathBuf,
    config: Option<PathBuf>,
    ownership: Option<PathBuf>,
    migrate_adhoc: bool,
}

#[cfg(target_os = "macos")]
fn parse_mac_update_args(args: &[String]) -> Result<MacUpdateArgs> {
    let (mut source, mut config, mut ownership, mut migrate_adhoc) = (None, None, None, false);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--source" | "--config" | "--ownership" => {
                let option = args[index].as_str();
                let value = args.get(index + 1).context(
                    crate::i18n::Language::English
                        .message(MessageKey::InstallDiagnosticsUpdateOptionRequiresPath),
                )?;
                if value.starts_with('-') {
                    bail!(
                        "{}",
                        crate::i18n::Language::English
                            .message(MessageKey::InstallDiagnosticsUpdateOptionRequiresPath)
                    );
                }
                let slot = match option {
                    "--source" => &mut source,
                    "--config" => &mut config,
                    _ => &mut ownership,
                };
                if slot.replace(PathBuf::from(value)).is_some() {
                    bail!(
                        "{}",
                        crate::i18n::Language::English
                            .message(MessageKey::InstallDiagnosticsUpdateOptionRepeated)
                            .replace("{0}", option)
                    );
                }
                index += 2;
            }
            "--migrate-adhoc" => {
                if migrate_adhoc {
                    bail!(
                        "{}",
                        crate::i18n::Language::English
                            .message(MessageKey::InstallDiagnosticsMacMigrateOptionRepeated)
                    );
                }
                migrate_adhoc = true;
                index += 1;
            }
            option => bail!(
                "{}",
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsMacUnknownOption)
                    .replace("{0}", option)
            ),
        }
    }
    let source = source.context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsMacUpdateSourceRequired),
    )?;
    if config.is_some() && ownership.is_some() {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacConfigOrOwnershipExclusive)
        );
    }
    Ok(MacUpdateArgs {
        source,
        config,
        ownership,
        migrate_adhoc,
    })
}

#[cfg(target_os = "macos")]
fn validate_mac_update_migration(
    pinned_requirement: Option<&str>,
    migrate_adhoc: bool,
    existing_signature_is_adhoc: Option<bool>,
) -> Result<()> {
    if pinned_requirement.is_some() {
        if migrate_adhoc {
            bail!(
                "{}",
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsMacMigrateRequiresUnpinned)
            );
        }
        return Ok(());
    }
    if !migrate_adhoc {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacMigrationFlagRequired)
        );
    }
    if existing_signature_is_adhoc != Some(true) {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacAdhocSignatureNotVerified)
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn update_mac(args: &[String]) -> Result<()> {
    use crate::mac_signing::{
        adopt_owned_binary_identifier, sign_binary_with_language, verify_binary,
    };

    let args = parse_mac_update_args(args)?;
    let source = args.source;
    let config = args.config;
    let owner_override = args.ownership;
    let migrate = args.migrate_adhoc;
    let mut owner_args = Vec::new();
    if let Some(path) = &config {
        owner_args.extend(["--config".to_owned(), path.display().to_string()]);
    }
    if let Some(path) = &owner_override {
        owner_args.extend(["--ownership".to_owned(), path.display().to_string()]);
    }
    let owner_path =
        mac_permissions_ownership_path(&owner_args, &roots()?.config.join("ownership.json"))?
            .canonicalize()
            .context(
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsMacResolveOwnershipRecord),
            )?;
    let mut owner: Ownership = read_json(&owner_path).context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsMacReadOwnershipRecord),
    )?;
    let config_path = owner.config_file.canonicalize().context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsMacResolveProfileConfig),
    )?;
    if config
        .as_ref()
        .is_some_and(|p| p.canonicalize().ok().as_deref() != Some(config_path.as_path()))
    {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacConfigDoesNotMatchOwnership)
        );
    }
    let adjacent_owner = config_path
        .parent()
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacOwnedConfigParentMissing),
        )?
        .join("ownership.json")
        .canonicalize()
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacResolveAdjacentOwnership),
        )?;
    if adjacent_owner != owner_path {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacOwnershipNotAdjacent)
        );
    }
    let data_root = ownership_data_root(&owner)?;
    let recorded_worker = owner.worker_executable.as_ref().context(
        crate::i18n::Language::English.message(MessageKey::InstallDiagnosticsMacWorkerNotRecorded),
    )?;
    let installed = recorded_worker.canonicalize().context(
        crate::i18n::Language::English.message(MessageKey::InstallDiagnosticsMacResolveWorker),
    )?;
    if recorded_worker != &installed {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacWorkerPathNotCanonical)
        );
    }
    if !installed.is_file() || !is_owned_worker_executable(&installed, &data_root) {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacWorkerOutsideOwnedBin)
        );
    }
    if owner.service_kind.as_deref() != Some("launchd-user") {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsMacNotOwnedLaunchAgent)
        );
    }
    let mut profile_config = Config::load(&config_path)?;
    let language = profile_config.language;
    let (label, plist) = launchd_identity(&config_path)?;
    if owner.service_file.as_deref() != Some(plist.as_path()) {
        bail!(
            "{}",
            language.message(MessageKey::InstallDiagnosticsMacLaunchAgentPathMismatch)
        );
    }
    let expected_plist = launchd_plist(&label, &installed, &config_path);
    if !existing_launchd_plist_matches(&plist, &expected_plist, true)? {
        bail!(
            "{}",
            language.message(MessageKey::InstallDiagnosticsMacLaunchAgentPlistMissing)
        );
    }
    let was_loaded = launchd_is_loaded(&label, &installed, &config_path)?;

    let source = source
        .canonicalize()
        .context(language.message(MessageKey::InstallDiagnosticsMacResolveUpdateSource))?;
    if !source.is_file() {
        bail!(
            "{}",
            language
                .message(MessageKey::InstallDiagnosticsMacUpdateSourceNotRegular)
                .replace("{0}", &source.display().to_string())
        );
    }
    if source == installed {
        bail!(
            "{}",
            language.message(MessageKey::InstallDiagnosticsMacUpdateSourceIsInstalledWorker)
        );
    }
    if let Some(pin) = owner.mac_signing_requirement.as_deref() {
        validate_mac_update_migration(Some(pin), migrate, None)?;
        adopt_owned_binary_identifier(&installed, pin)
            .context(language.message(MessageKey::InstallDiagnosticsMacPreserveSigningIdentity))?;
        verify_binary(&installed, pin).context(
            language.message(MessageKey::InstallDiagnosticsMacVerifyPinnedInstalledBinary),
        )?;
    } else {
        let output = Command::new("/usr/bin/codesign")
            .args(["-dv", "--verbose=4"])
            .arg(&installed)
            .output()
            .context(
                language.message(MessageKey::InstallDiagnosticsMacInspectInstalledSignature),
            )?;
        let details = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let adhoc =
            output.status.success() && details.lines().any(|line| line.trim() == "Signature=adhoc");
        validate_mac_update_migration(None, migrate, Some(adhoc))?;
    }
    if migrate {
        require_mac_fda_interactive_terminal(
            &installed,
            language.message(MessageKey::InstallDiagnosticsMacMigrationFdaReason),
        )?;
    }

    let parent = installed
        .parent()
        .context(language.message(MessageKey::InstallDiagnosticsMacInstalledWorkerParentMissing))?;
    let old_bytes = fs::read(&installed)
        .context(language.message(MessageKey::InstallDiagnosticsMacSnapshotInstalledWorker))?;
    let old_permissions = fs::metadata(&installed)?.permissions();
    let old_owner_bytes = fs::read(&owner_path)
        .context(language.message(MessageKey::InstallDiagnosticsMacSnapshotOwnershipRecord))?;
    let old_owner_permissions = fs::metadata(&owner_path)?.permissions();
    let old_pin = owner.mac_signing_requirement.clone();
    let staged = tempfile::NamedTempFile::new_in(parent)
        .context(language.message(MessageKey::InstallDiagnosticsMacStageBesideWorker))?;
    fs::copy(&source, staged.path())
        .context(language.message(MessageKey::InstallDiagnosticsMacCopyUpdateToStage))?;
    set_executable(staged.path())?;
    let requirement = sign_binary_with_language(staged.path(), language)
        .context(language.message(MessageKey::InstallDiagnosticsMacSignStagedWorker))?;
    staged
        .as_file()
        .sync_all()
        .context(language.message(MessageKey::InstallDiagnosticsMacFlushSignedStage))?;
    if old_pin.as_deref().is_some_and(|pin| pin != requirement) {
        bail!(
            "{}",
            language.message(MessageKey::InstallDiagnosticsMacUpdateSignerMismatch)
        );
    }
    verify_binary(staged.path(), &requirement)
        .context(language.message(MessageKey::InstallDiagnosticsMacVerifyStagedWorker))?;

    let previous_owner = owner.clone();
    let profile_snapshot = ProfileRuntimeSnapshot::read(&owner)?;
    let runtime = runtime::prepare(&data_root)?;

    let mut booted_out = false;
    let mut replaced = false;
    let update = (|| -> Result<()> {
        if was_loaded {
            run_checked(
                "launchctl",
                &["bootout", &format!("{}/{}", launchd_domain()?, label)],
            )?;
            booted_out = true;
            wait_for_launchd_unloaded(&label, &installed, &config_path)?;
        }
        staged.persist(&installed).map_err(|e| {
            anyhow!(
                "{}: {}",
                language.message(MessageKey::InstallDiagnosticsMacReplaceWorker),
                e.error
            )
        })?;
        replaced = true;
        set_executable(&installed)?;
        verify_binary(&installed, &requirement)
            .context(language.message(MessageKey::InstallDiagnosticsMacVerifyReplacedWorker))?;
        owner.mac_signing_requirement = Some(requirement.clone());
        activate_runtime(&mut owner, &mut profile_config, &runtime)?;
        if migrate {
            request_mac_fda_guidance(&owner_path, language)?;
        }
        if was_loaded {
            let domain = launchd_domain()?;
            run_checked(
                "launchctl",
                &[
                    "bootstrap",
                    &domain,
                    plist.to_str().context(
                        language.message(MessageKey::InstallDiagnosticsMacLaunchAgentPathNotUtf8),
                    )?,
                ],
            )?;
            run_checked("launchctl", &["kickstart", &format!("{domain}/{label}")])?;
            if !launchd_is_loaded(&label, &installed, &config_path)? {
                bail!(
                    "{}",
                    language.message(MessageKey::InstallDiagnosticsMacUpdatedLaunchAgentNotLoaded)
                );
            }
        }
        Ok(())
    })();
    if let Err(error) = update {
        let loaded = if booted_out {
            launchd_is_loaded(&label, &installed, &config_path)
        } else {
            Ok(false)
        };
        let service_needs_restore = booted_out && loaded.as_ref().is_ok_and(|loaded| !loaded);
        let rollback =
            loaded.and_then(|loaded| {
                recover_mac_update(booted_out, replaced, loaded, |step| {
                    match step {
                        UpdateRecoveryStep::EnsureServiceStopped => {
                            run_checked(
                                "launchctl",
                                &["bootout", &format!("{}/{}", launchd_domain()?, label)],
                            )?;
                            wait_for_launchd_unloaded(&label, &installed, &config_path)?;
                        }
                        UpdateRecoveryStep::RestoreWorker => {
                            atomic_restore(&installed, &old_bytes, &old_permissions)?;
                        }
                        UpdateRecoveryStep::RestoreProfile => {
                            atomic_restore(&owner_path, &old_owner_bytes, &old_owner_permissions)
                                .context(language.message(
                                MessageKey::InstallDiagnosticsMacRestoreOwnershipRecord,
                            ))?;
                            profile_snapshot
                                .restore(replaced)
                                .context(language.message(
                                    MessageKey::InstallDiagnosticsMacRestorePreviousProfileRuntime,
                                ))?;
                        }
                        UpdateRecoveryStep::DiscardPreparedRuntime => {
                            runtime.discard().context(language.message(
                                MessageKey::InstallDiagnosticsStagedRuntimeCleanupFailed,
                            ))?
                        }
                        UpdateRecoveryStep::RestoreService => {
                            let domain = launchd_domain()?;
                            run_checked(
                                "launchctl",
                                &[
                                    "bootstrap",
                                    &domain,
                                    plist.to_str().context(language.message(
                                        MessageKey::InstallDiagnosticsMacLaunchAgentPathNotUtf8,
                                    ))?,
                                ],
                            )?;
                            run_checked("launchctl", &["kickstart", &format!("{domain}/{label}")])?;
                            if !launchd_is_loaded(&label, &installed, &config_path)? {
                                bail!(
                                "{}",
                                language.message(
                                    MessageKey::InstallDiagnosticsMacPreviousLaunchAgentNotLoaded
                                )
                            );
                            }
                        }
                    }
                    Ok(())
                })
            });
        return match rollback {
            Ok(()) if replaced => Err(error).context(
                language.message(MessageKey::InstallDiagnosticsMacUpdateFailedRollbackRestored),
            ),
            Ok(()) if service_needs_restore => Err(error).context(
                language.message(MessageKey::InstallDiagnosticsMacUpdateFailedServiceRestored),
            ),
            Ok(()) => Err(error).context(
                language.message(MessageKey::InstallDiagnosticsMacUpdateFailedBeforeReplacement),
            ),
            Err(restore) => Err(anyhow!(
                "{}",
                language
                    .message(MessageKey::InstallDiagnosticsMacUpdateAndRollbackFailed)
                    .replace("{0}", &format!("{error:#}"))
                    .replace("{1}", &format!("{restore:#}"))
            )),
        };
    }
    retire_previous_runtime(&previous_owner, &runtime);
    println!(
        "{}",
        language
            .message(MessageKey::InstallDiagnosticsMacUpdateCompleted)
            .replace("{0}", &installed.display().to_string())
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdateRecoveryStep {
    EnsureServiceStopped,
    RestoreWorker,
    RestoreProfile,
    DiscardPreparedRuntime,
    RestoreService,
}

#[cfg(any(target_os = "macos", test))]
fn recover_mac_update(
    booted_out: bool,
    replaced: bool,
    service_loaded: bool,
    recover: impl FnMut(UpdateRecoveryStep) -> Result<()>,
) -> Result<()> {
    recover_update(
        booted_out && replaced && service_loaded,
        replaced,
        booted_out && (replaced || !service_loaded),
        recover,
    )
}

fn recover_update(
    stop_service: bool,
    replaced: bool,
    restore_service: bool,
    mut recover: impl FnMut(UpdateRecoveryStep) -> Result<()>,
) -> Result<()> {
    if stop_service {
        recover(UpdateRecoveryStep::EnsureServiceStopped)?;
    }
    if replaced {
        recover(UpdateRecoveryStep::RestoreWorker)?;
        recover(UpdateRecoveryStep::RestoreProfile)?;
    }
    if restore_service {
        recover(UpdateRecoveryStep::RestoreService)?;
    }
    recover(UpdateRecoveryStep::DiscardPreparedRuntime)
}

#[cfg(all(test, target_os = "macos"))]
mod mac_update_policy_tests {
    use super::{
        MacUpdateArgs, mac_fda_guidance_answer, mac_worker_path_unchanged, parse_mac_update_args,
        require_mac_fda_interactive_terminal_state, validate_mac_update_migration,
    };
    use std::path::PathBuf;

    #[test]
    fn fda_guidance_is_skipped_only_for_the_same_existing_worker_path() {
        let temp = tempfile::tempdir().unwrap();
        let old_worker = temp.path().join("old").join("discodex");
        let new_worker = temp.path().join("new").join("discodex");
        std::fs::create_dir_all(old_worker.parent().unwrap()).unwrap();
        std::fs::create_dir_all(new_worker.parent().unwrap()).unwrap();
        std::fs::write(&old_worker, b"old worker").unwrap();
        std::fs::write(&new_worker, b"new worker").unwrap();

        assert!(mac_worker_path_unchanged(
            Some(&old_worker),
            Some(&old_worker)
        ));
        assert!(!mac_worker_path_unchanged(
            Some(&old_worker),
            Some(&new_worker)
        ));
        assert!(!mac_worker_path_unchanged(None, Some(&new_worker)));
    }

    #[test]
    fn parses_source_and_one_ownership_selector() {
        let input = [
            "--source",
            "/tmp/new-worker",
            "--config",
            "/profile/config.json",
        ]
        .map(str::to_owned);
        let parsed = parse_mac_update_args(&input).unwrap();
        assert_eq!(
            parsed,
            MacUpdateArgs {
                source: PathBuf::from("/tmp/new-worker"),
                config: Some(PathBuf::from("/profile/config.json")),
                ownership: None,
                migrate_adhoc: false,
            }
        );
    }

    #[test]
    fn rejects_duplicate_or_conflicting_options() {
        let duplicate = ["--source", "/tmp/one", "--source", "/tmp/two"].map(str::to_owned);
        assert!(parse_mac_update_args(&duplicate).is_err());
        let conflict = [
            "--source",
            "/tmp/new",
            "--config",
            "/profile/config.json",
            "--ownership",
            "/profile/ownership.json",
        ]
        .map(str::to_owned);
        assert!(parse_mac_update_args(&conflict).is_err());
        assert!(parse_mac_update_args(&[]).is_err());
        assert!(parse_mac_update_args(&["--source".to_owned()]).is_err());
    }

    #[test]
    fn migration_flag_only_authorizes_verified_unpinned_adhoc_builds() {
        assert!(validate_mac_update_migration(None, false, Some(true)).is_err());
        assert!(validate_mac_update_migration(None, true, Some(true)).is_ok());
        assert!(validate_mac_update_migration(None, true, Some(false)).is_err());
        assert!(
            validate_mac_update_migration(Some("designated => identifier \"worker\""), false, None)
                .is_ok()
        );
        assert!(validate_mac_update_migration(Some("pinned"), true, None).is_err());
    }

    #[test]
    fn fda_gate_requires_interactive_acknowledgment_and_explicit_input() {
        let executable = PathBuf::from("/owned/bin/codex-worker-renewal");
        assert!(
            require_mac_fda_interactive_terminal_state(true, true, &executable, "install").is_ok()
        );
        assert!(
            require_mac_fda_interactive_terminal_state(false, true, &executable, "install")
                .is_err()
        );
        assert!(
            require_mac_fda_interactive_terminal_state(true, false, &executable, "install")
                .is_err()
        );
        assert_eq!(mac_fda_guidance_answer(1, "\n").unwrap(), true);
        assert_eq!(mac_fda_guidance_answer(2, "q\n").unwrap(), false);
        assert_eq!(mac_fda_guidance_answer(2, "Q\n").unwrap(), false);
        assert!(mac_fda_guidance_answer(4, "yes\n").is_err());
        assert!(mac_fda_guidance_answer(0, "").is_err());
    }
}

fn atomic_restore(path: &Path, bytes: &[u8], permissions: &fs::Permissions) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(
        path.parent().context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnostic652dcc39d538),
        )?,
    )?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    fs::set_permissions(temp.path(), permissions.clone())?;
    temp.persist(path)
        .map_err(|e| anyhow!("atomically restore file: {}", e.error))?;
    Ok(())
}

#[cfg(windows)]
fn is_current_executable(path: &Path) -> bool {
    let Ok(current) = env::current_exe() else {
        return false;
    };
    let current = current.canonicalize().unwrap_or(current);
    let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    current == path
}

#[cfg(windows)]
fn defer_windows_self_cleanup(
    ownership: &Ownership,
    ownership_path: &Path,
    runtime_dir: Option<&Path>,
    config: Option<&Config>,
) -> Result<()> {
    let worker = ownership.worker_executable.as_ref().context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnosticFdfd12afa0eb),
    )?;
    let mut preserved_paths = vec![ownership.project_dir.clone(), ownership.state_dir.clone()];
    if let Some(config) = config {
        preserved_paths.extend([config.project_dir.clone(), config.state_dir.clone()]);
        preserved_paths.extend(config.codex_home.clone());
    }
    let preserved_paths = preserved_paths
        .into_iter()
        .map(|path| path.canonicalize().unwrap_or(path))
        .collect::<Vec<_>>();
    let preserved_paths = serde_json::to_string(&preserved_paths).context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic2f25a2fd6fd9),
    )?;
    let parent_pid = std::process::id().to_string();
    let script = r#"
$ErrorActionPreference = 'Stop'
$parentId = [int]$env:DISCODEX_UNINSTALL_PARENT_PID
while ($true) {
    $parent = Get-Process -Id $parentId -ErrorAction SilentlyContinue
    if ($null -eq $parent) { break }
    $parent.WaitForExit(250) | Out-Null
}
try {
    $runtime = $env:DISCODEX_UNINSTALL_RUNTIME_DIR
    if (-not [string]::IsNullOrEmpty($runtime) -and (Test-Path -LiteralPath $runtime)) {
        $preserved = @($env:DISCODEX_UNINSTALL_PRESERVED_PATHS | ConvertFrom-Json)
        $configPath = $env:DISCODEX_UNINSTALL_CONFIG
        if (Test-Path -LiteralPath $configPath) {
            $config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
            foreach ($name in @('codexHome', 'projectDir', 'stateDir')) {
                $value = $config.$name
                if (-not [string]::IsNullOrEmpty($value)) {
                    if (-not [IO.Path]::IsPathRooted($value)) {
                        $value = Join-Path (Split-Path -Parent $configPath) $value
                    }
                    $preserved += $value
                }
            }
        }
        $runtimePath = [IO.Path]::GetFullPath($runtime).TrimEnd('\')
        foreach ($path in $preserved) {
            $path = [IO.Path]::GetFullPath($path).TrimEnd('\')
            if ($path.Equals($runtimePath, [StringComparison]::OrdinalIgnoreCase) -or $path.StartsWith($runtimePath + '\', [StringComparison]::OrdinalIgnoreCase)) {
                throw 'Codex user data, project, or state overlaps the owned runtime; its files and ownership were preserved.'
            }
        }
        $directory = Get-Item -LiteralPath $runtime -Force
        if (-not $directory.PSIsContainer -or ($directory.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Owned runtime changed to a non-directory or link; its files and ownership were preserved.'
        }
        $names = @('codex.exe', 'codex-code-mode-host.exe')
        foreach ($entry in @(Get-ChildItem -LiteralPath $runtime -Force)) {
            if ($entry.PSIsContainer -or ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $entry.Name -cnotin $names) {
                throw 'Owned runtime contains unrecorded files or user data; its files and ownership were preserved.'
            }
        }
        foreach ($name in $names) {
            $path = Join-Path $runtime $name
            if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
        }
        [IO.Directory]::Delete($runtime, $false)
    }
    Remove-Item -LiteralPath $env:DISCODEX_UNINSTALL_EXECUTABLE -Force
    foreach ($name in @('CONFIG', 'CREDENTIALS', 'INSTALL_LIST')) {
        $path = [Environment]::GetEnvironmentVariable("DISCODEX_UNINSTALL_$name")
        if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
    }
    Remove-Item -LiteralPath $env:DISCODEX_UNINSTALL_OWNERSHIP -Force
    Write-Output 'Owned Windows files removed; native state and user Codex data were preserved.'
    exit 0
} catch {
    [Console]::Error.WriteLine("Deferred uninstall cleanup failed: $($_.Exception.Message). Ownership/configuration records remain where deletion had not yet completed.")
    exit 1
}
"#;

    command_without_console("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .env("DISCODEX_UNINSTALL_PARENT_PID", parent_pid)
        .env("DISCODEX_UNINSTALL_PRESERVED_PATHS", preserved_paths)
        .env("DISCODEX_UNINSTALL_EXECUTABLE", worker)
        .env(
            "DISCODEX_UNINSTALL_RUNTIME_DIR",
            runtime_dir.map_or_else(|| std::ffi::OsStr::new(""), Path::as_os_str),
        )
        .env("DISCODEX_UNINSTALL_CONFIG", &ownership.config_file)
        .env(
            "DISCODEX_UNINSTALL_CREDENTIALS",
            &ownership.credentials_file,
        )
        .env(
            "DISCODEX_UNINSTALL_INSTALL_LIST",
            &ownership.install_list_file,
        )
        .env("DISCODEX_UNINSTALL_OWNERSHIP", ownership_path)
        .stdin(Stdio::null())
        .spawn()
        .context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnosticE47bfa4f43ea),
        )?;
    Ok(())
}

#[derive(Clone, Debug)]
struct Roots {
    config: PathBuf,
    data: PathBuf,
}

fn roots() -> Result<Roots> {
    roots_for_app(APP_NAME)
}

fn roots_for_app(app_name: &str) -> Result<Roots> {
    #[cfg(windows)]
    let config_base = env::var_os("APPDATA").map(PathBuf::from).context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic04c7eeb9bd5a),
    )?;
    #[cfg(not(windows))]
    let config_base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnostic0f3fb1ef0ac0),
        )?;

    #[cfg(windows)]
    let data_base = env::var_os("LOCALAPPDATA").map(PathBuf::from).context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic0c7054af933b),
    )?;
    #[cfg(not(windows))]
    let data_base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnostic9b961d0be2ae),
        )?;

    Ok(Roots {
        config: config_base.join(app_name),
        data: data_base.join(app_name),
    })
}

fn default_codex_home() -> Result<PathBuf> {
    #[cfg(windows)]
    let home = env::var_os("USERPROFILE").context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnosticFb9ae00e73dc),
    )?;
    #[cfg(not(windows))]
    let home = env::var_os("HOME").context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic5469a622b9bd),
    )?;
    Ok(PathBuf::from(home).join(".codex"))
}

fn choose_codex_home(roots: &Roots, language: Language) -> Result<PathBuf> {
    let config_path = roots.config.join("config.json");
    if config_path.is_file()
        && let Some(home) = Config::load(&config_path)?.codex_home
    {
        return Ok(home);
    }
    let existing = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or(default_codex_home()?);
    let separate = roots.data.join("codex-home");
    println!(
        "{}",
        language.message(MessageKey::InstallCodexData1ExistingHomeShareAccountMemory)
    );
    println!("1) {}\n2) {}", existing.display(), separate.display());
    let default = if existing.is_dir() { "1" } else { "2" };
    loop {
        match ask_text(language, MessageKey::InstallDataHome, default)?.as_str() {
            "1" => {
                return Ok(PathBuf::from(ask_text(
                    language,
                    MessageKey::InstallExistingCodexHome,
                    &existing.display().to_string(),
                )?));
            }
            "2" => return Ok(separate),
            _ => println!("{}", language.message(MessageKey::InstallChoose1Or2)),
        }
    }
}

fn choose_antigravity_install(language: Language) -> Result<AntigravityInstallChoice> {
    println!(
        "{}",
        language.message(MessageKey::InstallAntigravity1InstallADiscodexManagedCli2S)
    );
    loop {
        match ask_text(language, MessageKey::InstallAntigravity, "2")?.as_str() {
            "1" => return choose_antigravity_settings(language),
            "2" => return Ok(AntigravityInstallChoice::Disabled),
            "q" | "Q" => return Err(crate::agy_setup::SetupCancelled.into()),
            _ => println!("{}", language.message(MessageKey::InstallChoose12OrQ)),
        }
    }
}

fn choose_antigravity_settings(language: Language) -> Result<AntigravityInstallChoice> {
    println!(
        "{}",
        language.message(MessageKey::InstallAntigravitySettings1ShareExistingSettings2)
    );
    loop {
        let answer = ask_text(language, MessageKey::InstallSettings, "2")?;
        match antigravity_settings_choice(&answer) {
            Some(choice) => return Ok(choice),
            None if answer.eq_ignore_ascii_case("q") => {
                return Err(crate::agy_setup::SetupCancelled.into());
            }
            None => println!("{}", language.message(MessageKey::InstallChoose12OrQ)),
        }
    }
}

fn antigravity_settings_choice(answer: &str) -> Option<AntigravityInstallChoice> {
    match answer {
        "1" => Some(AntigravityInstallChoice::SharedSettings),
        "2" => Some(AntigravityInstallChoice::SeparateSettings),
        _ => None,
    }
}

fn configure_antigravity_install(
    config: &Config,
    requested: Option<AntigravityInstallChoice>,
) -> Result<()> {
    let store = NativeFeatureStore::new(&config.state_dir);
    let choice = match requested {
        Some(choice) => choice,
        None => return Ok(()),
    };
    let Some(share_settings) = choice.share_settings() else {
        let mut preferences = store.read()?;
        preferences.antigravity_enabled = false;
        store.apply(&preferences)?;
        return Ok(());
    };
    crate::agy_settings::prepare(config, share_settings)?;
    let runtime = crate::agy::Runtime::for_config(config)?;
    run_async(prepare_antigravity_at_console(
        crate::agy_setup::SetupConfig {
            executable: runtime.executable,
            cwd: config.project_dir.clone(),
            environment: runtime.environment,
            arguments: runtime.arguments,
        },
        config.language,
    ))?;
    // Installation and sign-in can take time. Preserve any other settings saved
    // while the user completes the native authentication flow.
    let mut preferences = store.read()?;
    apply_antigravity_ready(&mut preferences, share_settings);
    store.apply(&preferences)?;
    println!(
        "{}",
        config
            .language
            .message(MessageKey::InstallAntigravityIsInstalledSignedInAndEnabled)
    );
    Ok(())
}

fn apply_antigravity_ready(preferences: &mut NativeFeaturePreferences, share_settings: bool) {
    preferences.antigravity_enabled = true;
    preferences.antigravity_share_settings = Some(share_settings);
}

struct SetupTerminal;

impl SetupTerminal {
    fn open() -> Result<Self> {
        crossterm::terminal::enable_raw_mode().context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnostic301d57bdbcfd),
        )?;
        Ok(Self)
    }
}

impl Drop for SetupTerminal {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SetupTerminalInput {
    Wait,
    Send(String),
    Cancel,
}

fn antigravity_terminal_input(
    event: crossterm::event::Event,
    text: &mut String,
    accepts_code: bool,
    accepts_navigation: bool,
) -> SetupTerminalInput {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
    let Event::Key(key) = event else {
        return SetupTerminalInput::Wait;
    };
    if key.kind == KeyEventKind::Release {
        return SetupTerminalInput::Wait;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c' | 'd') => SetupTerminalInput::Cancel,
            KeyCode::Char('u') => {
                text.clear();
                SetupTerminalInput::Wait
            }
            _ => SetupTerminalInput::Wait,
        };
    }
    match key.code {
        KeyCode::Enter if accepts_code || accepts_navigation => {
            let mut input = std::mem::take(text);
            input.push('\r');
            SetupTerminalInput::Send(input)
        }
        KeyCode::Char(character) if accepts_code => {
            // Submit complete lines so the CLI adapter can redact authentication
            // codes even when native terminal echo arrives in separate chunks.
            text.push(character);
            SetupTerminalInput::Wait
        }
        KeyCode::Backspace => {
            text.pop();
            SetupTerminalInput::Wait
        }
        KeyCode::Esc if accepts_navigation => {
            text.clear();
            SetupTerminalInput::Send("\u{1b}".into())
        }
        KeyCode::Up if accepts_navigation => SetupTerminalInput::Send("\u{1b}[A".into()),
        KeyCode::Down if accepts_navigation => SetupTerminalInput::Send("\u{1b}[B".into()),
        KeyCode::Left if accepts_navigation => SetupTerminalInput::Send("\u{1b}[D".into()),
        KeyCode::Right if accepts_navigation => SetupTerminalInput::Send("\u{1b}[C".into()),
        KeyCode::Tab if accepts_navigation => SetupTerminalInput::Send("\t".into()),
        _ => SetupTerminalInput::Wait,
    }
}

async fn prepare_antigravity_at_console(
    config: crate::agy_setup::SetupConfig,
    language: Language,
) -> Result<crate::agy_setup::SetupReady> {
    use crate::agy_setup::{InputDisposition, SetupCancelled, SetupInput, SetupProgress};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::sync::{mpsc, watch};

    println!(
        "{}",
        language.message(MessageKey::InstallPreparingAntigravityCtrlCOrCtrlDCancelsA)
    );
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    let terminal = interactive.then(SetupTerminal::open).transpose()?;
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let (input_tx, input_rx) = mpsc::channel(32);
    let stop_reader = Arc::new(AtomicBool::new(false));
    let code_input = Arc::new(AtomicBool::new(false));
    let navigation_input = Arc::new(AtomicBool::new(false));
    let reader = if interactive {
        let stop = stop_reader.clone();
        let cancel = cancel_tx.clone();
        let codes = code_input.clone();
        let navigation = navigation_input.clone();
        Some(std::thread::spawn(move || -> io::Result<()> {
            let result = (|| {
                let mut text = String::new();
                let mut outgoing = std::collections::VecDeque::new();
                let mut acknowledgements: Vec<tokio::sync::oneshot::Receiver<InputDisposition>> =
                    Vec::new();
                while !stop.load(Ordering::Acquire) {
                    while let Some(input) = outgoing.pop_front() {
                        match input_tx.try_send(input) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(input)) => {
                                outgoing.push_front(input);
                                break;
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
                        }
                    }
                    acknowledgements.retain_mut(|received| match received.try_recv() {
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => true,
                        Ok(InputDisposition::NotAccepting) => {
                            print!(
                                "{}\r\n",
                                language.message(
                                    MessageKey::InstallTheCliPromptChangedFollowTheCurrentPrompt
                                )
                            );
                            let _ = io::stdout().flush();
                            false
                        }
                        _ => false,
                    });
                    if !crossterm::event::poll(std::time::Duration::from_millis(100))? {
                        continue;
                    }
                    match antigravity_terminal_input(
                        crossterm::event::read()?,
                        &mut text,
                        codes.load(Ordering::Acquire),
                        navigation.load(Ordering::Acquire),
                    ) {
                        SetupTerminalInput::Wait => {}
                        SetupTerminalInput::Send(input) => {
                            let (acknowledgement, received) = tokio::sync::oneshot::channel();
                            outgoing.push_back(SetupInput {
                                data: input,
                                acknowledgement,
                            });
                            acknowledgements.push(received);
                        }
                        SetupTerminalInput::Cancel => {
                            let _ = cancel.send(true);
                            break;
                        }
                    }
                }
                Ok(())
            })();
            if result.is_err() {
                let _ = cancel.send(true);
            }
            result
        }))
    } else {
        drop(input_tx);
        None
    };
    let mut setup = Box::pin(crate::agy_setup::prepare(
        config, events_tx, input_rx, cancel_rx,
    ));
    let mut input_unavailable = false;
    let mut signal_error = None;
    let mut signal_received = false;
    let result = loop {
        tokio::select! {
            result = &mut setup => break result,
            signal = tokio::signal::ctrl_c(), if !signal_received => {
                signal_error = signal.err();
                signal_received = true;
                let _ = cancel_tx.send(true);
            }
            Some(event) = events_rx.recv() => {
                let (accepts_code, accepts_navigation) = match &event {
                    SetupProgress::Authentication { accepts_code, accepts_navigation, .. } => (*accepts_code, *accepts_navigation),
                    _ => (false, false),
                };
                code_input.store(accepts_code, Ordering::Release);
                navigation_input.store(accepts_navigation, Ordering::Release);
                let message = match event {
                    SetupProgress::Checking => language.message(MessageKey::InstallCheckingManagedAntigravity).to_owned(),
                    SetupProgress::Installing => language.message(MessageKey::InstallDownloadingOfficialAntigravityRelease).to_owned(),
                    SetupProgress::CheckingAccount => language.message(MessageKey::InstallCheckingAntigravitySignIn).to_owned(),
                    SetupProgress::Verifying => language.message(MessageKey::InstallVerifyingTheAntigravityModelCatalog).to_owned(),
                    SetupProgress::Authentication { output, .. } => {
                        if !interactive {
                            input_unavailable = true;
                            let _ = cancel_tx.send(true);
                        }
                        output
                    }
                };
                print!("{}\r\n", message.replace('\n', "\r\n"));
                let _ = io::stdout().flush();
            }
        }
    };
    stop_reader.store(true, Ordering::Release);
    // Stop setup before restoring the terminal; the input reader never blocks
    // on a child write or acknowledgement, so cancellation remains available.
    drop(setup);
    if let Some(reader) = reader {
        reader
            .join()
            .map_err(|_| anyhow!("Antigravity terminal input failed"))??;
    }
    drop(terminal);
    if let Some(error) = signal_error {
        return Err(error).context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnostic5160f0921dfc),
        );
    }
    if *cancel_tx.borrow() && result.is_ok() {
        return Err(SetupCancelled.into());
    }
    if input_unavailable {
        bail!(
            "{}",
            language.message(MessageKey::InstallAntigravitySignInNeedsAnInteractiveTermina)
        );
    }
    if result
        .as_ref()
        .is_err_and(|error| error.is::<SetupCancelled>())
    {
        println!(
            "{}",
            language.message(MessageKey::InstallAntigravitySetupCancelled)
        );
    }
    result
}

fn remove_owned_antigravity(state_dir: &Path) -> Result<()> {
    let executable = crate::agy::managed_executable(state_dir)?;
    let directory = executable.parent().context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnosticDd4390dd63f5),
    )?;
    let marker = directory.join(crate::agy_setup::OWNERSHIP_FILE);
    if !marker.exists() {
        return Ok(());
    }
    ensure!(
        crate::agy_setup::is_owned_install(&executable),
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnosticE00affd05477)
    );
    crate::agy_mcp::remove_owned_registration(&crate::agy_settings::root(state_dir)?)?;
    fs::remove_dir_all(directory).context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic12a998ef8bc8),
    )?;
    Ok(())
}

fn resolve_codex_home(
    previous: Option<&Path>,
    selected_env: Option<&Path>,
    project_dir: &Path,
    default_home: Option<&Path>,
) -> Result<PathBuf> {
    let selected = previous.or(selected_env).or(default_home).context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic1f0de93d2670),
    )?;
    if selected.is_absolute() {
        return Ok(selected.to_owned());
    }
    let base = if project_dir.is_absolute() {
        project_dir.to_owned()
    } else {
        env::current_dir()
            .context(
                crate::i18n::Language::English
                    .message(crate::i18n::MessageKey::InstallDiagnostic41d8fc5223b3),
            )?
            .join(project_dir)
    };
    Ok(base.join(selected))
}

fn validate_existing_profile_identity(
    existing_installation: bool,
    requested_profile: &str,
    ownership_profile: Option<&str>,
    config_profile: Option<&str>,
) -> Result<()> {
    if existing_installation && ownership_profile.is_none() && config_profile.is_none() {
        bail!(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsExistingProfileIdentityUnreadable)
        );
    }
    if ownership_profile.is_some_and(|profile| profile != requested_profile)
        || config_profile.is_some_and(|profile| profile != requested_profile)
    {
        bail!(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsExistingInstallationOtherProfile)
        );
    }
    Ok(())
}

fn previous_owns_completed_core(previous: &Ownership, codex_path: &Path) -> bool {
    previous.codex_source == "official-core-owned"
        && previous.runtime_executable.as_deref() == Some(codex_path)
        && (previous.runtime_owned
            || (previous.service_kind.is_some() && previous.service_file.is_some()))
}

fn same_profile_installation_paths(
    previous: &Ownership,
    config_path: &Path,
    credentials_path: &Path,
    install_list_path: &Path,
    state_dir: &Path,
    project_dir: &Path,
) -> bool {
    previous.config_file == config_path
        && previous.credentials_file == credentials_path
        && previous.install_list_file == install_list_path
        && previous.state_dir == state_dir
        && previous.project_dir == project_dir
}

fn prepare_owned_core_uninstall(
    ownership: &mut Ownership,
    data_root: &Path,
    config: Option<&Config>,
) -> Result<Option<PathBuf>> {
    let runtime_dir = owned_core_runtime_for_cleanup(ownership, data_root, config, &[])?;
    if runtime_dir.is_some() {
        // Service unlink persists this proof so a later command/cleanup failure can be retried.
        ownership.runtime_owned = true;
    }
    Ok(runtime_dir)
}

fn owned_core_runtime_for_cleanup(
    ownership: &Ownership,
    data_root: &Path,
    config: Option<&Config>,
    retained_native_paths: &[&Path],
) -> Result<Option<PathBuf>> {
    if ownership.codex_source == "discodex-core-owned" {
        if !ownership.runtime_owned {
            return Ok(None);
        }
        let expected_root = data_root.canonicalize()?.join("runtime");
        ensure!(
            ownership.runtime_dir.parent() == Some(expected_root.as_path()),
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedRuntimeOutsideProfile)
        );
        if !ownership.runtime_dir.exists() {
            return Ok(None);
        }
        let expected_executable = ownership
            .runtime_dir
            .join("bin")
            .join(codex_executable_name());
        ensure!(
            ownership.runtime_executable.as_ref() == Some(&expected_executable)
                && ownership.codex_path == expected_executable,
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedRuntimeDifferentPackage)
        );
        let mut preserved = vec![&ownership.project_dir, &ownership.state_dir];
        if let Some(config) = config {
            preserved.extend([&config.project_dir, &config.state_dir]);
            preserved.extend(config.codex_home.as_ref());
        }
        for path in preserved
            .into_iter()
            .map(PathBuf::as_path)
            .chain(retained_native_paths.iter().copied())
        {
            ensure!(
                !path
                    .canonicalize()
                    .unwrap_or_else(|_| path.to_owned())
                    .starts_with(&ownership.runtime_dir),
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsUserDataOverlapsRuntime)
            );
        }
        runtime::verify_owned(&ownership.runtime_dir)?;
        return Ok(Some(ownership.runtime_dir.clone()));
    }
    if ownership.codex_source != "official-core-owned" {
        return Ok(None);
    }
    let data_root = data_root.canonicalize().with_context(|| {
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsProfileDataRootResolve)
            .replace("{0}", &data_root.display().to_string())
    })?;
    let runtime_dir = data_root.join("official-codex");
    if ownership.runtime_dir != runtime_dir {
        bail!(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedRuntimeExpectedProfile)
        );
    }
    match fs::symlink_metadata(&runtime_dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => bail!(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedRuntimeRegularDirectory)
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context(
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsInspectOwnedCodexRuntimeBeforeCleanup),
            );
        }
    }
    let executable = runtime_dir.join(codex_executable_name());
    if ownership.runtime_executable.as_deref() != Some(executable.as_path())
        || ownership.codex_path != executable
        || !previous_owns_completed_core(ownership, &executable)
    {
        bail!(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedRuntimeIncompleteRecord)
        );
    }
    let mut preserved_paths = vec![
        ownership.project_dir.as_path(),
        ownership.state_dir.as_path(),
    ];
    if let Some(config) = config {
        preserved_paths.extend([config.project_dir.as_path(), config.state_dir.as_path()]);
        if let Some(home) = &config.codex_home {
            preserved_paths.push(home.as_path());
        }
    }
    preserved_paths.extend_from_slice(retained_native_paths);
    for path in preserved_paths {
        let resolved = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        if resolved.starts_with(&runtime_dir) {
            bail!(
                "{}",
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsOwnedRuntimeOverlapPath)
                    .replace("{0}", &path.display().to_string())
            );
        }
    }
    inspect_owned_core_runtime_contents(&runtime_dir)?;
    Ok(Some(runtime_dir))
}

fn inspect_owned_core_runtime_contents(runtime_dir: &Path) -> Result<()> {
    if !fs::symlink_metadata(runtime_dir)
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsInspectOwnedCodexRuntimeBeforeCleanup),
        )?
        .file_type()
        .is_dir()
    {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedRuntimeRegularDirectory)
        );
    }
    for entry in fs::read_dir(runtime_dir).context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsReadOwnedCodexRuntimeBeforeCleanup),
    )? {
        let entry = entry.context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsReadOwnedCodexRuntimeEntry),
        )?;
        let name = entry.file_name();
        if (name != codex_executable_name()
            && name != OsString::from(code_mode_host_executable_name()))
            || !entry
                .file_type()
                .context(
                    crate::i18n::Language::English
                        .message(MessageKey::InstallDiagnosticsInspectOwnedRuntimeEntry),
                )?
                .is_file()
        {
            bail!(
                "{}",
                crate::i18n::Language::English
                    .message(MessageKey::InstallDiagnosticsOwnedRuntimeUnrecordedContents)
                    .replace("{0}", &entry.path().display().to_string())
            );
        }
    }
    Ok(())
}

fn remove_owned_core_runtime(runtime_dir: &Path) -> Result<()> {
    if runtime_dir.join("discodex-runtime.json").is_file() {
        return runtime::remove_owned(runtime_dir);
    }
    inspect_owned_core_runtime_contents(runtime_dir)?;
    remove_if_file(&runtime_dir.join(codex_executable_name()))?;
    remove_if_file(&runtime_dir.join(code_mode_host_executable_name()))?;
    fs::remove_dir(runtime_dir).context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsOwnedRuntimeCleanupFailed),
    )
}

fn validate_existing_installation_plan(
    ownership_path: &Path,
    config_path: &Path,
    requested_profile: &str,
) -> Result<()> {
    let ownership = if ownership_path.exists() {
        Some(read_json::<Ownership>(ownership_path).with_context(|| {
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnosticCfc12728a10d)
                .replace("{0}", &ownership_path.display().to_string())
        })?)
    } else {
        None
    };
    let config = if config_path.exists() {
        Some(Config::load(config_path).with_context(|| {
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnosticCb03cb68368b)
                .replace("{0}", &config_path.display().to_string())
        })?)
    } else {
        None
    };
    validate_existing_profile_identity(
        ownership.is_some(),
        requested_profile,
        ownership
            .as_ref()
            .and_then(|value| value.profile.as_deref()),
        config.as_ref().map(|value| value.profile.as_str()),
    )
}

fn owned_bridge_executable(data_root: &Path) -> Option<PathBuf> {
    let executable = env::current_exe().ok()?.canonicalize().ok()?;
    is_owned_worker_executable(&executable, data_root).then_some(executable)
}

fn is_owned_worker_executable(executable: &Path, data_root: &Path) -> bool {
    let Ok(owned_bin_dir) = data_root.join("bin").canonicalize() else {
        return false;
    };
    let canonical_executable = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.to_owned());
    canonical_executable.starts_with(owned_bin_dir)
}

fn codex_executable_name() -> OsString {
    #[cfg(windows)]
    {
        OsString::from("codex.exe")
    }
    #[cfg(not(windows))]
    {
        OsString::from("codex")
    }
}

fn code_mode_host_executable_name() -> &'static str {
    #[cfg(windows)]
    {
        "codex-code-mode-host.exe"
    }
    #[cfg(not(windows))]
    {
        "codex-code-mode-host"
    }
}

fn install_user_service(
    config_path: &Path,
    ownership: &mut Ownership,
    previous_ownership: Option<&Ownership>,
    language: Language,
) -> Result<()> {
    let executable = recorded_service_executable(ownership)?
        .map(Ok)
        .unwrap_or_else(|| {
            env::current_exe()
                .context(language.message(crate::i18n::MessageKey::InstallDiagnostic6e312e3913b8))
        })?;
    #[cfg(target_os = "linux")]
    {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context(language.message(crate::i18n::MessageKey::InstallDiagnostic5469a622b9bd))?;
        let unit_dir = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let unit_dir = unit_dir.join("systemd/user");
        fs::create_dir_all(&unit_dir).with_context(|| {
            language
                .message(crate::i18n::MessageKey::InstallDiagnosticC6adae3fdf08)
                .replace("{0}", &unit_dir.display().to_string())
        })?;
        let unit_path = unit_dir.join(format!("{SERVICE_NAME}.service"));
        let loaded = Command::new("systemctl")
            .args([
                "--user",
                "show",
                "--property=LoadState",
                "--value",
                &format!("{SERVICE_NAME}.service"),
            ])
            .stdin(Stdio::null())
            .output()
            .context(
                language.message(MessageKey::InstallDiagnosticsCheckExistingSystemdUserUnit),
            )?;
        if !loaded.status.success() {
            bail!(
                "{}",
                language
                    .message(MessageKey::InstallDiagnosticsCheckSystemdServiceExists)
                    .replace("{0}", SERVICE_NAME)
            )
        }
        let unit = format!(
            "[Unit]\nDescription=Discodex Discord bridge\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={} run --config {}\nRestart=on-failure\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n",
            systemd_quote(&executable),
            systemd_quote(config_path)
        );
        let previous_record_owns_unit = previous_ownership.is_some_and(|previous| {
            previous.service_kind.as_deref() == Some("systemd-user")
                && previous.service_file.as_deref() == Some(unit_path.as_path())
                && previous.config_file == config_path
        });
        let existing_unit_matches = systemd_unit_matches(fs::read_to_string(&unit_path), &unit)
            .context(language.message(MessageKey::InstallDiagnosticsReadExistingSystemdUserUnit))?;
        let unit_loaded = String::from_utf8_lossy(&loaded.stdout).trim() != "not-found";
        if (unit_path.exists() || unit_loaded)
            && !(previous_record_owns_unit && existing_unit_matches)
        {
            bail!(
                "{}",
                language
                    .message(MessageKey::InstallDiagnosticsInstallServiceExisting)
                    .replace("{0}", &format!("{SERVICE_NAME}.service"))
            )
        }
        fs::write(&unit_path, unit).with_context(|| {
            language
                .message(crate::i18n::MessageKey::InstallDiagnostic1f34c6ac9ed5)
                .replace("{0}", &unit_path.display().to_string())
        })?;
        set_private_file(&unit_path)?;
        ownership.service_file = Some(unit_path.clone());
        ownership.service_kind = Some("systemd-user".into());
        let owner_file = config_path
            .parent()
            .context(language.message(MessageKey::InstallDiagnosticsConfigPathParentMissing))?
            .join("ownership.json");
        write_json_private(&owner_file, ownership)?;
        run_checked("systemctl", &["--user", "daemon-reload"])?;
        let unit_path_arg = unit_path
            .to_str()
            .context(language.message(crate::i18n::MessageKey::InstallDiagnostic22fb40ae0ac0))?;
        run_checked("systemctl", &["--user", "enable", "--now", unit_path_arg])?;
        Ok(())
    }
    #[cfg(windows)]
    {
        let executable = executable
            .canonicalize()
            .context(language.message(crate::i18n::MessageKey::InstallDiagnostic87d64e4bbbf3))?;
        if ownership.worker_executable.as_deref() != Some(executable.as_path()) {
            bail!(language.message(MessageKey::InstallDiagnosticsWindowsTaskExecutableNotOwned))
        }
        let task_executable = windows_task_executable_path(&executable, language)?;
        let expected_executable = ps_single_quote(&task_executable.display().to_string());
        let expected_arguments =
            ps_single_quote(&format!("run --config \"{}\"", config_path.display()));
        let previous_record_owns_task = previous_ownership.is_some_and(|previous| {
            (previous.service_kind.as_deref() == Some("scheduled-task-user")
                || previous.windows_task_pending_run_level.is_some())
                && previous.service_file.is_none()
                && previous.config_file == config_path
        });
        let (task_state, previous_level) = inspect_owned_windows_task(
            &expected_executable,
            &expected_arguments,
            ownership,
            language,
        )?;
        if !windows_task_may_be_replaced(&task_state, previous_record_owns_task) {
            bail!(
                "{}",
                language.message(MessageKey::InstallDiagnosticsInstallTaskExisting)
            )
        }
        let owner_file = config_path
            .parent()
            .context(language.message(MessageKey::InstallDiagnosticsConfigPathParentMissing))?
            .join("ownership.json");
        let registration_script = windows_task_registration_script(
            &expected_executable,
            &expected_arguments,
            previous_record_owns_task,
            previous_level,
        );
        println!(
            "Windows will request administrator consent to register this user's worker for elevated automatic startup. Worker commands will have administrator access."
        );
        // Persist intent before changing Task Scheduler. If a later ownership
        // write or rollback fails, retry/uninstall can inspect either exact level.
        ownership.windows_task_pending_run_level = Some(WindowsTaskRunLevel::Highest);
        write_json_private(&owner_file, ownership)
            .context(language.message(crate::i18n::MessageKey::InstallDiagnosticD17e52d2831b))?;
        run_elevated_powershell(&registration_script).context(
            language.message(MessageKey::InstallDiagnosticsRegisterElevatedWindowsTaskRetry),
        )?;
        ownership.service_kind = Some("scheduled-task-user".into());
        ownership.service_file = None;
        ownership.windows_task_run_level = WindowsTaskRunLevel::Highest;
        ownership.windows_task_pending_run_level = None;
        if let Err(error) = write_json_private(&owner_file, ownership) {
            let cleanup = run_elevated_powershell(&windows_task_unregistration_script(
                &expected_executable,
                &expected_arguments,
                WindowsTaskRunLevel::Highest,
                "Discodex",
            ));
            if let Err(cleanup_error) = cleanup {
                return Err(anyhow!(
                    "could not save elevated task ownership ({error:#}); task cleanup also failed ({cleanup_error:#}); the task may remain registered"
                ));
            }
            return Err(error)
                .context(language.message(crate::i18n::MessageKey::InstallDiagnosticDb958cdaa84f));
        }
        // Same-user task owners can start their task without elevating again.
        run_powershell(&windows_task_start_script(
            &expected_executable,
            &expected_arguments,
            WindowsTaskRunLevel::Highest,
        ))
        .context(language.message(crate::i18n::MessageKey::InstallDiagnosticC67d01d408f2))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let executable = executable
            .canonicalize()
            .context(language.message(crate::i18n::MessageKey::InstallDiagnostic41ee4a8baebd))?;
        let config_path = config_path
            .canonicalize()
            .context(language.message(crate::i18n::MessageKey::InstallDiagnostic01ebbf45085c))?;
        let requirement = crate::mac_signing::expected_requirement().context(
            language
                .message(MessageKey::InstallDiagnosticsLoadConfiguredMacosWorkerSigningIdentity),
        )?;
        crate::mac_signing::verify_binary(&executable, &requirement)
            .context(language.message(crate::i18n::MessageKey::InstallDiagnostic595d75e9a0f2))?;
        if previous_ownership
            .and_then(|previous| previous.mac_signing_requirement.as_deref())
            .is_some_and(|previous| previous != requirement)
        {
            bail!(
                language.message(MessageKey::InstallDiagnosticsLaunchagentSigningIdentityMismatch)
            );
        }
        ownership.mac_signing_requirement = Some(requirement);
        let (label, plist_path) = launchd_identity(&config_path)?;
        let expected_plist = launchd_plist(&label, &executable, &config_path);
        let previous_record_owns_agent = previous_ownership.is_some_and(|previous| {
            previous.service_kind.as_deref() == Some("launchd-user")
                && previous.service_file.as_deref() == Some(plist_path.as_path())
                && previous.config_file == config_path
                && previous.worker_executable.as_deref() == Some(executable.as_path())
        });
        let plist_exists = existing_launchd_plist_matches(
            &plist_path,
            &expected_plist,
            previous_record_owns_agent,
        )?;
        let loaded = launchd_is_loaded(&label, &executable, &config_path)?;
        if loaded && !previous_record_owns_agent {
            bail!(
                "{}",
                language
                    .message(MessageKey::InstallDiagnosticsLaunchagentExisting)
                    .replace("{0}", &label)
            );
        }
        if loaded {
            run_checked(
                "launchctl",
                &["bootout", &format!("{}/{}", launchd_domain()?, label)],
            )?;
            wait_for_launchd_unloaded(&label, &executable, &config_path)?;
        }
        let agent_dir = plist_path
            .parent()
            .context(language.message(crate::i18n::MessageKey::InstallDiagnosticC73901316980))?;
        fs::create_dir_all(agent_dir).with_context(|| {
            language
                .message(crate::i18n::MessageKey::InstallDiagnostic2170bcd5dab7)
                .replace("{0}", &agent_dir.display().to_string())
        })?;
        if plist_exists {
            remove_if_file(&plist_path)?;
        }
        let mut plist_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&plist_path)
            .with_context(|| {
                language
                    .message(crate::i18n::MessageKey::InstallDiagnosticC82cd426a735)
                    .replace("{0}", &plist_path.display().to_string())
            })?;
        plist_file
            .write_all(expected_plist.as_bytes())
            .with_context(|| {
                language
                    .message(crate::i18n::MessageKey::InstallDiagnostic1f34c6ac9ed5)
                    .replace("{0}", &plist_path.display().to_string())
            })?;
        plist_file.sync_all().with_context(|| {
            language
                .message(crate::i18n::MessageKey::InstallDiagnostic07af7d91b87a)
                .replace("{0}", &plist_path.display().to_string())
        })?;
        set_private_file(&plist_path)?;
        ownership.service_file = Some(plist_path.clone());
        ownership.service_kind = Some("launchd-user".into());
        ownership.worker_executable = Some(executable.clone());
        let owner_file = config_path
            .parent()
            .context(language.message(MessageKey::InstallDiagnosticsConfigPathParentMissing))?
            .join("ownership.json");
        write_json_private(&owner_file, ownership)?;
        let domain = launchd_domain()?;
        run_checked(
            "launchctl",
            &[
                "bootstrap",
                &domain,
                plist_path.to_str().context(
                    language.message(crate::i18n::MessageKey::InstallDiagnostic4e1eaa350966),
                )?,
            ],
        )?;
        run_checked("launchctl", &["kickstart", &format!("{domain}/{label}")])?;
        if !launchd_is_loaded(&label, &executable, &config_path)? {
            bail!(
                "{}",
                language
                    .message(MessageKey::InstallDiagnosticsLaunchagentNotLoaded)
                    .replace("{0}", &label)
            );
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (config_path, ownership, executable);
        bail!(
            "{}",
            language.message(crate::i18n::MessageKey::InstallDiagnostic1f1662532541)
        )
    }
}

#[cfg(target_os = "macos")]
fn launchd_identity(config_path: &Path) -> Result<(String, PathBuf)> {
    launchd_identity_for_service(config_path, SERVICE_NAME)
}

#[cfg(target_os = "macos")]
fn launchd_identity_for_service(
    config_path: &Path,
    service_name: &str,
) -> Result<(String, PathBuf)> {
    use std::os::unix::ffi::OsStrExt;

    let config_root = config_path
        .parent()
        .context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnosticD291d3959b7f),
        )?
        .canonicalize()
        .context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnosticDb69884ec0ce),
        )?;
    let digest = Sha256::digest(config_root.as_os_str().as_bytes());
    let suffix = format!("{digest:x}");
    let label = format!("{service_name}.{}", &suffix[..16]);
    let home = env::var_os("HOME").map(PathBuf::from).context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic8ff08c663d6f),
    )?;
    let plist = home
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    Ok((label, plist))
}

#[cfg(target_os = "macos")]
fn launchd_domain() -> Result<String> {
    let output = command_without_console("id").arg("-u").output().context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic49a7e8526a28),
    )?;
    if !output.status.success() {
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsLaunchdDomainIdFailed)
        );
    }
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsLaunchdDomainUidInvalid)
        );
    }
    Ok(format!("gui/{uid}"))
}

#[cfg(target_os = "macos")]
fn launchd_plist(label: &str, executable: &Path, config_path: &Path) -> String {
    fn xml(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    let config_root = config_path.parent().unwrap_or(config_path);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>run</string><string>--config</string><string>{}</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>3</integer><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string></dict></plist>\n",
        xml(label),
        xml(&executable.display().to_string()),
        xml(&config_path.display().to_string()),
        xml(&config_root.join("worker.stdout.log").display().to_string()),
        xml(&config_root.join("worker.stderr.log").display().to_string())
    )
}

#[cfg(target_os = "macos")]
fn existing_launchd_plist_matches(
    path: &Path,
    expected_contents: &str,
    ownership_matches: bool,
) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                bail!(
                    "{}",
                    crate::i18n::Language::English
                        .message(MessageKey::InstallDiagnosticsLaunchdPlistNotRegular)
                        .replace("{0}", &path.display().to_string())
                );
            }
            let matches = ownership_matches
                && fs::read_to_string(path).ok().as_deref() == Some(expected_contents);
            if !matches {
                bail!(
                    "{}",
                    crate::i18n::Language::English
                        .message(MessageKey::InstallDiagnosticsLaunchdPlistMismatch)
                        .replace("{0}", &path.display().to_string())
                );
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsLaunchdPlistInspect)
                .replace("{0}", &path.display().to_string()),
        ),
    }
}

#[cfg(target_os = "macos")]
fn launchd_is_loaded(label: &str, executable: &Path, config_path: &Path) -> Result<bool> {
    let domain = launchd_domain()?;
    let target = format!("{domain}/{label}");
    let output = command_without_console("launchctl")
        .args(["print", &target])
        .output()
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsInspectUserLaunchagentRegistration),
        )?;
    if !output.status.success() {
        let detail = format!(
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if detail.contains("Could not find service") || detail.contains("No such process") {
            return Ok(false);
        }
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsLaunchctlInspectFailed)
                .replace("{0}", &target)
                .replace("{1}", detail.trim())
        );
    }

    let report = String::from_utf8_lossy(&output.stdout);
    let expected_program = executable.display().to_string();
    let program = report
        .lines()
        .find_map(|line| line.trim().strip_prefix("program = "))
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsLoadedLaunchagentNoProgramReadback),
        )?;
    if program != expected_program {
        bail!(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsLaunchdProgramMismatch)
                .replace("{0}", label)
                .replace("{1}", &program)
        );
    }

    let mut lines = report.lines();
    let mut arguments = None;
    while let Some(line) = lines.next() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("arguments = {") {
            let mut values = Vec::new();
            let rest = rest.trim();
            if let Some(rest) = rest.strip_suffix('}') {
                if !rest.trim().is_empty() {
                    values.extend(rest.split_whitespace().map(str::to_owned));
                }
            } else {
                if !rest.is_empty() {
                    values.push(rest.to_owned());
                }
                for argument in lines.by_ref() {
                    let argument = argument.trim();
                    if argument == "}" {
                        break;
                    }
                    if !argument.is_empty() {
                        values.push(argument.to_owned());
                    }
                }
            }
            arguments = Some(values);
            break;
        }
    }
    let expected = vec![
        executable.display().to_string(),
        "run".to_owned(),
        "--config".to_owned(),
        config_path.display().to_string(),
    ];
    if arguments.as_deref() != Some(expected.as_slice()) {
        bail!(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsLoadedLaunchagentUnexpectedArguments)
                .replace("{0}", label)
        );
    }
    Ok(true)
}

#[cfg(target_os = "macos")]
fn wait_for_launchd_unloaded(label: &str, executable: &Path, config_path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match launchd_is_loaded(label, executable, config_path) {
            Ok(false) => return Ok(()),
            Ok(true) if Instant::now() >= deadline => {
                bail!(
                    crate::i18n::Language::English
                        .message(MessageKey::InstallDiagnosticsLaunchdBootoutTimeout)
                        .replace("{0}", label)
                )
            }
            Ok(true) => {}
            Err(error) if Instant::now() >= deadline => {
                return Err(error).context(
                    crate::i18n::Language::English
                        .message(MessageKey::InstallDiagnosticsLaunchagentUnloadConfirmFailed)
                        .replace("{0}", label),
                );
            }
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(any(windows, test))]
fn windows_task_recorded_levels(ownership: &Ownership) -> Vec<WindowsTaskRunLevel> {
    let mut levels = vec![ownership.windows_task_run_level];
    if let Some(pending) = ownership.windows_task_pending_run_level {
        if !levels.contains(&pending) {
            levels.push(pending);
        }
    }
    levels
}

#[cfg(windows)]
fn windows_task_name_for_config(config_path: &Path) -> &'static str {
    if config_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some(LEGACY_APP_NAME)
    {
        "CodexWorker Renewal"
    } else {
        "Discodex"
    }
}

#[cfg(windows)]
fn inspect_owned_windows_task(
    executable: &str,
    arguments: &str,
    ownership: &Ownership,
    language: Language,
) -> Result<(String, WindowsTaskRunLevel)> {
    for level in windows_task_recorded_levels(ownership) {
        let output = command_without_console("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &windows_task_inspection_script(
                    executable,
                    arguments,
                    level,
                    windows_task_name_for_config(&ownership.config_file),
                ),
            ])
            .stdin(Stdio::null())
            .output()
            .context(language.message(crate::i18n::MessageKey::InstallDiagnostic30ad19392dcf))?;
        if !output.status.success() {
            bail!(language.message(MessageKey::InstallDiagnosticsWindowsTaskInspectionFailed));
        }
        let state = String::from_utf8(output.stdout)?.trim().to_owned();
        match state.as_str() {
            "matching" | "absent" => return Ok((state, level)),
            "different" => {}
            _ => bail!(
                language.message(MessageKey::InstallDiagnosticsWindowsTaskInspectionUnexpected)
            ),
        }
    }
    Ok(("different".into(), ownership.windows_task_run_level))
}

#[cfg(windows)]
fn windows_task_contract_script(
    expected_executable: &str,
    expected_arguments: &str,
    task_name: &str,
) -> String {
    let task_name = ps_single_quote(task_name);
    format!(
        r#"$ErrorActionPreference='Stop'
function Resolve-UserSid([string]$user) {{
    if ($user.StartsWith('S-1-')) {{ return $user }}
    try {{ return ([System.Security.Principal.NTAccount]::new($user).Translate([System.Security.Principal.SecurityIdentifier])).Value }} catch {{ return '' }}
}}
function Normalize-LocalPath([string]$path) {{
    if ([string]::IsNullOrWhiteSpace($path)) {{ return '' }}
    if ($path.StartsWith('\\?\', [System.StringComparison]::OrdinalIgnoreCase)) {{ $path=$path.Substring(4) }}
    if ($path.Length -lt 3 -or $path[1] -ne ':' -or $path[2] -ne '\') {{ return '' }}
    try {{ return [System.IO.Path]::GetFullPath($path) }} catch {{ return '' }}
}}
function Same-LocalPath([string]$left, [string]$right) {{
    $left=Normalize-LocalPath $left
    $right=Normalize-LocalPath $right
    return $left -ne '' -and [System.String]::Equals($left, $right, [System.StringComparison]::OrdinalIgnoreCase)
}}
$identity=[System.Security.Principal.WindowsIdentity]::GetCurrent()
$sid=$identity.User.Value
$taskName='{task_name}'
$taskPath='\'
function Test-ExactTask($task, [string]$level) {{
    if (!$task) {{ return $false }}
    $valid=($task.Actions.Count -eq 1 -and (Same-LocalPath $task.Actions[0].Execute '{expected_executable}') -and $task.Actions[0].Arguments -ceq '{expected_arguments}')
    $valid=$valid -and ((Resolve-UserSid $task.Principal.UserId) -eq $sid) -and ($task.Principal.LogonType -eq 'Interactive') -and ($task.Principal.RunLevel -eq $level)
    $valid=$valid -and ($task.Triggers.Count -eq 1) -and ($task.Triggers[0].CimClass.CimClassName -ceq 'MSFT_TaskLogonTrigger') -and ($task.Triggers[0].Enabled -eq $true) -and ((Resolve-UserSid $task.Triggers[0].UserId) -eq $sid)
    return $valid
}}
function Stop-OwnedTask($task) {{
    if ($task.State -eq 'Running') {{
        Stop-ScheduledTask -InputObject $task
        $limit=(Get-Date).AddSeconds(15)
        do {{ Start-Sleep -Milliseconds 200; $task=Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName }} while ($task.State -eq 'Running' -and (Get-Date) -lt $limit)
        if ($task.State -eq 'Running') {{ throw 'Owned bridge task did not stop.' }}
    }}
}}
"#
    )
}

#[cfg(windows)]
fn windows_task_inspection_script(
    expected_executable: &str,
    expected_arguments: &str,
    level: WindowsTaskRunLevel,
    task_name: &str,
) -> String {
    let contract = windows_task_contract_script(expected_executable, expected_arguments, task_name);
    let level = level.powershell();
    format!(
        r#"{contract}
$t=Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName -ErrorAction SilentlyContinue
if (!$t) {{ 'absent'; exit 0 }}
if (Test-ExactTask $t '{level}') {{ 'matching' }} else {{ 'different' }}"#
    )
}

#[cfg(windows)]
fn windows_task_registration_script(
    expected_executable: &str,
    expected_arguments: &str,
    previous_record_owns_task: bool,
    previous_level: WindowsTaskRunLevel,
) -> String {
    windows_task_registration_script_named(
        expected_executable,
        expected_arguments,
        previous_record_owns_task,
        previous_level,
        "Discodex",
    )
}

#[cfg(windows)]
fn windows_task_registration_script_named(
    expected_executable: &str,
    expected_arguments: &str,
    previous_record_owns_task: bool,
    previous_level: WindowsTaskRunLevel,
    task_name: &str,
) -> String {
    let contract = windows_task_contract_script(expected_executable, expected_arguments, task_name);
    let owns = if previous_record_owns_task {
        "$true"
    } else {
        "$false"
    };
    let previous_level = previous_level.powershell();
    format!(
        r#"{contract}
$t=Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName -ErrorAction SilentlyContinue
$previousXml=$null
$wasRunning=$false
if ($t) {{
    if (!(Test-ExactTask $t '{previous_level}') -or !{owns}) {{ throw 'An existing task does not match this installation record and current Windows identity; it was left unchanged.' }}
    $previousXml=Export-ScheduledTask -InputObject $t
    $wasRunning=$t.State -eq 'Running'
    Stop-OwnedTask $t
}}
$registered=$false
try {{
    $action=New-ScheduledTaskAction -Execute '{expected_executable}' -Argument '{expected_arguments}'
    $trigger=New-ScheduledTaskTrigger -AtLogOn -User $sid
    $principal=New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Highest
    $settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    if ($previousXml) {{
        Register-ScheduledTask -TaskName $taskName -TaskPath $taskPath -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description 'Discodex Discord bridge (administrator)' -Force | Out-Null
    }} else {{
        Register-ScheduledTask -TaskName $taskName -TaskPath $taskPath -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description 'Discodex Discord bridge (administrator)' | Out-Null
    }}
    $registered=$true
    $t=Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName
    if (!(Test-ExactTask $t 'Highest')) {{ throw 'The elevated task failed its action, principal, and logon-trigger readback.' }}
}} catch {{
    $failure=$_.Exception.Message
    try {{
        $current=Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName -ErrorAction SilentlyContinue
        if ($current -and !(Test-ExactTask $current 'Highest') -and !(Test-ExactTask $current '{previous_level}')) {{ throw 'Task changed unexpectedly; it was preserved.' }}
        if ($previousXml) {{
            Register-ScheduledTask -TaskName $taskName -TaskPath $taskPath -Xml $previousXml -Force | Out-Null
            $restored=Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName
            if (!(Test-ExactTask $restored '{previous_level}')) {{ throw 'Previous task restoration failed readback.' }}
            if ($wasRunning) {{ Start-ScheduledTask -InputObject $restored }}
        }} elseif ($registered -and $current) {{
            Unregister-ScheduledTask -InputObject $current -Confirm:$false
        }}
    }} catch {{ throw "$failure Rollback also failed: $($_.Exception.Message)" }}
    throw $failure
}}
"#
    )
}

#[cfg(windows)]
fn windows_task_start_script(
    expected_executable: &str,
    expected_arguments: &str,
    level: WindowsTaskRunLevel,
) -> String {
    windows_task_start_script_named(expected_executable, expected_arguments, level, "Discodex")
}

#[cfg(windows)]
fn windows_task_start_script_named(
    expected_executable: &str,
    expected_arguments: &str,
    level: WindowsTaskRunLevel,
    task_name: &str,
) -> String {
    let contract = windows_task_contract_script(expected_executable, expected_arguments, task_name);
    let level = level.powershell();
    format!(
        r#"{contract}
$t=Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName
if (!(Test-ExactTask $t '{level}')) {{ throw 'The registered task changed before startup; it was left unchanged.' }}
Start-ScheduledTask -InputObject $t
$limit=(Get-Date).AddSeconds(15)
do {{ Start-Sleep -Milliseconds 200; $t=Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName }} while ($t.State -ne 'Running' -and (Get-Date) -lt $limit)
if ($t.State -ne 'Running') {{ throw 'The Windows user task did not reach Running state.' }}"#
    )
}

#[cfg(windows)]
fn windows_task_unregistration_script(
    expected_executable: &str,
    expected_arguments: &str,
    level: WindowsTaskRunLevel,
    task_name: &str,
) -> String {
    let contract = windows_task_contract_script(expected_executable, expected_arguments, task_name);
    let level = level.powershell();
    format!(
        r#"{contract}
$t=Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName -ErrorAction SilentlyContinue
if ($t) {{
    if (!(Test-ExactTask $t '{level}')) {{ throw 'The task no longer matches its ownership record; it was preserved.' }}
    Stop-OwnedTask $t
    Unregister-ScheduledTask -InputObject $t -Confirm:$false
}}"#
    )
}

/// Elevates only task management, leaving account/profile setup in the caller.
/// Explicit paths and a same-SID check prevent using another administrator's profile.
#[cfg(windows)]
fn run_elevated_powershell(operation: &str) -> Result<()> {
    let identity = command_without_console("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command",
            "$i=[System.Security.Principal.WindowsIdentity]::GetCurrent(); $p=[System.Security.Principal.WindowsPrincipal]::new($i); @{sid=$i.User.Value; elevated=$p.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)} | ConvertTo-Json -Compress"])
        .stdin(Stdio::null()).output().context(crate::i18n::Language::English.message(crate::i18n::MessageKey::InstallDiagnosticE376a5401de1))?;
    if !identity.status.success() {
        bail!(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsWindowsIdentityUnavailable)
        );
    }
    let identity: serde_json::Value = serde_json::from_slice(&identity.stdout).context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic686af139d784),
    )?;
    let sid = identity["sid"].as_str().context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic91ae76642013),
    )?;
    if !valid_windows_sid(sid) {
        bail!(
            crate::i18n::Language::English.message(MessageKey::InstallDiagnosticsWindowsSidInvalid)
        );
    }
    if identity["elevated"].as_bool() == Some(true) {
        return run_powershell(operation);
    }
    let temp = tempfile::tempdir().context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic9f52d4f57bd6),
    )?;
    set_private_dir(temp.path())?;
    let script_path = temp.path().join("task-operation.ps1");
    let result_path = temp.path().join("result.txt");
    fs::write(&result_path, "")?;
    set_private_file(&result_path)?;
    let result = ps_single_quote(&result_path.display().to_string());
    let script = format!(
        r#"$ErrorActionPreference='Stop'
try {{
    $identity=[System.Security.Principal.WindowsIdentity]::GetCurrent()
    if ($identity.User.Value -cne '{sid}') {{ throw 'Use the same Windows account that started setup. Another administrator account cannot manage this user profile.' }}
    $principal=[System.Security.Principal.WindowsPrincipal]::new($identity)
    if (!$principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {{ throw 'This Windows account must have administrator rights to manage elevated automatic startup.' }}
    & {{
{operation}
    }}
    exit 0
}} catch {{
    [System.IO.File]::WriteAllText('{result}', $_.Exception.Message)
    exit 1
}}
"#
    );
    // Windows PowerShell 5.1 requires a BOM for non-ASCII profile paths.
    fs::write(&script_path, format!("\u{feff}{script}"))?;
    set_private_file(&script_path)?;
    let powershell = PathBuf::from(
        env::var_os("SystemRoot").context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnostic36de562c10ec),
        )?,
    )
    .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let launcher = format!(
        r#"$ErrorActionPreference='Stop'
$p=Start-Process -FilePath '{}' -Verb RunAs -WindowStyle Hidden -ArgumentList @('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File','"{}"') -Wait -PassThru
exit $p.ExitCode"#,
        ps_single_quote(&powershell.display().to_string()),
        ps_single_quote(&script_path.display().to_string())
    );
    let launched = command_without_console(&powershell)
        .args(["-NoProfile", "-NonInteractive", "-Command", &launcher])
        .stdin(Stdio::null())
        .output()
        .context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnostic365b1e5cf848),
        )?;
    if !launched.status.success() {
        let result_detail = match fs::read_to_string(&result_path) {
            Ok(detail) => detail.trim().to_owned(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => format!("read elevated Windows task operation result: {error}"),
        };
        let mut diagnostic_parts = Vec::new();
        if !result_detail.is_empty() {
            diagnostic_parts.push(result_detail);
        }
        let launcher_stdout = String::from_utf8_lossy(&launched.stdout).trim().to_owned();
        if !launcher_stdout.is_empty() {
            diagnostic_parts.push(format!("PowerShell stdout: {launcher_stdout}"));
        }
        let launcher_stderr = String::from_utf8_lossy(&launched.stderr).trim().to_owned();
        if !launcher_stderr.is_empty() {
            diagnostic_parts.push(format!("PowerShell stderr: {launcher_stderr}"));
        }
        let detail = diagnostic_parts.join("\n");
        if detail.trim().is_empty() {
            let detail = "Windows administrator consent was cancelled or the task operation could not start.";
            bail!(
                "{}",
                Language::English
                    .message(MessageKey::InstallErrorCommandFailed)
                    .replace("{0}", "Windows task operation")
                    .replace("{1}", &launched.status.to_string())
                    .replace("{2}", detail)
            );
        }
        bail!(
            "{}",
            Language::English
                .message(MessageKey::InstallErrorCommandFailed)
                .replace("{0}", "Windows task operation")
                .replace("{1}", &launched.status.to_string())
                .replace("{2}", &detail)
        );
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn valid_windows_sid(value: &str) -> bool {
    let mut parts = value.split('-');
    parts.next() == Some("S") && parts.next() == Some("1") && {
        let numbers: Vec<_> = parts.collect();
        numbers.len() >= 2
            && numbers
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }
}

fn uninstall_user_service(
    ownership: &mut Ownership,
    owner_file: &Path,
    language: Language,
) -> Result<()> {
    let service_kind = ownership.service_kind.as_deref().or_else(|| {
        ownership
            .windows_task_pending_run_level
            .map(|_| "scheduled-task-user")
    });
    match service_kind {
        Some("systemd-user") => {
            let unit_path = ownership.service_file.as_deref().context(
                language.message(MessageKey::InstallDiagnosticsOwnedSystemdUnitPathMissing),
            )?;
            let unit_name = unit_path
                .file_name()
                .and_then(|name| name.to_str())
                .context(
                    language.message(MessageKey::InstallDiagnosticsOwnedSystemdServiceNameMissing),
                )?;
            let expected_service = if ownership
                .config_file
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some(LEGACY_APP_NAME)
            {
                LEGACY_SERVICE_NAME
            } else {
                SERVICE_NAME
            };
            if unit_name != format!("{expected_service}.service") {
                bail!(
                    language
                        .message(MessageKey::InstallDiagnosticsOwnedSystemdUnitUnexpected)
                        .replace("{0}", expected_service)
                )
            }
            run_checked("systemctl", &["--user", "disable", "--now", unit_name]).context(
                language.message(MessageKey::InstallDiagnosticsStopOwnedUserServiceFailed),
            )?;
            if let Some(path) = &ownership.service_file {
                remove_if_file(path)?;
            }
            ownership.service_kind = None;
            ownership.service_file = None;
            write_json_private(owner_file, ownership)?;
            run_checked("systemctl", &["--user", "daemon-reload"])?;
        }
        #[cfg(windows)]
        Some("scheduled-task-user") => {
            let executable =
                ownership
                    .worker_executable
                    .as_deref()
                    .context(language.message(
                        MessageKey::InstallDiagnosticsOwnedWindowsTaskExecutableMissing,
                    ))?;
            let task_executable = windows_task_executable_path(executable, language)?;
            let expected_executable = ps_single_quote(&task_executable.display().to_string());
            let expected_arguments = ps_single_quote(&format!(
                "run --config \"{}\"",
                ownership.config_file.display()
            ));
            let (state, level) = inspect_owned_windows_task(
                &expected_executable,
                &expected_arguments,
                ownership,
                language,
            )?;
            if state == "different" {
                bail!(language.message(MessageKey::InstallDiagnosticsWindowsTaskOwnershipMismatch));
            }
            if state != "absent" {
                run_elevated_powershell(&windows_task_unregistration_script(
                    &expected_executable,
                    &expected_arguments,
                    level,
                    windows_task_name_for_config(&ownership.config_file),
                ))
                .context(
                    language
                        .message(MessageKey::InstallDiagnosticsUnregisterOwnedWindowsTaskFailed),
                )?;
            }
            ownership.windows_task_pending_run_level = None;
            ownership.service_kind = None;
            ownership.service_file = None;
            write_json_private(owner_file, ownership)?;
        }
        #[cfg(target_os = "macos")]
        Some("launchd-user") => {
            let executable =
                ownership
                    .worker_executable
                    .as_deref()
                    .context(language.message(
                        MessageKey::InstallDiagnosticsOwnedLaunchagentExecutableMissing,
                    ))?;
            let config_path = ownership
                .config_file
                .canonicalize()
                .context(language.message(
                    MessageKey::InstallDiagnosticsResolveOwnedProfileConfigForLaunchagentRemoval,
                ))?;
            let service_name = if config_path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some(LEGACY_APP_NAME)
            {
                LEGACY_SERVICE_NAME
            } else {
                SERVICE_NAME
            };
            let (label, expected_plist_path) =
                launchd_identity_for_service(&config_path, service_name)?;
            let plist_path = ownership.service_file.as_deref().context(
                language.message(MessageKey::InstallDiagnosticsOwnedLaunchagentPlistMissing),
            )?;
            if plist_path != expected_plist_path {
                bail!(
                    language
                        .message(MessageKey::InstallDiagnosticsOwnedLaunchagentIdentityMismatch)
                );
            }
            let executable = executable
                .canonicalize()
                .context(language.message(MessageKey::InstallDiagnosticsOwnedBridgePathResolve))?;
            let expected_plist = launchd_plist(&label, &executable, &config_path);
            existing_launchd_plist_matches(plist_path, &expected_plist, true).context(
                language.message(MessageKey::InstallDiagnosticsOwnedLaunchagentPlistChanged),
            )?;
            if launchd_is_loaded(&label, &executable, &config_path)? {
                let domain = launchd_domain()?;
                run_checked("launchctl", &["bootout", &format!("{domain}/{label}")]).context(
                    language.message(MessageKey::InstallDiagnosticsStopOwnedLaunchagentFailed),
                )?;
            }
            if plist_path.exists() {
                remove_if_file(plist_path)?;
            }
            let config_parent = config_path
                .parent()
                .context(language.message(MessageKey::InstallDiagnosticsConfigPathParentMissing))?;
            remove_if_file(&config_parent.join("worker.stdout.log"))?;
            remove_if_file(&config_parent.join("worker.stderr.log"))?;
            ownership.service_kind = None;
            ownership.service_file = None;
            write_json_private(owner_file, ownership)?;
        }
        #[cfg(all(not(windows), not(target_os = "macos")))]
        Some("scheduled-task-user") => {
            bail!(language.message(MessageKey::InstallDiagnosticsWindowsTaskUninstallPlatform))
        }
        _ => {}
    }
    Ok(())
}

fn remove_registered_commands(
    config_path: &Path,
    credential_path: &Path,
    state_dir: &Path,
    language: Language,
) -> Result<()> {
    if !config_path.is_file() || !credential_path.is_file() {
        if state_dir.join("discord-command-id.json").exists() {
            bail!(
                language.message(MessageKey::InstallDiagnosticsOwnedDiscordCommandMarkerIncomplete)
            )
        }
        return Ok(());
    }
    let config: Config = read_json(config_path)?;
    run_async(crate::commands::unregister_commands(&config)).with_context(|| {
        format!(
            "remove `/discodex` only from configured guild {}; token came from {}",
            config.guild_id,
            credential_path.display()
        )
    })
}

async fn configure_native_profile(config: &mut Config, config_path: &Path) -> Result<()> {
    let native = crate::native::Native::connect(config).await?;
    let result = async {
        ensure_native_account(&native, config.language).await?;

        let models = native.model_catalog().await.context(crate::i18n::Language::English.message(MessageKey::InstallDiagnosticsNativeModelCatalogRead))?;
        if let Some(selected) = &config.model {
            if !crate::model_catalog::catalog_has_model(&models, selected) {
                bail!(crate::i18n::Language::English.message(MessageKey::InstallDiagnosticsNativeSelectedModelMissing));
            }
        }

        let profiles = native.permission_profiles().await.context(crate::i18n::Language::English.message(MessageKey::InstallDiagnosticsNativePermissionProfilesRead))?;
        let profile_id = choose_permission_profile(&profiles, config.permissions.as_deref(), config.language)?;
        config.permissions = Some(profile_id.clone());
        write_json_private(config_path, config)?;
        println!("The live permission catalog allows profile `{profile_id}`; its exact ID was saved for the runtime's first thread.");
        Ok::<(), anyhow::Error>(())
    }.await;
    let shutdown = native.shutdown().await;
    result?;
    shutdown.context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnosticBbde88b3361c),
    )?;
    Ok(())
}

async fn ensure_native_account(
    native: &crate::native::Native,
    language: Language,
) -> Result<serde_json::Value> {
    let account_read = || native.request("account/read", serde_json::json!({}));
    let mut response = account_read().await.context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnosticB8e810f6a08f),
    )?;
    if !crate::model_catalog::needs_chatgpt_login(&response) {
        println!(
            "{}",
            language.message(MessageKey::InstallUsingTheActiveCodexProviderAndItsExisting)
        );
        return Ok(response);
    }

    let mut events = native.subscribe();
    let login = native
        .request(
            "account/login/start",
            serde_json::json!({"type": "chatgpt"}),
        )
        .await
        .context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnosticB9008a5c8306),
        )?;
    let login_id = login
        .get("loginId")
        .and_then(serde_json::Value::as_str)
        .context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnostic9bb67b063a71),
        )?
        .to_owned();
    let auth_url = login
        .get("authUrl")
        .and_then(serde_json::Value::as_str)
        .context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnostic804d161cfb96),
        )?;
    println!(
        "Complete ChatGPT sign-in in the official Codex login page. MFA and other identity checks remain user interactions."
    );
    println!("Official Codex authorization URL: {auth_url}");
    if let Err(error) = open_login_url(auth_url) {
        eprintln!("Could not open a browser automatically: {error:#}");
    }

    let completed = tokio::time::timeout(std::time::Duration::from_secs(30 * 60), async {
        loop {
            match events.recv().await {
                Ok(event)
                    if event.get("method").and_then(serde_json::Value::as_str)
                        == Some("account/login/completed") =>
                {
                    let params = event.get("params").unwrap_or(&serde_json::Value::Null);
                    if params.get("loginId").and_then(serde_json::Value::as_str) != Some(&login_id)
                    {
                        continue;
                    }
                    if params.get("success").and_then(serde_json::Value::as_bool) == Some(true) {
                        break Ok::<(), anyhow::Error>(());
                    }
                    let reason = params
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("login was not completed");
                    break Err(anyhow!("official ChatGPT login failed: {reason}"));
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break Err(anyhow!(
                        "Codex app-server connection closed during ChatGPT login"
                    ));
                }
            }
        }
    })
    .await
    .context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnosticF7a7a98ad83b),
    )?;
    completed?;

    response = account_read().await.context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic9e2391b40a76),
    )?;
    if response
        .pointer("/account/type")
        .and_then(serde_json::Value::as_str)
        != Some("chatgpt")
    {
        bail!(
            "{}",
            language
                .message(MessageKey::InstallDiagnosticsChatgptLoginAccountMissing)
                .replace("{0}", &serde_json::to_string(&response)?)
        );
    }
    println!("Codex account/read now confirms a ChatGPT account.");
    Ok(response)
}

#[cfg(target_os = "linux")]
fn open_login_url(url: &str) -> Result<()> {
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnosticD2b430952bb1),
        )?;
    Ok(())
}

#[cfg(windows)]
fn open_login_url(url: &str) -> Result<()> {
    Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context(
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnostic8dc4f52e9de4),
        )?;
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn open_login_url(_url: &str) -> Result<()> {
    bail!(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnosticFeda221fb8f4)
    )
}

fn permission_profile_is_allowed(item: &serde_json::Value) -> bool {
    item.get("allowed").and_then(serde_json::Value::as_bool) == Some(true)
        && item
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| !id.is_empty())
}

fn permission_profile_is_full_access(item: &serde_json::Value) -> bool {
    permission_profile_is_allowed(item)
        && (item.get("id").and_then(serde_json::Value::as_str) == Some(":danger-full-access")
            || ["name", "label", "description"].iter().any(|key| {
                item.get(key)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|text| text.to_ascii_lowercase().contains("full access"))
            }))
}

fn permission_profile_is_canonical_full_access(item: &serde_json::Value) -> bool {
    permission_profile_is_allowed(item)
        && item.get("id").and_then(serde_json::Value::as_str) == Some(":danger-full-access")
}

fn preferred_permission_profile_id(
    entries: &[serde_json::Value],
    preferred_id: Option<&str>,
) -> Option<String> {
    let preferred_id = preferred_id?;
    entries
        .iter()
        .find(|item| {
            item.get("id").and_then(serde_json::Value::as_str) == Some(preferred_id)
                && permission_profile_is_allowed(item)
        })
        .and_then(|item| item.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

fn default_permission_profile_id(entries: &[serde_json::Value]) -> Option<String> {
    entries
        .iter()
        .find(|item| permission_profile_is_canonical_full_access(item))
        .or_else(|| {
            entries
                .iter()
                .find(|item| permission_profile_is_full_access(item))
        })
        .or_else(|| {
            entries
                .iter()
                .find(|item| permission_profile_is_allowed(item))
        })
        .and_then(|item| item.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

fn choose_permission_profile(
    catalog: &serde_json::Value,
    preferred_id: Option<&str>,
    language: Language,
) -> Result<String> {
    let entries = catalog
        .get("data")
        .and_then(serde_json::Value::as_array)
        .context(
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsPermissionProfileDataMissing),
        )?;
    if let Some(id) = preferred_permission_profile_id(entries, preferred_id) {
        println!("The current native catalog still allows saved permission profile `{id}`.");
        return Ok(id);
    }
    let default_id = default_permission_profile_id(entries).context(
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsNativePermissionProfilesMissing),
    )?;
    println!(
        "Choose an allowed permission profile by its exact ID. Press Enter to use the default."
    );
    for item in entries {
        println!(
            "  id={} allowed={} description={}",
            item.get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("(missing)"),
            permission_profile_is_allowed(item),
            item.get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        );
    }
    loop {
        let selected = ask_text(
            language,
            MessageKey::InstallAllowedPermissionProfileId,
            &default_id,
        )?;
        if let Some(item) = entries.iter().find(|item| {
            item.get("id").and_then(serde_json::Value::as_str) == Some(selected.as_str())
        }) {
            if permission_profile_is_allowed(item) {
                return Ok(selected);
            }
            println!("That profile is not allowed by the current native catalog.");
        } else {
            println!("That ID is absent from the current native catalog.");
        }
    }
}

fn run_async<F, T>(future: F) -> Result<T>
where
    F: Future<Output = Result<T>> + Send,
    T: Send,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context(
                crate::i18n::Language::English
                    .message(crate::i18n::MessageKey::InstallDiagnosticAb07350c58fb),
            )?
            .block_on(future)
    }
}

fn remove_if_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedFileRemove)
                .replace("{0}", &path.display().to_string())
        }),
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| {
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsJsonReadPath)
            .replace("{0}", &path.display().to_string())
    })?;
    serde_json::from_slice(&bytes).with_context(|| {
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsJsonParsePath)
            .replace("{0}", &path.display().to_string())
    })
}

fn write_json_private<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context(
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnosticEeda37817ff9),
    )?;
    fs::create_dir_all(parent).with_context(|| {
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsJsonCreateParent)
            .replace("{0}", &parent.display().to_string())
    })?;
    set_private_dir(parent)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(path, bytes).with_context(|| {
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsJsonWritePath)
            .replace("{0}", &path.display().to_string())
    })?;
    set_private_file(path)
}

fn write_secret(path: &Path, secret: &str) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| {
            crate::i18n::Language::English
                .message(MessageKey::InstallDiagnosticsOwnedSecretReplace)
                .replace("{0}", &path.display().to_string())
        })?;
    }
    fs::write(path, format!("{secret}\n")).with_context(|| {
        crate::i18n::Language::English
            .message(MessageKey::InstallDiagnosticsOwnedSecretWrite)
            .replace("{0}", &path.display().to_string())
    })?;
    set_private_file(path)
}

fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| {
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnosticC6adae3fdf08)
            .replace("{0}", &path.display().to_string())
    })?;
    set_private_dir(path)
}

fn set_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).with_context(|| {
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnostic717e831a3658)
                .replace("{0}", &path.display().to_string())
        })?;
    }
    #[cfg(windows)]
    {
        run_powershell(&format!(
            "$ErrorActionPreference='Stop'; icacls.exe '{}' /inheritance:r /grant:r \"$($env:USERNAME):(OI)(CI)F\" | Out-Null; if ($LASTEXITCODE -ne 0) {{ throw 'Could not restrict directory permissions.' }}",
            ps_single_quote(&path.display().to_string())
        ))?;
    }
    Ok(())
}

fn set_private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(|| {
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallDiagnosticAb8520f1e238)
                .replace("{0}", &path.display().to_string())
        })?;
    }
    #[cfg(windows)]
    {
        run_powershell(&format!(
            "$ErrorActionPreference='Stop'; icacls.exe '{}' /inheritance:r /grant:r \"$($env:USERNAME):F\" | Out-Null; if ($LASTEXITCODE -ne 0) {{ throw 'Could not restrict file permissions.' }}",
            ps_single_quote(&path.display().to_string())
        ))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).with_context(|| {
        crate::i18n::Language::English
            .message(crate::i18n::MessageKey::InstallDiagnostic19693d52d60e)
            .replace("{0}", &path.display().to_string())
    })
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn systemd_quote(path: &Path) -> String {
    let value = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`");
    format!("\"{value}\"")
}

#[cfg(windows)]
fn run_powershell(script: &str) -> Result<()> {
    run_checked(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", script],
    )
}

#[cfg(windows)]
fn ps_single_quote(value: &str) -> String {
    value.replace('\'', "''")
}

fn run_checked(program: &str, args: &[&str]) -> Result<()> {
    let output = command_without_console(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| {
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::UpdateErrorStartCommand)
                .replace("{0}", program)
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!(
            "{}",
            crate::i18n::Language::English
                .message(crate::i18n::MessageKey::InstallErrorCommandFailed)
                .replace("{0}", program)
                .replace("{1}", &output.status.to_string())
                .replace("{2}", &detail)
        );
    }
    Ok(())
}

fn command_without_console(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn env_language() -> Result<Option<Language>> {
    parse_environment_language(env::var_os("DISCODEX_LANGUAGE"))
}

fn parse_environment_language(value: Option<std::ffi::OsString>) -> Result<Option<Language>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            Language::English
                .message(MessageKey::InstallErrorEnvironmentVariableNotUtf8)
                .replace("{0}", "DISCODEX_LANGUAGE")
        )
    })?;
    let language = value.parse::<Language>().map_err(|_| {
        anyhow::anyhow!(
            "{}",
            Language::English.message(MessageKey::InstallLanguageEnvironmentError)
        )
    })?;
    Ok(Some(language))
}

fn systemd_unit_matches(read: io::Result<String>, expected: &str) -> io::Result<bool> {
    match read {
        Ok(existing) => Ok(existing == expected),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn explicit_token_environment_value(
    name: &str,
    value: Option<std::ffi::OsString>,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            Language::English
                .message(MessageKey::InstallErrorEnvironmentVariableNotUtf8)
                .replace("{0}", name)
        )
    })?;
    if value.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(value.to_owned()))
    }
}

fn select_token_environment(
    primary: Result<Option<String>>,
    legacy: impl FnOnce() -> Result<Option<String>>,
    saved: Option<String>,
) -> Result<String> {
    let token = match primary? {
        Some(token) => Some(token),
        None => legacy()?,
    };
    Ok(token.or(saved).unwrap_or_default())
}

fn configured_language(language: Language) -> Result<Language> {
    Ok(env_language()?.unwrap_or(language))
}

fn saved_install_language(roots: &Roots) -> Result<Option<Language>> {
    let config_path = roots.config.join("config.json");
    if config_path.is_file() {
        return Ok(Some(Config::load(&config_path)?.language));
    }
    let list_path = roots.config.join("installation-list.json");
    if list_path.is_file() {
        return Ok(Some(read_json::<InstallationList>(&list_path)?.language));
    }
    Ok(None)
}

fn ask_install_message_access(
    language: Language,
    operator_ids: &[u64],
    default_channels: &[u64],
) -> Result<MessageAccess> {
    let policy = loop {
        println!(
            "{}",
            language.message(MessageKey::InstallMessageReceivePolicy1EveryoneAllServersAn)
        );
        let choice = ask_text(language, MessageKey::InstallChoosePolicy14, "4")?;
        match choice.trim().to_ascii_lowercase().as_str() {
            "1" | "everyone" => break MessagePolicy::Everyone,
            "2" | "registeredserver" | "registered-server" => {
                break MessagePolicy::RegisteredServer;
            }
            "3" | "registeredchannels" | "registered-channels" => {
                break MessagePolicy::RegisteredChannels;
            }
            "4" | "paired" | "pairedusers" | "paired-users" => break MessagePolicy::Paired,
            _ => println!("{}", language.message(MessageKey::InstallChoose123Or4)),
        }
    };

    let paired_user_ids = if policy == MessagePolicy::Paired {
        ask_ids_with_default(
            language,
            MessageKey::InstallPairedUserIdsEnterNoneToClear,
            operator_ids,
        )?
    } else {
        operator_ids.to_vec()
    };
    let channel_ids = if policy == MessagePolicy::RegisteredChannels {
        ask_ids_with_default(
            language,
            MessageKey::InstallRegisteredChannelIdsBlankKeepsHomeTranscri,
            default_channels,
        )?
    } else {
        default_channels.to_vec()
    };
    let whitelist = ask_optional_ids(
        language,
        MessageKey::InstallWhitelistUserIdsOptionalCommaSeparated,
    )?;
    let blacklist = ask_optional_ids(
        language,
        MessageKey::InstallBlacklistUserIdsOptionalCommaSeparated,
    )?;
    Ok(MessageAccess {
        policy,
        paired_user_ids,
        whitelist,
        blacklist,
        channel_ids,
    })
}

fn ask_ids_with_default(
    language: Language,
    key: MessageKey,
    default_ids: &[u64],
) -> Result<Vec<u64>> {
    let default = default_ids
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    loop {
        let line = ask_text(language, key, &default)?;
        let line = line.trim();
        if line.eq_ignore_ascii_case("none") {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        let mut valid = true;
        for item in line
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            match item.parse::<u64>() {
                Ok(id) => ids.push(id),
                Err(_) => {
                    valid = false;
                    break;
                }
            }
        }
        if valid && !ids.is_empty() {
            return Ok(ids);
        }
        println!(
            "{}",
            language.message(MessageKey::InstallEnterNumericIdsSeparatedByCommasOrNoneTo)
        );
    }
}

fn ask_optional_ids(language: Language, key: MessageKey) -> Result<Vec<u64>> {
    loop {
        let line = ask_text(language, key, "")?;
        if line.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        let mut valid = true;
        for item in line
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            match item.parse::<u64>() {
                Ok(id) => ids.push(id),
                Err(_) => {
                    valid = false;
                    break;
                }
            }
        }
        if valid && !ids.is_empty() {
            return Ok(ids);
        }
        println!(
            "{}",
            language.message(MessageKey::InstallEnterNumericIdsSeparatedByCommasOrLeaveB)
        );
    }
}

fn choose_language(existing: Option<Language>) -> Result<Language> {
    if let Some(language) = env_language()?.or(existing) {
        return Ok(language);
    }
    println!("Choose language / 언어 선택 / 选择语言 / 言語を選択");
    println!("  1) English (default)  2) 한국어  3) 简体中文  4) 日本語");
    loop {
        let choice = ask("Language", "1")?;
        match choice.trim().to_ascii_lowercase().as_str() {
            "1" | "en" | "english" => return Ok(Language::English),
            "2" | "ko" | "korean" => return Ok(Language::Korean),
            "3" | "zh" | "zh-cn" | "chinese" => return Ok(Language::Chinese),
            "4" | "ja" | "japanese" => return Ok(Language::Japanese),
            _ => println!("Choose 1, 2, 3, or 4."),
        }
    }
}

fn ask_text(language: Language, key: MessageKey, default: &str) -> Result<String> {
    ask(language.message(key), default)
}

fn ask(prompt: &str, default: &str) -> Result<String> {
    print!("{prompt} [{default}]: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim();
    Ok(if answer.is_empty() {
        default.to_owned()
    } else {
        answer.to_owned()
    })
}

fn ask_u64(language: Language, key: MessageKey) -> Result<u64> {
    loop {
        let value = ask_text(language, key, "")?;
        match value.parse() {
            Ok(value) => return Ok(value),
            Err(_) => println!(
                "{}",
                language.message(MessageKey::InstallEnterTheNumericDiscordId)
            ),
        }
    }
}

fn ask_ids(language: Language, key: MessageKey) -> Result<Vec<u64>> {
    loop {
        let line = ask_text(language, key, "")?;
        let mut ids = Vec::new();
        let mut valid = true;
        for item in line
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            match item.parse() {
                Ok(id) => ids.push(id),
                Err(_) => {
                    valid = false;
                    break;
                }
            }
        }
        if valid && !ids.is_empty() {
            return Ok(ids);
        }
        println!(
            "{}",
            language.message(MessageKey::InstallEnterOneOrMoreNumericIdsSeparatedByComma)
        );
    }
}

fn optional_value(language: Language, key: MessageKey) -> Result<Option<String>> {
    let value = ask_text(language, key, "")?;
    Ok((!value.is_empty()).then_some(value))
}

fn unix_time_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_language_environment_value_is_validated_without_fallback() {
        assert_eq!(parse_environment_language(None).unwrap(), None);
        assert_eq!(
            parse_environment_language(Some("ko".into())).unwrap(),
            Some(Language::Korean)
        );
        assert!(parse_environment_language(Some("invalid".into())).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn invalid_environment_encoding_reports_name_without_value() {
        use std::os::unix::ffi::OsStringExt;
        let secret = std::ffi::OsString::from_vec(vec![0xff, 0xfe]);
        let error = explicit_token_environment_value("DISCODEX_DISCORD_TOKEN", Some(secret))
            .unwrap_err()
            .to_string();
        assert!(error.contains("DISCODEX_DISCORD_TOKEN"));
        assert!(!error.contains("0xff"));
    }

    #[test]
    fn selected_token_uses_legacy_value_only_when_primary_is_missing() {
        let selected = select_token_environment(
            Ok(Some("primary-token".into())),
            || anyhow::bail!("legacy value is invalid"),
            Some("saved-token".into()),
        )
        .unwrap();
        assert_eq!(selected, "primary-token");

        let selected = select_token_environment(
            Ok(None),
            || Ok(Some("legacy-token".into())),
            Some("saved-token".into()),
        )
        .unwrap();
        assert_eq!(selected, "legacy-token");

        let selected =
            select_token_environment(Ok(None), || Ok(None), Some("saved-token".into())).unwrap();
        assert_eq!(selected, "saved-token");
    }

    #[test]
    fn systemd_unit_read_distinguishes_absence_from_read_failure() {
        assert!(!systemd_unit_matches(Err(io::ErrorKind::NotFound.into()), "expected").unwrap());
        assert!(systemd_unit_matches(Ok("expected".into()), "expected").unwrap());
        assert!(!systemd_unit_matches(Ok("different".into()), "expected").unwrap());
        let error = systemd_unit_matches(Err(io::ErrorKind::PermissionDenied.into()), "expected")
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn antigravity_installer_choices_select_sharing_without_enabling_early() {
        assert_eq!(
            antigravity_settings_choice("1"),
            Some(AntigravityInstallChoice::SharedSettings)
        );
        assert_eq!(
            antigravity_settings_choice("2"),
            Some(AntigravityInstallChoice::SeparateSettings)
        );
        assert_eq!(antigravity_settings_choice("invalid"), None);
        let mut preferences = NativeFeaturePreferences {
            subagent_model: Some("example-child-model".into()),
            approval_policy: serde_json::json!("on-request"),
            ..NativeFeaturePreferences::default()
        };
        assert!(!preferences.antigravity_enabled);
        apply_antigravity_ready(&mut preferences, false);
        assert!(preferences.antigravity_enabled);
        assert_eq!(preferences.antigravity_share_settings, Some(false));
        assert_eq!(
            preferences.subagent_model.as_deref(),
            Some("example-child-model")
        );
        assert_eq!(preferences.approval_policy, serde_json::json!("on-request"));
        apply_antigravity_ready(&mut preferences, true);
        assert_eq!(preferences.antigravity_share_settings, Some(true));
    }

    #[test]
    fn unattended_install_skip_preserves_legacy_antigravity_preferences() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_dir = directory.path().join("state");
        let store = NativeFeatureStore::new(&state_dir);
        let preferences = NativeFeaturePreferences {
            antigravity_enabled: true,
            antigravity_share_settings: None,
            antigravity_models: vec!["example-model".into()],
            ..NativeFeaturePreferences::default()
        };
        store.apply(&preferences)?;
        let config = crate::config::Config {
            profile: "example".into(),
            name: "Example".into(),
            token_env: "DISCODEX_TEST_TOKEN".into(),
            guild_id: 1,
            home_channel_id: 2,
            transcript_channel_id: 3,
            codex_path: directory.path().join("codex"),
            project_dir: directory.path().into(),
            state_dir,
            credentials_file: None,
            operator_ids: Vec::new(),
            message_access: None,
            voice_channel_ids: Vec::new(),
            worker_path: None,
            codex_home: None,
            model: None,
            effort: None,
            service_tier: None,
            memory_enabled: None,
            permissions: None,
            voice: None,
            input_mode: crate::config::InputMode::default(),
            language: crate::i18n::Language::English,
        };

        configure_antigravity_install(&config, None)?;

        assert_eq!(store.read()?, preferences);
        Ok(())
    }

    #[test]
    fn antigravity_terminal_cancellation_is_available_in_every_stage() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        for (code, navigation) in [(false, false), (true, false), (false, true)] {
            for key in ['c', 'd'] {
                let event = Event::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL));
                assert_eq!(
                    antigravity_terminal_input(event, &mut String::new(), code, navigation),
                    SetupTerminalInput::Cancel
                );
            }
        }
    }

    #[test]
    fn antigravity_terminal_submits_codes_as_complete_lines_and_native_navigation() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        let mut text = String::new();
        assert_eq!(
            antigravity_terminal_input(key(KeyCode::Char('a')), &mut text, false, false),
            SetupTerminalInput::Wait
        );
        assert!(text.is_empty());
        for character in "example-code".chars() {
            assert_eq!(
                antigravity_terminal_input(key(KeyCode::Char(character)), &mut text, true, false),
                SetupTerminalInput::Wait
            );
        }
        assert_eq!(
            antigravity_terminal_input(key(KeyCode::Enter), &mut text, true, false),
            SetupTerminalInput::Send("example-code\r".into())
        );
        assert!(text.is_empty());
        assert_eq!(
            antigravity_terminal_input(key(KeyCode::Down), &mut text, false, true),
            SetupTerminalInput::Send("\u{1b}[B".into())
        );
        assert_eq!(
            antigravity_terminal_input(key(KeyCode::Enter), &mut text, false, true),
            SetupTerminalInput::Send("\r".into())
        );
        assert_eq!(
            antigravity_terminal_input(key(KeyCode::Enter), &mut text, false, false),
            SetupTerminalInput::Wait
        );
    }

    #[test]
    fn mac_update_failure_before_replacement_keeps_owned_bytes_unchanged() {
        use UpdateRecoveryStep::*;

        for (booted_out, still_loaded, expected_steps) in [
            (false, true, vec![DiscardPreparedRuntime]),
            (true, true, vec![DiscardPreparedRuntime]),
            (true, false, vec![RestoreService, DiscardPreparedRuntime]),
        ] {
            let original = (
                b"installed worker".to_vec(),
                b"existing profile and session reference".to_vec(),
                b"existing ownership".to_vec(),
            );
            let mut current = original.clone();
            let mut service_loaded = still_loaded;
            let mut prepared_runtime = true;
            let mut steps = Vec::new();
            recover_mac_update(booted_out, false, still_loaded, |step| {
                steps.push(step);
                match step {
                    EnsureServiceStopped => service_loaded = false,
                    RestoreWorker | RestoreProfile => {
                        // A restore here would overwrite the existing installation.
                        current = (Vec::new(), Vec::new(), Vec::new());
                    }
                    DiscardPreparedRuntime => prepared_runtime = false,
                    RestoreService => service_loaded = true,
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(steps, expected_steps);
            assert_eq!(current, original);
            assert!(service_loaded);
            assert!(!prepared_runtime);
        }
    }

    #[test]
    fn mac_update_recovery_preserves_stop_failure_without_later_writes() {
        use UpdateRecoveryStep::*;

        let mut steps = Vec::new();
        let error = recover_mac_update(true, true, true, |step| {
            steps.push(step);
            if step == EnsureServiceStopped {
                bail!("owned service could not stop");
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "owned service could not stop");
        assert_eq!(steps, vec![EnsureServiceStopped]);
    }

    #[test]
    fn update_recovery_stops_at_failed_dependencies_and_cleans_up_after_restoring_service() {
        use UpdateRecoveryStep::*;

        let all_steps = [
            EnsureServiceStopped,
            RestoreWorker,
            RestoreProfile,
            RestoreService,
            DiscardPreparedRuntime,
        ];
        for mac in [false, true] {
            for (failure, cause, expected_state, expected_count) in [
                (
                    Some(EnsureServiceStopped),
                    "service stop rejected",
                    ("new worker", "new profile", true, true),
                    1,
                ),
                (
                    Some(RestoreWorker),
                    "worker snapshot write failed",
                    ("new worker", "new profile", false, true),
                    2,
                ),
                (
                    Some(RestoreProfile),
                    "profile snapshot write failed",
                    ("old worker", "new profile", false, true),
                    3,
                ),
                (
                    Some(RestoreService),
                    "previous service did not become active",
                    ("old worker", "old profile", false, true),
                    4,
                ),
                (
                    Some(DiscardPreparedRuntime),
                    "unused runtime cleanup failed",
                    ("old worker", "old profile", true, true),
                    5,
                ),
                (None, "", ("old worker", "old profile", true, false), 5),
            ] {
                let mut state = ("new worker", "new profile", true, true);
                let mut steps = Vec::new();
                let recover = |step| {
                    steps.push(step);
                    if Some(step) == failure {
                        bail!("{cause}");
                    }
                    match step {
                        EnsureServiceStopped => state.2 = false,
                        RestoreWorker => state.0 = "old worker",
                        RestoreProfile => state.1 = "old profile",
                        RestoreService => {
                            assert_eq!((state.0, state.1), ("old worker", "old profile"));
                            state.2 = true;
                        }
                        DiscardPreparedRuntime => {
                            assert_eq!(
                                (state.0, state.1, state.2),
                                ("old worker", "old profile", true)
                            );
                            state.3 = false;
                        }
                    }
                    Ok(())
                };
                let result = if mac {
                    recover_mac_update(true, true, true, recover)
                } else {
                    recover_update(true, true, true, recover)
                };
                if failure.is_some() {
                    assert_eq!(result.unwrap_err().to_string(), cause);
                } else {
                    result.unwrap();
                }
                assert_eq!(steps, all_steps[..expected_count]);
                assert_eq!(state, expected_state);
            }
        }
    }

    #[test]
    fn project_directory_is_created_and_canonicalized() {
        let root = tempfile::tempdir().expect("temporary parent directory");
        let selected = root.path().join("new").join("nested").join("project");
        let prepared = prepare_project_directory(&selected, Language::English)
            .expect("prepare project directory");
        assert_eq!(
            prepared,
            selected.canonicalize().expect("canonical project path")
        );
    }

    #[test]
    fn update_args_require_explicit_source_and_config() {
        let args = vec![
            "--source".to_owned(),
            "/tmp/discodex-new".to_owned(),
            "--config".to_owned(),
            "/profile/config.json".to_owned(),
        ];
        let parsed = parse_update_args(&args).unwrap();
        assert_eq!(parsed.source, PathBuf::from("/tmp/discodex-new"));
        assert_eq!(parsed.config, PathBuf::from("/profile/config.json"));
        assert!(parse_update_args(&args[..2]).is_err());
        let duplicate = vec![
            "--source".to_owned(),
            "one".to_owned(),
            "--source".to_owned(),
            "two".to_owned(),
            "--config".to_owned(),
            "config.json".to_owned(),
        ];
        assert!(parse_update_args(&duplicate).is_err());
    }

    #[test]
    fn staged_migration_worker_is_selected_from_verified_marker() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = temp
            .path()
            .join("profiles/alpha/config")
            .join(LEGACY_APP_NAME);
        let roots = Roots {
            config: rename_legacy_path(&legacy),
            data: rename_legacy_path(
                &temp
                    .path()
                    .join("profiles/alpha/data")
                    .join(LEGACY_APP_NAME),
            ),
        };
        create_private_dir(&roots.config).unwrap();
        create_private_dir(&roots.data).unwrap();
        let old_config_root = legacy;
        fs::write(
            roots.config.join("migration-in-progress"),
            serde_json::to_vec(&old_config_root).unwrap(),
        )
        .unwrap();
        let source = temp.path().join("migration-worker-fixture");
        fs::write(&source, b"migration worker fixture").unwrap();
        let staged = stage_worker_file(&source, &roots.data).unwrap();
        assert_eq!(staged_migration_worker(&roots).unwrap(), Some(staged));
        fs::remove_file(roots.config.join("migration-in-progress")).unwrap();
        assert_eq!(staged_migration_worker(&roots).unwrap(), None);
    }

    #[test]
    fn windows_task_level_preserves_legacy_and_elevated_ownership() {
        let legacy = serde_json::json!({
            "formatVersion": 1, "configFile": "/profile/config.json",
            "credentialsFile": "/profile/discord-token",
            "installListFile": "/profile/installation-list.json",
            "runtimeDir": "/profile/runtime", "runtimeExecutable": null,
            "serviceFile": null, "serviceKind": "scheduled-task-user",
            "codexSource": "existing-cli", "codexPath": "/codex",
            "codexVersion": null, "stateDir": "/profile/state", "projectDir": "/project"
        });
        let mut owner: Ownership = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(
            windows_task_recorded_levels(&owner),
            vec![WindowsTaskRunLevel::Limited]
        );
        owner.windows_task_pending_run_level = Some(WindowsTaskRunLevel::Highest);
        let interrupted: Ownership =
            serde_json::from_value(serde_json::to_value(&owner).unwrap()).unwrap();
        assert_eq!(
            windows_task_recorded_levels(&interrupted),
            vec![WindowsTaskRunLevel::Limited, WindowsTaskRunLevel::Highest,]
        );
        owner.windows_task_run_level = WindowsTaskRunLevel::Highest;
        owner.windows_task_pending_run_level = None;
        let persisted = serde_json::to_value(&owner).unwrap();
        let restored: Ownership = serde_json::from_value(persisted).unwrap();
        assert_eq!(
            restored.windows_task_run_level,
            WindowsTaskRunLevel::Highest
        );
        assert_eq!(restored.config_file, owner.config_file);
        assert_eq!(restored.state_dir, owner.state_dir);
        let mut invalid = legacy;
        invalid["windowsTaskRunLevel"] = serde_json::json!("system");
        assert!(serde_json::from_value::<Ownership>(invalid).is_err());
    }

    #[test]
    fn windows_elevation_sid_accepts_only_numeric_components() {
        assert!(valid_windows_sid("S-1-5-21-123456-789012-345678-1001"));
        assert!(!valid_windows_sid(""));
        assert!(!valid_windows_sid("S-1-5"));
        assert!(!valid_windows_sid("S-1-5-"));
        assert!(!valid_windows_sid("S-1-5-21'; exit 0; '"));
        assert!(!valid_windows_sid("S-1-5-21\n"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_task_replacement_requires_both_a_matching_task_and_ownership_record() {
        assert!(windows_task_may_be_replaced("absent", false));
        assert!(windows_task_may_be_replaced("absent", true));
        assert!(windows_task_may_be_replaced("matching", true));
        assert!(!windows_task_may_be_replaced("matching", false));
        assert!(!windows_task_may_be_replaced("different", true));
        assert!(!windows_task_may_be_replaced("unexpected", true));
    }

    #[cfg(windows)]
    #[test]
    fn task_names_follow_the_recorded_profile_layout() {
        assert_eq!(
            windows_task_name_for_config(Path::new(
                r"C:\profiles\alpha\config\codex-worker\config.json"
            )),
            "CodexWorker Renewal"
        );
        assert_eq!(
            windows_task_name_for_config(Path::new(
                r"C:\profiles\alpha\config\discodex\config.json"
            )),
            "Discodex"
        );
    }

    #[cfg(windows)]
    #[test]
    fn migration_preserves_recorded_config_spelling_after_file_identity_check() {
        let temp = tempfile::tempdir().unwrap();
        let recorded = temp.path().join("config.json");
        let other = temp.path().join("other.json");
        fs::write(&recorded, b"config").unwrap();
        fs::write(&other, b"other").unwrap();
        let selected = recorded.canonicalize().unwrap();
        assert_eq!(
            checked_recorded_config_path(&recorded, &selected).unwrap(),
            recorded
        );
        assert!(checked_recorded_config_path(&recorded, &other).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn scheduled_task_executable_path_strips_only_a_local_verbatim_prefix() {
        assert_eq!(
            windows_task_executable_path(
                Path::new(r"\\?\C:\Users\test\AppData\Local\codex-worker\bin\worker.exe"),
                Language::English
            )
            .expect("normalize a local extended path"),
            PathBuf::from(r"C:\Users\test\AppData\Local\codex-worker\bin\worker.exe")
        );
        assert_eq!(
            windows_task_executable_path(
                Path::new(r"C:\Users\test\AppData\Local\codex-worker\bin\worker.exe"),
                Language::English
            )
            .expect("retain a local DOS path"),
            PathBuf::from(r"C:\Users\test\AppData\Local\codex-worker\bin\worker.exe")
        );
        assert!(
            windows_task_executable_path(
                Path::new(r"\\?\UNC\server\share\worker.exe"),
                Language::English
            )
            .is_err()
        );
        assert!(
            windows_task_executable_path(
                Path::new(r"\\server\share\worker.exe"),
                Language::English
            )
            .is_err()
        );
    }

    #[test]
    fn reinstall_keeps_profile_preferences_and_allowed_permission_selection() {
        let native_home_dir = tempfile::tempdir().expect("temporary native home");
        let native_home = native_home_dir.path().join("native-home");
        let previous = Config {
            language: Language::Korean,
            profile: "alpha".into(),
            name: "Alpha identity".into(),
            token_env: "OLD_TOKEN_ENV".into(),
            credentials_file: Some("old-token".into()),
            guild_id: 1,
            home_channel_id: 2,
            transcript_channel_id: 3,
            operator_ids: vec![4],
            message_access: Some(MessageAccess {
                policy: MessagePolicy::RegisteredServer,
                paired_user_ids: vec![44],
                whitelist: vec![45],
                blacklist: vec![46],
                channel_ids: vec![47],
            }),
            voice_channel_ids: vec![5],
            codex_path: "old-codex".into(),
            worker_path: None,
            codex_home: Some(native_home.clone()),
            project_dir: "/old/project".into(),
            state_dir: "/old/state".into(),
            model: Some("old-model".into()),
            effort: Some("ultra".into()),
            service_tier: Some("off".into()),
            memory_enabled: Some(true),
            permissions: Some(":danger-full-access".into()),
            voice: Some("alpha".into()),
            input_mode: InputMode::Steering,
        };
        let list = InstallationList {
            format_version: 1,
            language: Language::default(),
            profile: "alpha".into(),
            native_source: NativeSource::ExistingCli,
            native_path: Some("/selected/codex".into()),
            codex_home: None,
            antigravity: None,
            discord_app: "existing-app".into(),
            guild_id: 11,
            home_channel_id: 12,
            transcript_channel_id: 13,
            operator_ids: vec![14],
            message_access: None,
            message_access_plan_pending: false,
            voice_channel_ids: vec![15],
            project_dir: "/selected/project".into(),
            model: Some("gpt-5.6-luna".into()),
            requested_permission_profile: "Full Access".into(),
            created_at: "now".into(),
        };

        let merged = merge_install_config(
            Some(previous.clone()),
            &list,
            Path::new("/new/config/discord-token"),
            Path::new("/selected/codex"),
            Some(Path::new("/owned/bin/bridge")),
            Path::new("/new/data/state"),
            &native_home,
        )
        .unwrap();

        assert_eq!(merged.profile, "alpha");
        assert_eq!(merged.name, "Alpha identity");
        assert_eq!(merged.voice.as_deref(), Some("alpha"));
        assert_eq!(merged.codex_home.as_deref(), Some(native_home.as_path()));
        assert_eq!(merged.input_mode, InputMode::Steering);
        assert_eq!(merged.permissions.as_deref(), Some(":danger-full-access"));
        assert_eq!(merged.codex_path, Path::new("/selected/codex"));
        assert_eq!(merged.project_dir, Path::new("/selected/project"));
        assert_eq!(merged.model.as_deref(), Some("old-model"));
        assert_eq!(merged.effort.as_deref(), Some("ultra"));
        assert_eq!(merged.service_tier.as_deref(), Some("off"));
        assert_eq!(merged.memory_enabled, Some(true));
        assert_eq!(merged.guild_id, 11);
        assert_eq!(
            merged.credentials_file.as_deref(),
            Some(Path::new("/new/config/discord-token"))
        );
        assert_eq!(merged.state_dir, Path::new("/new/data/state"));
        assert_eq!(
            merged.message_access.as_ref().unwrap().policy,
            MessagePolicy::RegisteredServer
        );
        assert_eq!(
            merged.message_access.as_ref().unwrap().paired_user_ids,
            vec![44]
        );
        assert_eq!(merged.message_access.as_ref().unwrap().whitelist, vec![45]);
        assert_eq!(merged.message_access.as_ref().unwrap().blacklist, vec![46]);
        assert_eq!(
            merged.message_access.as_ref().unwrap().channel_ids,
            vec![47]
        );

        // A newly generated installation plan is an explicit policy choice and
        // overrides the prior config value.
        let mut updated_list = list.clone();
        updated_list.message_access_plan_pending = true;
        updated_list.message_access = Some(MessageAccess {
            policy: MessagePolicy::Everyone,
            paired_user_ids: vec![60],
            whitelist: vec![61],
            blacklist: vec![62],
            channel_ids: vec![63],
        });
        let planned = merge_install_config(
            Some(previous),
            &updated_list,
            Path::new("/new/config/discord-token"),
            Path::new("/selected/codex"),
            None,
            Path::new("/new/data/state"),
            &native_home,
        )
        .unwrap();
        assert_eq!(planned.message_access, updated_list.message_access);

        // Older saved installation lists remain readable and preserve the
        // previous config policy when they have no new plan field.
        let mut legacy_list = serde_json::to_value(&list).unwrap();
        legacy_list.as_object_mut().unwrap().remove("messageAccess");
        legacy_list
            .as_object_mut()
            .unwrap()
            .remove("messageAccessPlanPending");
        let legacy_list: InstallationList = serde_json::from_value(legacy_list).unwrap();
        assert!(legacy_list.message_access.is_none());
        assert!(!legacy_list.message_access_plan_pending);

        let config_dir = tempfile::tempdir().expect("temporary config directory");
        let config_path = config_dir.path().join("config.json");
        write_json_private(&config_path, &merged).expect("write merged config");
        let reloaded = Config::load(&config_path).expect("reload merged config");
        assert_eq!(reloaded.codex_home, merged.codex_home);
        assert_eq!(reloaded.effort, merged.effort);
        assert_eq!(reloaded.service_tier, merged.service_tier);
        assert_eq!(reloaded.memory_enabled, merged.memory_enabled);

        let catalog = serde_json::json!({
            "data": [
                {"id": "limited", "name": "Standard", "allowed": true},
                {"id": "custom-full", "name": "Full Access", "allowed": true},
                {"id": ":danger-full-access", "allowed": true}
            ]
        });
        let entries = catalog["data"].as_array().unwrap();
        assert_eq!(
            default_permission_profile_id(entries).as_deref(),
            Some(":danger-full-access")
        );
        assert_eq!(
            preferred_permission_profile_id(entries, Some("limited")).as_deref(),
            Some("limited")
        );
        assert_eq!(
            preferred_permission_profile_id(entries, Some("no-longer-allowed")),
            None
        );
        let without_full_access = serde_json::json!({
            "data": [{"id": "limited", "name": "Standard", "allowed": true}]
        });
        assert_eq!(
            default_permission_profile_id(without_full_access["data"].as_array().unwrap())
                .as_deref(),
            Some("limited")
        );
    }

    #[test]
    fn fresh_codex_home_is_absolute_and_prefers_selected_environment() {
        let project = tempfile::tempdir().expect("temporary project directory");
        let default_home = project.path().join("default-home");
        let selected = resolve_codex_home(
            None,
            Some(Path::new("env-home")),
            project.path(),
            Some(&default_home),
        )
        .expect("resolve selected CODEX_HOME");
        assert_eq!(selected, project.path().join("env-home"));

        let explicit = project.path().join("explicit-home");
        assert_eq!(
            resolve_codex_home(None, Some(&explicit), project.path(), Some(&default_home)).unwrap(),
            explicit
        );
        assert_eq!(
            resolve_codex_home(None, None, project.path(), Some(&default_home)).unwrap(),
            default_home
        );
        assert_eq!(
            resolve_codex_home(
                Some(Path::new("legacy-home")),
                Some(Path::new("ignored-env-home")),
                project.path(),
                Some(&default_home),
            )
            .unwrap(),
            project.path().join("legacy-home")
        );
        let shared = project.path().join("discovered-shared-home");
        let separate = project.path().join("profile/separate-home");
        assert_eq!(
            resolve_codex_home(None, Some(&separate), project.path(), Some(&shared)).unwrap(),
            separate
        );
        for saved in [&shared, &separate] {
            assert_eq!(
                resolve_codex_home(
                    Some(saved),
                    Some(&explicit),
                    project.path(),
                    Some(&default_home)
                )
                .unwrap(),
                *saved
            );
        }
        assert!(resolve_codex_home(None, None, project.path(), None).is_err());
    }

    #[test]
    fn reinstall_rejects_existing_different_profile_identity() {
        assert!(
            validate_existing_profile_identity(true, "alpha", Some("beta"), Some("beta")).is_err()
        );
        assert!(validate_existing_profile_identity(true, "alpha", None, Some("beta")).is_err());
        assert!(
            validate_existing_profile_identity(true, "alpha", Some("alpha"), Some("alpha")).is_ok()
        );
        assert!(validate_existing_profile_identity(true, "alpha", None, None).is_err());
    }

    #[test]
    fn legacy_partial_core_cannot_adopt_existing_unowned_executable() {
        let previous: Ownership = serde_json::from_value(serde_json::json!({
            "formatVersion": 1,
            "configFile": "/profile/config.json",
            "credentialsFile": "/profile/discord-token",
            "installListFile": "/profile/installation-list.json",
            "runtimeDir": "/profile/official-codex",
            "runtimeExecutable": "/profile/official-codex/codex",
            "workerExecutable": null,
            "serviceFile": null,
            "serviceKind": null,
            "codexSource": "official-core-owned",
            "codexPath": "/profile/official-codex/codex",
            "codexVersion": null,
            "stateDir": "/profile/state",
            "projectDir": "/profile/project"
        }))
        .unwrap();
        assert!(!previous_owns_completed_core(
            &previous,
            Path::new("/profile/official-codex/codex")
        ));
    }

    #[test]
    fn legacy_completed_core_with_registered_service_remains_reusable() {
        let previous: Ownership = serde_json::from_value(serde_json::json!({
            "formatVersion": 1,
            "configFile": "/profile/config.json",
            "credentialsFile": "/profile/discord-token",
            "installListFile": "/profile/installation-list.json",
            "runtimeDir": "/profile/official-codex",
            "runtimeExecutable": "/profile/official-codex/codex",
            "workerExecutable": null,
            "serviceFile": "/profile/LaunchAgent.plist",
            "serviceKind": "launchd-user",
            "codexSource": "official-core-owned",
            "codexPath": "/profile/official-codex/codex",
            "codexVersion": null,
            "stateDir": "/profile/state",
            "projectDir": "/profile/project"
        }))
        .unwrap();
        assert!(previous_owns_completed_core(
            &previous,
            Path::new("/profile/official-codex/codex")
        ));
    }

    #[test]
    fn reinstall_requires_all_common_installation_paths_to_match() {
        let previous: Ownership = serde_json::from_value(serde_json::json!({
            "formatVersion": 1,
            "configFile": "/profile/config.json",
            "credentialsFile": "/profile/discord-token",
            "installListFile": "/profile/installation-list.json",
            "runtimeDir": "/profile/official-codex",
            "runtimeExecutable": null,
            "workerExecutable": null,
            "serviceFile": null,
            "serviceKind": null,
            "codexSource": "official-core-owned",
            "codexPath": "/profile/official-codex/codex",
            "codexVersion": null,
            "stateDir": "/profile/state",
            "projectDir": "/profile/project"
        }))
        .unwrap();
        assert!(same_profile_installation_paths(
            &previous,
            Path::new("/profile/config.json"),
            Path::new("/profile/discord-token"),
            Path::new("/profile/installation-list.json"),
            Path::new("/profile/state"),
            Path::new("/profile/project"),
        ));
        assert!(!same_profile_installation_paths(
            &previous,
            Path::new("/profile/config.json"),
            Path::new("/profile/discord-token"),
            Path::new("/profile/installation-list.json"),
            Path::new("/profile/state"),
            Path::new("/other/project"),
        ));
    }

    #[test]
    fn legacy_ownership_without_profile_uses_readable_config_identity() {
        let legacy: Ownership = serde_json::from_value(serde_json::json!({
            "formatVersion": 1,
            "configFile": "/profile/config.json",
            "credentialsFile": "/profile/discord-token",
            "installListFile": "/profile/installation-list.json",
            "runtimeDir": "/profile/runtime",
            "runtimeExecutable": null,
            "workerExecutable": null,
            "serviceFile": null,
            "serviceKind": null,
            "codexSource": "existing-cli",
            "codexPath": "/usr/local/bin/codex",
            "codexVersion": null,
            "stateDir": "/profile/state",
            "projectDir": "/profile/project"
        }))
        .unwrap();
        assert!(legacy.profile.is_none());
        assert!(!legacy.runtime_owned);
        assert!(validate_existing_profile_identity(true, "alpha", None, Some("alpha")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_partial_runtime_is_preserved_and_rejected() {
        use std::os::unix::fs::symlink;

        let data_root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let sentinel = external.path().join("foreign-sentinel");
        fs::write(&sentinel, b"preserve").unwrap();
        let runtime_dir = data_root.path().join("official-codex");
        symlink(external.path(), &runtime_dir).unwrap();
        let previous: Ownership = serde_json::from_value(serde_json::json!({
            "formatVersion": 1,
            "profile": "alpha",
            "runtimeOwned": true,
            "configFile": "/profile/config.json",
            "credentialsFile": "/profile/discord-token",
            "installListFile": "/profile/installation-list.json",
            "runtimeDir": runtime_dir,
            "runtimeExecutable": null,
            "workerExecutable": null,
            "serviceFile": null,
            "serviceKind": null,
            "codexSource": "official-core-owned",
            "codexPath": "/profile/official-codex/codex",
            "codexVersion": null,
            "stateDir": "/profile/state",
            "projectDir": "/profile/project"
        }))
        .unwrap();
        assert!(owned_core_runtime_for_cleanup(&previous, data_root.path(), None, &[]).is_err());
    }

    #[test]
    fn install_plan_rejects_different_existing_profile_before_plan_write() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        fs::write(
            &config_path,
            serde_json::to_vec(&serde_json::json!({
                "profile": "beta",
                "name": "Beta",
                "tokenEnv": "TOKEN",
                "guildId": 1,
                "homeChannelId": 2,
                "transcriptChannelId": 2,
                "codexPath": "/codex",
                "projectDir": "/project",
                "stateDir": "/state"
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(
            validate_existing_installation_plan(
                &dir.path().join("ownership.json"),
                &config_path,
                "alpha"
            )
            .is_err()
        );
    }
}

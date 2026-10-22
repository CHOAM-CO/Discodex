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
use crate::i18n::Language;
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
) -> Config {
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
            configured_language(config.language),
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
            configured_language(list.language),
            list.model.clone(),
            list.message_access.clone(),
        ),
    };
    Config {
        language: env_language().unwrap_or(language),
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
    }
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
fn windows_task_executable_path(path: &Path) -> Result<PathBuf> {
    let mut value = path
        .to_str()
        .context("Windows task executable path is not valid UTF-8")?;
    if let Some(unprefixed) = value.strip_prefix(r"\\?\") {
        value = unprefixed;
    }
    let bytes = value.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        bail!(
            "Windows task executable must be a local drive path; UNC and device paths are not supported"
        )
    }
    Ok(PathBuf::from(value))
}

fn prepare_project_directory(path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(path)
        .with_context(|| format!("create Codex project/work directory {}", path.display()))?;
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolve Codex project/work directory {}", path.display()))?;
    ensure!(
        canonical.is_dir(),
        "Codex project/work path is not a directory"
    );
    Ok(canonical)
}

pub fn print_plan() -> Result<()> {
    let language = choose_language(None)?;
    print_plan_for_language(language)
}

fn print_plan_for_language(language: Language) -> Result<()> {
    let roots = roots()?;
    fs::create_dir_all(&roots.config)
        .with_context(|| format!("create {}", roots.config.display()))?;
    set_private_dir(&roots.config)?;

    let profile = ask_text(
        language,
        "Discord profile name",
        "Discord 프로필 이름",
        "Discord 配置名称",
        "Discord プロファイル名",
        "default",
    )?;
    let native_source = NativeSource::ManagedCore;
    let native_path = None;
    let codex_home = Some(choose_codex_home(&roots, language)?);
    let antigravity = Some(choose_antigravity_install(language)?);
    let discord_app = ask_text(
        language,
        "Discord application profile (existing app)",
        "Discord 애플리케이션 프로필 (기존 앱)",
        "Discord 应用配置（现有应用）",
        "Discord アプリケーションプロファイル（既存アプリ）",
        &profile,
    )?;
    let guild_id = ask_u64(
        language,
        "Home guild/server ID",
        "홈 서버 ID",
        "主页服务器 ID",
        "ホームサーバー ID",
    )?;
    let home_channel_id = ask_u64(
        language,
        "Home text channel ID",
        "홈 텍스트 채널 ID",
        "主页文本频道 ID",
        "ホームテキストチャンネル ID",
    )?;
    let transcript_channel_id = ask_u64(
        language,
        "Voice transcript channel ID",
        "음성 전사 채널 ID",
        "语音转录频道 ID",
        "音声文字起こしチャンネル ID",
    )?;
    let voice_channel_ids = ask_ids(
        language,
        "Allowed voice channel IDs (comma-separated)",
        "허용할 음성 채널 ID (쉼표로 구분)",
        "允许的语音频道 ID（逗号分隔）",
        "許可する音声チャンネル ID（カンマ区切り）",
    )?;
    let operator_ids = ask_ids(
        language,
        "Operator user IDs (comma-separated)",
        "운영자 사용자 ID (쉼표로 구분)",
        "操作员用户 ID（逗号分隔）",
        "オペレーターのユーザー ID（カンマ区切り）",
    )?;
    let message_access = ask_install_message_access(
        language,
        &operator_ids,
        &[home_channel_id, transcript_channel_id],
    )?;
    let project_default = env::current_dir().context("read current directory")?;
    let project_dir = prepare_project_directory(&PathBuf::from(ask_text(
        language,
        "Codex project/work directory",
        "Codex 프로젝트/작업 디렉터리",
        "Codex 项目/工作目录",
        "Codex プロジェクト/作業ディレクトリ",
        &project_default.display().to_string(),
    )?))?;
    let model = optional_value(
        language,
        "Codex model (leave empty for native default)",
        "Codex 모델 (비우면 기본값)",
        "Codex 模型（留空使用默认值）",
        "Codex モデル（空欄なら標準）",
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
        recorded.canonicalize().context("resolve recorded config")?
            == selected.canonicalize().context("resolve selected config")?,
        "selected config is not the file recorded by ownership"
    );
    Ok(recorded.to_owned())
}

pub fn migrate_installation(config_path: &Path) -> Result<()> {
    let source_config = config_path
        .canonicalize()
        .with_context(|| format!("resolve selected legacy config {}", config_path.display()))?;
    let legacy_config_root = source_config
        .parent()
        .context("legacy config path has no parent")?
        .to_owned();
    ensure!(
        legacy_config_root
            .file_name()
            .and_then(|name| name.to_str())
            == Some(LEGACY_APP_NAME),
        "migration requires an explicit config inside a {LEGACY_APP_NAME} profile directory"
    );
    let legacy_owner_path = legacy_config_root.join("ownership.json");
    let legacy: Ownership = read_json(&legacy_owner_path)
        .with_context(|| format!("read legacy ownership {}", legacy_owner_path.display()))?;
    // Windows canonical paths add a verbatim prefix. Prove file identity, then
    // retain the ownership record's spelling for sibling paths and task arguments.
    #[cfg(windows)]
    let source_config = checked_recorded_config_path(&legacy.config_file, &source_config)?;
    #[cfg(windows)]
    let legacy_config_root = source_config
        .parent()
        .context("recorded legacy config path has no parent")?
        .to_owned();
    let old_data_root = legacy
        .state_dir
        .parent()
        .context("legacy state directory has no data root")?
        .to_owned();
    ensure!(
        legacy.state_dir == old_data_root.join("state")
            && old_data_root.file_name().and_then(|name| name.to_str()) == Some(LEGACY_APP_NAME),
        "legacy ownership does not identify the expected codex-worker state layout"
    );

    let new_config = rename_legacy_path(&source_config);
    let new_data = rename_legacy_path(&old_data_root);
    let roots = Roots {
        config: new_config
            .parent()
            .context("Discodex config path has no parent")?
            .to_owned(),
        data: new_data,
    };
    validate_legacy_migration_source(&legacy, &legacy_config_root, &old_data_root, &source_config)?;
    ensure!(
        !roots.config.exists() && !roots.data.exists(),
        "Discodex profile paths already exist; migration will not overwrite them"
    );

    let service_backup = validate_legacy_service(&legacy)?;
    let migration_language = choose_language(Some(Config::load(&source_config)?.language))?;
    #[cfg(target_os = "macos")]
    if let Some(requirement) = legacy.mac_signing_requirement.as_deref() {
        let executable = legacy
            .worker_executable
            .as_deref()
            .context("legacy ownership has no signed worker executable")?
            .canonicalize()
            .context("resolve legacy signed worker executable")?;
        crate::mac_signing::adopt_owned_binary_identifier(&executable, requirement)
            .context("preserve the verified legacy macOS signing identity")?;
    }
    stage_legacy_profile(&legacy_config_root, &roots)?;
    let staged_worker = match stage_worker_file(&env::current_exe()?, &roots.data) {
        Ok(path) => path,
        Err(error) => {
            remove_migration_staging(&roots, &legacy_config_root)
                .context("worker staging failed and staged migration files could not be removed")?;
            return Err(error)
                .context("could not stage the Discodex worker before stopping the legacy service");
        }
    };
    #[cfg(target_os = "macos")]
    if let Err(error) = sign_staged_migration_worker(
        &staged_worker,
        migration_language,
        legacy.mac_signing_requirement.as_deref(),
    ) {
        remove_migration_staging(&roots, &legacy_config_root)
            .context("worker signing failed and staged migration files could not be removed")?;
        return Err(error).context(
            "could not prepare the signed Discodex worker before stopping the legacy service",
        );
    }
    let _ = staged_worker;
    let mut stopped_ownership = legacy.clone();
    if let Err(error) = uninstall_user_service(&mut stopped_ownership, &legacy_owner_path) {
        let staged_cleanup = remove_migration_staging(&roots, &legacy_config_root);
        let service_restore = restore_legacy_service(&service_backup, &legacy_owner_path);
        if let Err(restore_error) = service_restore {
            return Err(anyhow!(
                "could not stop the verified legacy service ({error:#}) and its restoration failed ({restore_error:#})"
            ));
        }
        if let Err(cleanup_error) = staged_cleanup {
            return Err(anyhow!(
                "could not stop the verified legacy service ({error:#}); the old service was restored but staged Discodex files remain ({cleanup_error:#})"
            ));
        }
        return Err(error)
            .context("could not stop the verified legacy service before Discodex startup");
    }
    if let Err(error) =
        copy_private_directory(&old_data_root.join("state"), &roots.data.join("state"))
    {
        let staged_cleanup = remove_migration_staging(&roots, &legacy_config_root);
        let service_restore = restore_legacy_service(&service_backup, &legacy_owner_path);
        if let Err(restore_error) = service_restore {
            return Err(anyhow!(
                "could not copy final stopped legacy state ({error:#}) and the old service restoration failed ({restore_error:#})"
            ));
        }
        if let Err(cleanup_error) = staged_cleanup {
            return Err(anyhow!(
                "could not copy final stopped legacy state ({error:#}); old service restored but staged files remain ({cleanup_error:#})"
            ));
        }
        return Err(error).context("could not copy the legacy state after stopping its service");
    }
    match install_at(
        roots.clone(),
        migration_language,
        legacy.worker_executable.as_deref(),
    ) {
        Ok(()) => {
            if let Err(error) = remove_migration_marker(&roots) {
                eprintln!(
                    "Discodex is installed, but its migration marker could not be removed: {error:#}"
                );
            }
            println!(
                "Migrated profile {} to Discodex. Legacy config and native data remain at {}.",
                legacy.profile.as_deref().unwrap_or("unknown"),
                legacy_config_root.display()
            );
            Ok(())
        }
        Err(error) => {
            let cleanup = rollback_new_migration_service(&roots);
            if let Err(cleanup_error) = cleanup {
                return Err(anyhow!(
                    "Discodex migration failed ({error:#}); its new service could not be verified and removed ({cleanup_error:#}), so the legacy service was left stopped to avoid two workers using the same native thread"
                ));
            }
            let files_cleanup = remove_migration_staging(&roots, &legacy_config_root);
            restore_legacy_service(&service_backup, &legacy_owner_path).context(
                "Discodex migration failed and the previous service could not be restored",
            )?;
            if let Err(cleanup_error) = files_cleanup {
                Err(anyhow!(
                    "Discodex migration failed ({error:#}); the previous service was restored, but staged files remain ({cleanup_error:#})"
                ))
            } else {
                Err(error).context("Discodex migration failed; the previous service was restored")
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
fn systemctl_user_state(operation: &str, unit: &str) -> Result<String> {
    let output = command_without_console("systemctl")
        .args(["--user", operation, unit])
        .stdin(Stdio::null())
        .output()
        .context("inspect the legacy user service state")?;
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
        "unexpected systemctl {operation} state for {unit}: {state}"
    );
    Ok(state)
}

#[cfg(windows)]
fn inspect_windows_task_state(task_name: &str) -> Result<(bool, bool)> {
    let task_name = ps_single_quote(task_name);
    let script = format!(
        r#"$ErrorActionPreference='Stop'; $task=Get-ScheduledTask -TaskPath '\' -TaskName '{task_name}'; Write-Output ($task.Settings.Enabled.ToString() + '|' + $task.State.ToString())"#
    );
    let output = command_without_console("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .output()
        .context("inspect Windows task state")?;
    ensure!(output.status.success(), "could not read Windows task state");
    let value = String::from_utf8(output.stdout)?.trim().to_owned();
    let (enabled, state) = value
        .split_once('|')
        .context("Windows task returned an invalid state")?;
    let enabled = enabled.eq_ignore_ascii_case("true");
    ensure!(
        state == "Running" || state == "Ready" || state == "Disabled",
        "Windows task is in a transitional state; operation refused"
    );
    ensure!(
        (enabled && state != "Disabled") || (!enabled && state == "Disabled"),
        "Windows task enabled flag and state disagree"
    );
    Ok((enabled, state == "Running"))
}

fn validate_legacy_service(ownership: &Ownership) -> Result<LegacyServiceBackup> {
    let executable = ownership
        .worker_executable
        .as_deref()
        .context("legacy ownership has no worker executable")?
        .canonicalize()
        .context("resolve legacy owned worker executable")?;
    let config = ownership
        .config_file
        .canonicalize()
        .context("resolve legacy owned config")?;
    #[cfg(target_os = "linux")]
    {
        let path = ownership
            .service_file
            .as_deref()
            .context("legacy systemd ownership has no unit path")?;
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
            "legacy ownership does not identify the expected systemd user service"
        );
        let expected_unit = format!(
            "[Unit]\nDescription=Codex Worker Discord bridge\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={} run --config {}\nRestart=on-failure\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n",
            systemd_quote(&executable),
            systemd_quote(&config)
        );
        let actual = fs::read_to_string(path).context("read legacy systemd unit")?;
        ensure!(
            actual == expected_unit,
            "legacy systemd unit differs from its recorded executable and config"
        );
        let was_enabled = systemctl_user_state("is-enabled", &expected_name)? == "enabled";
        let was_active = systemctl_user_state("is-active", &expected_name)? == "active";
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
        let path = ownership
            .service_file
            .as_deref()
            .context("legacy LaunchAgent ownership has no plist path")?;
        ensure!(
            ownership.service_kind.as_deref() == Some("launchd-user") && path == expected_path,
            "legacy ownership does not identify the expected LaunchAgent"
        );
        let expected = launchd_plist(&label, &executable, &config);
        ensure!(
            existing_launchd_plist_matches(path, &expected, true)?,
            "legacy LaunchAgent plist is missing"
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
            "legacy ownership does not identify the expected Windows task"
        );
        let task_executable = windows_task_executable_path(&executable)?;
        let expected_executable = ps_single_quote(&task_executable.display().to_string());
        let expected_arguments = ps_single_quote(&format!(
            "run --config \"{}\"",
            ownership.config_file.display()
        ));
        let (state, _) =
            inspect_owned_windows_task(&expected_executable, &expected_arguments, ownership)?;
        ensure!(
            state == "matching",
            "legacy Windows task differs from its ownership record"
        );
        let (was_enabled, was_active) =
            inspect_windows_task_state(windows_task_name_for_config(&ownership.config_file))?;
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
        bail!("legacy service migration is supported on Linux, macOS, and Windows")
    }
}

fn restore_legacy_service(backup: &LegacyServiceBackup, owner_file: &Path) -> Result<()> {
    let mut ownership = backup.ownership.clone();
    #[cfg(target_os = "linux")]
    {
        let path = ownership
            .service_file
            .as_deref()
            .context("legacy systemd unit path missing")?;
        fs::write(
            path,
            backup
                .service_contents
                .as_deref()
                .context("legacy systemd unit snapshot missing")?,
        )?;
        set_private_file(path)?;
        write_json_private(owner_file, &ownership)?;
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .context("legacy systemd unit filename invalid")?;
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
        let path = ownership
            .service_file
            .as_deref()
            .context("legacy LaunchAgent plist path missing")?;
        let contents = backup
            .service_contents
            .as_deref()
            .context("legacy LaunchAgent snapshot missing")?;
        let config = ownership.config_file.canonicalize()?;
        let (label, _) = launchd_identity_for_service(&config, LEGACY_SERVICE_NAME)?;
        if path.exists() {
            ensure!(
                existing_launchd_plist_matches(
                    path,
                    &launchd_plist(
                        &label,
                        ownership
                            .worker_executable
                            .as_deref()
                            .context("legacy LaunchAgent executable missing")?,
                        &config
                    ),
                    true
                )?,
                "existing legacy LaunchAgent differs from its ownership record"
            );
        } else {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .with_context(|| format!("restore legacy LaunchAgent {}", path.display()))?;
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
                    .context("legacy LaunchAgent executable missing")?,
                &config,
            )?
        {
            run_checked(
                "launchctl",
                &[
                    "bootstrap",
                    &domain,
                    path.to_str()
                        .context("legacy LaunchAgent path is not UTF-8")?,
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
            .context("legacy task executable missing")?;
        let task_executable = windows_task_executable_path(executable)?;
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
) -> Result<()> {
    ensure!(
        ownership.config_file == config_path
            && ownership.credentials_file == config_root.join("discord-token")
            && ownership.install_list_file == config_root.join("installation-list.json")
            && ownership.state_dir == data_root.join("state")
            && ownership.project_dir.is_absolute(),
        "legacy ownership paths do not match the selected profile; refusing migration"
    );
    let config = Config::load(config_path).context("read legacy profile settings")?;
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
        "legacy config does not match its ownership record; refusing migration"
    );
    let list: InstallationList =
        read_json(&ownership.install_list_file).context("read legacy installation list")?;
    ensure!(
        list.profile == config.profile && list.project_dir == ownership.project_dir,
        "legacy installation list does not match its ownership record; refusing migration"
    );
    let credentials_metadata = fs::symlink_metadata(&ownership.credentials_file)
        .context("inspect legacy Discord credential file")?;
    ensure!(
        credentials_metadata.file_type().is_file(),
        "legacy Discord credentials are not a regular file; refusing migration"
    );
    let executable = ownership
        .worker_executable
        .as_deref()
        .context("legacy ownership has no worker executable")?
        .canonicalize()
        .context("resolve legacy worker executable")?;
    ensure!(
        executable.is_file() && is_owned_worker_executable(&executable, data_root),
        "legacy worker executable is outside its recorded owned bin directory"
    );
    ensure!(
        ownership.runtime_dir
            == data_root.join(if ownership.codex_source == "official-core-owned" {
                "official-codex"
            } else {
                "codex-runtime"
            }),
        "legacy runtime directory does not match its owned data root"
    );
    Ok(())
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
    let source = source
        .canonicalize()
        .context("resolve running Discodex executable")?;
    ensure!(
        fs::symlink_metadata(&source)?.file_type().is_file(),
        "running Discodex executable is not a regular file"
    );
    let bin = data_root.join("bin");
    create_private_dir(&bin)?;
    let destination = bin.join(worker_executable_name());
    copy_private_regular_file(&source, &destination)?;
    set_executable(&destination)?;
    destination
        .canonicalize()
        .context("resolve staged Discodex executable")
}

#[cfg(target_os = "macos")]
fn sign_staged_migration_worker(
    path: &Path,
    language: Language,
    expected: Option<&str>,
) -> Result<()> {
    use crate::mac_signing::{sign_binary_with_language, verify_binary};
    let requirement = sign_binary_with_language(path, language)
        .context("sign staged Discodex worker with the preserved macOS identity")?;
    if let Some(expected) = expected {
        ensure!(
            requirement == expected,
            "staged Discodex signing identity differs from the legacy pin"
        );
    }
    verify_binary(path, &requirement).context("verify staged Discodex worker signature")
}

fn recorded_service_executable(ownership: &Ownership) -> Result<Option<PathBuf>> {
    let Some(recorded) = ownership.worker_executable.as_deref() else {
        return Ok(None);
    };
    let data_root = ownership
        .state_dir
        .parent()
        .context("owned state directory has no data root")?;
    ensure!(
        ownership.state_dir == data_root.join("state"),
        "owned state directory does not match the worker data layout"
    );
    let executable = recorded
        .canonicalize()
        .with_context(|| format!("resolve recorded worker executable {}", recorded.display()))?;
    ensure!(
        recorded == executable
            && executable.is_file()
            && is_owned_worker_executable(&executable, data_root),
        "recorded worker executable is not a regular file in its canonical owned bin directory"
    );
    Ok(Some(executable))
}

fn staged_migration_worker(roots: &Roots) -> Result<Option<PathBuf>> {
    let marker = roots.config.join("migration-in-progress");
    let metadata = match fs::symlink_metadata(&marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect migration marker"),
    };
    ensure!(
        metadata.file_type().is_file(),
        "migration marker is not a regular file"
    );
    let old_config_root: PathBuf =
        serde_json::from_slice(&fs::read(&marker).context("read migration marker")?)
            .context("parse migration marker")?;
    ensure!(
        old_config_root.file_name().and_then(|name| name.to_str()) == Some(LEGACY_APP_NAME)
            && rename_legacy_path(&old_config_root) == roots.config,
        "migration marker does not map to the selected Discodex profile"
    );
    let candidate = roots.data.join("bin").join(worker_executable_name());
    if !candidate.is_file() {
        return Ok(None);
    }
    let candidate = candidate
        .canonicalize()
        .context("resolve staged migration worker")?;
    let data_root = roots
        .data
        .canonicalize()
        .context("resolve migration data root")?;
    ensure!(
        candidate.starts_with(data_root.join("bin"))
            && is_owned_worker_executable(&candidate, &data_root),
        "staged migration worker is outside the new profile's owned bin directory"
    );
    Ok(Some(candidate))
}

fn stage_legacy_profile(old_config_root: &Path, roots: &Roots) -> Result<()> {
    let config_parent = roots
        .config
        .parent()
        .context("Discodex config path has no parent")?;
    let data_parent = roots
        .data
        .parent()
        .context("Discodex data path has no parent")?;
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
    fs::rename(&staged_config, &roots.config)
        .with_context(|| format!("publish staged config at {}", roots.config.display()))?;
    if let Err(error) = fs::rename(&staged_data, &roots.data) {
        let cleanup = fs::remove_dir_all(&roots.config);
        return match cleanup {
            Ok(()) => Err(error)
                .with_context(|| format!("publish staged data at {}", roots.data.display())),
            Err(cleanup_error) => Err(anyhow!(
                "could not publish staged data at {} ({error}); cleanup of newly-created config path {} also failed ({cleanup_error})",
                roots.data.display(),
                roots.config.display()
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
            "write migration marker failed ({error}); staged root cleanup results: config={config_cleanup:?}, data={data_cleanup:?}"
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
                "migration marker is not a regular file"
            );
            fs::remove_file(&marker).context("remove completed migration marker")
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("inspect migration marker"),
    }
}

fn remove_migration_staging(roots: &Roots, old_config_root: &Path) -> Result<()> {
    let marker = roots.config.join("migration-in-progress");
    let metadata =
        fs::symlink_metadata(&marker).context("verify ownership of staged migration roots")?;
    ensure!(
        metadata.file_type().is_file(),
        "migration marker is not a regular file"
    );
    ensure!(
        fs::read(&marker)? == serde_json::to_vec(old_config_root)?,
        "migration marker does not match the selected legacy profile"
    );
    for path in [&roots.config, &roots.data] {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("inspect staged migration root {}", path.display()))?;
        ensure!(
            metadata.file_type().is_dir(),
            "staged migration root {} is not a regular directory",
            path.display()
        );
    }
    fs::remove_dir_all(&roots.data)
        .with_context(|| format!("remove staged data {}", roots.data.display()))?;
    fs::remove_dir_all(&roots.config)
        .with_context(|| format!("remove staged config {}", roots.config.display()))?;
    Ok(())
}

fn copy_private_regular_file(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("inspect migration source {}", source.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "migration source {} is not a regular file",
        source.display()
    );
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("create staged migration file {}", destination.display()))?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    set_private_file(destination)?;
    Ok(())
}

fn copy_private_directory(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("inspect migration state directory {}", source.display()))?;
    ensure!(
        metadata.file_type().is_dir(),
        "migration state path {} is not a regular directory",
        source.display()
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

fn rollback_new_migration_service(roots: &Roots) -> Result<()> {
    let owner_path = roots.config.join("ownership.json");
    if !owner_path.is_file() {
        return Ok(());
    }
    let mut ownership: Ownership = read_json(&owner_path)?;
    uninstall_user_service(&mut ownership, &owner_path)
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
    fs::create_dir_all(&roots.config)
        .with_context(|| format!("create {}", roots.config.display()))?;
    fs::create_dir_all(&roots.data).with_context(|| format!("create {}", roots.data.display()))?;
    set_private_dir(&roots.config)?;
    set_private_dir(&roots.data)?;

    let list_path = roots.config.join("installation-list.json");
    if !list_path.exists() {
        println!(
            "{}",
            language.text(
                "No installation list is saved. Create it before setup starts.",
                "저장된 설치 목록이 없습니다. 설정을 시작하기 전에 목록을 만듭니다.",
                "未找到已保存的安装列表。请先创建列表再开始设置。",
                "保存済みのインストールリストがありません。設定を始める前に作成します。"
            )
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
            "ownership record belongs to another profile; existing files were preserved"
        );
        Some(previous)
    } else {
        None
    };
    let previous_config = if config_path.is_file() {
        Some(Config::load(&config_path).with_context(|| {
            format!(
                "read existing profile settings from {}; refusing to replace an unreadable config",
                config_path.display()
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
        bail!("CODEX_HOME must not be empty");
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
            .context("inspect saved Discord credential file")?;
        ensure!(
            metadata.file_type().is_file(),
            "saved Discord credentials are not a regular file; refusing to replace them"
        );
        Some(fs::read_to_string(&credentials_path).context("read saved Discord credential file")?)
    } else {
        None
    };
    let token = env::var("DISCODEX_DISCORD_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            env::var("CODEX_WORKER_DISCORD_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| saved_token.unwrap_or_default());
    let token = if token.trim().is_empty() {
        rpassword::prompt_password(language.text(
            "Discord bot token (input hidden): ",
            "Discord 봇 토큰 (입력 숨김): ",
            "Discord 机器人令牌（隐藏输入）：",
            "Discord ボットトークン（入力非表示）：",
        ))
        .context("read Discord token privately")?
    } else {
        token
    };
    if token.trim().is_empty() {
        bail!("Discord token was empty; no credential file was written");
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
    );
    write_json_private(&config_path, &config)?;
    if list.message_access_plan_pending {
        let access = list
            .message_access
            .as_ref()
            .or(config.message_access.as_ref())
            .context("new installation plan did not include a message receive policy")?;
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
        .context("Discord profile check failed; saved installation list and private config remain for correction")?;
    for id in [list.home_channel_id, list.transcript_channel_id] {
        let channel = discord
            .channels
            .iter()
            .find(|channel| channel.id == id)
            .ok_or_else(|| anyhow!("Discord validation omitted configured text channel {id}"))?;
        if channel.kind != "text" {
            bail!(
                "configured home/transcript channel {id} is {}, expected a text channel",
                channel.kind
            );
        }
    }
    for id in &list.voice_channel_ids {
        let channel = discord
            .channels
            .iter()
            .find(|channel| channel.id == *id)
            .ok_or_else(|| anyhow!("Discord validation omitted configured voice channel {id}"))?;
        if channel.kind != "voice" && channel.kind != "stage" {
            bail!(
                "configured voice channel {id} is {}, expected voice or stage",
                channel.kind
            );
        }
    }
    for channel in &discord.channels {
        for warning in &channel.warnings {
            eprintln!(
                "{}",
                format!(
                    "{}: {warning}",
                    language.text(
                        "Discord channel permission check",
                        "Discord 채널 권한 확인",
                        "Discord 频道权限检查",
                        "Discord チャンネル権限の確認"
                    )
                )
            );
        }
    }
    println!("{}", language.text(
        "Verified the bot's current role and channel overrides for read, write, voice, and transcript-thread permissions on Discord.",
        "Discord의 현재 봇 역할과 채널 덮어쓰기에서 읽기·쓰기·통화·기록 스레드 권한을 확인했습니다.",
        "已验证 Discord 中机器人当前角色和频道覆盖设置具备读取、写入、语音和记录线程权限。",
        "Discord の現在のボットロールとチャンネル上書き設定で、読み取り、書き込み、音声、記録スレッドの権限を確認しました。"
    ));
    println!(
        "Discord API authenticated bot {} and can read guild {} ({}).",
        discord.bot_id, discord.guild_id, discord.guild_name
    );
    run_async(crate::commands::register_commands(&config)).context(
        "Discord /discodex command registration failed; saved setup files remain for retry",
    )?;
    configure_antigravity_install(&config, list.antigravity)?;
    run_async(configure_native_profile(&mut config, &config_path)).context(
        "Codex account or Full Access profile check failed; saved setup files remain for retry",
    )?;
    let previous_run = crate::installation_check::read(&config)
        .ok()
        .flatten()
        .map(|report| report.run_id);
    install_user_service(&config_path, &mut ownership, previous_ownership.as_ref())?;
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
    println!("{}", language.text(
        "Codex provider authentication settings were checked; the permission profile ID was saved and the user service start was requested. Check /discodex status for active thread settings.",
        "Codex 공급자의 인증 설정을 확인했습니다. 권한 프로필 ID를 저장했고 사용자 서비스를 시작하도록 요청했습니다. 활성 대화 설정은 /discodex status에서 확인하세요.",
        "已检查 Codex 提供商的身份验证设置，保存权限配置 ID，并请求启动用户服务。请通过 /discodex status 查看当前对话设置。",
        "Codex プロバイダーの認証設定を確認しました。権限プロファイル ID を保存し、ユーザーサービスの起動を要求しました。現在のスレッド設定は /discodex status で確認してください。"
    ));
    confirm_installed_features(&config, previous_run)?;
    Ok(())
}

fn confirm_installed_features(config: &Config, previous_run: Option<String>) -> Result<()> {
    let language = configured_language(config.language);
    println!("{}", language.text(
        "Send a real text message in Discord and try settings, question forms, and approval buttons. Speak in an allowed voice channel and verify voice work and its finalized transcript. Handle sign-in, MFA, OS consent, and microphone prompts in their official user interfaces.",
        "Discord에서 실제 텍스트를 보내고 설정 변경·질문 폼·승인 버튼을 사용하세요. 허용한 음성 채널에서 말한 뒤 음성 업무와 확정 전사를 확인하세요. 로그인·MFA·OS 동의·마이크는 공식 사용자 화면에서 처리합니다.",
        "请在 Discord 中发送真实文本，并尝试设置、问题表单和批准按钮。在获准的语音频道中讲话，验证语音任务和最终转录。请在官方用户界面中处理登录、MFA、操作系统授权和麦克风提示。",
        "Discord で実際のテキストを送り、設定、質問フォーム、承認ボタンを試してください。許可された音声チャンネルで話し、音声タスクと確定した文字起こしを確認してください。サインイン、MFA、OS の許可、マイクの確認は公式ユーザー画面で行ってください."
    ));
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
                        language.text(
                            "Could not read installation feature observations",
                            "설치 기능 관측을 읽지 못했습니다",
                            "无法读取安装功能观察结果",
                            "インストール機能の観測結果を読み取れませんでした"
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
                        language.text("failed", "실패", "失败", "失敗")
                    ),
                    crate::installation_check::Observation::Incomplete { reason } => println!(
                        "{name}: {} — {reason}",
                        language.text("incomplete", "미완료", "未完成", "未完了")
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
                println!("{}", language.text(
                    "READY: This install successfully exercised text, settings, approvals, two-way voice, work linked to the same conversation, and the finalized transcript.",
                    "준비 완료: 이 설치에서 텍스트·설정·승인·양방향 음성·같은 대화 업무 연결·확정 전사가 실제로 동작했습니다.",
                    "就绪：此安装已成功验证文本、设置、批准、双向语音、关联同一对话的任务以及最终转录。",
                    "準備完了：このインストールで、テキスト、設定、承認、双方向音声、同じ会話に紐づくタスク、確定した文字起こしが動作しました。"
                ));
                return Ok(());
            }
        } else {
            println!(
                "{}",
                language.text(
                    "No feature result from the current install has been observed yet.",
                    "현재 설치의 기능 결과가 아직 관측되지 않았습니다。",
                    "尚未观察到当前安装的功能结果。",
                    "現在のインストールでの機能結果はまだ確認されていません。"
                )
            );
        }
        if !io::stdin().is_terminal() {
            println!("{}", language.text(
                "Settings and conversation history were preserved. Feature verification is incomplete.",
                "설정과 대화 이력을 보존했습니다. 기능 확인은 미완료입니다.",
                "设置和对话历史已保留。功能验证尚未完成。",
                "設定と会話履歴は保持されています。機能確認は未完了です。"
            ));
            return Ok(());
        }
        print!(
            "{}",
            language.text(
                "Press Enter to read results again, or q to exit with verification incomplete > ",
                "Enter: 실제 결과 다시 읽기, q: 미완료 상태로 종료 > ",
                "按 Enter 重新读取结果，或按 q 在验证未完成时退出 > ",
                "Enter で結果を再読込、q で確認未完了のまま終了 > "
            )
        );
        io::stdout().flush()?;
        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 || input.trim().eq_ignore_ascii_case("q") {
            println!("{}", language.text(
                "Settings and conversation history were preserved. Feature verification is incomplete.",
                "설정과 대화 이력을 보존했습니다. 기능 확인은 미완료입니다.",
                "设置和对话历史已保留。功能验证尚未完成。",
                "設定と会話履歴は保持されています。機能確認は未完了です。"
            ));
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
                "ownership records bridge executable {} but it is not inside the canonical owned bin directory {}; preserving config and ownership for investigation",
                executable.display(),
                roots.data.join("bin").display()
            )
        }
    }

    let config = if matches!(
        ownership.codex_source.as_str(),
        "official-core-owned" | "discodex-core-owned"
    ) && ownership.config_file.exists()
    {
        Some(Config::load(&ownership.config_file).context(
            "read profile settings before uninstall; configuration and ownership were preserved",
        )?)
    } else {
        None
    };
    let mut ownership = ownership;
    let runtime_dir = prepare_owned_core_uninstall(&mut ownership, &roots.data, config.as_ref())?;
    uninstall_user_service(&mut ownership, &ownership_path)?;
    remove_registered_commands(&ownership.config_file, &ownership.credentials_file, &ownership.state_dir)
        .context("could not remove the owned Discord command; service is stopped and credentials/config/ownership were preserved for retry")?;

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
    let language = ownership
        .config_file
        .is_file()
        .then(|| {
            Config::load(&ownership.config_file)
                .ok()
                .map(|config| config.language)
        })
        .flatten()
        .unwrap_or_else(|| configured_language(Language::default()));
    open_mac_permissions_for_executable(&executable)?;
    println!("{}", language.text(
        "In Finder, add the selected executable to System Settings > Privacy & Security > Full Disk Access and enable it: {}",
        "Finder에서 선택한 실행 파일을 시스템 설정 > 개인정보 보호 및 보안 > 전체 디스크 접근 권한에 추가하고 켜세요: {}",
        "在 Finder 中将选定的可执行文件添加到“系统设置”>“隐私与安全性”>“完全磁盘访问权限”并启用：{}",
        "Finder で選択した実行ファイルを「システム設定」>「プライバシーとセキュリティ」>「フルディスクアクセス」に追加して有効にしてください：{}"
    ).replace("{}", &executable.display().to_string()));
    println!(
        "{}",
        language.text(
            "This command does not automatically check or change macOS permissions.",
            "이 명령은 macOS 권한 부여 여부를 자동으로 확인하거나 변경하지 않습니다.",
            "此命令不会自动检查或更改 macOS 权限。",
            "このコマンドは macOS の権限を自動で確認または変更しません。"
        )
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn owned_mac_worker(ownership_path: &Path) -> Result<(Ownership, PathBuf)> {
    let ownership: Ownership = read_json(ownership_path).with_context(|| {
        format!(
            "read owned installation record {}",
            ownership_path.display()
        )
    })?;
    let data_root = ownership_data_root(&ownership)?;
    let recorded_executable = ownership
        .worker_executable
        .as_ref()
        .context("the owned installation has no recorded worker executable")?;
    let executable = recorded_executable.canonicalize().with_context(|| {
        format!(
            "resolve the owned worker executable {}",
            recorded_executable.display()
        )
    })?;
    ensure!(
        recorded_executable == &executable,
        "recorded worker path is not canonical: {}",
        recorded_executable.display()
    );
    if !executable.is_file() || !is_owned_worker_executable(&executable, &data_root) {
        bail!(
            "the recorded worker executable is not a regular file in its canonical owned bin directory: {}",
            executable.display()
        );
    }
    Ok((ownership, executable))
}

#[cfg(target_os = "macos")]
fn open_mac_permissions_for_executable(executable: &Path) -> Result<()> {
    let executable_text = executable
        .to_str()
        .context("the owned worker executable path is not valid UTF-8")?;
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
        "Full Disk Access setup must be reviewed in an interactive terminal before {operation}; owned worker: {}. Rerun the operation from a terminal.",
        executable.display()
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
        "no response received; service startup was cancelled before Full Disk Access guidance was acknowledged"
    );
    match answer.trim() {
        "" => Ok(true),
        "q" | "Q" => Ok(false),
        _ => bail!("enter an empty line to continue or q to cancel; service was not started"),
    }
}

#[cfg(target_os = "macos")]
fn request_mac_fda_guidance(ownership_path: &Path, language: Language) -> Result<()> {
    use crate::mac_signing::{expected_requirement, verify_binary};

    let (ownership, executable) = owned_mac_worker(ownership_path)?;
    let requirement =
        expected_requirement().context("read the configured macOS signing identity")?;
    if let Some(pin) = ownership.mac_signing_requirement.as_deref() {
        ensure!(
            pin == requirement,
            "owned worker signing identity does not match the configured identity"
        );
    }
    verify_binary(&executable, &requirement)
        .context("verify the actual owned worker before opening Full Disk Access settings")?;
    require_mac_fda_interactive_terminal(
        &executable,
        "installation can configure the native profile or start the service",
    )?;
    open_mac_permissions_for_executable(&executable).with_context(|| {
        format!(
            "open Full Disk Access guidance for signed worker {}",
            executable.display()
        )
    })?;
    println!(
        "{}",
        language
            .text(
                "Selected signed worker executable: {}",
                "선택한 서명 작업자 실행 파일: {}",
                "选定的签名工作程序：{}",
                "選択した署名済みワーカー実行ファイル: {}"
            )
            .replace("{}", &executable.display().to_string())
    );
    println!("{}", language.text(
        "macOS may ask this executable for protected-folder access separately. Add this exact worker to Full Disk Access only if your workflow needs that broader permission.",
        "macOS가 이 실행 파일에 보호된 폴더 접근을 별도로 요청할 수 있습니다. 더 넓은 권한이 필요한 작업에서만 이 실행 파일을 전체 디스크 접근에 추가하세요.",
        "macOS 可能会单独询问此可执行文件是否可访问受保护的文件夹。只有工作流确实需要更广泛权限时，才将此准确路径的工作程序添加到“完全磁盘访问权限”。",
        "macOS はこの実行ファイルに保護フォルダへのアクセスを別途求める場合があります。ワークフローにより広い権限が必要な場合に限り、このパスのワーカーをフルディスクアクセスに追加してください。"
    ));
    println!("{}", language.text(
        "If you grant Full Disk Access, select the exact executable path shown above and enable it. Press Enter to continue or q to cancel. Continuing does not confirm that macOS granted access.",
        "전체 디스크 접근을 허용하려면 위에 표시된 정확한 실행 파일을 선택하고 켜세요. Enter로 계속하거나 q로 취소할 수 있습니다. 계속해도 macOS가 권한을 허용했는지는 확인되지 않습니다.",
        "如需授予“完全磁盘访问权限”，请选择上方显示的准确可执行文件并启用。按 Enter 继续，或按 q 取消。继续操作不会确认 macOS 已授予访问权限。",
        "フルディスクアクセスを許可する場合は、上記の正確な実行ファイルを選択して有効にしてください。Enter で続行、q でキャンセルできます。続行しても macOS がアクセスを許可したことは確認されません。"
    ));
    print!(
        "{}",
        language.text(
            "Continue after FDA guidance [Enter] / cancel [q] > ",
            "FDA 안내 후 계속 [Enter] / 취소 [q] > ",
            "完成完全磁盘访问提示后继续 [Enter] / 取消 [q] > ",
            "フルディスクアクセスの案内後に続行 [Enter] / キャンセル [q] > "
        )
    );
    io::stdout().flush()?;
    let mut answer = String::new();
    let read = io::stdin().read_line(&mut answer)?;
    if mac_fda_guidance_answer(read, &answer)? {
        Ok(())
    } else {
        bail!(
            "installation cancelled before service startup; signed worker: {}",
            executable.display()
        )
    }
}

#[cfg(target_os = "macos")]
fn ownership_data_root(ownership: &Ownership) -> Result<PathBuf> {
    let state_dir = ownership.state_dir.canonicalize().with_context(|| {
        format!(
            "resolve owned state directory {}",
            ownership.state_dir.display()
        )
    })?;
    if state_dir.file_name().and_then(|name| name.to_str()) != Some("state") {
        bail!(
            "owned state directory does not use the expected profile data layout: {}",
            state_dir.display()
        );
    }
    let data_root = state_dir
        .parent()
        .context("owned state directory has no profile data root")?
        .to_owned();
    if !ownership.runtime_dir.is_absolute() || !ownership.runtime_dir.starts_with(&data_root) {
        bail!(
            "owned runtime directory is outside the profile data root: {}",
            ownership.runtime_dir.display()
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
            let value = args
                .get(index + 1)
                .context("mac-permissions option requires a path")?;
            if value.starts_with('-') {
                bail!("mac-permissions option requires a path");
            }
            if option == "--config" {
                if config.replace(PathBuf::from(value)).is_some() {
                    bail!("mac-permissions accepts --config only once");
                }
            } else if ownership.replace(PathBuf::from(value)).is_some() {
                bail!("mac-permissions accepts --ownership only once");
            }
            index += 2;
        } else {
            bail!("unknown mac-permissions option `{option}`");
        }
    }
    if config.is_some() && ownership.is_some() {
        bail!("mac-permissions accepts either --config or --ownership, not both");
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
            "unknown update option {option}"
        );
        let value = args.get(i + 1).context("update option requires a path")?;
        ensure!(!value.starts_with('-'), "update option requires a path");
        let slot = if option == "--source" {
            &mut source
        } else {
            &mut config
        };
        ensure!(
            slot.replace(PathBuf::from(value)).is_none(),
            "{option} may be used only once"
        );
        i += 2;
    }
    Ok(UpdateArgs {
        source: source.context("update requires --source <new-binary>")?,
        config: config.context("update requires --config <existing-config>")?,
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
            .context("run the incoming worker's update installer")?;
        ensure!(status.success(), "incoming worker update failed: {status}");
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
        bail!("binary updates are supported on Linux, macOS, and Windows")
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
        .context("config path has no parent")?
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
    let source = args
        .source
        .canonicalize()
        .context("resolve update source")?;
    ensure!(
        fs::symlink_metadata(&source)?.file_type().is_file(),
        "update source is not a regular file"
    );
    let config_path = args
        .config
        .canonicalize()
        .context("resolve selected config")?;
    let mut owner = resolve_profile_ownership(&config_path)?;
    let previous_owner = owner.clone();
    ensure!(
        owner.config_file.canonicalize()? == config_path,
        "selected config differs from ownership record"
    );
    let data_root = owner
        .state_dir
        .parent()
        .context("owned state directory has no data root")?
        .to_owned();
    ensure!(
        owner.state_dir == data_root.join("state"),
        "owned state directory layout is invalid"
    );
    let installed = recorded_service_executable(&owner)?
        .context("ownership record has no validated worker executable")?;
    ensure!(
        source != installed,
        "update source must be separate from the installed executable"
    );
    ensure!(
        installed.file_name() == Some(worker_executable_name().as_os_str()),
        "owned executable does not use the Discodex worker filename"
    );
    let mut config = Config::load(&config_path)?;
    ensure!(
        config.state_dir == owner.state_dir
            && config.credentials_file.as_deref() == Some(owner.credentials_file.as_path()),
        "profile config differs from ownership record"
    );
    #[cfg(target_os = "linux")]
    let service = validate_owned_linux_update_service(&owner, &installed, &config_path)?;
    #[cfg(windows)]
    let service = validate_owned_windows_update_service(&owner, &installed, &config_path)?;
    let profile_snapshot = ProfileRuntimeSnapshot::read(&owner)?;
    let runtime = runtime::prepare(&data_root)?;
    let previous = fs::read(&installed).context("snapshot owned worker")?;
    let permissions = fs::metadata(&installed)?.permissions();
    let parent = installed
        .parent()
        .context("worker executable has no parent")?;
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
        staged
            .persist(&installed)
            .map_err(|error| anyhow!("replace owned worker: {}", error.error))?;
        replaced = true;
        set_executable(&installed)?;
        ensure!(
            fs::metadata(&installed)?.len() > 0,
            "updated worker is empty"
        );
        activate_runtime(&mut owner, &mut config, &runtime)?;
        #[cfg(target_os = "linux")]
        {
            if service.was_active {
                run_checked("systemctl", &["--user", "start", &service.unit])?;
                ensure!(
                    systemctl_user_state("is-active", &service.unit)? == "active",
                    "updated Discodex worker did not become active"
                );
            }
            let enabled_state = systemctl_user_state("is-enabled", &service.unit)?;
            let remains_enabled = matches!(enabled_state.as_str(), "enabled" | "enabled-runtime");
            ensure!(
                remains_enabled == service.was_enabled,
                "updated systemd unit did not retain its previous enabled state"
            );
            stopped = false;
        }
        #[cfg(windows)]
        {
            set_windows_update_task(&service.task_name, service.was_enabled, service.was_active)?;
            let (enabled, active) = inspect_windows_task_state(&service.task_name)?;
            ensure!(
                enabled == service.was_enabled && active == service.was_active,
                "updated scheduled task did not return to its previous enabled/running state"
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
                    run_checked("systemctl", &["--user", "stop", &service.unit])
                        .context("could not stop updated service before restoring worker")?;
                    #[cfg(windows)]
                    {
                        set_windows_update_task(&service.task_name, false, false)
                            .context("could not stop updated task before restoring worker")?;
                        let (enabled, active) = inspect_windows_task_state(&service.task_name)?;
                        ensure!(
                            !enabled && !active,
                            "Windows task is still enabled or running"
                        );
                    }
                }
                UpdateRecoveryStep::RestoreWorker => {
                    restore_worker_bytes(&installed, &previous, permissions.clone())
                        .context("binary restore failed")?;
                }
                UpdateRecoveryStep::RestoreProfile => {
                    profile_snapshot
                        .restore(replaced)
                        .context("profile runtime restore failed")?;
                }
                UpdateRecoveryStep::RestoreService => {
                    #[cfg(target_os = "linux")]
                    {
                        if service.was_active {
                            run_checked("systemctl", &["--user", "start", &service.unit])
                                .context("previous service restart failed")?;
                        }
                        ensure!(
                            (systemctl_user_state("is-active", &service.unit)? == "active")
                                == service.was_active,
                            "previous systemd unit did not return to its prior active state"
                        );
                        let enabled = systemctl_user_state("is-enabled", &service.unit)?;
                        ensure!(
                            matches!(enabled.as_str(), "enabled" | "enabled-runtime")
                                == service.was_enabled,
                            "previous systemd unit did not retain its prior enabled state"
                        );
                    }
                    #[cfg(windows)]
                    {
                        set_windows_update_task(
                            &service.task_name,
                            service.was_enabled,
                            service.was_active,
                        )
                        .context("previous task state restore failed")?;
                        let (enabled, active) = inspect_windows_task_state(&service.task_name)?;
                        ensure!(
                            enabled == service.was_enabled && active == service.was_active,
                            "previous scheduled task did not return to its prior enabled/running state"
                        );
                    }
                }
                UpdateRecoveryStep::DiscardPreparedRuntime => {
                    runtime.discard().context("staged runtime cleanup failed")?
                }
            }
            Ok(())
        });
        return match rollback {
            Ok(()) => Err(error).context(if replaced {
                "update failed; previous worker and service state were restored"
            } else {
                "update failed before replacement; existing worker and profile were unchanged"
            }),
            Err(restore) => Err(anyhow!(
                "update failed ({error:#}); rollback incomplete ({restore:#})"
            )),
        };
    }
    retire_previous_runtime(&previous_owner, &runtime);
    println!(
        "{}",
        config
            .language
            .text(
                "Updated Discodex worker at {}.",
                "Discodex 작업자를 업데이트했습니다: {}",
                "已更新 Discodex 工作程序：{}",
                "Discodex ワーカーを更新しました：{}"
            )
            .replace("{}", &installed.display().to_string())
    );
    Ok(())
}

struct ProfileRuntimeSnapshot {
    config_path: PathBuf,
    config: Vec<u8>,
    ownership_path: PathBuf,
    ownership: Vec<u8>,
}

impl ProfileRuntimeSnapshot {
    fn read(owner: &Ownership) -> Result<Self> {
        let ownership_path = owner
            .config_file
            .parent()
            .context("profile has no directory")?
            .join("ownership.json");
        Ok(Self {
            config_path: owner.config_file.clone(),
            config: fs::read(&owner.config_file)?,
            ownership: fs::read(&ownership_path)?,
            ownership_path,
        })
    }

    fn restore(&self, replaced: bool) -> Result<()> {
        for (path, bytes) in self.restore_inputs(replaced).into_iter().flatten() {
            write_bytes_private(path, bytes)?;
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
    let mut staged =
        tempfile::NamedTempFile::new_in(path.parent().context("profile has no directory")?)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    set_private_file(staged.path())?;
    staged
        .persist(path)
        .map_err(|error| anyhow!("replace profile state: {}", error.error))?;
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
            .context("profile has no directory")?
            .join("ownership.json"),
        owner,
    )
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
    let parent = path.parent().context("worker path has no parent")?;
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
) -> Result<OwnedLinuxUpdateService> {
    let unit = format!("{SERVICE_NAME}.service");
    let path = owner
        .service_file
        .as_deref()
        .context("ownership has no service unit path")?;
    ensure!(
        owner.service_kind.as_deref() == Some("systemd-user")
            && path.file_name().and_then(|n| n.to_str()) == Some(unit.as_str()),
        "ownership does not identify the Discodex systemd user unit"
    );
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let dir = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("systemd/user");
    ensure!(
        path == &dir.join(&unit),
        "service unit path differs from this user's Discodex service path"
    );
    let expected = format!(
        "[Unit]\nDescription=Discodex Discord bridge\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={} run --config {}\nRestart=on-failure\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(executable),
        systemd_quote(config)
    );
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file() && fs::read_to_string(path)? == expected,
        "owned systemd unit differs from the verified worker/config"
    );
    let active_state = systemctl_user_state("is-active", &unit)?;
    ensure!(
        active_state == "active" || active_state == "inactive",
        "systemd unit is in a transitional or failed state; update refused"
    );
    let enabled_state = systemctl_user_state("is-enabled", &unit)?;
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
) -> Result<OwnedWindowsUpdateService> {
    ensure!(
        owner.service_kind.as_deref() == Some("scheduled-task-user")
            && owner.service_file.is_none(),
        "ownership does not identify the Discodex scheduled task"
    );
    let task_name = windows_task_name_for_config(config).to_owned();
    let task_exe = windows_task_executable_path(executable)?;
    let expected_exe = ps_single_quote(&task_exe.display().to_string());
    let expected_args = ps_single_quote(&format!(
        r#"run --config "{}""#,
        owner.config_file.display()
    ));
    let (state, _) = inspect_owned_windows_task(&expected_exe, &expected_args, owner)?;
    ensure!(
        state == "matching",
        "scheduled task differs from ownership record"
    );
    let (was_enabled, was_active) = inspect_windows_task_state(&task_name)?;
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
                let value = args
                    .get(index + 1)
                    .context("update option requires a path")?;
                if value.starts_with('-') {
                    bail!("update option requires a path");
                }
                let slot = match option {
                    "--source" => &mut source,
                    "--config" => &mut config,
                    _ => &mut ownership,
                };
                if slot.replace(PathBuf::from(value)).is_some() {
                    bail!("{option} may be used only once");
                }
                index += 2;
            }
            "--migrate-adhoc" => {
                if migrate_adhoc {
                    bail!("--migrate-adhoc may be used only once");
                }
                migrate_adhoc = true;
                index += 1;
            }
            option => bail!("unknown mac-update option `{option}`"),
        }
    }
    let source = source.context("mac-update requires --source <new-build-binary>")?;
    if config.is_some() && ownership.is_some() {
        bail!("mac-update accepts either --config or --ownership, not both");
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
                "--migrate-adhoc is only valid for an installation without a pinned signing identity"
            );
        }
        return Ok(());
    }
    if !migrate_adhoc {
        bail!(
            "no signing identity is pinned; pass --migrate-adhoc once to migrate the ad-hoc build"
        );
    }
    if existing_signature_is_adhoc != Some(true) {
        bail!("the unpinned installation is not verified as ad-hoc signed; migration refused");
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
            .context("resolve ownership record")?;
    let mut owner: Ownership = read_json(&owner_path).context("read owned installation record")?;
    let config_path = owner
        .config_file
        .canonicalize()
        .context("resolve owned profile config")?;
    if config
        .as_ref()
        .is_some_and(|p| p.canonicalize().ok().as_deref() != Some(config_path.as_path()))
    {
        bail!("--config does not identify the config recorded by this ownership record");
    }
    let adjacent_owner = config_path
        .parent()
        .context("owned config has no parent")?
        .join("ownership.json")
        .canonicalize()
        .context("resolve ownership record beside config")?;
    if adjacent_owner != owner_path {
        bail!("ownership record is not beside its owned profile config");
    }
    let data_root = ownership_data_root(&owner)?;
    let recorded_worker = owner
        .worker_executable
        .as_ref()
        .context("owned install has no recorded worker executable")?;
    let installed = recorded_worker
        .canonicalize()
        .context("resolve recorded worker executable")?;
    if recorded_worker != &installed {
        bail!("recorded worker path is not its canonical owned executable path");
    }
    if !installed.is_file() || !is_owned_worker_executable(&installed, &data_root) {
        bail!(
            "recorded worker executable is not a regular file in the canonical owned bin directory"
        );
    }
    if owner.service_kind.as_deref() != Some("launchd-user") {
        bail!("ownership record does not identify an owned macOS LaunchAgent");
    }
    let mut profile_config = Config::load(&config_path)?;
    let language = profile_config.language;
    let (label, plist) = launchd_identity(&config_path)?;
    if owner.service_file.as_deref() != Some(plist.as_path()) {
        bail!("recorded LaunchAgent path does not match this owned profile");
    }
    let expected_plist = launchd_plist(&label, &installed, &config_path);
    if !existing_launchd_plist_matches(&plist, &expected_plist, true)? {
        bail!("owned LaunchAgent plist is missing");
    }
    let was_loaded = launchd_is_loaded(&label, &installed, &config_path)?;

    let source = source.canonicalize().context("resolve update source")?;
    if !source.is_file() {
        bail!("update source is not a regular file: {}", source.display());
    }
    if source == installed {
        bail!("update source must be separate from the installed worker executable");
    }
    if let Some(pin) = owner.mac_signing_requirement.as_deref() {
        validate_mac_update_migration(Some(pin), migrate, None)?;
        adopt_owned_binary_identifier(&installed, pin)
            .context("preserve the verified owned signing identity")?;
        verify_binary(&installed, pin)
            .context("verify installed binary against pinned signing identity")?;
    } else {
        let output = Command::new("/usr/bin/codesign")
            .args(["-dv", "--verbose=4"])
            .arg(&installed)
            .output()
            .context("inspect installed signature")?;
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
            "ad-hoc migration can stop or replace the service",
        )?;
    }

    let parent = installed
        .parent()
        .context("installed executable has no parent")?;
    let old_bytes = fs::read(&installed).context("snapshot installed executable")?;
    let old_permissions = fs::metadata(&installed)?.permissions();
    let old_owner_bytes = fs::read(&owner_path).context("snapshot ownership record")?;
    let old_owner_permissions = fs::metadata(&owner_path)?.permissions();
    let old_pin = owner.mac_signing_requirement.clone();
    let staged =
        tempfile::NamedTempFile::new_in(parent).context("stage beside installed executable")?;
    fs::copy(&source, staged.path()).context("copy update source into staging file")?;
    set_executable(staged.path())?;
    let requirement =
        sign_binary_with_language(staged.path(), language).context("sign staged executable")?;
    staged
        .as_file()
        .sync_all()
        .context("flush signed staged executable")?;
    if old_pin.as_deref().is_some_and(|pin| pin != requirement) {
        bail!("update signer does not match pinned installation identity");
    }
    verify_binary(staged.path(), &requirement).context("verify staged executable")?;

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
        staged
            .persist(&installed)
            .map_err(|e| anyhow!("atomically replace executable: {}", e.error))?;
        replaced = true;
        set_executable(&installed)?;
        verify_binary(&installed, &requirement).context("verify replaced executable")?;
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
                    plist.to_str().context("LaunchAgent path is not UTF-8")?,
                ],
            )?;
            run_checked("launchctl", &["kickstart", &format!("{domain}/{label}")])?;
            if !launchd_is_loaded(&label, &installed, &config_path)? {
                bail!("updated LaunchAgent did not load");
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
        let rollback = loaded.and_then(|loaded| {
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
                            .context("restore ownership record")?;
                        profile_snapshot
                            .restore(replaced)
                            .context("restore previous profile runtime")?;
                    }
                    UpdateRecoveryStep::DiscardPreparedRuntime => {
                        runtime.discard().context("staged runtime cleanup failed")?
                    }
                    UpdateRecoveryStep::RestoreService => {
                        let domain = launchd_domain()?;
                        run_checked(
                            "launchctl",
                            &[
                                "bootstrap",
                                &domain,
                                plist.to_str().context("LaunchAgent path is not UTF-8")?,
                            ],
                        )?;
                        run_checked("launchctl", &["kickstart", &format!("{domain}/{label}")])?;
                        if !launchd_is_loaded(&label, &installed, &config_path)? {
                            bail!("previous LaunchAgent did not return to loaded state");
                        }
                    }
                }
                Ok(())
            })
        });
        return match rollback {
            Ok(()) if replaced => Err(error).context("update failed; previous executable, ownership record, and loaded state were restored"),
            Ok(()) if service_needs_restore => Err(error).context("update failed before replacement; existing executable and profile were unchanged, and the previous service state was restored"),
            Ok(()) => Err(error).context("update failed before replacement; existing executable, profile, and service were unchanged"),
            Err(restore) => Err(anyhow!("update failed ({error:#}); rollback failed ({restore:#})")),
        };
    }
    retire_previous_runtime(&previous_owner, &runtime);
    println!(
        "Updated owned macOS worker and runtime at {}.",
        installed.display()
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

#[cfg(target_os = "macos")]
fn atomic_restore(path: &Path, bytes: &[u8], permissions: &fs::Permissions) -> Result<()> {
    let mut temp =
        tempfile::NamedTempFile::new_in(path.parent().context("worker path has no parent")?)?;
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
    let worker = ownership
        .worker_executable
        .as_ref()
        .context("self-cleanup requires the owned worker executable path")?;
    let mut preserved_paths = vec![ownership.project_dir.clone(), ownership.state_dir.clone()];
    if let Some(config) = config {
        preserved_paths.extend([config.project_dir.clone(), config.state_dir.clone()]);
        preserved_paths.extend(config.codex_home.clone());
    }
    let preserved_paths = preserved_paths
        .into_iter()
        .map(|path| path.canonicalize().unwrap_or(path))
        .collect::<Vec<_>>();
    let preserved_paths = serde_json::to_string(&preserved_paths)
        .context("record Codex user data and project paths for deferred cleanup")?;
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
        .context("start the user-scoped Windows cleanup helper")?;
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
    let config_base = env::var_os("APPDATA")
        .map(PathBuf::from)
        .context("APPDATA is not set")?;
    #[cfg(not(windows))]
    let config_base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context("XDG_CONFIG_HOME or HOME is not set")?;

    #[cfg(windows)]
    let data_base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .context("LOCALAPPDATA is not set")?;
    #[cfg(not(windows))]
    let data_base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .context("XDG_DATA_HOME or HOME is not set")?;

    Ok(Roots {
        config: config_base.join(app_name),
        data: data_base.join(app_name),
    })
}

fn default_codex_home() -> Result<PathBuf> {
    #[cfg(windows)]
    let home = env::var_os("USERPROFILE").context("USERPROFILE is not set")?;
    #[cfg(not(windows))]
    let home = env::var_os("HOME").context("HOME is not set")?;
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
    println!("{}", language.text(
        "Codex data: 1) Existing home (share account, memory and history); 2) Separate Discodex home. Shared settings are preserved.",
        "Codex 데이터: 1) 기존 홈 공유 (계정·메모리·기록); 2) Discodex 전용 홈. 공유 설정은 보존합니다.",
        "Codex 数据：1) 共享现有目录（账户、记忆和历史）；2) Discodex 独立目录。保留共享设置。",
        "Codex データ：1) 既存ホームを共有（アカウント・メモリ・履歴）；2) Discodex 専用ホーム。共有設定は保持します。"
    ));
    println!("1) {}\n2) {}", existing.display(), separate.display());
    let default = if existing.is_dir() { "1" } else { "2" };
    loop {
        match ask_text(
            language,
            "Data home",
            "데이터 홈",
            "数据目录",
            "データホーム",
            default,
        )?
        .as_str()
        {
            "1" => {
                return Ok(PathBuf::from(ask_text(
                    language,
                    "Existing Codex home",
                    "기존 Codex 홈",
                    "现有 Codex 目录",
                    "既存 Codex ホーム",
                    &existing.display().to_string(),
                )?));
            }
            "2" => return Ok(separate),
            _ => println!(
                "{}",
                language.text(
                    "Choose 1 or 2.",
                    "1 또는 2를 선택하세요.",
                    "请选择 1 或 2。",
                    "1 または 2 を選んでください。"
                )
            ),
        }
    }
}

fn choose_antigravity_install(language: Language) -> Result<AntigravityInstallChoice> {
    println!("{}", language.text(
        "Antigravity: 1) Install a Discodex-managed CLI; 2) Skip (you can enable it later in Discord). Enter q to exit setup.",
        "Antigravity: 1) Discodex 전용 CLI 설치; 2) 건너뛰기 (나중에 Discord에서 활성화 가능). q를 입력하면 설치를 종료합니다.",
        "Antigravity：1) 安装由 Discodex 管理的 CLI；2) 跳过（以后可在 Discord 中启用）。输入 q 退出安装。",
        "Antigravity：1) Discodex 管理の CLI をインストール; 2) スキップ（後で Discord から有効化できます）。q でセットアップを終了します。"
    ));
    loop {
        match ask_text(
            language,
            "Antigravity",
            "Antigravity",
            "Antigravity",
            "Antigravity",
            "2",
        )?
        .as_str()
        {
            "1" => return choose_antigravity_settings(language),
            "2" => return Ok(AntigravityInstallChoice::Disabled),
            "q" | "Q" => return Err(crate::agy_setup::SetupCancelled.into()),
            _ => println!(
                "{}",
                language.text(
                    "Choose 1, 2, or q.",
                    "1, 2 또는 q를 선택하세요.",
                    "请选择 1、2 或 q。",
                    "1、2、q のいずれかを選んでください。"
                )
            ),
        }
    }
}

fn choose_antigravity_settings(language: Language) -> Result<AntigravityInstallChoice> {
    println!("{}", language.text(
        "Antigravity settings: 1) Share existing settings; 2) Separate Discodex settings. The CLI still uses the operating system's account keyring. Shared settings are preserved on uninstall. Enter q to exit.",
        "Antigravity 설정: 1) 기존 설정 공유; 2) Discodex 전용 설정. 로그인 계정은 CLI의 운영체제 키링을 사용합니다. 공유 설정은 삭제할 때 보존합니다. q를 입력하면 종료합니다.",
        "Antigravity 设置：1) 共享现有设置；2) Discodex 独立设置。CLI 仍使用操作系统账户密钥环。卸载时保留共享设置。输入 q 退出。",
        "Antigravity 設定：1) 既存の設定を共有; 2) Discodex 専用の設定。CLI は引き続き OS のアカウントキーチェーンを使用します。共有設定はアンインストール時も保持します。q で終了します。"
    ));
    loop {
        let answer = ask_text(language, "Settings", "설정", "设置", "設定", "2")?;
        match antigravity_settings_choice(&answer) {
            Some(choice) => return Ok(choice),
            None if answer.eq_ignore_ascii_case("q") => {
                return Err(crate::agy_setup::SetupCancelled.into());
            }
            None => println!(
                "{}",
                language.text(
                    "Choose 1, 2, or q.",
                    "1, 2 또는 q를 선택하세요.",
                    "请选择 1、2 或 q。",
                    "1、2、q のいずれかを選んでください。"
                )
            ),
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
    let runtime = crate::agy::Runtime::for_config(config, share_settings)?;
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
        config.language.text(
            "Antigravity is installed, signed in, and enabled.",
            "Antigravity 설치와 로그인을 확인했고 활성화했습니다.",
            "已确认 Antigravity 安装和登录成功，并已启用。",
            "Antigravity のインストールとログインを確認し、有効化しました。"
        )
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
        crossterm::terminal::enable_raw_mode()
            .context("enable terminal input for Antigravity setup")?;
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

    println!("{}", language.text(
        "Preparing Antigravity. Ctrl+C or Ctrl+D cancels and exits at any stage. Use arrow keys and Enter for CLI choices; type an answer or login code, then press Enter. Typed text is hidden.",
        "Antigravity를 준비합니다. 어느 단계에서든 Ctrl+C 또는 Ctrl+D로 취소하고 나갈 수 있습니다. CLI 선택은 방향키와 Enter를 사용하고, 답변이나 로그인 코드는 입력 후 Enter를 누르세요. 입력 내용은 숨겨집니다.",
        "正在准备 Antigravity。任何阶段都可按 Ctrl+C 或 Ctrl+D 取消并退出。使用方向键和 Enter 选择 CLI 选项；输入回答或登录代码后按 Enter。输入内容不显示。",
        "Antigravity を準備します。各段階で Ctrl+C または Ctrl+D を押すと中止して終了できます。CLI の選択は矢印キーと Enter を使い、回答やログインコードは入力後に Enter を押してください。入力内容は非表示です。"
    ));
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
                            print!("{}\r\n", language.text(
                                "The CLI prompt changed. Follow the current prompt and try again.",
                                "CLI 입력 단계가 바뀌었습니다. 현재 안내에 따라 다시 입력하세요.",
                                "CLI 提示已更改。请按照当前提示重新输入。",
                                "CLI の入力段階が変わりました。現在の案内に従って再入力してください。"
                            ));
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
                    SetupProgress::Checking => language.text("Checking managed Antigravity…", "전용 Antigravity를 확인합니다…", "正在检查托管的 Antigravity…", "管理対象の Antigravity を確認しています…").to_owned(),
                    SetupProgress::Installing => language.text("Downloading official Antigravity release…", "공식 Antigravity 릴리스를 다운로드합니다…", "正在下载 Antigravity 官方版本…", "Antigravity の公式リリースをダウンロードしています…").to_owned(),
                    SetupProgress::CheckingAccount => language.text("Checking Antigravity sign-in…", "Antigravity 로그인을 확인합니다…", "正在检查 Antigravity 登录状态…", "Antigravity のログインを確認しています…").to_owned(),
                    SetupProgress::Verifying => language.text("Verifying the Antigravity model catalog…", "Antigravity 모델 목록을 확인합니다…", "正在验证 Antigravity 模型列表…", "Antigravity のモデル一覧を確認しています…").to_owned(),
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
        return Err(error).context("listen for setup cancellation");
    }
    if *cancel_tx.borrow() && result.is_ok() {
        return Err(SetupCancelled.into());
    }
    if input_unavailable {
        bail!("{}", language.text(
            "Antigravity sign-in needs an interactive terminal. Run the installer in a terminal or enable Antigravity from Discord settings.",
            "Antigravity 로그인에 대화형 터미널이 필요합니다. 터미널에서 설치 프로그램을 실행하거나 Discord 설정에서 Antigravity를 활성화하세요.",
            "Antigravity 登录需要交互式终端。请在终端中运行安装程序，或从 Discord 设置启用 Antigravity。",
            "Antigravity のログインには対話型端末が必要です。端末でインストーラーを実行するか、Discord の設定から Antigravity を有効化してください。"
        ));
    }
    if result
        .as_ref()
        .is_err_and(|error| error.is::<SetupCancelled>())
    {
        println!(
            "{}",
            language.text(
                "Antigravity setup cancelled.",
                "Antigravity 설정을 취소했습니다.",
                "已取消 Antigravity 设置。",
                "Antigravity のセットアップを中止しました。"
            )
        );
    }
    result
}

fn remove_owned_antigravity(state_dir: &Path) -> Result<()> {
    let executable = crate::agy::managed_executable(state_dir)?;
    let directory = executable
        .parent()
        .context("managed Antigravity path has no parent")?;
    let marker = directory.join(crate::agy_setup::OWNERSHIP_FILE);
    if !marker.exists() {
        return Ok(());
    }
    ensure!(
        crate::agy_setup::is_owned_install(&executable),
        "Antigravity ownership record is invalid; its files were preserved"
    );
    fs::remove_dir_all(directory).context("remove Discodex-managed Antigravity installation")?;
    Ok(())
}

fn resolve_codex_home(
    previous: Option<&Path>,
    selected_env: Option<&Path>,
    project_dir: &Path,
    default_home: Option<&Path>,
) -> Result<PathBuf> {
    let selected = previous
        .or(selected_env)
        .or(default_home)
        .context("no CODEX_HOME or default home was available")?;
    if selected.is_absolute() {
        return Ok(selected.to_owned());
    }
    let base = if project_dir.is_absolute() {
        project_dir.to_owned()
    } else {
        env::current_dir()
            .context("resolve relative project directory for CODEX_HOME")?
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
            "an existing installation has no readable profile identity; refusing to reuse its state"
        );
    }
    if ownership_profile.is_some_and(|profile| profile != requested_profile)
        || config_profile.is_some_and(|profile| profile != requested_profile)
    {
        bail!(
            "an existing installation belongs to another profile; refusing to replace its config, credentials, or ownership"
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
            "managed runtime is outside this profile's runtime directory"
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
            "managed runtime ownership points to a different package"
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
                "user data overlaps the managed runtime; preserving files"
            );
        }
        runtime::verify_owned(&ownership.runtime_dir)?;
        return Ok(Some(ownership.runtime_dir.clone()));
    }
    if ownership.codex_source != "official-core-owned" {
        return Ok(None);
    }
    let data_root = data_root
        .canonicalize()
        .with_context(|| format!("resolve profile data root {}", data_root.display()))?;
    let runtime_dir = data_root.join("official-codex");
    if ownership.runtime_dir != runtime_dir {
        bail!(
            "owned Codex runtime is not the expected profile directory; configuration and ownership were preserved"
        );
    }
    match fs::symlink_metadata(&runtime_dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => bail!(
            "owned Codex runtime is not a regular directory; configuration and ownership were preserved"
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect owned Codex runtime before cleanup"),
    }
    let executable = runtime_dir.join(codex_executable_name());
    if ownership.runtime_executable.as_deref() != Some(executable.as_path())
        || ownership.codex_path != executable
        || !previous_owns_completed_core(ownership, &executable)
    {
        bail!(
            "remaining Codex runtime has an incomplete ownership record; configuration and ownership were preserved"
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
                "Codex user data, project, state, or a selected executable overlaps the owned runtime at {}; its files, configuration, and ownership were preserved",
                path.display()
            );
        }
    }
    inspect_owned_core_runtime_contents(&runtime_dir)?;
    Ok(Some(runtime_dir))
}

fn inspect_owned_core_runtime_contents(runtime_dir: &Path) -> Result<()> {
    if !fs::symlink_metadata(runtime_dir)
        .context("inspect owned Codex runtime before cleanup")?
        .file_type()
        .is_dir()
    {
        bail!(
            "owned Codex runtime changed to a non-directory or link; its files, configuration, and ownership were preserved"
        );
    }
    for entry in fs::read_dir(runtime_dir).context("read owned Codex runtime before cleanup")? {
        let entry = entry.context("read owned Codex runtime entry")?;
        let name = entry.file_name();
        if (name != codex_executable_name()
            && name != OsString::from(code_mode_host_executable_name()))
            || !entry
                .file_type()
                .context("inspect owned runtime entry")?
                .is_file()
        {
            bail!(
                "owned runtime contains unrecorded files or user data at {}; its files, configuration, and ownership were preserved",
                entry.path().display()
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
    fs::remove_dir(runtime_dir)
        .context("remove empty owned runtime; configuration and ownership remain if cleanup failed")
}

fn validate_existing_installation_plan(
    ownership_path: &Path,
    config_path: &Path,
    requested_profile: &str,
) -> Result<()> {
    let ownership = if ownership_path.exists() {
        Some(read_json::<Ownership>(ownership_path).with_context(|| {
            format!(
                "read existing installation ownership {}; refusing to replace its plan",
                ownership_path.display()
            )
        })?)
    } else {
        None
    };
    let config = if config_path.exists() {
        Some(Config::load(config_path).with_context(|| {
            format!(
                "read existing profile settings from {}; refusing to replace its plan",
                config_path.display()
            )
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
) -> Result<()> {
    let executable = recorded_service_executable(ownership)?
        .map(Ok)
        .unwrap_or_else(|| env::current_exe().context("find Discodex executable"))?;
    #[cfg(target_os = "linux")]
    {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let unit_dir = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let unit_dir = unit_dir.join("systemd/user");
        fs::create_dir_all(&unit_dir).with_context(|| format!("create {}", unit_dir.display()))?;
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
            .context("check for an existing systemd user unit")?;
        if !loaded.status.success() {
            bail!(
                "could not check whether `{SERVICE_NAME}.service` already exists; it was left unchanged"
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
        let existing_unit_matches =
            fs::read_to_string(&unit_path).ok().as_deref() == Some(unit.as_str());
        let unit_loaded = String::from_utf8_lossy(&loaded.stdout).trim() != "not-found";
        if (unit_path.exists() || unit_loaded)
            && !(previous_record_owns_unit && existing_unit_matches)
        {
            bail!(
                "the user service `{SERVICE_NAME}.service` already exists outside this install record; it was left unchanged"
            )
        }
        fs::write(&unit_path, unit).with_context(|| format!("write {}", unit_path.display()))?;
        set_private_file(&unit_path)?;
        ownership.service_file = Some(unit_path.clone());
        ownership.service_kind = Some("systemd-user".into());
        let owner_file = config_path
            .parent()
            .context("config path has no parent")?
            .join("ownership.json");
        write_json_private(&owner_file, ownership)?;
        run_checked("systemctl", &["--user", "daemon-reload"])?;
        let unit_path_arg = unit_path
            .to_str()
            .context("systemd unit path is not valid UTF-8")?;
        run_checked("systemctl", &["--user", "enable", "--now", unit_path_arg])?;
        Ok(())
    }
    #[cfg(windows)]
    {
        let executable = executable
            .canonicalize()
            .context("resolve the Windows bridge executable before task registration")?;
        if ownership.worker_executable.as_deref() != Some(executable.as_path()) {
            bail!(
                "the Windows task can only use the bridge executable recorded inside this installation's owned bin directory; no task was changed"
            )
        }
        let task_executable = windows_task_executable_path(&executable)?;
        let expected_executable = ps_single_quote(&task_executable.display().to_string());
        let expected_arguments =
            ps_single_quote(&format!("run --config \"{}\"", config_path.display()));
        let previous_record_owns_task = previous_ownership.is_some_and(|previous| {
            (previous.service_kind.as_deref() == Some("scheduled-task-user")
                || previous.windows_task_pending_run_level.is_some())
                && previous.service_file.is_none()
                && previous.config_file == config_path
        });
        let (task_state, previous_level) =
            inspect_owned_windows_task(&expected_executable, &expected_arguments, ownership)?;
        if !windows_task_may_be_replaced(&task_state, previous_record_owns_task) {
            bail!(
                "the user scheduled task already exists outside this install record; it was left unchanged"
            )
        }
        let owner_file = config_path
            .parent()
            .context("config path has no parent")?
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
            .context("record the pending Windows task transition before requesting UAC")?;
        run_elevated_powershell(&registration_script).context(
            "register the elevated Windows user task; pending ownership remains for retry",
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
            return Err(error).context("could not save elevated task ownership; the new task was removed and startup was not requested");
        }
        // Same-user task owners can start their task without elevating again.
        run_powershell(&windows_task_start_script(
            &expected_executable,
            &expected_arguments,
            WindowsTaskRunLevel::Highest,
        ))
        .context("start the registered elevated Windows user task")?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let executable = executable
            .canonicalize()
            .context("resolve the bridge executable for the user LaunchAgent")?;
        let config_path = config_path
            .canonicalize()
            .context("resolve the profile config for the user LaunchAgent")?;
        let requirement = crate::mac_signing::expected_requirement().context(
            "load the configured macOS worker signing identity; run the signed installer",
        )?;
        crate::mac_signing::verify_binary(&executable, &requirement)
            .context("verify the worker executable before changing the LaunchAgent")?;
        if previous_ownership
            .and_then(|previous| previous.mac_signing_requirement.as_deref())
            .is_some_and(|previous| previous != requirement)
        {
            bail!(
                "the signing identity differs from this installation's recorded identity; LaunchAgent was left unchanged"
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
                "LaunchAgent {label} is already loaded without matching ownership; it was left unchanged"
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
            .context("LaunchAgent path has no parent")?;
        fs::create_dir_all(agent_dir)
            .with_context(|| format!("create LaunchAgents directory {}", agent_dir.display()))?;
        if plist_exists {
            remove_if_file(&plist_path)?;
        }
        let mut plist_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&plist_path)
            .with_context(|| {
                format!(
                    "create {} without replacing another service",
                    plist_path.display()
                )
            })?;
        plist_file
            .write_all(expected_plist.as_bytes())
            .with_context(|| format!("write {}", plist_path.display()))?;
        plist_file
            .sync_all()
            .with_context(|| format!("flush {}", plist_path.display()))?;
        set_private_file(&plist_path)?;
        ownership.service_file = Some(plist_path.clone());
        ownership.service_kind = Some("launchd-user".into());
        ownership.worker_executable = Some(executable.clone());
        let owner_file = config_path
            .parent()
            .context("config path has no parent")?
            .join("ownership.json");
        write_json_private(&owner_file, ownership)?;
        let domain = launchd_domain()?;
        run_checked(
            "launchctl",
            &[
                "bootstrap",
                &domain,
                plist_path
                    .to_str()
                    .context("LaunchAgent path is not valid UTF-8")?,
            ],
        )?;
        run_checked("launchctl", &["kickstart", &format!("{domain}/{label}")])?;
        if !launchd_is_loaded(&label, &executable, &config_path)? {
            bail!("launchctl accepted {label}, but the owned LaunchAgent is not loaded");
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (config_path, ownership, executable);
        bail!("user service installation is currently supported on Linux, macOS, and Windows")
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
        .context("profile config has no parent directory")?
        .canonicalize()
        .context("resolve profile config directory for LaunchAgent identity")?;
    let digest = Sha256::digest(config_root.as_os_str().as_bytes());
    let suffix = format!("{digest:x}");
    let label = format!("{service_name}.{}", &suffix[..16]);
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set for the user LaunchAgent")?;
    let plist = home
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    Ok((label, plist))
}

#[cfg(target_os = "macos")]
fn launchd_domain() -> Result<String> {
    let output = command_without_console("id")
        .arg("-u")
        .output()
        .context("read current user id for LaunchAgent domain")?;
    if !output.status.success() {
        bail!("id -u failed while resolving the user LaunchAgent domain");
    }
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("id -u returned an invalid user id for the LaunchAgent domain");
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
                    "LaunchAgent path {} is not a regular owned file",
                    path.display()
                );
            }
            let matches = ownership_matches
                && fs::read_to_string(path).ok().as_deref() == Some(expected_contents);
            if !matches {
                bail!(
                    "the LaunchAgent plist {} is not the matching owned service; it was left unchanged",
                    path.display()
                );
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("inspect LaunchAgent plist {}", path.display()))
        }
    }
}

#[cfg(target_os = "macos")]
fn launchd_is_loaded(label: &str, executable: &Path, config_path: &Path) -> Result<bool> {
    let domain = launchd_domain()?;
    let target = format!("{domain}/{label}");
    let output = command_without_console("launchctl")
        .args(["print", &target])
        .output()
        .context("inspect user LaunchAgent registration")?;
    if !output.status.success() {
        let detail = format!(
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if detail.contains("Could not find service") || detail.contains("No such process") {
            return Ok(false);
        }
        bail!("launchctl could not inspect {target}: {}", detail.trim());
    }

    let report = String::from_utf8_lossy(&output.stdout);
    let expected_program = executable.display().to_string();
    let program = report
        .lines()
        .find_map(|line| line.trim().strip_prefix("program = "))
        .context("loaded LaunchAgent has no program readback")?;
    if program != expected_program {
        bail!("loaded LaunchAgent {label} runs unexpected program {program}");
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
            "loaded LaunchAgent {label} has unexpected arguments; refusing to stop or replace it"
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
                bail!("LaunchAgent {label} remained loaded after bootout timeout")
            }
            Ok(true) => {}
            Err(error) if Instant::now() >= deadline => {
                return Err(error).context(format!(
                    "could not confirm that LaunchAgent {label} unloaded after bootout"
                ));
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
            .context("inspect the owned Windows task")?;
        if !output.status.success() {
            bail!("could not inspect the Windows task; it was left unchanged");
        }
        let state = String::from_utf8(output.stdout)?.trim().to_owned();
        match state.as_str() {
            "matching" | "absent" => return Ok((state, level)),
            "different" => {}
            _ => bail!("unexpected Windows task inspection result; it was left unchanged"),
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
        .stdin(Stdio::null()).output().context("read installer Windows identity")?;
    if !identity.status.success() {
        bail!("could not determine the installing Windows user; no elevated operation was started");
    }
    let identity: serde_json::Value =
        serde_json::from_slice(&identity.stdout).context("decode installer Windows identity")?;
    let sid = identity["sid"]
        .as_str()
        .context("installer Windows SID is missing")?;
    if !valid_windows_sid(sid) {
        bail!("invalid installer Windows SID; no elevated operation was started");
    }
    if identity["elevated"].as_bool() == Some(true) {
        return run_powershell(operation);
    }
    let temp = tempfile::tempdir().context("prepare temporary Windows task operation")?;
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
    let powershell = PathBuf::from(env::var_os("SystemRoot").context("SystemRoot is not set")?)
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
        .context("request Windows administrator consent")?;
    if !launched.status.success() {
        let detail = fs::read_to_string(&result_path).unwrap_or_default();
        if detail.trim().is_empty() {
            bail!(
                "Windows administrator consent was cancelled or the task operation could not start: {}",
                String::from_utf8_lossy(&launched.stderr).trim()
            );
        }
        bail!("Windows task operation failed: {}", detail.trim());
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

fn uninstall_user_service(ownership: &mut Ownership, owner_file: &Path) -> Result<()> {
    let service_kind = ownership.service_kind.as_deref().or_else(|| {
        ownership
            .windows_task_pending_run_level
            .map(|_| "scheduled-task-user")
    });
    match service_kind {
        Some("systemd-user") => {
            let unit_path = ownership
                .service_file
                .as_deref()
                .context("owned systemd service has no recorded unit path")?;
            let unit_name = unit_path
                .file_name()
                .and_then(|name| name.to_str())
                .context("owned systemd unit path has no valid service name")?;
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
                    "owned systemd unit path does not name the expected {expected_service}.service; preserving configuration and ownership"
                )
            }
            run_checked(
                "systemctl",
                &["--user", "disable", "--now", unit_name],
            )
                .context("could not stop and disable the owned user service; config and credentials were preserved")?;
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
            let executable = ownership
                .worker_executable
                .as_deref()
                .context("owned Windows task has no recorded worker executable; preserving config and ownership")?;
            let task_executable = windows_task_executable_path(executable)?;
            let expected_executable = ps_single_quote(&task_executable.display().to_string());
            let expected_arguments = ps_single_quote(&format!(
                "run --config \"{}\"",
                ownership.config_file.display()
            ));
            let (state, level) =
                inspect_owned_windows_task(&expected_executable, &expected_arguments, ownership)?;
            if state == "different" {
                bail!(
                    "Windows task differs from this ownership record; config and credentials were preserved"
                );
            }
            if state != "absent" {
                run_elevated_powershell(&windows_task_unregistration_script(
                    &expected_executable, &expected_arguments, level, windows_task_name_for_config(&ownership.config_file),
                ))
                .context("could not verify, stop, and unregister the owned Windows task; config and credentials were preserved")?;
            }
            ownership.windows_task_pending_run_level = None;
            ownership.service_kind = None;
            ownership.service_file = None;
            write_json_private(owner_file, ownership)?;
        }
        #[cfg(target_os = "macos")]
        Some("launchd-user") => {
            let executable = ownership
                .worker_executable
                .as_deref()
                .context("owned LaunchAgent has no recorded bridge executable; preserving configuration and ownership")?;
            let config_path = ownership
                .config_file
                .canonicalize()
                .context("resolve the owned profile config before removing its LaunchAgent")?;
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
            let plist_path = ownership
                .service_file
                .as_deref()
                .context("owned LaunchAgent has no recorded plist path")?;
            if plist_path != expected_plist_path {
                bail!(
                    "owned LaunchAgent path does not match this profile identity; preserving configuration and ownership"
                );
            }
            let executable = executable
                .canonicalize()
                .context("resolve the owned bridge executable before removing its LaunchAgent")?;
            let expected_plist = launchd_plist(&label, &executable, &config_path);
            existing_launchd_plist_matches(plist_path, &expected_plist, true)
                .context("owned LaunchAgent plist changed outside this install; preserving configuration and ownership")?;
            if launchd_is_loaded(&label, &executable, &config_path)? {
                let domain = launchd_domain()?;
                run_checked("launchctl", &["bootout", &format!("{domain}/{label}")])
                    .context("could not stop the matching owned LaunchAgent; config and credentials were preserved")?;
            }
            if plist_path.exists() {
                remove_if_file(plist_path)?;
            }
            let config_parent = config_path.parent().context("config path has no parent")?;
            remove_if_file(&config_parent.join("worker.stdout.log"))?;
            remove_if_file(&config_parent.join("worker.stderr.log"))?;
            ownership.service_kind = None;
            ownership.service_file = None;
            write_json_private(owner_file, ownership)?;
        }
        #[cfg(all(not(windows), not(target_os = "macos")))]
        Some("scheduled-task-user") => bail!(
            "this installation owns a Windows task; uninstall it on Windows so its running process can be stopped safely"
        ),
        _ => {}
    }
    Ok(())
}

fn remove_registered_commands(
    config_path: &Path,
    credential_path: &Path,
    state_dir: &Path,
) -> Result<()> {
    if !config_path.is_file() || !credential_path.is_file() {
        if state_dir.join("discord-command-id.json").exists() {
            bail!("the owned Discord command marker exists, but its config or token is missing")
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

        let models = native.model_catalog().await.context("read the native provider model catalog")?;
        if let Some(selected) = &config.model {
            if !crate::model_catalog::catalog_has_model(&models, selected) {
                bail!("selected model is absent from the native provider catalog and configuration");
            }
        }

        let profiles = native.permission_profiles().await.context("read permissionProfile/list")?;
        let profile_id = choose_permission_profile(&profiles, config.permissions.as_deref(), config.language)?;
        config.permissions = Some(profile_id.clone());
        write_json_private(config_path, config)?;
        println!("The live permission catalog allows profile `{profile_id}`; its exact ID was saved for the runtime's first thread.");
        Ok::<(), anyhow::Error>(())
    }.await;
    let shutdown = native.shutdown().await;
    result?;
    shutdown.context("shut down native installation probe")?;
    Ok(())
}

async fn ensure_native_account(
    native: &crate::native::Native,
    language: Language,
) -> Result<serde_json::Value> {
    let account_read = || native.request("account/read", serde_json::json!({}));
    let mut response = account_read()
        .await
        .context("query official Codex account/read")?;
    if !crate::model_catalog::needs_chatgpt_login(&response) {
        println!(
            "{}",
            language.text(
                "Using the active Codex provider and its existing authentication settings.",
                "활성 Codex 공급자와 기존 인증 설정을 사용합니다.",
                "使用当前 Codex 提供商及其现有身份验证设置。",
                "有効な Codex プロバイダーと既存の認証設定を使用します。"
            )
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
        .context("start the official ChatGPT account login")?;
    let login_id = login
        .get("loginId")
        .and_then(serde_json::Value::as_str)
        .context("official ChatGPT login did not return a login ID")?
        .to_owned();
    let auth_url = login
        .get("authUrl")
        .and_then(serde_json::Value::as_str)
        .context("official ChatGPT login did not return an authorization URL")?;
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
    .context("timed out waiting for the official ChatGPT sign-in and MFA interaction")?;
    completed?;

    response = account_read()
        .await
        .context("verify official ChatGPT account/read after login")?;
    if response
        .pointer("/account/type")
        .and_then(serde_json::Value::as_str)
        != Some("chatgpt")
    {
        bail!(
            "official login finished, but account/read still does not report a ChatGPT account: {}",
            serde_json::to_string(&response)?
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
        .context("start xdg-open for the official ChatGPT login URL")?;
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
        .context("open the official ChatGPT login URL")?;
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn open_login_url(_url: &str) -> Result<()> {
    bail!("automatic browser opening is supported on Linux and Windows")
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
        .context("permissionProfile/list returned no data array")?;
    if let Some(id) = preferred_permission_profile_id(entries, preferred_id) {
        println!("The current native catalog still allows saved permission profile `{id}`.");
        return Ok(id);
    }
    let default_id = default_permission_profile_id(entries)
        .context("the current native catalog has no allowed permission profiles")?;
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
            "Allowed permission profile ID",
            "허용된 권한 프로필 ID",
            "允许的权限配置 ID",
            "許可された権限プロファイル ID",
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
            .context("create setup runtime")?
            .block_on(future)
    }
}

fn remove_if_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove owned file {}", path.display())),
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

fn write_json_private<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("file has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    set_private_dir(parent)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    set_private_file(path)
}

fn write_secret(path: &Path, secret: &str) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("replace owned secret file {}", path.display()))?;
    }
    fs::write(path, format!("{secret}\n"))
        .with_context(|| format!("write secret file {}", path.display()))?;
    set_private_file(path)
}

fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    set_private_dir(path)
}

fn set_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict directory permissions for {}", path.display()))?;
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
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restrict file permissions for {}", path.display()))?;
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
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("make {} executable", path.display()))
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
        .with_context(|| format!("start {program}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!(
            "{program} exited with {}{}",
            output.status,
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
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

fn env_language() -> Option<Language> {
    env::var("DISCODEX_LANGUAGE")
        .ok()
        .and_then(|value| value.parse::<Language>().ok())
}

fn configured_language(language: Language) -> Language {
    env_language().unwrap_or(language)
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
            language.text(
                "Message receive policy: 1) Everyone (all servers and DMs)  2) Registered server  3) Registered channels  4) Paired users",
                "메시지 수신 정책: 1) 모두 (모든 서버 및 DM)  2) 등록된 서버  3) 등록된 채널  4) 페어링된 사용자",
                "消息接收策略：1) 所有人（所有服务器和私信）  2) 已注册服务器  3) 已注册频道  4) 已配对用户",
                "メッセージ受信ポリシー: 1) 全員（すべてのサーバーとDM）  2) 登録サーバー  3) 登録チャンネル  4) ペアユーザー"
            )
        );
        let choice = ask_text(
            language,
            "Choose policy (1-4)",
            "정책 선택 (1-4)",
            "选择策略（1-4）",
            "ポリシーを選択 (1-4)",
            "4",
        )?;
        match choice.trim().to_ascii_lowercase().as_str() {
            "1" | "everyone" => break MessagePolicy::Everyone,
            "2" | "registeredserver" | "registered-server" => {
                break MessagePolicy::RegisteredServer;
            }
            "3" | "registeredchannels" | "registered-channels" => {
                break MessagePolicy::RegisteredChannels;
            }
            "4" | "paired" | "pairedusers" | "paired-users" => break MessagePolicy::Paired,
            _ => println!(
                "{}",
                language.text(
                    "Choose 1, 2, 3, or 4.",
                    "1, 2, 3, 4 중에서 선택하세요.",
                    "请选择 1、2、3 或 4。",
                    "1、2、3、4 から選択してください。"
                )
            ),
        }
    };

    let paired_user_ids = if policy == MessagePolicy::Paired {
        ask_ids_with_default(
            language,
            "Paired user IDs; enter 'none' to clear",
            "페어링 사용자 ID ('none' 입력 시 비움)",
            "已配对用户 ID（输入 none 清空）",
            "ペアユーザー ID（none で消去）",
            operator_ids,
        )?
    } else {
        operator_ids.to_vec()
    };
    let channel_ids = if policy == MessagePolicy::RegisteredChannels {
        ask_ids_with_default(
            language,
            "Registered channel IDs; blank keeps home/transcript defaults, 'none' clears",
            "등록 채널 ID (비우면 홈/전사 채널, 'none' 입력 시 비움)",
            "已注册频道 ID（留空使用主页/转录频道，输入 none 清空）",
            "登録チャンネル ID（空欄でホーム/文字起こし、none で消去）",
            default_channels,
        )?
    } else {
        default_channels.to_vec()
    };
    let whitelist = ask_optional_ids(
        language,
        "Whitelist user IDs (optional; comma-separated)",
        "허용 목록 사용자 ID (선택 사항, 쉼표로 구분)",
        "允许列表用户 ID（可选，逗号分隔）",
        "許可リストのユーザー ID（任意、カンマ区切り）",
    )?;
    let blacklist = ask_optional_ids(
        language,
        "Blacklist user IDs (optional; comma-separated)",
        "차단 목록 사용자 ID (선택 사항, 쉼표로 구분)",
        "阻止列表用户 ID（可选，逗号分隔）",
        "拒否リストのユーザー ID（任意、カンマ区切り）",
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
    en: &str,
    ko: &str,
    zh: &str,
    ja: &str,
    default_ids: &[u64],
) -> Result<Vec<u64>> {
    let default = default_ids
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    loop {
        let line = ask_text(language, en, ko, zh, ja, &default)?;
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
            language.text(
                "Enter numeric IDs separated by commas, or 'none' to clear.",
                "숫자 ID를 쉼표로 구분해 입력하거나 'none'을 입력해 비우세요.",
                "请输入逗号分隔的数字 ID，或输入 none 清空。",
                "数値 ID をカンマ区切りで入力するか、none で消去してください。"
            )
        );
    }
}

fn ask_optional_ids(
    language: Language,
    en: &str,
    ko: &str,
    zh: &str,
    ja: &str,
) -> Result<Vec<u64>> {
    loop {
        let line = ask_text(language, en, ko, zh, ja, "")?;
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
            language.text(
                "Enter numeric IDs separated by commas, or leave blank for none.",
                "숫자 ID를 쉼표로 구분해 입력하거나 비워 두세요.",
                "请输入逗号分隔的数字 ID，或留空表示无。",
                "数値 ID をカンマ区切りで入力するか、空欄のままにしてください。"
            )
        );
    }
}

fn choose_language(existing: Option<Language>) -> Result<Language> {
    if let Some(language) = env_language().or(existing) {
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

fn ask_text(
    language: Language,
    en: &str,
    ko: &str,
    zh: &str,
    ja: &str,
    default: &str,
) -> Result<String> {
    ask(language.text(en, ko, zh, ja), default)
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

fn ask_u64(language: Language, en: &str, ko: &str, zh: &str, ja: &str) -> Result<u64> {
    loop {
        let value = ask_text(language, en, ko, zh, ja, "")?;
        match value.parse() {
            Ok(value) => return Ok(value),
            Err(_) => println!(
                "{}",
                language.text(
                    "Enter the numeric Discord ID.",
                    "숫자 Discord ID를 입력하세요.",
                    "请输入数字 Discord ID。",
                    "Discord の数値 ID を入力してください。"
                )
            ),
        }
    }
}

fn ask_ids(language: Language, en: &str, ko: &str, zh: &str, ja: &str) -> Result<Vec<u64>> {
    loop {
        let line = ask_text(language, en, ko, zh, ja, "")?;
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
            language.text(
                "Enter one or more numeric IDs separated by commas.",
                "숫자 ID를 하나 이상 쉼표로 구분해 입력하세요.",
                "请输入一个或多个数字 ID，并用逗号分隔。",
                "数値 ID を1つ以上、カンマ区切りで入力してください。"
            )
        );
    }
}

fn optional_value(
    language: Language,
    en: &str,
    ko: &str,
    zh: &str,
    ja: &str,
) -> Result<Option<String>> {
    let value = ask_text(language, en, ko, zh, ja, "")?;
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
        let prepared = prepare_project_directory(&selected).expect("prepare project directory");
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
            windows_task_executable_path(Path::new(
                r"\\?\C:\Users\test\AppData\Local\codex-worker\bin\worker.exe"
            ))
            .expect("normalize a local extended path"),
            PathBuf::from(r"C:\Users\test\AppData\Local\codex-worker\bin\worker.exe")
        );
        assert_eq!(
            windows_task_executable_path(Path::new(
                r"C:\Users\test\AppData\Local\codex-worker\bin\worker.exe"
            ))
            .expect("retain a local DOS path"),
            PathBuf::from(r"C:\Users\test\AppData\Local\codex-worker\bin\worker.exe")
        );
        assert!(
            windows_task_executable_path(Path::new(r"\\?\UNC\server\share\worker.exe")).is_err()
        );
        assert!(windows_task_executable_path(Path::new(r"\\server\share\worker.exe")).is_err());
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
        );

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
        );
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

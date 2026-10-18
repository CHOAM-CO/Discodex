//! Release discovery and prebuilt-asset update preparation for Discord notifications.
//!
//! Discovery, preparation, state changes, and the eventual service update are kept
//! separate so the Discord loop can do network work without holding its state
//! mutex and can make the final idle decision immediately before handoff.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const RELEASE_URL: &str = "https://api.github.com/repos/CHOAM-CO/Discodex/releases/latest";
const COMMIT_URL: &str = "https://api.github.com/repos/CHOAM-CO/Discodex/commits/";
const RELEASE_BY_ID_URL: &str = "https://api.github.com/repos/CHOAM-CO/Discodex/releases/";
pub const DEFAULT_CHECK_INTERVAL_SECS: u64 = 6 * 60 * 60;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseNotice {
    pub release_id: u64,
    pub version: String,
    pub tag: String,
    pub commit: String,
    pub url: String,
    #[serde(default)]
    pub asset: Option<ReleaseAsset>,
    pub body: String,
    pub published_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PinnedRelease {
    pub release_id: u64,
    pub version: String,
    pub tag: String,
    pub commit: String,
    pub url: String,
    #[serde(default)]
    pub asset: Option<ReleaseAsset>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
    pub digest: Option<String>,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreparedUpdate {
    pub release_id: u64,
    pub version: String,
    pub commit: String,
    pub binary: PathBuf,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateSnapshot {
    pub status: UpdateStatus,
    pub last_checked_unix: Option<u64>,
    pub last_check_error: Option<String>,
    pub etag: Option<String>,
    pub latest_release: Option<ReleaseNotice>,
    pub notified_release_id: Option<u64>,
    pub message_id: Option<u64>,
    pub notice_started_unix: Option<u64>,
    pub skipped_release_id: Option<u64>,
    pub muted: bool,
    pub pending: Option<PinnedRelease>,
    pub prepared: Option<PreparedUpdate>,
    pub last_error: Option<String>,
    pub last_result: Option<PinnedRelease>,
    pub last_applied_release_id: Option<u64>,
    pub apply_attempt_id: Option<String>,
    pub helper_started_attempt_id: Option<String>,
    pub helper_pid: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct UpdateFile {
    format_version: u32,
    #[serde(flatten)]
    snapshot: UpdateSnapshot,
}

#[derive(Clone, Debug)]
pub struct UpdateStore {
    path: PathBuf,
    snapshot: UpdateSnapshot,
    base: UpdateSnapshot,
    writable: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    #[default]
    Idle,
    Selected,
    Preparing,
    Ready,
    Applying,
    Failed,
    Applied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckResult {
    Modified {
        checked_at_unix: u64,
        etag: Option<String>,
        release: Option<ReleaseNotice>,
    },
    NotModified {
        checked_at_unix: u64,
        etag: Option<String>,
    },
    NoPublishedRelease {
        checked_at_unix: u64,
        etag: Option<String>,
    },
}

#[derive(Deserialize)]
struct GithubRelease {
    id: u64,
    tag_name: String,
    html_url: String,
    body: Option<String>,
    published_at: Option<String>,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    size: u64,
}

#[derive(Deserialize)]
struct GithubCommit {
    sha: String,
}

impl UpdateStore {
    pub fn load(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join("updates.json");
        let snapshot = read_snapshot(&path)?;
        Ok(Self {
            path,
            base: snapshot.clone(),
            snapshot,
            writable: true,
        })
    }

    /// Fail-closed fallback for the host bridge when persisted updater state is
    /// unreadable. Callers should surface the load error and skip updater actions.
    pub fn unavailable(state_dir: &Path) -> Self {
        let snapshot = UpdateSnapshot {
            muted: true,
            ..UpdateSnapshot::default()
        };
        Self {
            path: state_dir.join("updates.json"),
            base: snapshot.clone(),
            snapshot,
            writable: false,
        }
    }

    pub fn snapshot(&self) -> UpdateSnapshot {
        self.snapshot.clone()
    }

    pub fn due(&self, now_unix: u64) -> bool {
        self.snapshot
            .last_checked_unix
            .is_none_or(|last| now_unix.saturating_sub(last) >= DEFAULT_CHECK_INTERVAL_SECS)
    }

    /// Merge only the result of discovery into the latest in-memory state. The caller
    /// can perform `check_release` from a cloned snapshot outside its mutex.
    pub fn record_check_failure(&mut self, now_unix: u64, error: String) -> Result<()> {
        self.snapshot.last_checked_unix = Some(now_unix);
        self.snapshot.last_check_error = Some(error);
        self.persist()
    }

    pub fn apply_check(&mut self, result: CheckResult) -> Result<()> {
        self.snapshot.last_check_error = None;
        match result {
            CheckResult::Modified {
                checked_at_unix,
                etag,
                release,
            } => {
                self.snapshot.last_checked_unix = Some(checked_at_unix);
                self.snapshot.etag = etag;
                self.snapshot.latest_release = release;
            }
            CheckResult::NotModified {
                checked_at_unix,
                etag,
            }
            | CheckResult::NoPublishedRelease {
                checked_at_unix,
                etag,
            } => {
                self.snapshot.last_checked_unix = Some(checked_at_unix);
                self.snapshot.etag = etag;
            }
        }
        self.persist()
    }

    pub fn select_update(&mut self, release_id: u64) -> Result<PinnedRelease> {
        let release = self
            .snapshot
            .latest_release
            .as_ref()
            .filter(|release| release.release_id == release_id)
            .context("selected release is no longer the latest discovered release")?;
        let pinned = PinnedRelease {
            release_id: release.release_id,
            version: release.version.clone(),
            tag: release.tag.clone(),
            commit: release.commit.clone(),
            url: release.url.clone(),
            asset: release.asset.clone(),
        };
        self.snapshot.pending = Some(pinned.clone());
        self.snapshot.status = UpdateStatus::Selected;
        self.snapshot.prepared = None;
        self.snapshot.last_error = None;
        self.persist()?;
        Ok(pinned)
    }

    /// Retry an already selected, immutable target without resolving it through the
    /// latest-release slot; discovery may have moved on while a build was pending.
    pub fn retry_update(&mut self, release_id: u64) -> Result<PinnedRelease> {
        let target = self
            .snapshot
            .pending
            .as_ref()
            .filter(|target| target.release_id == release_id)
            .context("selected release is no longer pending")?
            .clone();
        ensure!(
            self.snapshot.status == UpdateStatus::Failed,
            "only a failed update can be retried"
        );
        self.snapshot.status = if self.snapshot.prepared.is_some() {
            UpdateStatus::Ready
        } else {
            UpdateStatus::Selected
        };
        self.snapshot.last_error = None;
        self.persist()?;
        Ok(target)
    }

    pub fn skip_release(&mut self, release_id: u64) -> Result<()> {
        self.snapshot.skipped_release_id = Some(release_id);
        if self
            .snapshot
            .pending
            .as_ref()
            .is_some_and(|r| r.release_id == release_id)
        {
            self.snapshot.pending = None;
            self.snapshot.prepared = None;
            self.snapshot.status = UpdateStatus::Idle;
        }
        self.persist()
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<()> {
        self.snapshot.muted = muted;
        self.persist()
    }

    pub fn begin_notice(&mut self, release_id: u64, now_unix: u64) -> Result<()> {
        self.snapshot.notified_release_id = Some(release_id);
        self.snapshot.message_id = None;
        self.snapshot.notice_started_unix = Some(now_unix);
        self.persist()
    }

    pub fn record_notice(&mut self, release_id: u64, message_id: u64) -> Result<()> {
        self.snapshot.notified_release_id = Some(release_id);
        self.snapshot.message_id = Some(message_id);
        self.persist()
    }

    pub fn clear_notice(&mut self, release_id: u64) -> Result<()> {
        if self.snapshot.notified_release_id == Some(release_id) {
            self.snapshot.notified_release_id = None;
            self.snapshot.message_id = None;
            self.snapshot.notice_started_unix = None;
        }
        self.persist()
    }

    pub fn mark_preparing(&mut self, release_id: u64) -> Result<()> {
        ensure!(
            self.snapshot
                .pending
                .as_ref()
                .is_some_and(|p| p.release_id == release_id),
            "release is not selected"
        );
        self.snapshot.status = UpdateStatus::Preparing;
        self.snapshot.last_error = None;
        self.persist()
    }

    pub fn store_prepared(&mut self, prepared: PreparedUpdate) -> Result<()> {
        ensure!(
            self.snapshot
                .pending
                .as_ref()
                .is_some_and(|target| target.release_id == prepared.release_id
                    && target.commit == prepared.commit),
            "prepared binary does not match the selected pinned release"
        );
        self.snapshot.prepared = Some(prepared);
        self.snapshot.status = UpdateStatus::Ready;
        self.snapshot.last_error = None;
        self.persist()
    }

    pub fn mark_applying(&mut self, release_id: u64) -> Result<String> {
        let attempt_id = Uuid::new_v4().to_string();
        let token = attempt_id.clone();
        self.persist_with(|snapshot| {
            ensure!(
                snapshot.status == UpdateStatus::Ready,
                "release is not ready to apply"
            );
            ensure!(
                snapshot
                    .prepared
                    .as_ref()
                    .is_some_and(|p| p.release_id == release_id),
                "release is not prepared"
            );
            ensure!(
                snapshot
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.release_id == release_id),
                "release is no longer pending"
            );
            snapshot.status = UpdateStatus::Applying;
            snapshot.apply_attempt_id = Some(token);
            snapshot.helper_started_attempt_id = None;
            snapshot.helper_pid = None;
            snapshot.last_error = None;
            Ok(())
        })?;
        Ok(attempt_id)
    }

    /// Atomically claim the handoff ticket before the helper performs installer actions.
    pub fn begin_helper(&mut self, release_id: u64, attempt_id: &str, commit: &str) -> Result<()> {
        let attempt = attempt_id.to_owned();
        let commit = commit.to_owned();
        self.persist_with(|snapshot| {
            ensure!(
                snapshot.status == UpdateStatus::Applying,
                "update handoff is no longer active"
            );
            ensure!(
                snapshot.pending.as_ref().is_some_and(
                    |target| target.release_id == release_id && target.commit == commit
                ),
                "helper target no longer matches selected release"
            );
            ensure!(
                snapshot
                    .prepared
                    .as_ref()
                    .is_some_and(
                        |prepared| prepared.release_id == release_id && prepared.commit == commit
                    ),
                "helper binary no longer matches prepared target"
            );
            ensure!(
                snapshot.apply_attempt_id.as_deref() == Some(attempt.as_str()),
                "update handoff ticket is no longer active"
            );
            ensure!(
                snapshot.helper_started_attempt_id.is_none(),
                "update helper already claimed this handoff"
            );
            snapshot.helper_started_attempt_id = Some(attempt);
            snapshot.helper_pid = Some(std::process::id());
            Ok(())
        })
    }

    /// Cancel a handoff only if the helper has not claimed its ticket. Both this
    /// transition and helper startup use the same file lock, eliminating the timeout race.
    pub fn cancel_unstarted_handoff(
        &mut self,
        release_id: u64,
        attempt_id: &str,
        error: String,
    ) -> Result<bool> {
        let attempt = attempt_id.to_owned();
        let mut cancelled = false;
        self.persist_with(|snapshot| {
            if snapshot.status == UpdateStatus::Applying
                && snapshot
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.release_id == release_id)
                && snapshot.apply_attempt_id.as_deref() == Some(attempt.as_str())
                && snapshot.helper_started_attempt_id.is_none()
            {
                snapshot.status = UpdateStatus::Failed;
                snapshot.last_error = Some(error.clone());
                snapshot.last_result = snapshot.pending.clone();
                snapshot.apply_attempt_id = None;
                snapshot.helper_started_attempt_id = None;
                snapshot.helper_pid = None;
                cancelled = true;
            }
            Ok(())
        })?;
        Ok(cancelled)
    }

    /// Fail an acknowledged helper that the OS reports as stopped. The attempt check shares the state-file lock with helper outcome writes, so Applied cannot be overwritten by stale liveness.
    pub fn fail_stopped_helper(&mut self, attempt_id: &str, error: String) -> Result<bool> {
        let attempt = attempt_id.to_owned();
        let mut failed = false;
        self.persist_with(|snapshot| {
            if snapshot.status == UpdateStatus::Applying
                && snapshot.apply_attempt_id.as_deref() == Some(attempt.as_str())
                && snapshot.helper_started_attempt_id.as_deref() == Some(attempt.as_str())
            {
                snapshot.status = UpdateStatus::Failed;
                snapshot.last_error = Some(error.clone());
                snapshot.last_result = snapshot.pending.clone();
                snapshot.apply_attempt_id = None;
                snapshot.helper_started_attempt_id = None;
                snapshot.helper_pid = None;
                failed = true;
            }
            Ok(())
        })?;
        Ok(failed)
    }

    pub fn mark_apply_failed(&mut self, release_id: u64, error: String) -> Result<()> {
        if self
            .snapshot
            .pending
            .as_ref()
            .is_some_and(|p| p.release_id == release_id)
        {
            self.snapshot.last_error = Some(error);
            self.snapshot.last_result = self.snapshot.pending.clone();
            self.snapshot.status = UpdateStatus::Failed;
            self.snapshot.apply_attempt_id = None;
            self.snapshot.helper_started_attempt_id = None;
            self.snapshot.helper_pid = None;
        }
        self.persist()
    }

    pub fn mark_applied(&mut self, release_id: u64) -> Result<()> {
        self.persist_with(|snapshot| {
            if snapshot
                .pending
                .as_ref()
                .is_some_and(|pending| pending.release_id == release_id)
            {
                snapshot.last_applied_release_id = Some(release_id);
                snapshot.last_result = snapshot.pending.clone();
                snapshot.status = UpdateStatus::Applied;
                snapshot.pending = None;
                snapshot.prepared = None;
                snapshot.last_error = None;
                snapshot.apply_attempt_id = None;
                snapshot.helper_started_attempt_id = None;
                snapshot.helper_pid = None;
            } else if snapshot.last_applied_release_id == Some(release_id)
                && snapshot.status == UpdateStatus::Applied
            {
                // A restarted worker may already have recorded this helper's result.
                // Preserve the retained release card and all newer pending selections.
            }
            Ok(())
        })
    }

    fn persist(&mut self) -> Result<()> {
        self.persist_with(|_| Ok(()))
    }

    fn persist_with(
        &mut self,
        update: impl FnOnce(&mut UpdateSnapshot) -> Result<()>,
    ) -> Result<()> {
        ensure!(
            self.writable,
            "update state is unavailable; refusing updater actions"
        );
        let parent = self
            .path
            .parent()
            .context("update state path has no parent")?;
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        let lock_path = self.path.with_extension("json.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open update state lock {}", lock_path.display()))?;
        lock.lock().context("lock update state")?;
        let disk = read_snapshot(&self.path)?;
        let mut merged = merge_snapshot(&self.base, &self.snapshot, &disk);
        update(&mut merged)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).context("stage update state")?;
        let file = UpdateFile {
            format_version: 1,
            snapshot: merged.clone(),
        };
        serde_json::to_writer_pretty(&mut temp, &file).context("serialize update state")?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all().context("flush update state")?;
        temp.persist(&self.path)
            .map_err(|error| anyhow!("replace update state: {}", error.error))?;
        self.snapshot = merged.clone();
        self.base = merged;
        Ok(())
    }
}

fn read_snapshot(path: &Path) -> Result<UpdateSnapshot> {
    match fs::read(path) {
        Ok(bytes) => {
            let file: UpdateFile = serde_json::from_slice(&bytes)
                .with_context(|| format!("read update state {}", path.display()))?;
            ensure!(file.format_version == 1, "unsupported update state version");
            Ok(file.snapshot)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(UpdateSnapshot::default()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn merge_snapshot(
    base: &UpdateSnapshot,
    desired: &UpdateSnapshot,
    disk: &UpdateSnapshot,
) -> UpdateSnapshot {
    macro_rules! field {
        ($name:ident) => {
            if desired.$name != base.$name {
                desired.$name.clone()
            } else {
                disk.$name.clone()
            }
        };
    }
    UpdateSnapshot {
        status: field!(status),
        last_checked_unix: field!(last_checked_unix),
        last_check_error: field!(last_check_error),
        etag: field!(etag),
        latest_release: field!(latest_release),
        notified_release_id: field!(notified_release_id),
        message_id: field!(message_id),
        notice_started_unix: field!(notice_started_unix),
        skipped_release_id: field!(skipped_release_id),
        muted: field!(muted),
        pending: field!(pending),
        prepared: field!(prepared),
        last_error: field!(last_error),
        last_result: field!(last_result),
        last_applied_release_id: field!(last_applied_release_id),
        apply_attempt_id: field!(apply_attempt_id),
        helper_started_attempt_id: field!(helper_started_attempt_id),
        helper_pid: field!(helper_pid),
    }
}

pub fn current_build_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
pub fn current_build_commit() -> &'static str {
    env!("DISCODEX_BUILD_COMMIT")
}

/// Fetch the latest stable release and resolve the release tag to an immutable commit.
pub fn check_release(
    etag: Option<String>,
    now_unix: u64,
    installed_version: &str,
    installed_commit: &str,
) -> Result<CheckResult> {
    let client = Client::builder()
        .user_agent(concat!("Discodex/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(25))
        .build()?;
    let mut request = client
        .get(RELEASE_URL)
        .header("Accept", "application/vnd.github+json");
    if let Some(etag) = etag.as_deref() {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let response = request.send().context("check Discodex releases")?;
    let checked_etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or(etag);
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(CheckResult::NotModified {
            checked_at_unix: now_unix,
            etag: checked_etag,
        });
    }
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(CheckResult::NoPublishedRelease {
            checked_at_unix: now_unix,
            etag: checked_etag,
        });
    }
    let release: GithubRelease = response
        .error_for_status()
        .context("GitHub releases endpoint returned an error")?
        .json()
        .context("parse latest stable Discodex release")?;
    if release.draft
        || release.prerelease
        || !is_newer_version(&release.tag_name, installed_version)
    {
        return Ok(CheckResult::Modified {
            checked_at_unix: now_unix,
            etag: checked_etag,
            release: None,
        });
    }
    let commit: GithubCommit = client
        .get(format!("{COMMIT_URL}{}", release.tag_name))
        .header("Accept", "application/vnd.github+json")
        .send()
        .context("resolve release tag to a commit")?
        .error_for_status()
        .context("GitHub could not resolve release tag")?
        .json()
        .context("parse release commit")?;
    if commit.sha == installed_commit {
        return Ok(CheckResult::Modified {
            checked_at_unix: now_unix,
            etag: checked_etag,
            release: None,
        });
    }
    let asset_name = platform_asset_name()?;
    let asset = release_asset(release.assets, asset_name);
    let notice = ReleaseNotice {
        release_id: release.id,
        version: release.tag_name.trim_start_matches('v').to_owned(),
        tag: release.tag_name,
        commit: commit.sha,
        url: release.html_url,
        asset,
        body: release.body.unwrap_or_default(),
        published_at: release.published_at,
    };
    Ok(CheckResult::Modified {
        checked_at_unix: now_unix,
        etag: checked_etag,
        release: Some(notice),
    })
}

fn is_newer_version(candidate: &str, installed: &str) -> bool {
    fn parts(value: &str) -> Option<[u64; 3]> {
        let value = value.trim_start_matches('v');
        let mut parts = value.split('.');
        Some([
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.split(['-', '+']).next()?.parse().ok()?,
        ])
    }
    match (parts(candidate), parts(installed)) {
        (Some(candidate), Some(installed)) => candidate > installed,
        _ => false,
    }
}

/// Download the pinned release's prebuilt platform executable into profile state.
/// Legacy pending records without asset metadata resolve that same release ID once.
pub fn prepare_update(mut pinned: PinnedRelease, state_dir: &Path) -> Result<PreparedUpdate> {
    ensure!(
        !pinned.commit.is_empty(),
        "pinned release has no commit SHA"
    );
    let asset_name = platform_asset_name()?;
    let client = Client::builder()
        .user_agent(concat!("Discodex/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .build()?;
    if pinned.asset.is_none() {
        let release: GithubRelease = client
            .get(format!("{RELEASE_BY_ID_URL}{}", pinned.release_id))
            .header("Accept", "application/vnd.github+json")
            .send()
            .context("resolve selected GitHub release assets")?
            .error_for_status()
            .context("GitHub could not resolve selected release")?
            .json()
            .context("parse selected release assets")?;
        ensure!(
            release.id == pinned.release_id,
            "GitHub returned a different release ID"
        );
        pinned.asset = release_asset(release.assets, asset_name);
    }
    let asset = require_release_asset(pinned.asset.clone(), &pinned.tag, asset_name)?;
    ensure!(
        asset.name == asset_name,
        "pinned release asset does not match this platform"
    );
    let update_root = state_dir.join("updates");
    let binary_root = update_root
        .join("prepared")
        .join(pinned.release_id.to_string());
    fs::create_dir_all(&binary_root)?;
    let mut response = client
        .get(&asset.download_url)
        .header("Accept", "application/octet-stream")
        .send()
        .context("download prebuilt release executable")?
        .error_for_status()
        .context("GitHub rejected prebuilt release executable download")?;
    let binary = binary_root.join(asset_name);
    let staged = tempfile::NamedTempFile::new_in(&binary_root)?;
    let mut output = staged.reopen()?;
    copy_verified_asset(&mut response, &mut output, &asset)?;
    output
        .sync_all()
        .context("flush prepared release executable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o755))?;
    }
    drop(output);
    staged
        .persist(&binary)
        .map_err(|error| anyhow!("save prepared update: {}", error.error))?;
    Ok(PreparedUpdate {
        release_id: pinned.release_id,
        version: pinned.version,
        commit: pinned.commit,
        binary,
    })
}

fn copy_verified_asset(
    reader: &mut impl Read,
    output: &mut impl Write,
    asset: &ReleaseAsset,
) -> Result<()> {
    let digest = asset
        .digest
        .as_deref()
        .context("GitHub release asset has no SHA-256 digest")?;
    let expected_hash = digest
        .strip_prefix("sha256:")
        .context("GitHub release asset digest is not SHA-256")?;
    ensure!(
        expected_hash.len() == 64 && expected_hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "GitHub release asset SHA-256 digest is malformed"
    );
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .context("read prebuilt release executable")?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .context("save prebuilt release executable")?;
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    ensure!(
        size == asset.size,
        "release executable size mismatch (expected {}, received {size})",
        asset.size
    );
    let actual_hash = format!("{:x}", hasher.finalize());
    ensure!(
        actual_hash.eq_ignore_ascii_case(expected_hash),
        "release executable SHA-256 mismatch"
    );
    Ok(())
}

fn release_asset(assets: Vec<GithubAsset>, expected_name: &str) -> Option<ReleaseAsset> {
    assets
        .into_iter()
        .find(|asset| asset.name == expected_name)
        .map(|asset| ReleaseAsset {
            name: asset.name,
            download_url: asset.browser_download_url,
            digest: asset.digest,
            size: asset.size,
        })
}

fn require_release_asset(
    asset: Option<ReleaseAsset>,
    release_tag: &str,
    asset_name: &str,
) -> Result<ReleaseAsset> {
    asset.with_context(|| {
        format!("release {release_tag} has no prebuilt executable for this platform ({asset_name})")
    })
}

fn platform_asset_name() -> Result<&'static str> {
    platform_asset_name_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn platform_asset_name_for(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("discodex-x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("discodex-aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Ok("discodex-x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("discodex-aarch64-apple-darwin"),
        ("windows", "x86_64") => Ok("discodex-x86_64-pc-windows-msvc.exe"),
        ("windows", "aarch64") => Ok("discodex-aarch64-pc-windows-msvc.exe"),
        (os, arch) => bail!("automatic release updates are not available for {arch}-{os}"),
    }
}

/// Run the detached updater and persist its outcome after rollback/restart.
pub fn run_helper(args: &[String]) -> Result<()> {
    let mut source: Option<String> = None;
    let mut config_path: Option<String> = None;
    let mut release_id: Option<String> = None;
    let mut attempt_id: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        let option = args[index].as_str();
        ensure!(
            matches!(
                option,
                "--source" | "--config" | "--release-id" | "--attempt-id"
            ),
            "unknown update-helper option {option}"
        );
        let value = args
            .get(index + 1)
            .context("update-helper option requires a value")?;
        if option != "--release-id" {
            ensure!(
                !value.starts_with('-'),
                "update-helper option requires a path"
            );
        }
        let slot = match option {
            "--source" => &mut source,
            "--config" => &mut config_path,
            "--release-id" => &mut release_id,
            _ => &mut attempt_id,
        };
        ensure!(
            slot.replace(value.clone()).is_none(),
            "{option} may be used only once"
        );
        index += 2;
    }
    let source = source.context("update-helper requires --source")?;
    let config_path = PathBuf::from(config_path.context("update-helper requires --config")?);
    let release_id: u64 = release_id
        .context("update-helper requires --release-id")?
        .parse()
        .context("invalid update release ID")?;
    let attempt_id = attempt_id.context("update-helper requires --attempt-id")?;
    let config = crate::config::Config::load(&config_path)?;
    let source_path = PathBuf::from(&source)
        .canonicalize()
        .context("resolve prepared updater executable")?;
    ensure!(
        std::env::current_exe()?.canonicalize()? == source_path,
        "helper executable does not match selected prepared binary"
    );
    let mut store = UpdateStore::load(&config.state_dir)?;
    store.begin_helper(release_id, &attempt_id, current_build_commit())?;
    let result = super::update(&[
        "--source".into(),
        source,
        "--config".into(),
        config_path.display().to_string(),
    ]);
    let mut store = UpdateStore::load(&config.state_dir)?;
    match result {
        Ok(()) => store.mark_applied(release_id),
        Err(error) => {
            store.mark_apply_failed(release_id, format!("{error:#}"))?;
            Err(error)
        }
    }
}

/// Dispatch update CLI to a per-user helper job outside the running worker's service
/// process group. The helper invokes the existing ownership-checked installer path.
pub fn launch_apply(prepared: &PreparedUpdate, config: &Path, attempt_id: &str) -> Result<()> {
    ensure!(
        prepared.binary.is_file(),
        "prepared update binary is missing"
    );
    let config = config
        .canonicalize()
        .context("resolve selected profile config")?;
    let config_data = crate::config::Config::load(&config)?;
    let label = helper_label(attempt_id);
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("/bin/launchctl")
            .arg("submit")
            .arg("-l")
            .arg(&label)
            .arg("--")
            .arg(&prepared.binary)
            .arg("update-helper")
            .arg("--source")
            .arg(&prepared.binary)
            .arg("--config")
            .arg(&config)
            .arg("--release-id")
            .arg(prepared.release_id.to_string())
            .arg("--attempt-id")
            .arg(attempt_id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("start independent launchd update job")?;
        ensure!(status.success(), "launchctl submit failed: {status}");
    }
    #[cfg(target_os = "linux")]
    {
        let status = Command::new("systemd-run")
            .arg("--user")
            .arg("--collect")
            .arg("--unit")
            .arg(&label)
            .arg("--property=Type=exec")
            .arg(&prepared.binary)
            .arg("update-helper")
            .arg("--source")
            .arg(&prepared.binary)
            .arg("--config")
            .arg(&config)
            .arg("--release-id")
            .arg(prepared.release_id.to_string())
            .arg("--attempt-id")
            .arg(attempt_id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("start independent systemd user update job")?;
        ensure!(status.success(), "systemd-run failed: {status}");
    }
    #[cfg(windows)]
    {
        launch_windows_task(
            prepared,
            &config,
            &config_data.state_dir,
            &label,
            attempt_id,
        )?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    return Err(anyhow!(
        "detached update handoff is unsupported on this platform"
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let snapshot = UpdateStore::load(&config_data.state_dir)?.snapshot();
        if snapshot.status != UpdateStatus::Applying
            || snapshot.apply_attempt_id.as_deref() != Some(attempt_id)
        {
            bail!("update handoff was cancelled before helper startup");
        }
        if snapshot.helper_started_attempt_id.as_deref() == Some(attempt_id) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            let mut store = UpdateStore::load(&config_data.state_dir)?;
            let cancelled = store.cancel_unstarted_handoff(
                prepared.release_id,
                attempt_id,
                "Detached updater did not acknowledge startup".into(),
            )?;
            if !cancelled {
                let latest = UpdateStore::load(&config_data.state_dir)?.snapshot();
                if latest.status == UpdateStatus::Applying
                    && latest.apply_attempt_id.as_deref() == Some(attempt_id)
                    && latest.helper_started_attempt_id.as_deref() == Some(attempt_id)
                {
                    return Ok(());
                }
            }
            bail!("detached update helper did not acknowledge startup");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn helper_label(attempt_id: &str) -> String {
    format!("discodex-update-{attempt_id}")
}

/// Check the detached helper process after its durable startup acknowledgement.
/// The caller should reread UpdateStore before acting on a false result.
pub fn helper_running(state_dir: &Path, attempt_id: &str) -> Result<bool> {
    let snapshot = UpdateStore::load(state_dir)?.snapshot();
    if snapshot.status != UpdateStatus::Applying
        || snapshot.apply_attempt_id.as_deref() != Some(attempt_id)
        || snapshot.helper_started_attempt_id.as_deref() != Some(attempt_id)
    {
        return Ok(false);
    }
    #[cfg(windows)]
    {
        let task = helper_label(attempt_id);
        let Some(pid) = snapshot.helper_pid else {
            return Ok(false);
        };
        let Some(prepared) = snapshot.prepared.as_ref() else {
            return Ok(false);
        };
        let exe = ps_quote(&prepared.binary.display().to_string());
        let script = format!(
            "$ErrorActionPreference='Stop'; try {{ $t=Get-ScheduledTask -TaskPath '\\' -TaskName '{}' -ErrorAction SilentlyContinue; $p=Get-CimInstance Win32_Process -Filter 'ProcessId = {}' -ErrorAction SilentlyContinue; if (($t -and $t.State -eq 'Running') -or ($p -and $p.ExecutablePath -ieq '{}' -and $p.CommandLine -like '*update-helper*')) {{ Write-Output 'running' }} else {{ Write-Output 'stopped' }} }} catch {{ exit 2 }}",
            ps_quote(&task),
            pid,
            exe
        );
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::null())
            .output()
            .context("inspect detached Scheduled Task updater")?;
        ensure!(
            output.status.success(),
            "could not inspect detached Scheduled Task updater"
        );
        return Ok(String::from_utf8_lossy(&output.stdout).trim() == "running");
    }
    #[cfg(unix)]
    {
        let Some(pid) = snapshot.helper_pid else {
            return Ok(false);
        };
        let Some(prepared) = snapshot.prepared else {
            return Ok(false);
        };
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output()
            .context("inspect detached update helper process")?;
        if !output.status.success() {
            return Ok(false);
        }
        let command = String::from_utf8_lossy(&output.stdout);
        let executable = prepared.binary.canonicalize().unwrap_or(prepared.binary);
        return Ok(command.contains("update-helper")
            && command.contains(&executable.display().to_string()));
    }
    #[allow(unreachable_code)]
    Ok(false)
}

#[cfg(windows)]
fn launch_windows_task(
    prepared: &PreparedUpdate,
    config: &Path,
    state_dir: &Path,
    label: &str,
    attempt_id: &str,
) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    // Reuse the actual installed worker task's principal only after verifying its
    // executable, arguments, current-user identity, logon type, and recorded level.
    let ownership = super::resolve_profile_ownership(config)?;
    ensure!(
        ownership.service_kind.as_deref() == Some("scheduled-task-user"),
        "profile does not use an owned Windows Scheduled Task"
    );
    let service_executable = ownership
        .worker_executable
        .as_ref()
        .context("owned Windows worker executable is missing")?;
    let task_executable = super::windows_task_executable_path(service_executable)?;
    let expected_executable = super::ps_single_quote(&task_executable.display().to_string());
    let expected_arguments =
        super::ps_single_quote(&format!("run --config \"{}\"", config.display()));
    let (task_state, run_level) =
        super::inspect_owned_windows_task(&expected_executable, &expected_arguments, &ownership)?;
    ensure!(
        task_state == "matching",
        "owned Windows worker task does not match its ownership record"
    );

    let task_path = state_dir.join("updates");
    fs::create_dir_all(&task_path)?;
    let task_script = task_path.join(format!("{attempt_id}.ps1"));
    let exe = ps_quote(&prepared.binary.display().to_string());
    let config_arg = ps_quote(&config.display().to_string());
    let task_name = ps_quote(label);
    let task_script_body = format!(
        "$ErrorActionPreference='Stop'\ntry {{\n  & '{exe}' update-helper --source '{exe}' --config '{config_arg}' --release-id {} --attempt-id '{}'\n  $helperExit=$LASTEXITCODE\n}} finally {{\n  Unregister-ScheduledTask -TaskPath '\\' -TaskName '{task_name}' -Confirm:$false -ErrorAction SilentlyContinue\n  Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue\n}}\nexit $helperExit\n",
        prepared.release_id,
        ps_quote(attempt_id),
    );
    fs::write(&task_script, task_script_body).context("write temporary updater task wrapper")?;
    let run_level = run_level.powershell();
    let task = label.to_owned();
    let script_path = ps_quote(&task_script.display().to_string());
    let registration = format!(
        "$ErrorActionPreference='Stop'; $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $action=New-ScheduledTaskAction -Execute 'powershell.exe' -Argument '-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File \"{script_path}\"'; $principal=New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel '{run_level}'; $settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries; Register-ScheduledTask -TaskName '{task}' -TaskPath '\\' -Action $action -Principal $principal -Settings $settings | Out-Null; Start-ScheduledTask -TaskName '{task}' -TaskPath '\\'"
    );
    let status = Command::new("powershell.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &registration,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("start independent Scheduled Task updater")?;
    if !status.success() {
        let _ = fs::remove_file(&task_script);
        bail!("could not start independent Scheduled Task updater: {status}");
    }
    Ok(())
}

#[cfg(windows)]
fn ps_quote(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn notice(release_id: u64) -> ReleaseNotice {
        ReleaseNotice {
            release_id,
            version: "1.2.3".into(),
            tag: "v1.2.3".into(),
            commit: "abc".into(),
            url: "https://example.test/release".into(),
            asset: None,
            body: String::new(),
            published_at: None,
        }
    }

    #[test]
    fn preferences_merge_with_check_results_and_survive_reload() {
        let dir = tempdir().unwrap();
        let mut store = UpdateStore::load(dir.path()).unwrap();
        store
            .apply_check(CheckResult::Modified {
                checked_at_unix: 100,
                etag: Some("tag".into()),
                release: Some(notice(5)),
            })
            .unwrap();
        store.begin_notice(5, 110).unwrap();
        store.record_notice(5, 12).unwrap();
        store.set_muted(true).unwrap();
        store
            .apply_check(CheckResult::NotModified {
                checked_at_unix: 200,
                etag: Some("tag".into()),
            })
            .unwrap();
        let loaded = UpdateStore::load(dir.path()).unwrap().snapshot();
        assert_eq!(loaded.last_checked_unix, Some(200));
        assert_eq!(loaded.notified_release_id, Some(5));
        assert_eq!(loaded.message_id, Some(12));
        assert_eq!(loaded.notice_started_unix, Some(110));
        assert!(loaded.muted);
        assert_eq!(loaded.latest_release.unwrap().release_id, 5);
    }

    #[test]
    fn skip_and_pending_selection_are_release_id_pinned() {
        let dir = tempdir().unwrap();
        let mut store = UpdateStore::load(dir.path()).unwrap();
        store
            .apply_check(CheckResult::Modified {
                checked_at_unix: 100,
                etag: None,
                release: Some(notice(5)),
            })
            .unwrap();
        let selected = store.select_update(5).unwrap();
        assert_eq!(selected.release_id, 5);
        store.skip_release(5).unwrap();
        let snapshot = store.snapshot();
        assert_eq!(snapshot.skipped_release_id, Some(5));
        assert!(snapshot.pending.is_none());
    }

    #[test]
    fn concurrent_check_and_preference_writes_merge_without_losing_either() {
        let dir = tempdir().unwrap();
        let mut preferences = UpdateStore::load(dir.path()).unwrap();
        let mut check = UpdateStore::load(dir.path()).unwrap();
        preferences.set_muted(true).unwrap();
        check
            .apply_check(CheckResult::Modified {
                checked_at_unix: 100,
                etag: Some("release-etag".into()),
                release: Some(notice(9)),
            })
            .unwrap();
        let snapshot = UpdateStore::load(dir.path()).unwrap().snapshot();
        assert!(snapshot.muted);
        assert_eq!(snapshot.latest_release.unwrap().release_id, 9);
    }

    #[test]
    fn retry_keeps_the_pinned_target_after_latest_release_changes() {
        let dir = tempdir().unwrap();
        let mut store = UpdateStore::load(dir.path()).unwrap();
        store
            .apply_check(CheckResult::Modified {
                checked_at_unix: 100,
                etag: None,
                release: Some(notice(5)),
            })
            .unwrap();
        store.select_update(5).unwrap();
        store
            .mark_apply_failed(5, "rollback completed".into())
            .unwrap();
        store
            .apply_check(CheckResult::Modified {
                checked_at_unix: 200,
                etag: None,
                release: Some(notice(6)),
            })
            .unwrap();
        assert_eq!(store.retry_update(5).unwrap().release_id, 5);
        assert_eq!(store.snapshot().pending.unwrap().release_id, 5);
    }

    #[test]
    fn repeated_applied_acknowledgement_preserves_result_and_newer_state() {
        let dir = tempdir().unwrap();
        let mut owner = UpdateStore::load(dir.path()).unwrap();
        owner
            .apply_check(CheckResult::Modified {
                checked_at_unix: 100,
                etag: None,
                release: Some(notice(5)),
            })
            .unwrap();
        let pinned = owner.select_update(5).unwrap();
        owner
            .store_prepared(PreparedUpdate {
                release_id: 5,
                version: pinned.version.clone(),
                commit: pinned.commit.clone(),
                binary: dir.path().join("binary"),
            })
            .unwrap();
        let mut stale_helper = UpdateStore::load(dir.path()).unwrap();
        owner.mark_applied(5).unwrap();
        stale_helper.mark_applied(5).unwrap();
        let snapshot = UpdateStore::load(dir.path()).unwrap().snapshot();
        assert_eq!(snapshot.status, UpdateStatus::Applied);
        assert_eq!(snapshot.last_result, Some(pinned));
        assert_eq!(snapshot.last_applied_release_id, Some(5));
    }

    #[test]
    fn stopped_helper_monitor_cannot_overwrite_a_concurrent_applied_result() {
        let dir = tempdir().unwrap();
        let mut owner = UpdateStore::load(dir.path()).unwrap();
        owner
            .apply_check(CheckResult::Modified {
                checked_at_unix: 100,
                etag: None,
                release: Some(notice(5)),
            })
            .unwrap();
        let pinned = owner.select_update(5).unwrap();
        owner
            .store_prepared(PreparedUpdate {
                release_id: 5,
                version: pinned.version.clone(),
                commit: pinned.commit.clone(),
                binary: dir.path().join("binary"),
            })
            .unwrap();
        let attempt = owner.mark_applying(5).unwrap();
        let mut helper = UpdateStore::load(dir.path()).unwrap();
        helper.begin_helper(5, &attempt, &pinned.commit).unwrap();
        helper.mark_applied(5).unwrap();
        let mut monitor = UpdateStore::load(dir.path()).unwrap();
        assert!(
            !monitor
                .fail_stopped_helper(&attempt, "stopped".into())
                .unwrap()
        );
        assert_eq!(
            UpdateStore::load(dir.path()).unwrap().snapshot().status,
            UpdateStatus::Applied
        );
    }

    #[test]
    fn older_state_files_default_new_optional_preferences() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("updates.json"),
            r#"{"format_version":1,"status":"idle"}"#,
        )
        .unwrap();
        let snapshot = UpdateStore::load(dir.path()).unwrap().snapshot();
        assert!(!snapshot.muted);
        assert!(snapshot.pending.is_none());
        assert!(snapshot.helper_pid.is_none());
    }

    #[test]
    fn unavailable_fallback_is_muted_and_never_overwrites_unreadable_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("updates.json");
        let corrupt = b"{not valid json";
        fs::write(&path, corrupt).unwrap();
        let mut fallback = UpdateStore::unavailable(dir.path());
        assert!(fallback.snapshot().muted);
        assert!(fallback.set_muted(false).is_err());
        assert_eq!(fs::read(&path).unwrap(), corrupt);
        fs::remove_file(&path).unwrap();
        assert!(fallback.set_muted(false).is_err());
        assert!(!path.exists());
        assert!(!dir.path().join("updates.json.lock").exists());
    }

    #[test]
    fn helper_claim_and_timeout_cancellation_are_serialized_by_the_state_lock() {
        let dir = tempdir().unwrap();
        let mut owner = UpdateStore::load(dir.path()).unwrap();
        owner
            .apply_check(CheckResult::Modified {
                checked_at_unix: 100,
                etag: None,
                release: Some(notice(5)),
            })
            .unwrap();
        let pinned = owner.select_update(5).unwrap();
        owner
            .store_prepared(PreparedUpdate {
                release_id: pinned.release_id,
                version: pinned.version.clone(),
                commit: pinned.commit.clone(),
                binary: dir.path().join("prepared-binary"),
            })
            .unwrap();
        let attempt = owner.mark_applying(5).unwrap();
        let mut helper = UpdateStore::load(dir.path()).unwrap();
        helper.begin_helper(5, &attempt, &pinned.commit).unwrap();
        let mut timeout = UpdateStore::load(dir.path()).unwrap();
        assert!(
            !timeout
                .cancel_unstarted_handoff(5, &attempt, "timed out".into())
                .unwrap()
        );
        assert_eq!(
            UpdateStore::load(dir.path())
                .unwrap()
                .snapshot()
                .helper_started_attempt_id
                .as_deref(),
            Some(attempt.as_str())
        );

        let attempt = {
            let mut store = UpdateStore::load(dir.path()).unwrap();
            store.mark_apply_failed(5, "retry".into()).unwrap();
            store.retry_update(5).unwrap();
            store.mark_applying(5).unwrap()
        };
        let mut timeout = UpdateStore::load(dir.path()).unwrap();
        assert!(
            timeout
                .cancel_unstarted_handoff(5, &attempt, "timed out".into())
                .unwrap()
        );
        let mut late_helper = UpdateStore::load(dir.path()).unwrap();
        assert!(
            late_helper
                .begin_helper(5, &attempt, &pinned.commit)
                .is_err()
        );
    }

    #[test]
    fn release_asset_copy_checks_official_size_and_sha256() {
        let bytes = b"verified executable";
        let digest = format!("sha256:{:x}", Sha256::digest(bytes));
        let asset = ReleaseAsset {
            name: "discodex-test".into(),
            download_url: "https://example.test/asset".into(),
            digest: Some(digest),
            size: bytes.len() as u64,
        };
        let mut output = Vec::new();
        copy_verified_asset(&mut &bytes[..], &mut output, &asset).unwrap();
        assert_eq!(output, bytes);

        let mut wrong_size = asset.clone();
        wrong_size.size += 1;
        assert!(copy_verified_asset(&mut &bytes[..], &mut Vec::new(), &wrong_size).is_err());
        let mut wrong_digest = asset;
        wrong_digest.digest = Some(format!("sha256:{}", "0".repeat(64)));
        assert!(copy_verified_asset(&mut &bytes[..], &mut Vec::new(), &wrong_digest).is_err());
    }

    #[test]
    fn release_assets_map_to_all_six_supported_platforms() {
        for ((os, arch), expected) in [
            (("linux", "x86_64"), "discodex-x86_64-unknown-linux-gnu"),
            (("linux", "aarch64"), "discodex-aarch64-unknown-linux-gnu"),
            (("macos", "x86_64"), "discodex-x86_64-apple-darwin"),
            (("macos", "aarch64"), "discodex-aarch64-apple-darwin"),
            (("windows", "x86_64"), "discodex-x86_64-pc-windows-msvc.exe"),
            (
                ("windows", "aarch64"),
                "discodex-aarch64-pc-windows-msvc.exe",
            ),
        ] {
            assert_eq!(platform_asset_name_for(os, arch).unwrap(), expected);
        }
        assert!(platform_asset_name_for("freebsd", "x86_64").is_err());
    }

    #[test]
    fn missing_platform_asset_returns_a_clear_error() {
        let error = require_release_asset(None, "v1.2.3", "discodex-aarch64-apple-darwin")
            .unwrap_err()
            .to_string();
        assert!(error.contains("v1.2.3"));
        assert!(error.contains("discodex-aarch64-apple-darwin"));
    }

    #[test]
    fn legacy_pinned_release_without_asset_metadata_deserializes_for_id_refetch() {
        let pinned: PinnedRelease = serde_json::from_str(
            r#"{"releaseId":7,"version":"1.2.3","tag":"v1.2.3","commit":"abc","url":"https://example.test/release"}"#,
        ).unwrap();
        assert_eq!(pinned.release_id, 7);
        assert!(pinned.asset.is_none());
    }

    #[test]
    fn semver_comparison_is_numeric_and_ignores_prefix() {
        assert!(is_newer_version("v1.10.0", "1.9.9"));
        assert!(!is_newer_version("1.2.3", "v1.2.3"));
        assert!(!is_newer_version("preview", "1.2.3"));
    }
}

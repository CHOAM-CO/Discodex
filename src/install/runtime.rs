//! The release's prebuilt Codex package, kept separate from CODEX_HOME.
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

fn manifest_file_name() -> &'static str {
    &super::product::resources().product.runtime_manifest_file
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Manifest {
    pub format_version: u32,
    pub worker_version: String,
    pub codex_commit: String,
    pub target: String,
    pub archive: String,
    pub sha256: String,
    pub files: BTreeMap<String, String>,
}

pub(super) struct Prepared {
    pub directory: PathBuf,
    pub executable: PathBuf,
    pub manifest: Manifest,
    created: bool,
}

impl Prepared {
    pub fn discard(&self) -> Result<()> {
        if self.created {
            remove_owned(&self.directory)?;
        }
        Ok(())
    }
}

pub(super) fn target() -> Result<String> {
    let platform = super::product::platform(std::env::consts::OS, std::env::consts::ARCH).context(
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnostic07c7b15fabf3),
    )?;
    Ok(platform.target.clone())
}

/// Download the runtime selected by this worker release, before stopping services.
pub(super) fn prepare(data_root: &Path) -> Result<Prepared> {
    let version = env!("CARGO_PKG_VERSION");
    let target = target()?;
    let product = &super::product::resources().product;
    let base = format!(
        "{}/download/{}{version}",
        product.release_download_root.trim_end_matches('/'),
        product.version_tag_prefix
    );
    let client = Client::builder()
        .user_agent(format!("{}/{}", product.name, env!("CARGO_PKG_VERSION")))
        // The runtime includes the code-mode host; do not apply reqwest's
        // default whole-request deadline to a large streamed release download.
        .timeout(None)
        .build()?;
    let manifest_asset = product
        .runtime_manifest_asset_template
        .replace("{target}", &target);
    let manifest: Manifest = client
        .get(format!("{base}/{manifest_asset}"))
        .send()?
        .error_for_status()?
        .json()
        .context(
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnosticE735d32a17b2),
        )?;
    validate_selected_manifest(&manifest, version, &target, env!("DISCODEX_CODEX_COMMIT"))?;
    let runtimes = data_root.join(&product.runtime_directory);
    fs::create_dir_all(&runtimes)?;
    let runtimes = runtimes.canonicalize()?;
    let destination = runtimes.join(format!("{version}-{}", manifest.codex_commit));
    if destination.exists() {
        ensure!(
            read_manifest(&destination)? == manifest,
            "{}",
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnostic03b57d1f8364)
        );
        verify_files(&destination, &manifest)?;
        return Ok(prepared(destination, manifest, false));
    }
    let mut archive = tempfile::NamedTempFile::new_in(&runtimes)?;
    let mut response = client
        .get(format!("{base}/{}", manifest.archive))
        .send()?
        .error_for_status()?;
    std::io::copy(&mut response, &mut archive)?;
    verify_checksum(
        &digest_file(archive.path())?,
        &manifest.sha256,
        "managed runtime archive",
    )?;
    install_archive(archive.path(), &runtimes, &destination, manifest)
}

fn validate_selected_manifest(
    manifest: &Manifest,
    version: &str,
    target: &str,
    codex_commit: &str,
) -> Result<()> {
    validate_manifest(manifest, version, target)?;
    ensure!(
        manifest.codex_commit == codex_commit,
        "{}",
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnosticB8ade295b973)
    );
    Ok(())
}

fn validate_manifest(manifest: &Manifest, version: &str, target: &str) -> Result<()> {
    ensure!(
        manifest.format_version
            == super::product::resources()
                .product
                .runtime_manifest_format_version,
        "{}",
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnosticCa42f447972e)
    );
    ensure!(
        manifest.worker_version == version && manifest.target == target,
        "{}",
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnostic071b1b7a14af)
    );
    ensure!(
        manifest.codex_commit.len() == 40
            && manifest.codex_commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "{}",
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnostic095c867211ec)
    );
    ensure!(
        manifest.archive
            == super::product::resources()
                .product
                .runtime_archive_template
                .replace("{target}", target),
        "{}",
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnosticF818872960ce)
    );
    ensure!(
        manifest.sha256.len() == 64,
        "{}",
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnosticB1bf0d2003fa)
    );
    for (path, digest) in &manifest.files {
        ensure!(
            safe_relative(Path::new(path))
                && path.as_str()
                    != super::product::resources()
                        .product
                        .runtime_manifest_file
                        .as_str(),
            "{}",
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnostic16fa8e175521)
        );
        ensure!(
            digest.len() == 64,
            "{}",
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnosticD566932879ec)
        );
    }
    let platform = super::product::platform_for_target(target).context(
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnosticFadaec66212c),
    )?;
    let extension = &platform.extension;
    let metadata = &super::product::resources();
    let mut required = metadata
        .runtime
        .common_files
        .iter()
        .map(|path| path.replace("{extension}", extension))
        .collect::<Vec<_>>();
    if let Some(platform_files) = metadata.runtime.platform_files.get(platform.os.as_str()) {
        required.extend(platform_files.iter().cloned());
    }
    for path in required {
        ensure!(
            manifest.files.contains_key(&path),
            "{}",
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnostic0a67a2e96f40)
                .replace("{0}", &path)
        );
    }
    Ok(())
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn install_archive(
    archive: &Path,
    parent: &Path,
    destination: &Path,
    manifest: Manifest,
) -> Result<Prepared> {
    let staging = tempfile::tempdir_in(parent)?;
    let mut archive = tar::Archive::new(GzDecoder::new(fs::File::open(archive)?));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        ensure!(
            safe_relative(&path),
            "{}",
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnostic5d5dece9007c)
        );
        let kind = entry.header().entry_type();
        ensure!(
            kind.is_file() || kind.is_dir(),
            "{}",
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnostic275f5114aa6f)
        );
        if kind.is_file() {
            ensure!(
                manifest
                    .files
                    .contains_key(&path.to_string_lossy().replace('\\', "/")),
                "{}",
                crate::i18n::Language::default()
                    .message(crate::i18n::MessageKey::InstallDiagnostic51d161544bed)
            );
        }
        ensure!(
            entry.unpack_in(staging.path())?,
            "{}",
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnosticD65ad117f784)
        );
    }
    verify_files(staging.path(), &manifest)?;
    fs::write(
        staging.path().join(manifest_file_name()),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    fs::rename(staging.path(), destination).context(
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnosticF70950a542d7),
    )?;
    Ok(prepared(destination.to_owned(), manifest, true))
}

fn prepared(directory: PathBuf, manifest: Manifest, created: bool) -> Prepared {
    Prepared {
        executable: directory
            .join(super::binary_directory_name())
            .join(super::codex_executable_name()),
        directory,
        manifest,
        created,
    }
}

fn read_manifest(directory: &Path) -> Result<Manifest> {
    Ok(serde_json::from_slice(&fs::read(
        directory.join(manifest_file_name()),
    )?)?)
}

fn digest_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn verify_checksum(actual: &str, expected: &str, component: &str) -> Result<()> {
    ensure!(
        actual == expected,
        "{}",
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnostic98a2d72efb23)
            .replace("{0}", component)
    );
    Ok(())
}

fn verify_files(directory: &Path, manifest: &Manifest) -> Result<()> {
    ensure!(
        fs::symlink_metadata(directory)?.file_type().is_dir(),
        "{}",
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnostic3f1c3fca7c97)
    );
    for (relative, digest) in &manifest.files {
        let path = directory.join(relative);
        for parent in Path::new(relative)
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
        {
            ensure!(
                fs::symlink_metadata(directory.join(parent))?
                    .file_type()
                    .is_dir(),
                "{}",
                crate::i18n::Language::default()
                    .message(crate::i18n::MessageKey::InstallDiagnostic3f4966d2495b)
            );
        }
        ensure!(
            fs::symlink_metadata(&path)?.file_type().is_file(),
            "{}",
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnostic127aa0dc35f3)
                .replace("{0}", relative)
        );
        verify_checksum(&digest_file(&path)?, digest, relative)?;
    }
    Ok(())
}

/// Remove only files recorded by this package. Extra user files remain in place.
pub(super) fn remove_owned(directory: &Path) -> Result<()> {
    let manifest = verify_owned(directory)?;
    for relative in manifest.files.keys() {
        fs::remove_file(directory.join(relative))?;
    }
    fs::remove_file(directory.join(manifest_file_name()))?;
    let mut parents: Vec<PathBuf> = manifest
        .files
        .keys()
        .flat_map(|file| {
            Path::new(file)
                .ancestors()
                .skip(1)
                .filter(|path| !path.as_os_str().is_empty())
                .map(Path::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    parents.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    parents.dedup();
    for parent in parents {
        remove_if_empty(&directory.join(parent))?;
    }
    remove_if_empty(directory)
}

pub(super) fn verify_owned(directory: &Path) -> Result<Manifest> {
    ensure!(
        fs::symlink_metadata(directory)?.file_type().is_dir(),
        "{}",
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnostic3f1c3fca7c97)
    );
    let manifest = read_manifest(directory)?;
    validate_manifest(&manifest, &manifest.worker_version, &manifest.target)?;
    // Verify first: a modified file no longer belongs to the immutable release.
    verify_files(directory, &manifest)?;
    Ok(manifest)
}

fn remove_if_empty(path: &Path) -> Result<()> {
    if path.is_dir() && fs::read_dir(path)?.next().is_none() {
        fs::remove_dir(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(target: &str, components: &[&str]) -> Manifest {
        Manifest {
            format_version: 1,
            worker_version: "1.0.0".into(),
            codex_commit: "a".repeat(40),
            target: target.into(),
            archive: format!("discodex-runtime-{target}.tar.gz"),
            sha256: "b".repeat(64),
            files: components
                .iter()
                .map(|path| ((*path).into(), "c".repeat(64)))
                .collect(),
        }
    }

    fn linux_fixture() -> Manifest {
        fixture(
            "x86_64-unknown-linux-gnu",
            &[
                "bin/codex",
                "bin/codex-code-mode-host",
                "codex-path/rg",
                "codex-resources/bwrap",
                "codex-package.json",
            ],
        )
    }

    #[test]
    fn manifest_accepts_complete_platform_packages_and_rejects_missing_helpers() {
        let platforms = [
            linux_fixture(),
            fixture(
                "aarch64-apple-darwin",
                &[
                    "bin/codex",
                    "bin/codex-code-mode-host",
                    "codex-path/rg",
                    "codex-package.json",
                ],
            ),
            fixture(
                "x86_64-pc-windows-msvc",
                &[
                    "bin/codex.exe",
                    "bin/codex-code-mode-host.exe",
                    "codex-path/rg.exe",
                    "codex-resources/codex-command-runner.exe",
                    "codex-resources/codex-windows-sandbox-setup.exe",
                    "codex-package.json",
                ],
            ),
        ];
        for manifest in platforms {
            assert!(validate_manifest(&manifest, "1.0.0", &manifest.target).is_ok());
            for component in manifest.files.keys() {
                let mut incomplete = manifest.clone();
                incomplete.files.remove(component);
                assert!(validate_manifest(&incomplete, "1.0.0", &manifest.target).is_err());
            }
        }
    }

    #[test]
    fn manifest_rejects_wrong_worker_platform_and_invalid_checksums_without_mutation() {
        let manifest = linux_fixture();
        let before = manifest.clone();
        assert!(validate_manifest(&manifest, "2.0.0", &manifest.target).is_err());
        assert!(validate_manifest(&manifest, "1.0.0", "aarch64-apple-darwin").is_err());
        assert_eq!(manifest, before);

        let mut invalid_archive = manifest.clone();
        invalid_archive.sha256 = "short".into();
        assert!(validate_manifest(&invalid_archive, "1.0.0", &manifest.target).is_err());
        let mut invalid_component = manifest.clone();
        invalid_component
            .files
            .insert("bin/codex".into(), "short".into());
        assert!(validate_manifest(&invalid_component, "1.0.0", &manifest.target).is_err());
    }

    #[test]
    fn selected_runtime_requires_the_workers_exact_core_commit_without_mutation() {
        let manifest = linux_fixture();
        let before = manifest.clone();
        let expected_commit = manifest.codex_commit.clone();
        assert!(
            validate_selected_manifest(&manifest, "1.0.0", &manifest.target, &expected_commit)
                .is_ok()
        );
        for (version, target, commit) in [
            ("2.0.0", manifest.target.as_str(), expected_commit.clone()),
            ("1.0.0", "aarch64-apple-darwin", expected_commit.clone()),
            ("1.0.0", manifest.target.as_str(), "d".repeat(40)),
        ] {
            assert!(validate_selected_manifest(&manifest, version, target, &commit).is_err());
            assert_eq!(manifest, before);
        }

        let mut other_revision = manifest.clone();
        other_revision.codex_commit = "d".repeat(40);
        let other_before = other_revision.clone();
        assert_eq!(
            validate_selected_manifest(
                &other_revision,
                "1.0.0",
                &manifest.target,
                &expected_commit,
            )
            .unwrap_err()
            .to_string(),
            "managed runtime Codex commit differs from this worker's pinned core"
        );
        // Ownership checks must still recognize an older runtime for rollback
        // and cleanup; only preparing the new runtime binds it to this worker.
        assert!(validate_manifest(&other_revision, "1.0.0", &manifest.target).is_ok());
        assert_eq!(other_revision, other_before);
        assert_eq!(manifest, before);
    }

    #[test]
    fn manifest_only_owns_package_relative_paths() {
        let manifest = linux_fixture();
        for path in [
            "../shared/config.toml",
            "/outside/session.jsonl",
            manifest_file_name(),
        ] {
            let mut invalid = manifest.clone();
            invalid.files.insert(path.into(), "c".repeat(64));
            assert!(validate_manifest(&invalid, "1.0.0", &manifest.target).is_err());
        }
        let before = manifest.clone();
        validate_manifest(&manifest, "1.0.0", &manifest.target).unwrap();
        assert_eq!(manifest, before);
    }

    #[test]
    fn checksum_comparison_rejects_changed_package_content() {
        let expected = "a".repeat(64);
        assert!(verify_checksum(&expected, &expected, "bin/codex").is_ok());
        assert_eq!(
            verify_checksum(&"b".repeat(64), &expected, "bin/codex")
                .unwrap_err()
                .to_string(),
            "bin/codex checksum differs from its release manifest"
        );
        assert_eq!(expected, "a".repeat(64));
    }

    #[test]
    fn runtime_selection_changes_only_owned_runtime_fields() {
        use super::super::{Ownership, apply_runtime_selection};
        use crate::config::Config;
        use serde_json::json;

        for home in [
            json!(null),
            json!("external home/shared"),
            json!("profile/separate-home"),
        ] {
            let mut config: Config = serde_json::from_value(json!({
                "language": "ko", "profile": "sample", "name": "Sample",
                "tokenEnv": "TEST_TOKEN", "credentialsFile": "profile/discord-token",
                "guildId": 1, "homeChannelId": 2, "transcriptChannelId": 3,
                "operatorIds": [4, 5], "voiceChannelIds": [6, 7],
                "codexPath": "external/core", "codexHome": home,
                "workerPath": "owned/worker", "projectDir": "arbitrary/project",
                "stateDir": "existing/session-state", "model": "custom-model",
                "voice": "cedar", "effort": "ultra", "serviceTier": "off",
                "memoryEnabled": true, "permissions": "full-access", "inputMode": "nextTurn"
            }))
            .unwrap();
            let mut owner: Ownership = serde_json::from_value(json!({
                "formatVersion": 1, "profile": "sample", "configFile": "profile/config.json",
                "credentialsFile": "profile/discord-token", "installListFile": "profile/installation-list.json",
                "runtimeDir": "external/runtime", "runtimeExecutable": null,
                "workerExecutable": "owned/worker", "serviceFile": "owned/service",
                "serviceKind": "original-service", "codexSource": "desktop-app",
                "codexPath": "external/core", "codexVersion": null,
                "stateDir": "existing/session-state", "projectDir": "arbitrary/project",
                "macSigningRequirement": "original-signature"
            })).unwrap();
            let mut expected_config = serde_json::to_value(&config).unwrap();
            let mut expected_owner = serde_json::to_value(&owner).unwrap();
            expected_config["codexPath"] = json!("managed/new/bin/core");
            expected_owner["codexPath"] = json!("managed/new/bin/core");
            expected_owner["runtimeExecutable"] = json!("managed/new/bin/core");
            expected_owner["runtimeDir"] = json!("managed/new");
            expected_owner["runtimeOwned"] = json!(true);
            expected_owner["codexSource"] = json!("discodex-core-owned");
            expected_owner["codexVersion"] = json!("a".repeat(40));
            let runtime = Prepared {
                directory: "managed/new".into(),
                executable: "managed/new/bin/core".into(),
                manifest: linux_fixture(),
                created: false,
            };

            apply_runtime_selection(&mut owner, &mut config, &runtime);

            assert_eq!(serde_json::to_value(config).unwrap(), expected_config);
            assert_eq!(serde_json::to_value(owner).unwrap(), expected_owner);
        }
    }

    #[test]
    fn rollback_inputs_keep_original_profile_bytes_and_destinations() {
        use super::super::ProfileRuntimeSnapshot;

        let config = b" {\"codexHome\": \"shared\", \"preserved\": true}\n".to_vec();
        let ownership = b"{\"runtimeDir\":\"previous\",\"extension\":{\"a\":1}}\r\n".to_vec();
        let snapshot = ProfileRuntimeSnapshot {
            config_path: "original profile/config.json".into(),
            config: config.clone(),
            ownership_path: "original profile/ownership.json".into(),
            ownership: ownership.clone(),
        };
        assert_eq!(snapshot.restore_inputs(false), None);
        let restored = snapshot.restore_inputs(true).unwrap();
        assert_eq!(
            restored,
            [
                (Path::new("original profile/config.json"), config.as_slice()),
                (
                    Path::new("original profile/ownership.json"),
                    ownership.as_slice()
                ),
            ]
        );
        assert_eq!(snapshot.config, config);
        assert_eq!(snapshot.ownership, ownership);
    }
}

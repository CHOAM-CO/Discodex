use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::i18n::Language;

const OWNERSHIP_FILE: &str = ".discodex-owned.json";

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Ownership {
    worker_version: String,
    language: Language,
    files: BTreeMap<String, String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SyncResult {
    written: Vec<PathBuf>,
    preserved: Vec<PathBuf>,
}

/// Optional help content must not prevent the Discord bridge from starting.
pub(crate) fn sync_for_profile(config: &Config) {
    match sync(&config.project_dir, config.language) {
        Ok(result) if !result.preserved.is_empty() => tracing::warn!(
            paths = ?result.preserved,
            "Preserved existing or modified Discodex skill files"
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(
            error = %format!("{error:#}"),
            "Could not synchronize the bundled Discodex skill"
        ),
    }
}

fn skill_directory(project: &Path) -> PathBuf {
    project.join(".agents").join("skills").join("discodex")
}

fn bundled_files(language: Language) -> BTreeMap<String, String> {
    let skill = language.text(
        include_str!("../skills/discodex/SKILL.md"),
        include_str!("../skills/discodex/SKILL.ko.md"),
        include_str!("../skills/discodex/SKILL.zh-CN.md"),
        include_str!("../skills/discodex/SKILL.ja.md"),
    );
    let mut files = BTreeMap::from([(
        "SKILL.md".to_owned(),
        skill.replace("{{version}}", env!("CARGO_PKG_VERSION")),
    )]);
    // Preserve the documentation's filenames and cross-language navigation.
    // Only the chosen language's entrypoint is exposed in the skills catalog.
    for (name, body) in [
        ("design.md", include_str!("../docs/design.md")),
        ("design.ko.md", include_str!("../docs/design.ko.md")),
        ("design.zh-CN.md", include_str!("../docs/design.zh-CN.md")),
        ("design.ja.md", include_str!("../docs/design.ja.md")),
        ("installation.md", include_str!("../docs/installation.md")),
        (
            "installation.ko.md",
            include_str!("../docs/installation.ko.md"),
        ),
        (
            "installation.zh-CN.md",
            include_str!("../docs/installation.zh-CN.md"),
        ),
        (
            "installation.ja.md",
            include_str!("../docs/installation.ja.md"),
        ),
    ] {
        files.insert(format!("references/{name}"), body.to_owned());
    }
    files
}

fn sync(project: &Path, language: Language) -> Result<SyncResult> {
    sync_files(project, language, bundled_files(language))
}

fn sync_files(
    project: &Path,
    language: Language,
    files: BTreeMap<String, String>,
) -> Result<SyncResult> {
    let root = skill_directory(project);
    let ownership_path = root.join(OWNERSHIP_FILE);
    let mut result = SyncResult::default();
    let mut ownership = match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            match fs::symlink_metadata(&ownership_path) {
                Ok(metadata) if metadata.file_type().is_file() => {
                    serde_json::from_slice::<Ownership>(&fs::read(&ownership_path)?).with_context(
                        || format!("read skill ownership {}", ownership_path.display()),
                    )?
                }
                Ok(_) => {
                    result.preserved.push(root);
                    return Ok(result);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    result.preserved.push(root);
                    return Ok(result);
                }
                Err(error) => return Err(error).context("inspect skill ownership"),
            }
        }
        Ok(_) => {
            result.preserved.push(root);
            return Ok(result);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(root.parent().context("skill has no parent")?)?;
            fs::create_dir(&root).with_context(|| format!("create skill {}", root.display()))?;
            let ownership = Ownership::default();
            write_atomic(&ownership_path, &serde_json::to_vec_pretty(&ownership)?)?;
            ownership
        }
        Err(error) => return Err(error).context("inspect skill directory"),
    };

    for (name, body) in files {
        let path = root.join(&name);
        let previous_hash = ownership.files.get(&name);
        let current = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => Some(fs::read(&path)?),
            Ok(_) => {
                result.preserved.push(path);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("inspect bundled skill file"),
        };
        if let Some(current) = current.as_deref() {
            if current == body.as_bytes() {
                // A prior sync may have written the file before its ownership
                // update completed. Identical bundled bytes need no replacement.
                ownership.files.insert(name, digest(current));
                continue;
            }
            if previous_hash.is_none_or(|previous| previous != &digest(current)) {
                result.preserved.push(path);
                continue;
            }
        }
        let parent = path.parent().context("skill file has no parent")?;
        fs::create_dir_all(parent)?;
        write_atomic(&path, body.as_bytes())?;
        ownership.files.insert(name, digest(body.as_bytes()));
        // Record each completed write so an interrupted sync can be retried.
        write_atomic(&ownership_path, &serde_json::to_vec_pretty(&ownership)?)?;
        result.written.push(path);
    }
    ownership.worker_version = env!("CARGO_PKG_VERSION").to_owned();
    ownership.language = language;
    let ownership_bytes = serde_json::to_vec_pretty(&ownership)?;
    if fs::read(&ownership_path)? != ownership_bytes {
        write_atomic(&ownership_path, &ownership_bytes)?;
    }
    Ok(result)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("skill file has no parent directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace bundled skill file {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_sync_reports_all_bundled_files_for_the_selected_language() {
        for language in [
            Language::English,
            Language::Korean,
            Language::Chinese,
            Language::Japanese,
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let expected = bundled_files(language);
            let result = sync(temporary.path(), language).unwrap();
            assert!(result.preserved.is_empty());
            assert_eq!(result.written.len(), expected.len());
            let root = skill_directory(temporary.path());
            let reported_names = result
                .written
                .iter()
                .map(|path| {
                    path.strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<std::collections::BTreeSet<_>>();
            let bundled_names = expected
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(reported_names, bundled_names);
        }
    }

    #[test]
    fn unchanged_skill_is_not_rewritten() {
        let temporary = tempfile::tempdir().unwrap();
        sync(temporary.path(), Language::English).unwrap();
        assert_eq!(
            sync(temporary.path(), Language::English).unwrap(),
            SyncResult::default()
        );
    }

    #[test]
    fn language_change_reports_only_the_owned_entrypoint_for_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let _other = temporary.path().join(".agents/skills/my-skill/SKILL.md");
        fs::create_dir_all(_other.parent().unwrap()).unwrap();
        fs::write(&_other, "user skill").unwrap();
        sync(temporary.path(), Language::English).unwrap();

        let result = sync(temporary.path(), Language::Japanese).unwrap();
        assert_eq!(
            result.written,
            vec![skill_directory(temporary.path()).join("SKILL.md")]
        );
        assert!(result.preserved.is_empty());
    }

    #[test]
    fn an_existing_user_skill_is_reported_as_preserved() {
        let temporary = tempfile::tempdir().unwrap();
        let root = skill_directory(temporary.path());
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("SKILL.md"), "user's Discodex help").unwrap();

        let result = sync(temporary.path(), Language::English).unwrap();
        assert_eq!(result.preserved, vec![root.clone()]);
        assert!(result.written.is_empty());
    }

    #[test]
    fn update_reports_user_edits_as_preserved_and_unchanged_owned_files_as_written() {
        let temporary = tempfile::tempdir().unwrap();
        let original = BTreeMap::from([
            ("SKILL.md".to_owned(), "old skill".to_owned()),
            ("references/design.md".to_owned(), "old design".to_owned()),
        ]);
        sync_files(temporary.path(), Language::English, original).unwrap();
        let root = skill_directory(temporary.path());
        fs::write(root.join("SKILL.md"), "custom skill").unwrap();
        fs::write(root.join("notes.txt"), "personal notes").unwrap();
        let updated = BTreeMap::from([
            ("SKILL.md".to_owned(), "new skill".to_owned()),
            ("references/design.md".to_owned(), "new design".to_owned()),
        ]);

        let result = sync_files(temporary.path(), Language::English, updated).unwrap();
        assert_eq!(result.preserved, vec![root.join("SKILL.md")]);
        assert_eq!(result.written, vec![root.join("references/design.md")]);
    }

    #[test]
    fn sync_reports_missing_owned_file_as_written() {
        let temporary = tempfile::tempdir().unwrap();
        sync(temporary.path(), Language::Korean).unwrap();
        let root = skill_directory(temporary.path());
        let deleted = root.join("references/design.ko.md");
        fs::remove_file(&deleted).unwrap();

        let result = sync(temporary.path(), Language::Korean).unwrap();
        assert_eq!(result.written, vec![deleted.clone()]);
    }

    #[test]
    fn sync_reports_reconciled_ownership_as_unchanged() {
        let temporary = tempfile::tempdir().unwrap();
        let root = skill_directory(temporary.path());
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(OWNERSHIP_FILE),
            serde_json::to_vec(&Ownership::default()).unwrap(),
        )
        .unwrap();
        fs::write(root.join("SKILL.md"), "installed bytes").unwrap();

        let result = sync_files(
            temporary.path(),
            Language::English,
            BTreeMap::from([("SKILL.md".to_owned(), "installed bytes".to_owned())]),
        )
        .unwrap();
        assert_eq!(result, SyncResult::default());
    }

    #[cfg(unix)]
    #[test]
    fn a_user_skill_symlink_is_reported_as_preserved() {
        let temporary = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let root = skill_directory(temporary.path());
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::write(user.path().join("SKILL.md"), "linked user skill").unwrap();
        std::os::unix::fs::symlink(user.path(), &root).unwrap();

        let result = sync(temporary.path(), Language::English).unwrap();
        assert_eq!(result.preserved, vec![root]);
    }
}

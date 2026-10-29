use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use tokio::io::AsyncReadExt;
use url::Url;

use crate::config::Config;
use crate::i18n::MessageKey;

// Discord create-message requests are limited to 25 MiB. Keep each individual
// read bounded by the largest possible request; callers batch complete messages.
// https://docs.discord.com/developers/resources/message#create-message
const MAX_REQUEST_BYTES: u64 = 25 * 1024 * 1024;
const MAX_FILES_PER_REQUEST: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReplyFile {
    pub(crate) filename: String,
    pub(crate) data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedReply {
    pub(crate) content: String,
    pub(crate) files: Vec<ReplyFile>,
}

/// Split prepared files into Discord create-message request batches.
/// Each request contains at most 10 files and 25 MiB of file payload.
pub(crate) fn attachment_batches(files: &[ReplyFile]) -> Vec<Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut total_bytes = 0_u64;

    for (index, file) in files.iter().enumerate() {
        let file_bytes = file.data.len() as u64;
        let full = index - start >= MAX_FILES_PER_REQUEST
            || total_bytes.saturating_add(file_bytes) > MAX_REQUEST_BYTES;
        if full {
            batches.push(start..index);
            start = index;
            total_bytes = 0;
        }
        total_bytes = total_bytes.saturating_add(file_bytes);
    }
    if start < files.len() {
        batches.push(start..files.len());
    }
    batches
}

#[derive(Clone, Debug)]
struct LocalLink {
    range: Range<usize>,
    path: PathBuf,
    filename: String,
}

#[derive(Clone, Debug)]
struct ReplyPlan {
    text: String,
    links: Vec<LocalLink>,
    reference_definition_ranges: Vec<Range<usize>>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum FileFailure {
    InvalidPath,
    Io(String),
    NotRegularFile,
    RequestTooLarge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LinkOutcome {
    File(usize),
    Failed(FileFailure),
}

#[derive(Default)]
struct ReplyAccumulator {
    files: Vec<ReplyFile>,
    seen: HashMap<PathBuf, LinkOutcome>,
}

impl ReplyAccumulator {
    fn previous(&self, key: &Path) -> Option<LinkOutcome> {
        self.seen.get(key).cloned()
    }

    fn accept(
        &mut self,
        key: PathBuf,
        result: std::result::Result<ReplyFile, FileFailure>,
    ) -> LinkOutcome {
        if let Some(previous) = self.seen.get(&key) {
            return previous.clone();
        }
        let outcome = match result {
            Err(failure) => LinkOutcome::Failed(failure),
            Ok(file) => {
                let index = self.files.len();
                self.files.push(file);
                LinkOutcome::File(index)
            }
        };
        self.seen.insert(key, outcome.clone());
        outcome
    }
}

fn plan_reply(text: &str, project_dir: &Path) -> ReplyPlan {
    let parser = Parser::new(text);
    let reference_definition_ranges = parser
        .reference_definitions()
        .iter()
        .filter_map(|(_, definition)| {
            local_link_path(&definition.dest, project_dir).map(|_| definition.span.clone())
        })
        .collect();
    let mut links = Vec::new();
    let mut pending: Option<(usize, String)> = None;

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                pending = Some((range.start, dest_url.into_string()));
            }
            Event::End(TagEnd::Link) => {
                if let Some((start, destination)) = pending.take() {
                    if let Some(path) = local_link_path(&destination, project_dir) {
                        let end = range.end.max(start);
                        if start <= end && end <= text.len() {
                            links.push(LocalLink {
                                range: start..end,
                                filename: safe_basename(&path),
                                path,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    links.sort_by_key(|link| link.range.start);
    ReplyPlan {
        text: text.to_owned(),
        links,
        reference_definition_ranges,
    }
}

fn local_link_path(destination: &str, project_dir: &Path) -> Option<PathBuf> {
    if is_windows_drive_path(destination) {
        let decoded = percent_encoding::percent_decode_str(destination)
            .decode_utf8()
            .ok()?;
        return Some(PathBuf::from(decoded.as_ref()));
    }
    if destination.starts_with("//") || has_uri_scheme(destination) && !is_file_scheme(destination)
    {
        return None;
    }

    if is_file_scheme(destination) {
        let mut url = Url::parse(destination).ok()?;
        url.set_fragment(None);
        url.set_query(None);
        return url.to_file_path().ok();
    }

    let path_text = destination
        .split_once('#')
        .map_or(destination, |(path, _)| path);
    if path_text.is_empty() {
        return None;
    }
    let decoded = percent_encoding::percent_decode_str(path_text)
        .decode_utf8()
        .ok()?;
    let path = PathBuf::from(decoded.as_ref());
    Some(if path.is_absolute() {
        path
    } else {
        project_dir.join(path)
    })
}

fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn has_uri_scheme(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '.' | '-'))
}

fn is_file_scheme(value: &str) -> bool {
    value
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file:"))
}

fn safe_basename(path: &Path) -> String {
    let value = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let value = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    if value.is_empty() || value == "." || value == ".." {
        "attachment".to_owned()
    } else {
        value
    }
}

fn display_filename(filename: &str) -> String {
    let mut escaped = String::new();
    for ch in filename.chars() {
        if ch == '@' {
            escaped.push('@');
            escaped.push('\u{200b}');
        } else {
            if matches!(
                ch,
                '`' | '\\' | '*' | '_' | '~' | '|' | '[' | ']' | '<' | '>'
            ) {
                escaped.push('\\');
            }
            escaped.push(ch);
        }
    }
    format!("`{escaped}`")
}

fn absolute_candidate(config: &Config, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        config.project_dir.join(path)
    }
}

async fn canonical_candidate(
    config: &Config,
    path: &Path,
) -> std::result::Result<PathBuf, FileFailure> {
    let candidate = absolute_candidate(config, path);
    let canonical = tokio::fs::canonicalize(&candidate)
        .await
        .map_err(map_io_failure)?;
    Ok(canonical)
}

async fn read_canonical_file(
    canonical: &Path,
    filename: &str,
) -> std::result::Result<ReplyFile, FileFailure> {
    // Reject devices and FIFOs before opening: opening a pipe can block even
    // though it would fail the regular-file check below.
    let path_metadata = tokio::fs::metadata(canonical)
        .await
        .map_err(map_io_failure)?;
    if !path_metadata.is_file() {
        return Err(FileFailure::NotRegularFile);
    }
    let file = tokio::fs::File::open(canonical)
        .await
        .map_err(map_io_failure)?;
    let metadata = file.metadata().await.map_err(map_io_failure)?;
    if !metadata.is_file() {
        return Err(FileFailure::NotRegularFile);
    }
    if metadata.len() > MAX_REQUEST_BYTES {
        return Err(FileFailure::RequestTooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(map_io_failure)?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(FileFailure::RequestTooLarge);
    }
    Ok(ReplyFile {
        filename: safe_upload_filename(filename),
        data: bytes,
    })
}

fn map_io_failure(error: std::io::Error) -> FileFailure {
    let message = error.to_string();
    let detail = match error.raw_os_error() {
        Some(code) => format!("{} (OS error {code})", message),
        None => format!("{:?}: {message}", error.kind()),
    };
    FileFailure::Io(detail)
}

fn safe_upload_filename(filename: &str) -> String {
    safe_basename(Path::new(filename))
}

fn failure_text(config: &Config, filename: &str, failure: FileFailure) -> String {
    let language = config.language;
    let reason = match failure {
        FileFailure::InvalidPath => language
            .message(MessageKey::ReplyFilesInvalidPath)
            .to_owned(),
        FileFailure::Io(detail) => language.message(MessageKey::ReplyFilesIoFailure).replace(
            "{detail}",
            &crate::discord::sanitize_discord_error(config, &detail),
        ),
        FileFailure::NotRegularFile => language
            .message(MessageKey::ReplyFilesNotRegular)
            .to_owned(),
        FileFailure::RequestTooLarge => language.message(MessageKey::ReplyFilesTooLarge).to_owned(),
    };
    let safe = display_filename(filename);
    language
        .message(MessageKey::ReplyFilesAttachFailed)
        .replace("{reason}", &reason)
        .replace("{name}", &safe)
}

fn finish_reply(
    plan: ReplyPlan,
    outcomes: &[LinkOutcome],
    files: Vec<ReplyFile>,
    config: &Config,
) -> PreparedReply {
    let mut content = String::new();
    let mut cursor = 0;
    let mut failures = Vec::new();
    let mut replacements = plan
        .links
        .iter()
        .enumerate()
        .map(|(index, link)| (link.range.clone(), Some(index)))
        .chain(
            plan.reference_definition_ranges
                .iter()
                .cloned()
                .map(|range| (range, None)),
        )
        .collect::<Vec<_>>();
    replacements.sort_by_key(|(range, _)| range.start);
    for (range, link_index) in replacements {
        if range.start < cursor || range.end > plan.text.len() {
            continue;
        }
        content.push_str(&plan.text[cursor..range.start]);
        if let Some(index) = link_index {
            let Some(link) = plan.links.get(index) else {
                continue;
            };
            content.push_str(&display_filename(&link.filename));
            if let Some(LinkOutcome::Failed(reason)) = outcomes.get(index) {
                let item = failure_text(config, &link.filename, reason.clone());
                if !failures.contains(&item) {
                    failures.push(item);
                }
            }
        }
        cursor = range.end;
    }
    content.push_str(&plan.text[cursor..]);
    if !failures.is_empty() {
        content.push_str("\n\n");
        content.push_str(&failures.join("\n"));
    }
    PreparedReply { content, files }
}

/// Prepare explicit local paths (for a user-requested file send). The operation
/// is all-or-nothing and applies no file-type or path-based policy.
pub(crate) async fn prepare_explicit_files(
    config: &Config,
    paths: &[String],
) -> Result<Vec<ReplyFile>> {
    let mut accumulator = ReplyAccumulator::default();

    for supplied in paths {
        let Some(path) = explicit_path(supplied, &config.project_dir) else {
            bail!(
                "{}",
                failure_text(
                    config,
                    &safe_basename(Path::new(supplied)),
                    FileFailure::InvalidPath
                )
            );
        };
        let name = safe_basename(&path);
        let canonical = canonical_candidate(config, &path)
            .await
            .map_err(|issue| anyhow::anyhow!("{}", failure_text(config, &name, issue)))?;
        if accumulator.previous(&canonical).is_some() {
            continue;
        }
        let result = read_canonical_file(&canonical, &name).await;
        match accumulator.accept(canonical, result) {
            LinkOutcome::File(_) => {}
            LinkOutcome::Failed(issue) => {
                bail!("{}", failure_text(config, &name, issue));
            }
        }
    }
    Ok(accumulator.files)
}

fn explicit_path(value: &str, project_dir: &Path) -> Option<PathBuf> {
    if is_file_scheme(value) {
        return local_link_path(value, project_dir);
    }
    if !is_windows_drive_path(value) && has_uri_scheme(value) {
        return None;
    }
    let path = PathBuf::from(value);
    Some(if path.is_absolute() {
        path
    } else {
        project_dir.join(path)
    })
}

/// Prepare a Markdown reply, converting explicitly linked local files into
/// Discord uploads. Ordinary prose, code spans, plain paths, and web links stay
/// untouched. Files are only read when Markdown explicitly links them.
pub(crate) async fn prepare_reply(config: &Config, text: &str) -> PreparedReply {
    let plan = plan_reply(text, &config.project_dir);
    let mut accumulator = ReplyAccumulator::default();
    let mut outcomes = Vec::with_capacity(plan.links.len());

    for link in &plan.links {
        let candidate = absolute_candidate(config, &link.path);
        let lexical_key = candidate.clone();
        match canonical_candidate(config, &link.path).await {
            Err(issue) => {
                let outcome = accumulator.accept(lexical_key, Err(issue));
                outcomes.push(outcome);
            }
            Ok(canonical) => {
                if let Some(outcome) = accumulator.previous(&canonical) {
                    outcomes.push(outcome);
                    continue;
                }
                let name = link.filename.clone();
                let result = read_canonical_file(&canonical, &name).await;
                let outcome = accumulator.accept(canonical, result);
                outcomes.push(outcome);
            }
        }
    }
    finish_reply(plan, &outcomes, accumulator.files, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;

    fn test_config(language: Language) -> Config {
        let value = serde_json::json!({
            "profile":"example", "name":"Example", "tokenEnv":"EXAMPLE_TOKEN",
            "guildId":1, "homeChannelId":2, "transcriptChannelId":3,
            "codexPath":"codex", "codexHome":"/tmp/codex",
            "projectDir":"/tmp/project", "stateDir":"/tmp/state",
            "credentialsFile":"/tmp/discord-token", "language":language
        });
        serde_json::from_value(value).unwrap()
    }

    #[tokio::test]
    async fn explicit_and_markdown_attachments_accept_all_readable_file_paths() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let home = root.join("codex");
        tokio::fs::create_dir_all(home.join("generated"))
            .await
            .unwrap();
        let config: Config = serde_json::from_value(serde_json::json!({
            "profile":"example", "name":"Example", "tokenEnv":"EXAMPLE_TOKEN",
            "guildId":1, "homeChannelId":2, "transcriptChannelId":3,
            "codexPath":"codex", "codexHome":home,
            "projectDir":root.join("project"), "stateDir":root.join("state"),
            "credentialsFile":root.join("discord-token")
        }))
        .unwrap();
        let paths = [
            home.join("auth.json"),
            home.join("config.toml"),
            root.join("discord-token"),
            home.join("generated").join("result.png"),
            root.join("outside-project.txt"),
        ];
        let data = b"synthetic attachment bytes";
        for path in &paths {
            tokio::fs::write(path, data).await.unwrap();
        }
        let supplied = paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let explicit = prepare_explicit_files(&config, &supplied).await.unwrap();
        assert_eq!(explicit.len(), paths.len());
        for (file, path) in explicit.iter().zip(&paths) {
            assert_eq!(file.filename, path.file_name().unwrap().to_str().unwrap());
            assert_eq!(file.data, data);
        }

        let markdown = paths
            .iter()
            .enumerate()
            .map(|(index, path)| format!("[file {index}]({})", Url::from_file_path(path).unwrap()))
            .collect::<Vec<_>>()
            .join(" ");
        let reply = prepare_reply(&config, &markdown).await;
        assert_eq!(reply.files, explicit);
    }

    #[test]
    fn upload_filename_preserves_the_complete_basename() {
        let filename = format!("{}.txt", "a".repeat(250));
        assert_eq!(safe_upload_filename(&filename), filename);
    }

    #[test]
    fn io_failure_keeps_os_cause_and_redacts_only_secret_values() {
        let failure = map_io_failure(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "permission denied; api_key=synthetic-secret",
        ));
        assert_eq!(
            failure,
            FileFailure::Io(
                "PermissionDenied: permission denied; api_key=synthetic-secret".to_owned()
            )
        );
        let message = failure_text(&test_config(Language::English), "report.txt", failure);
        assert!(message.contains("permission denied"));
        assert!(message.contains("PermissionDenied"));
        assert!(message.contains("[secret redacted]"));
        assert!(!message.contains("synthetic-secret"));
    }

    #[test]
    fn io_failure_keeps_available_os_error_code() {
        let failure = map_io_failure(std::io::Error::from_raw_os_error(13));
        assert!(matches!(failure, FileFailure::Io(detail) if detail.ends_with("(OS error 13)")));
    }

    #[test]
    fn markdown_planning_recognizes_local_link_forms_and_preserves_nonlinks() {
        let project = Path::new("project");
        let local_absolute = PathBuf::from(std::env::temp_dir()).join("reply file.pdf");
        let file_url = Url::from_file_path(&local_absolute).unwrap().to_string();
        let input = format!(
            "[relative](docs/a%20b.txt) [absolute](<docs/another file.png>) [file URL]({file_url}) [ref][r] [web](https://example.org/x) `[ignored](docs/code.txt)` docs/plain.txt [source](src/main.rs#L12)\n\n[r]: docs/ref.md"
        );
        let plan = plan_reply(&input, project);

        assert_eq!(plan.links.len(), 5);
        assert_eq!(plan.links[0].path, Path::new("project/docs/a b.txt"));
        assert_eq!(plan.links[0].filename, "a b.txt");
        assert_eq!(plan.links[1].filename, "another file.png");
        assert_eq!(plan.links[2].filename, "reply file.pdf");
        assert_eq!(plan.links[3].path, Path::new("project/docs/ref.md"));
        assert_eq!(plan.links[4].path, Path::new("project/src/main.rs"));
        assert_eq!(plan.reference_definition_ranges.len(), 1);
        let rendered = finish_reply(
            plan,
            &[
                LinkOutcome::File(0),
                LinkOutcome::File(1),
                LinkOutcome::File(2),
                LinkOutcome::File(3),
                LinkOutcome::File(4),
            ],
            Vec::new(),
            &test_config(Language::English),
        );
        assert!(!rendered.content.contains("[r]: docs/ref.md"));
        assert!(rendered.content.contains("`ref.md`"));
    }

    #[test]
    fn reply_replaces_links_with_filenames_and_deduplicates_injected_read_result() {
        let plan = plan_reply(
            "Please read [the report](docs/report.pdf) and [same file](docs/report.pdf). Keep https://example.org and `docs/report.pdf`.",
            Path::new("project"),
        );
        let mut accumulator = ReplyAccumulator::default();
        let file = ReplyFile {
            filename: "report.pdf".to_owned(),
            data: b"test bytes".to_vec(),
        };
        let outcomes = [
            accumulator.accept(PathBuf::from("canonical/report.pdf"), Ok(file.clone())),
            accumulator.accept(PathBuf::from("canonical/report.pdf"), Ok(file)),
        ];
        assert_eq!(outcomes, [LinkOutcome::File(0), LinkOutcome::File(0)]);
        let files = accumulator.files;
        let prepared = finish_reply(
            plan,
            &outcomes,
            files.clone(),
            &test_config(Language::English),
        );

        assert_eq!(prepared.files, files);
        assert!(prepared.content.contains("`report.pdf`"));
        assert!(prepared.content.contains("https://example.org"));
        assert!(prepared.content.contains("`docs/report.pdf`"));
        assert!(!prepared.content.contains("[the report](docs/report.pdf)"));
        assert!(!prepared.content.contains("/project"));
    }

    #[test]
    fn distinct_paths_with_same_basename_keep_original_upload_names() {
        let mut accumulator = ReplyAccumulator::default();
        for (path, filename) in [
            ("a/report.txt", "report.txt"),
            ("b/report.txt", "report.txt"),
            ("c/report-2.txt", "report-2.txt"),
            ("d/report.txt", "report.txt"),
        ] {
            let result = accumulator.accept(
                PathBuf::from(path),
                Ok(ReplyFile {
                    filename: filename.to_owned(),
                    data: vec![b'x'],
                }),
            );
            assert!(matches!(result, LinkOutcome::File(_)));
        }
        assert_eq!(
            accumulator
                .files
                .iter()
                .map(|file| file.filename.as_str())
                .collect::<Vec<_>>(),
            ["report.txt", "report.txt", "report-2.txt", "report.txt"]
        );
    }

    #[test]
    fn attachment_batches_follow_discord_file_and_payload_limits() {
        let mut files = (0..11)
            .map(|index| ReplyFile {
                filename: format!("file-{index}.txt"),
                data: vec![b'x'],
            })
            .collect::<Vec<_>>();
        files[10].data = vec![b'x'; (MAX_REQUEST_BYTES / 2) as usize];
        files.push(ReplyFile {
            filename: "large.txt".to_owned(),
            data: vec![b'x'; (MAX_REQUEST_BYTES / 2) as usize + 1],
        });

        let batches = attachment_batches(&files);
        assert_eq!(batches, [0..10, 10..11, 11..12]);
        for batch in batches {
            assert!(batch.len() <= MAX_FILES_PER_REQUEST);
            assert!(
                files[batch.clone()]
                    .iter()
                    .map(|file| file.data.len() as u64)
                    .sum::<u64>()
                    <= MAX_REQUEST_BYTES
            );
        }
    }

    #[test]
    fn failed_read_preserves_os_reason_and_safe_filename_in_selected_language() {
        let plan = plan_reply(
            "See [private name](<docs/missing file.txt>).",
            Path::new("private-root"),
        );
        let prepared = finish_reply(
            plan,
            &[LinkOutcome::Failed(FileFailure::Io(
                "No such file or directory (OS error 2)".to_owned(),
            ))],
            Vec::new(),
            &test_config(Language::Korean),
        );

        assert!(prepared.content.contains("missing file.txt"));
        assert!(prepared.content.contains("첨부할 수 없습니다"));
        assert!(
            prepared
                .content
                .contains("No such file or directory (OS error 2)")
        );
        assert!(!prepared.content.contains("private-root"));
        assert!(prepared.files.is_empty());
    }
    #[test]
    fn windows_drive_paths_are_not_mistaken_for_uri_schemes() {
        assert!(is_windows_drive_path("C:/Users/Example/file.txt"));
        assert!(is_windows_drive_path("D:\\work\\file.txt"));
        assert!(!is_windows_drive_path("https://example.org/file.txt"));
        assert!(explicit_path("C:/Users/Example/file.txt", Path::new("project")).is_some());
    }

    #[test]
    fn explicit_network_paths_and_local_link_fragments_follow_native_path_parsing() {
        assert_eq!(
            explicit_path("//example/share/report.txt", Path::new("project")),
            Some(PathBuf::from("//example/share/report.txt"))
        );
        let file = std::env::temp_dir().join("example.rs");
        let mut url = Url::from_file_path(&file).unwrap();
        url.set_fragment(Some("L42"));
        assert_eq!(
            local_link_path(url.as_str(), Path::new("project")),
            Some(file)
        );
        assert_eq!(
            local_link_path("//example.org/report.pdf", Path::new("project")),
            None
        );
    }

    #[test]
    fn reply_accumulator_accepts_more_than_ten_files() {
        let mut accumulator = ReplyAccumulator::default();
        for index in 0..12 {
            let result = accumulator.accept(
                PathBuf::from(format!("canonical/{index}")),
                Ok(ReplyFile {
                    filename: format!("file-{index}.txt"),
                    data: vec![b'x'],
                }),
            );
            assert_eq!(result, LinkOutcome::File(index));
        }
        assert_eq!(accumulator.files.len(), 12);
    }
}

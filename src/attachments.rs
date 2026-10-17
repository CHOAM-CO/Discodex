use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use serenity::all::{Attachment, Message};
use tokio::{fs::File, io::AsyncWriteExt};
use uuid::Uuid;

use crate::config::Config;

const ATTACHMENT_MANIFEST_MARKER: &str = "\n\nAttachment manifest:\n";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DownloadedAttachment {
    pub filename: String,
    pub path: PathBuf,
    pub is_image: bool,
}

/// Return all user-authored text and attachments carried by a Discord message,
/// including forwarded-message snapshots. Serenity already deserializes snapshots
/// from the gateway payload, so no additional REST fetch is required.
pub fn message_parts(message: &Message) -> (String, Vec<Attachment>) {
    let forwarded = message
        .message_snapshots
        .iter()
        .map(|snapshot| (snapshot.content.as_str(), snapshot.attachments.as_slice()))
        .collect();
    compose_message_parts(&message.content, &message.attachments, forwarded)
}

fn compose_message_parts(
    body: &str,
    direct_attachments: &[Attachment],
    forwarded: Vec<(&str, &[Attachment])>,
) -> (String, Vec<Attachment>) {
    let mut text = body.to_owned();
    let mut attachments = direct_attachments.to_vec();
    for (index, (forwarded_body, forwarded_attachments)) in forwarded.into_iter().enumerate() {
        if !forwarded_body.trim().is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&format!(
                "Forwarded message {}:\n{}",
                index + 1,
                forwarded_body
            ));
        }
        attachments.extend(forwarded_attachments.iter().cloned());
    }
    (text, attachments)
}

/// Serialize live ingress as data with explicit author, body, and forwarded fields.
pub fn format_ingress_message(body: &str, forwarded: &[Value], source: Value) -> String {
    let mut input = json!({"source": source, "body": body});
    if !forwarded.is_empty() {
        input["forwardedMessages"] = json!(forwarded);
    }
    input.to_string()
}

pub fn forwarded_message_data(message: &Message) -> Vec<Value> {
    message.message_snapshots.iter().map(|snapshot| json!({
        "body": snapshot.content,
        "attachments": snapshot.attachments.iter().map(|attachment| &attachment.filename).collect::<Vec<_>>(),
    })).collect()
}

pub async fn download_attachment(
    config: &Config,
    attachment: &Attachment,
) -> Result<DownloadedAttachment> {
    download_attachment_source(
        config,
        &attachment.url,
        attachment.id.get(),
        &attachment.filename,
    )
    .await
}

/// Download from a durable message attachment descriptor retained by the queue.
pub async fn download_attachment_source(
    config: &Config,
    source_url: &str,
    attachment_id: u64,
    filename: &str,
) -> Result<DownloadedAttachment> {
    let url = validate_discord_cdn_url(source_url)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .context("create Discord attachment downloader")?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| error.without_url())
        .context("download Discord attachment")?
        .error_for_status()
        .map_err(|error| error.without_url())
        .context("Discord attachment download returned an error status")?;

    let directory = private_attachment_dir(config)?;
    let temporary_path = directory.join(format!(".{}.part", Uuid::new_v4()));
    let std_file = create_private_file(&temporary_path)?;
    let mut cleanup = PartialAttachment::new(temporary_path.clone());
    let mut file = File::from_std(std_file);
    let mut downloaded = 0u64;
    let mut prefix = Vec::with_capacity(24);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| error.without_url())
        .context("read Discord attachment response")?
    {
        downloaded = downloaded
            .checked_add(chunk.len() as u64)
            .context("Discord attachment size overflow")?;
        let prefix_bytes = 24usize.saturating_sub(prefix.len()).min(chunk.len());
        prefix.extend_from_slice(&chunk[..prefix_bytes]);
        file.write_all(&chunk)
            .await
            .context("write downloaded Discord attachment")?;
    }
    file.flush()
        .await
        .context("flush downloaded Discord attachment")?;
    file.sync_all()
        .await
        .context("sync downloaded Discord attachment")?;
    drop(file);

    let image_extension = supported_image_extension(&prefix);
    let stored_filename = match image_extension {
        Some(extension) => format!("{}-{}-image.{extension}", attachment_id, Uuid::new_v4()),
        None => format!(
            "{}-{}-{}",
            attachment_id,
            Uuid::new_v4(),
            safe_filename(&filename)
        ),
    };
    let path = directory.join(stored_filename);
    fs::hard_link(&temporary_path, &path).context("finish downloaded Discord attachment")?;
    cleanup.track(path.clone());
    fs::remove_file(&temporary_path).context("remove temporary Discord attachment file")?;
    cleanup.disarm();
    Ok(DownloadedAttachment {
        filename: filename.to_owned(),
        path,
        is_image: image_extension.is_some(),
    })
}

fn attachment_manifest(files: &[DownloadedAttachment]) -> Value {
    Value::Array(
        files
            .iter()
            .map(|file| {
                json!({
                    "filename": file.filename,
                    "path": file.path,
                    "kind": if file.is_image { "image" } else { "file" },
                })
            })
            .collect(),
    )
}

/// Add the machine-readable local attachment manifest to text-only input.
/// Realtime appendText accepts this representation; local image content
/// blocks are only available on the ordinary turn input path below.
pub fn native_input_text(text: &str, files: &[DownloadedAttachment]) -> String {
    let mut text_input = text.to_owned();
    if !files.is_empty() {
        text_input.push_str(ATTACHMENT_MANIFEST_MARKER);
        text_input.push_str(&attachment_manifest(files).to_string());
    }
    text_input
}

pub fn native_input(text: &str, files: &[DownloadedAttachment]) -> Vec<Value> {
    let text_input = native_input_text(text, files);
    let mut input = vec![json!({ "type": "text", "text": text_input })];
    input.extend(
        files
            .iter()
            .filter(|file| file.is_image)
            .map(|file| json!({ "type": "localImage", "path": file.path })),
    );
    input
}

fn validate_discord_cdn_url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).context("invalid Discord attachment URL")?;
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https"
        || !matches!(host, "cdn.discordapp.com" | "media.discordapp.net")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
    {
        bail!("attachment URL is not an HTTPS Discord CDN URL");
    }
    Ok(url)
}

fn supported_image_extension(prefix: &[u8]) -> Option<&'static str> {
    if prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if prefix.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if prefix.len() >= 12 && &prefix[0..4] == b"RIFF" && &prefix[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

fn safe_filename(filename: &str) -> String {
    let mut safe = filename
        .chars()
        .take(120)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    while safe.starts_with('.') {
        safe.remove(0);
    }
    if safe.is_empty() {
        "attachment.bin".to_owned()
    } else {
        safe
    }
}

fn private_attachment_dir(config: &Config) -> Result<PathBuf> {
    let state_dir = fs::canonicalize(&config.state_dir)
        .context("resolve worker state directory for attachments")?;
    let directory = state_dir.join("attachments");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("managed attachment directory is not a regular directory");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match create_private_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error).context("create managed attachment directory"),
            }
        }
        Err(error) => return Err(error).context("inspect managed attachment directory"),
    }
    let metadata =
        fs::symlink_metadata(&directory).context("inspect managed attachment directory")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("managed attachment directory is not a regular directory");
    }
    #[cfg(unix)]
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .context("restrict managed attachment directory permissions")?;
    let canonical_directory =
        fs::canonicalize(&directory).context("resolve managed attachment directory")?;
    if !canonical_directory.starts_with(&state_dir) || canonical_directory == state_dir {
        bail!("managed attachment directory escapes worker state directory");
    }
    Ok(canonical_directory)
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

fn create_private_file(path: &Path) -> Result<std::fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .context("create private Discord attachment file")
}

struct PartialAttachment {
    paths: Vec<PathBuf>,
    armed: bool,
}

impl PartialAttachment {
    fn new(path: PathBuf) -> Self {
        Self {
            paths: vec![path],
            armed: true,
        }
    }

    fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PartialAttachment {
    fn drop(&mut self) {
        if self.armed {
            for path in &self.paths {
                let _ = fs::remove_file(path);
            }
        }
    }
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_images_are_recognized_from_content_bytes() {
        assert_eq!(
            supported_image_extension(b"\x89PNG\r\n\x1a\nrest"),
            Some("png")
        );
        assert_eq!(supported_image_extension(b"\xff\xd8\xffrest"), Some("jpg"));
        assert_eq!(supported_image_extension(b"RIFF1234WEBPrest"), Some("webp"));
        assert_eq!(supported_image_extension(b"GIF89a"), None);
        assert_eq!(supported_image_extension(b"%PDF-1.7"), None);
    }

    #[test]
    fn discord_attachment_urls_require_https_and_allowed_hosts() {
        assert!(
            validate_discord_cdn_url("https://cdn.discordapp.com/attachments/1/a.png?ex=1").is_ok()
        );
        assert!(
            validate_discord_cdn_url("https://media.discordapp.net/attachments/1/a.png").is_ok()
        );
        assert!(validate_discord_cdn_url("http://cdn.discordapp.com/attachments/1/a.png").is_err());
        assert!(validate_discord_cdn_url("https://example.com/a.png").is_err());
        assert!(validate_discord_cdn_url("https://cdn.discordapp.com.evil.example/a.png").is_err());
    }

    #[test]
    fn filenames_are_safe_single_path_components() {
        assert_eq!(
            safe_filename("../../hello world?.pdf"),
            "_.._hello_world_.pdf"
        );
        assert_eq!(safe_filename("..."), "attachment.bin");
    }

    fn test_attachment(id: u64, filename: &str) -> Attachment {
        serde_json::from_value(json!({
            "id": id.to_string(),
            "filename": filename,
            "proxy_url": format!("https://cdn.discordapp.com/attachments/{id}/{filename}"),
            "size": 10,
            "url": format!("https://cdn.discordapp.com/attachments/{id}/{filename}"),
        }))
        .unwrap()
    }

    #[test]
    fn forwarded_only_image_is_retained_for_native_image_input() {
        let image = test_attachment(22, "forwarded.png");
        let (text, attachments) =
            compose_message_parts("", &[], vec![("look", std::slice::from_ref(&image))]);
        assert_eq!(text, "Forwarded message 1:\nlook");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].id.get(), 22);
        assert_eq!(attachments[0].filename, "forwarded.png");
        let native_files = vec![DownloadedAttachment {
            filename: "forwarded.png".to_owned(),
            path: PathBuf::from("/state/attachments/forwarded.png"),
            is_image: true,
        }];
        let input = native_input(&text, &native_files);
        assert_eq!(
            input[1],
            json!({"type":"localImage","path":"/state/attachments/forwarded.png"})
        );
    }

    #[test]
    fn direct_and_forwarded_bodies_and_files_keep_their_order() {
        let direct = test_attachment(1, "direct.txt");
        let forwarded = test_attachment(2, "forwarded.txt");
        let (text, attachments) = compose_message_parts(
            "please review",
            std::slice::from_ref(&direct),
            vec![("quoted request", std::slice::from_ref(&forwarded))],
        );
        assert_eq!(
            text,
            "please review\n\nForwarded message 1:\nquoted request"
        );
        assert_eq!(
            attachments
                .iter()
                .map(|file| file.filename.as_str())
                .collect::<Vec<_>>(),
            vec!["direct.txt", "forwarded.txt"]
        );
    }

    #[test]
    fn empty_forwarded_body_does_not_create_false_content_but_keeps_files() {
        let file = test_attachment(3, "only-file.pdf");
        let (text, attachments) =
            compose_message_parts("", &[], vec![("  ", std::slice::from_ref(&file))]);
        assert!(text.is_empty());
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].id.get(), 3);
        assert_eq!(attachments[0].filename, "only-file.pdf");
    }

    #[test]
    fn ingress_formatter_keeps_live_body_and_named_source_fields_separate() {
        let home = format_ingress_message("do the task", &[], json!({"author":"Mina"}));
        assert_eq!(
            serde_json::from_str::<Value>(&home).unwrap(),
            json!({
                "source":{"author":"Mina"}, "body":"do the task"
            })
        );
        let elsewhere = format_ingress_message(
            "hello",
            &[],
            json!({
                "author":"Mina", "server":"Workshop", "channel":"planning"
            }),
        );
        assert_eq!(
            serde_json::from_str::<Value>(&elsewhere).unwrap(),
            json!({
                "source":{"author":"Mina", "server":"Workshop", "channel":"planning"},
                "body":"hello"
            })
        );
    }

    #[test]
    fn native_input_emits_manifest_and_only_local_image_items() {
        let files = vec![
            DownloadedAttachment {
                filename: "diagram.png".to_owned(),
                path: PathBuf::from("/state/attachments/4-image.png"),
                is_image: true,
            },
            DownloadedAttachment {
                filename: "report.pdf".to_owned(),
                path: PathBuf::from("/state/attachments/5-report.pdf"),
                is_image: false,
            },
        ];
        let input = native_input("Please inspect these.", &files);
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "text");
        let text = input[0]["text"].as_str().unwrap();
        assert!(text.starts_with("Please inspect these."));
        let (_, manifest_json) = text.rsplit_once(ATTACHMENT_MANIFEST_MARKER).unwrap();
        let manifest: Value = serde_json::from_str(manifest_json).unwrap();
        assert_eq!(
            manifest,
            json!([
                {
                    "filename": "diagram.png",
                    "path": "/state/attachments/4-image.png",
                    "kind": "image",
                },
                {
                    "filename": "report.pdf",
                    "path": "/state/attachments/5-report.pdf",
                    "kind": "file",
                },
            ])
        );
        assert_eq!(
            input[1],
            json!({"type":"localImage", "path":"/state/attachments/4-image.png"})
        );
    }

    #[test]
    fn realtime_text_input_preserves_attachment_only_and_mixed_file_manifests() {
        let files = vec![
            DownloadedAttachment {
                filename: "forwarded.png".to_owned(),
                path: PathBuf::from("/state/attachments/forwarded.png"),
                is_image: true,
            },
            DownloadedAttachment {
                filename: "report.pdf".to_owned(),
                path: PathBuf::from("/state/attachments/report.pdf"),
                is_image: false,
            },
        ];

        let text = native_input_text("", &files);
        let (body, manifest_json) = text.rsplit_once(ATTACHMENT_MANIFEST_MARKER).unwrap();
        assert_eq!(body, "");
        assert_eq!(
            serde_json::from_str::<Value>(manifest_json).unwrap(),
            json!([
                {"filename":"forwarded.png", "path":"/state/attachments/forwarded.png", "kind":"image"},
                {"filename":"report.pdf", "path":"/state/attachments/report.pdf", "kind":"file"},
            ])
        );

        let ordinary = native_input("", &files);
        assert_eq!(ordinary.len(), 2);
        assert_eq!(ordinary[0]["text"], text);
        assert_eq!(
            ordinary[1],
            json!({"type":"localImage", "path":"/state/attachments/forwarded.png"})
        );
    }
}

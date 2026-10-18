//! Discord table presentation. Generated PNGs and source bytes are owned only by
//! the outgoing request; neither images nor source lookup records are cached.
use anyhow::{Context as _, Result, bail};
use serenity::all::{
    ButtonStyle, ChannelId, ComponentInteraction, Context, CreateActionRow, CreateAllowedMentions,
    CreateAttachment, CreateButton, CreateInteractionResponse, CreateInteractionResponseMessage,
    CreateMessage, EditInteractionResponse, MessageId,
};

use crate::{i18n::Language, reply_files::ReplyFile, table_render};

const PREFIX: &str = "discodex:table:";
const SOURCE_PREFIX: &str = "discodex-table-";
const MESSAGE_BYTES: usize = 1650; // Leave room for routing/source labels.

pub(crate) struct PreparedMessage {
    pub(crate) content: String,
    files: Vec<ReplyFile>,
    components: Vec<CreateActionRow>,
}

impl PreparedMessage {
    pub(crate) fn has_files(&self) -> bool {
        !self.files.is_empty()
    }

    /// The caller supplies routed content and allowed mentions. Move upload
    /// buffers into Serenity's request, whose lifetime ends with the send.
    pub(crate) fn into_builder(self, builder: CreateMessage) -> CreateMessage {
        let builder = builder.add_files(
            self.files
                .into_iter()
                .map(|file| CreateAttachment::bytes(file.data, file.filename)),
        );
        if self.components.is_empty() {
            builder
        } else {
            builder.components(self.components)
        }
    }
}

fn text_page(content: String) -> PreparedMessage {
    PreparedMessage {
        content,
        files: Vec::new(),
        components: Vec::new(),
    }
}

pub(crate) fn prepare_plain(content: &str, files: Vec<ReplyFile>) -> Vec<PreparedMessage> {
    let mut pages = chunks(content, MESSAGE_BYTES)
        .into_iter()
        .map(text_page)
        .collect::<Vec<_>>();
    let batches = crate::reply_files::attachment_batches(&files);
    pages.resize_with(pages.len().max(batches.len()).max(1), || {
        text_page(String::new())
    });
    let mut files = files.into_iter();
    for (index, batch) in batches.into_iter().enumerate() {
        pages[index].files.extend(files.by_ref().take(batch.len()));
    }
    pages
}

pub(crate) async fn prepare(
    content: &str,
    files: Vec<ReplyFile>,
    language: Language,
) -> Vec<PreparedMessage> {
    let parts = table_render::parse(content);
    if !parts
        .iter()
        .any(|part| matches!(part, table_render::Part::Table(_)))
    {
        return prepare_plain(content, files);
    }
    let mut pages = Vec::new();
    for part in parts {
        match part {
            table_render::Part::Text(text) => {
                if !text.trim().is_empty() {
                    pages.extend(chunks(&text, MESSAGE_BYTES).into_iter().map(text_page));
                }
            }
            table_render::Part::Table(table) => {
                let source = table.source.clone();
                let rendered =
                    tokio::task::spawn_blocking(move || table_render::render(&table)).await;
                match rendered {
                    Ok(Ok(images))
                        if !images.is_empty()
                            && images.iter().all(|image| {
                                image.len().saturating_add(source.len())
                                    < 25 * 1024 * 1024 - 16 * 1024
                            }) =>
                    {
                        pages.extend(table_pages(source, images, language))
                    }
                    outcome => {
                        // Preserve all original text if rendering fails; never drop a table.
                        let reason = match outcome {
                            Err(error) => error.to_string(),
                            Ok(Err(error)) => error.to_string(),
                            Ok(Ok(_)) => {
                                "rendered table exceeds the Discord request limit or has no images"
                                    .to_owned()
                            }
                        };
                        tracing::warn!(%reason, "table rendering unavailable; sending original Markdown");
                        pages.extend(chunks(&source, MESSAGE_BYTES).into_iter().map(text_page));
                    }
                }
            }
        }
    }
    if !files.is_empty() {
        pages.extend(prepare_plain("", files));
    }
    pages
}

fn table_pages(source: String, images: Vec<Vec<u8>>, language: Language) -> Vec<PreparedMessage> {
    let key = uuid::Uuid::new_v4().simple().to_string();
    images
        .into_iter()
        .enumerate()
        .map(|(index, data)| PreparedMessage {
            content: String::new(),
            files: vec![
                ReplyFile {
                    filename: format!("table-{key}-{}.png", index + 1),
                    data,
                },
                ReplyFile {
                    filename: source_filename(&key),
                    data: source.as_bytes().to_vec(),
                },
            ],
            components: vec![CreateActionRow::Buttons(vec![
                CreateButton::new(format!("{PREFIX}{key}"))
                    .style(ButtonStyle::Secondary)
                    .label(language.text("View source", "원문 보기", "查看原文", "原文を表示")),
            ])],
        })
        .collect()
}

fn source_filename(key: &str) -> String {
    format!("{SOURCE_PREFIX}{key}.md")
}

pub(crate) fn is_source_button(custom_id: &str) -> bool {
    custom_id.starts_with(PREFIX)
}

#[derive(Debug, PartialEq, Eq)]
struct SourceRequest {
    key: String,
    channel: u64,
    message: u64,
    page: usize,
    initial: bool,
}

impl SourceRequest {
    fn parse(value: &str, channel: u64, message: u64) -> Result<Self> {
        let parts = value
            .strip_prefix(PREFIX)
            .context("unknown table button")?
            .split(':')
            .collect::<Vec<_>>();
        let key = parts.first().context("missing table key")?;
        if key.len() != 32 || !key.bytes().all(|ch| ch.is_ascii_hexdigit()) {
            bail!("invalid table key");
        }
        match parts.as_slice() {
            [_] => Ok(Self {
                key: (*key).to_owned(),
                channel,
                message,
                page: 0,
                initial: true,
            }),
            [_, channel, message, page] => Ok(Self {
                key: (*key).to_owned(),
                channel: channel.parse()?,
                message: message.parse()?,
                page: page.parse()?,
                initial: false,
            }),
            _ => bail!("invalid table button"),
        }
    }

    fn button(&self, page: usize, label: &str) -> CreateButton {
        CreateButton::new(format!(
            "{PREFIX}{}:{}:{}:{page}",
            self.key, self.channel, self.message
        ))
        .style(ButtonStyle::Secondary)
        .label(label)
    }
}

pub(crate) async fn handle_component(
    ctx: &Context,
    interaction: &ComponentInteraction,
    language: Language,
) -> Result<()> {
    let request = SourceRequest::parse(
        &interaction.data.custom_id,
        interaction.channel_id.get(),
        interaction.message.id.get(),
    )?;
    if request.initial {
        interaction
            .create_response(
                ctx,
                CreateInteractionResponse::Defer(
                    CreateInteractionResponseMessage::new().ephemeral(true),
                ),
            )
            .await?;
    } else {
        interaction
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await?;
    }
    let result = source_response(ctx, &request, language).await;
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "could not retrieve table Markdown from Discord");
            EditInteractionResponse::new().content(language.text(
                "Could not load the original table. You can download the Markdown attachment from the table message and retry.",
                "표 원문을 불러오지 못했습니다. 표 메시지의 Markdown 첨부를 다운로드하거나 다시 시도해 주세요.",
                "无法加载表格原文。请下载表格消息中的 Markdown 附件，或重试。",
                "表の原文を読み込めませんでした。表のメッセージにあるMarkdown添付をダウンロードするか、再試行してください。",
            )).components(Vec::new())
        }
    };
    interaction
        .edit_response(ctx, response.allowed_mentions(no_mentions()))
        .await?;
    Ok(())
}

async fn source_response(
    ctx: &Context,
    request: &SourceRequest,
    language: Language,
) -> Result<EditInteractionResponse> {
    // Re-fetch the message to refresh expiring Discord CDN URLs. The source
    // lives with the Discord message, so worker restarts need no local cache.
    let message = ChannelId::new(request.channel)
        .message(&ctx.http, MessageId::new(request.message))
        .await?;
    if message.author.id != ctx.http.get_current_user().await?.id {
        bail!("table source belongs to another application");
    }
    let filename = source_filename(&request.key);
    let attachment = message
        .attachments
        .iter()
        .find(|file| file.filename == filename)
        .context("table Markdown attachment is no longer available")?;
    let source = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .get(&attachment.url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let pages = source_pages(&source);
    let content = pages
        .get(request.page)
        .context("table source page is no longer available")?;
    let mut response = EditInteractionResponse::new()
        .content(content)
        .components(Vec::new());
    let mut buttons = Vec::new();
    if request.page > 0 {
        buttons.push(request.button(
            request.page - 1,
            language.text("Previous", "이전", "上一页", "前へ"),
        ));
    }
    if request.page + 1 < pages.len() {
        buttons.push(request.button(
            request.page + 1,
            language.text("Next", "다음", "下一页", "次へ"),
        ));
    }
    if !buttons.is_empty() {
        response = response.components(vec![CreateActionRow::Buttons(buttons)]);
    }
    Ok(response)
}

fn no_mentions() -> CreateAllowedMentions {
    CreateAllowedMentions::new()
        .all_users(false)
        .all_roles(false)
        .everyone(false)
        .replied_user(false)
}

fn chunks(text: &str, bytes: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut end = remaining.len().min(bytes);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        if end < remaining.len() {
            if let Some(newline) = remaining[..end].rfind('\n').filter(|&index| index > 0) {
                end = newline + 1;
            }
        }
        chunks.push(remaining[..end].to_owned());
        remaining = &remaining[end..];
    }
    chunks
}

fn source_pages(source: &str) -> Vec<String> {
    let mut run = 0;
    let mut longest = 0;
    for ch in source.chars() {
        run = if ch == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence_length = (longest + 1).max(3);
    // An unusually long backtick run must not make the surrounding fence exceed
    // Discord's 2000-character message limit. Plain text still preserves it.
    if fence_length > 150 {
        return chunks(source, MESSAGE_BYTES);
    }
    let fence = "`".repeat(fence_length);
    chunks(source, MESSAGE_BYTES)
        .into_iter()
        .map(|text| format!("{fence}markdown\n{text}\n{fence}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_existing_files_are_preserved_in_request_batches() {
        let input = "Hello 한글 中文 日本語\n".repeat(200);
        let files = (0..12)
            .map(|index| ReplyFile {
                filename: format!("{index}.txt"),
                data: vec![index],
            })
            .collect();
        let pages = prepare_plain(&input, files);
        assert_eq!(
            pages
                .iter()
                .map(|page| page.content.as_str())
                .collect::<String>(),
            input
        );
        assert!(pages.iter().all(|page| page.content.len() <= MESSAGE_BYTES));
        assert_eq!(pages[0].files.len(), 10);
        assert_eq!(pages[1].files.len(), 2);
        assert_eq!(pages[1].files[1].data, vec![11]);
    }

    #[test]
    fn table_uploads_own_image_and_original_source_and_localize_button() {
        let source = "| A | B |\n| - | - |\n| α | 한글 |\n";
        let pages = table_pages(
            source.to_owned(),
            vec![vec![1, 2], vec![3, 4]],
            Language::Korean,
        );
        assert_eq!(pages.len(), 2);
        assert!(pages.iter().all(PreparedMessage::has_files));
        assert_eq!(pages[0].files[0].data, vec![1, 2]);
        assert_eq!(pages[1].files[1].data, source.as_bytes());
        let key = pages[0].files[1]
            .filename
            .strip_prefix(SOURCE_PREFIX)
            .unwrap()
            .strip_suffix(".md")
            .unwrap();
        let request = SourceRequest::parse(&format!("{PREFIX}{key}"), 12, 34).unwrap();
        assert_eq!(request.channel, 12);
        assert_eq!(request.message, 34);
        let payload = serde_json::to_value(&pages[0].components).unwrap();
        assert_eq!(payload[0]["components"][0]["label"], "원문 보기");
    }

    #[tokio::test]
    async fn message_preparation_keeps_prose_table_order_and_original_source() {
        let first = "| Item | Count |\n| --- | ---: |\n| Apple | 2 |\n";
        let second = "| Name | Result |\n| --- | --- |\n| Beta | Ready |\n";
        let input = format!("Before\n\n{first}\nBetween\n\n{second}\nAfter");
        let pages = prepare(&input, Vec::new(), Language::English).await;
        assert_eq!(pages.len(), 5);
        assert_eq!(pages[0].content, "Before\n\n");
        assert_eq!(
            pages[1].files[1].data,
            first.trim_end_matches('\n').as_bytes()
        );
        assert_eq!(pages[2].content.trim(), "Between");
        assert_eq!(
            pages[3].files[1].data,
            second.trim_end_matches('\n').as_bytes()
        );
        assert_eq!(pages[4].content.trim(), "After");
        let restored = pages
            .iter()
            .map(|page| {
                if page.files.len() == 2 {
                    String::from_utf8(page.files[1].data.clone()).unwrap()
                } else {
                    page.content.clone()
                }
            })
            .collect::<String>();
        assert_eq!(restored, input);
    }

    #[test]
    fn source_buttons_follow_all_four_profile_languages() {
        for (language, expected) in [
            (Language::English, "View source"),
            (Language::Korean, "원문 보기"),
            (Language::Chinese, "查看原文"),
            (Language::Japanese, "原文を表示"),
        ] {
            let pages = table_pages("|A|\n|-|".to_owned(), vec![vec![1]], language);
            let payload = serde_json::to_value(&pages[0].components).unwrap();
            assert_eq!(payload[0]["components"][0]["label"], expected);
        }
    }

    #[test]
    fn source_paging_preserves_utf8_and_embedded_backticks() {
        let source = "| 한글 | `code` |\n| --- | --- |\n".repeat(100);
        let pages = source_pages(&source);
        assert!(pages.len() > 1);
        assert!(pages.iter().all(|page| page.chars().count() <= 2000));
        let restored = pages
            .iter()
            .map(|page| {
                page.strip_prefix("```markdown\n")
                    .unwrap()
                    .strip_suffix("\n```")
                    .unwrap()
            })
            .collect::<String>();
        assert_eq!(restored, source);
        assert_eq!(source_pages("x ``` y")[0], "````markdown\nx ``` y\n````");
        assert_eq!(source_pages(&"`".repeat(1700)).concat(), "`".repeat(1700));
    }

    #[test]
    fn source_page_buttons_retain_source_after_restart_without_local_lookup() {
        let key = "a".repeat(32);
        let request = SourceRequest::parse(&format!("{PREFIX}{key}:12:34:2"), 99, 88).unwrap();
        assert_eq!(
            request,
            SourceRequest {
                key,
                channel: 12,
                message: 34,
                page: 2,
                initial: false
            }
        );
        let button = serde_json::to_value(request.button(3, "Next")).unwrap();
        assert!(button["custom_id"].as_str().unwrap().len() <= 100);
        assert_eq!(
            SourceRequest::parse(button["custom_id"].as_str().unwrap(), 99, 88)
                .unwrap()
                .page,
            3
        );
        assert!(SourceRequest::parse("discodex:table:bad", 12, 34).is_err());
    }
}

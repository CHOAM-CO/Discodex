use super::*;
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn tools(server: &MockServer, directory: &std::path::Path) -> DiscordTools {
    let config = serde_json::from_value(json!({
        "profile":"test-worker", "name":"Test worker", "tokenEnv":"TEST_TOKEN",
        "guildId":10, "homeChannelId":20, "transcriptChannelId":30,
        "codexPath":"codex", "projectDir":directory, "stateDir":directory.join("state")
    }))
    .unwrap();
    let http = serenity::http::HttpBuilder::new("test-token")
        .proxy(server.uri())
        .ratelimiter_disabled(true)
        .build();
    DiscordTools::new(Arc::new(config), Arc::new(http))
}

fn dm() -> PrivateChannel {
    let mut dm = PrivateChannel::default();
    dm.id = ChannelId::new(42);
    dm.kind = ChannelType::Private;
    dm.recipient.id = UserId::new(7);
    dm.recipient.name = "sender".to_owned();
    dm.recipient.global_name = Some("Display Name".to_owned());
    dm
}

fn message(id: u64, body: &str) -> Message {
    let mut message = Message::default();
    message.id = MessageId::new(id);
    message.channel_id = ChannelId::new(42);
    message.author = dm().recipient;
    message.content = body.to_owned();
    message
}

fn attachment(url: &str) -> serenity::all::Attachment {
    serde_json::from_value(json!({
        "id":"456", "filename":"notes.txt", "size":16,
        "url":url, "proxy_url":url
    }))
    .unwrap()
}

async fn mock_dm(server: &MockServer, count: u64) {
    Mock::given(method("GET"))
        .and(path("/api/v10/channels/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(dm()))
        .expect(count)
        .mount(server)
        .await;
}

#[tokio::test]
async fn dm_send_uploads_files_without_guild_lookup_and_returns_private_links() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("notes.txt");
    std::fs::write(&file, "attachment bytes").unwrap();
    mock_dm(&server, 1).await;
    let mut sent = message(123, "<@7>\n\nReply in DM");
    sent.attachments.push(attachment(
        "https://cdn.discordapp.com/attachments/42/456/notes.txt",
    ));
    Mock::given(method("POST"))
        .and(path("/api/v10/channels/42/messages"))
        .and(body_string_contains("attachment bytes"))
        .and(body_string_contains("Reply in DM"))
        .and(body_string_contains("<@7>"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sent))
        .expect(1)
        .mount(&server)
        .await;

    let result = tools(&server, directory.path())
        .invoke(
            "discord_send_message",
            json!({
                "dm_id":"42", "content":"Reply in DM", "mentions":["Display Name"],
                "files":[file]
            }),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "sent");
    assert_eq!(result["channel"], "DM");
    assert_eq!(result["dm_id"], "42");
    assert!(result.get("server").is_none());
    assert_eq!(
        result["message_link"],
        "https://discord.com/channels/@me/42/123"
    );
    assert_eq!(result["attachments"][0]["filename"], "notes.txt");
    assert_eq!(result["messages_sent"], 1);
    server.verify().await;
}

#[tokio::test]
async fn dm_history_accepts_private_cursor_links_and_preserves_attachment_metadata() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    mock_dm(&server, 1).await;
    let mut first = message(122, "latest private message");
    first.attachments.push(attachment(
        "https://cdn.discordapp.com/attachments/42/456/notes.txt",
    ));
    Mock::given(method("GET"))
        .and(path("/api/v10/channels/42/messages"))
        .and(query_param("before", "123"))
        .and(query_param("limit", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(vec![first, message(121, "older private message")]),
        )
        .expect(1)
        .mount(&server)
        .await;
    let result = tools(&server, directory.path())
        .invoke(
            "discord_get_messages",
            json!({
                "dm_id":"42", "before":"https://discord.com/channels/@me/42/123", "limit":2
            }),
        )
        .await
        .unwrap();
    assert_eq!(result["dm_id"], "42");
    assert!(result.get("server").is_none());
    assert_eq!(result["messages"][0]["content"], "latest private message");
    assert_eq!(
        result["messages"][0]["attachments"][0]["filename"],
        "notes.txt"
    );
    assert_eq!(
        result["next_before"],
        "https://discord.com/channels/@me/42/121"
    );
    assert_eq!(
        result["next_after"],
        "https://discord.com/channels/@me/42/122"
    );
    server.verify().await;
}

#[tokio::test]
async fn dm_search_reads_private_history_without_server_membership() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    mock_dm(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/api/v10/channels/42/messages"))
        .and(query_param("limit", "10"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(vec![message(122, "find this"), message(121, "other text")]),
        )
        .expect(1)
        .mount(&server)
        .await;
    let result = tools(&server, directory.path())
        .invoke(
            "discord_search_messages",
            json!({"dm_id":"42", "query":"find", "max_scan":10}),
        )
        .await
        .unwrap();
    assert_eq!(result["dm_id"], "42");
    assert_eq!(result["matching_count"], 1);
    assert_eq!(result["scanned"], 2);
    assert_eq!(
        result["messages"][0]["link"],
        "https://discord.com/channels/@me/42/122"
    );
    assert_eq!(result["history_exhausted"], true);
    server.verify().await;
}

#[tokio::test]
async fn dm_download_selects_private_message_attachment_and_uses_existing_downloader() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    mock_dm(&server, 2).await;
    let mut selected = message(123, "file");
    // Stop at the existing downloader's URL validation, without contacting a real CDN.
    selected
        .attachments
        .push(attachment("http://example.invalid/notes.txt"));
    Mock::given(method("GET"))
        .and(path("/api/v10/channels/42/messages/123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(selected))
        .expect(1)
        .mount(&server)
        .await;
    let tools = tools(&server, directory.path());
    let error = tools.invoke("discord_download_attachment", json!({
        "dm_id":"42", "message_link":"https://discord.com/channels/@me/42/123", "filename":"notes.txt"
    })).await.unwrap_err();
    assert_eq!(
        format!("{error:#}"),
        "download selected attachment: attachment URL is not an HTTPS Discord CDN URL"
    );
    let mismatch = tools.invoke("discord_download_attachment", json!({
        "dm_id":"42", "message_link":"https://discord.com/channels/@me/43/123", "filename":"notes.txt"
    })).await.unwrap_err();
    assert_eq!(
        mismatch.to_string(),
        "message link belongs to a different server or channel"
    );
    server.verify().await;
}

#[tokio::test]
async fn dm_selector_rejects_a_server_channel_returned_by_discord() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let mut guild_channel = GuildChannel::default();
    guild_channel.id = ChannelId::new(42);
    guild_channel.guild_id = GuildId::new(10);
    Mock::given(method("GET"))
        .and(path("/api/v10/channels/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(guild_channel))
        .expect(1)
        .mount(&server)
        .await;
    let error = tools(&server, directory.path())
        .invoke("discord_get_messages", json!({"dm_id":"42"}))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "dm_id does not identify a direct-message conversation"
    );
    server.verify().await;
}

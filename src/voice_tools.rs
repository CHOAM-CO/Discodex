use std::path::Path;

use crate::i18n::{Language, MessageKey};
use anyhow::{Context, Result, bail};
use codex_uds::UnixStream;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

// The summary replaces one Discord message (no prefix is added).
pub(crate) const MAX_VOICE_LEAVE_SUMMARY_CHARS: usize = 2000;
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const DISCORD_TOOL_NAMES: [&str; 7] = [
    "voice_join",
    "discord_list_channels",
    "discord_send_message",
    "discord_get_messages",
    "discord_download_attachment",
    "discord_search_messages",
    "discord_list_users",
];

/// Runs the process-scoped MCP stdio server configured for the owned app-server.
/// MCP protocol messages alone are written to stdout; diagnostics go to stderr.
pub async fn run(endpoint: &Path, language: crate::i18n::Language) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut writer = tokio::io::stdout();
    let mut line = Vec::new();

    loop {
        line.clear();
        if !read_line(&mut reader, &mut line, language).await? {
            return Ok(());
        }
        while matches!(line.last(), Some(b'\r' | b'\n')) {
            line.pop();
        }
        if line.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(error) => {
                write_response(
                    &mut writer,
                    &json!({
                        "jsonrpc":"2.0",
                        "id":Value::Null,
                        "error":{"code":-32700,"message":language.message(MessageKey::VoiceToolsParseError).replace("{error}", &error.to_string())}
                    }),
                )
                .await?;
                continue;
            }
        };
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            write_response(
                &mut writer,
                &json!({
                    "jsonrpc":"2.0",
                    "id":request.get("id").cloned().unwrap_or(Value::Null),
                    "error":{"code":-32600,"message":language.message(MessageKey::VoiceToolsInvalidRequest)}
                }),
            )
            .await?;
            continue;
        };
        let Some(id) = request.get("id").cloned() else {
            if method == "exit" {
                return Ok(());
            }
            // MCP notifications, including notifications/initialized, have no response.
            continue;
        };
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        let response = match method {
            "initialize" => json!({
                "jsonrpc":"2.0",
                "id":id,
                "result":{
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities":{"tools":{}},
                    "serverInfo":{"name":"discodex-voice","version":env!("CARGO_PKG_VERSION")}
                }
            }),
            "ping" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
            "shutdown" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
            "tools/list" => {
                let mut tools = vec![json!({
                    "name":"voice_leave",
                    "description":format!("{} {}", language.message(MessageKey::VoiceToolsVoiceLeaveDescription), crate::instructions::voice_leave_policy(language)),
                    "inputSchema":{
                        "type":"object",
                        "properties":{
                            "summary":{
                                "type":"string",
                                "minLength":1,
                                "maxLength":MAX_VOICE_LEAVE_SUMMARY_CHARS,
                                "description":language.message(MessageKey::VoiceToolsSummaryDescription)
                            }
                        },
                        "required":["summary"],
                        "additionalProperties":false
                    }
                })];
                tools.push(voice_join_definition(language));
                tools.extend(crate::discord_tools::tool_definitions(language));
                tools.push(json!({
                    "name":"discord_read_voice_inbox",
                    "description":language.message(MessageKey::VoiceToolsReadInboxDescription),
                    "inputSchema":{"type":"object","properties":{"call_id":{"type":"string","description":language.message(MessageKey::VoiceToolsCallIdDescription)},"message_ids":{"type":"array","minItems":1,"items":{"type":"integer","minimum":1},"description":language.message(MessageKey::VoiceToolsMessageIdsDescription)}},"required":["call_id","message_ids"],"additionalProperties":false}
                }));
                json!({
                    "jsonrpc":"2.0",
                    "id":id,
                    "result":{"tools":tools}
                })
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if name == "voice_leave" {
                    match parse_leave_summary(&args, language) {
                        Ok(_) => {
                            tool_result(id, invoke_parent(endpoint, name, args, language).await)?
                        }
                        Err(error) => {
                            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":error.to_string()}})
                        }
                    }
                } else if name == "discord_read_voice_inbox" || DISCORD_TOOL_NAMES.contains(&name) {
                    if !args.is_object() {
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":language.message(MessageKey::VoiceToolsArgumentsObject)}})
                    } else {
                        tool_result(id, invoke_parent(endpoint, name, args, language).await)?
                    }
                } else {
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":language.message(MessageKey::VoiceToolsUnknownTool).replace("{name}", name)}})
                }
            }
            _ => json!({
                "jsonrpc":"2.0",
                "id":id,
                "error":{"code":-32601,"message":language.message(MessageKey::VoiceToolsMethodNotFound).replace("{name}", method)}
            }),
        };
        write_response(&mut writer, &response).await?;
    }
}

fn voice_join_definition(language: crate::i18n::Language) -> Value {
    json!({
        "name":"voice_join",
        "description":crate::instructions::voice_join_policy(language),
        "inputSchema":{
            "type":"object",
            "properties":{
                "channel":{"type":"string","minLength":1,"description":language.message(MessageKey::VoiceToolsChannelDescription)},
                "channel_choice":{"type":"string","minLength":1,"description":language.message(MessageKey::VoiceToolsChannelChoiceDescription)}
            },
            "oneOf":[{"required":["channel"]},{"required":["channel_choice"]}],
            "additionalProperties":false
        }
    })
}

async fn read_line<R>(reader: &mut R, line: &mut Vec<u8>, language: Language) -> Result<bool>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let (consumed, found_newline) = {
            let available = reader
                .fill_buf()
                .await
                .context(language.message(MessageKey::VoiceToolsStdinReadError))?;
            if available.is_empty() {
                return Ok(!line.is_empty());
            }
            let content_len = available
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or(available.len());
            let found_newline = content_len < available.len();
            line.extend_from_slice(&available[..content_len]);
            (content_len + usize::from(found_newline), found_newline)
        };
        reader.consume(consumed);
        if found_newline {
            return Ok(true);
        }
    }
}

pub(crate) fn parse_leave_summary(args: &Value, language: Language) -> Result<String> {
    let object = args
        .as_object()
        .context(language.message(MessageKey::VoiceToolsLeaveArgsObject))?;
    if object.len() != 1 || !object.contains_key("summary") {
        bail!(
            "{}",
            language.message(MessageKey::VoiceToolsLeaveArgsOnlySummary)
        );
    }
    let summary = object["summary"]
        .as_str()
        .context(language.message(MessageKey::VoiceToolsLeaveSummaryString))?
        .trim();
    if summary.is_empty() {
        bail!(
            "{}",
            language.message(MessageKey::VoiceToolsLeaveSummaryNonempty)
        );
    }
    if summary.chars().count() > MAX_VOICE_LEAVE_SUMMARY_CHARS {
        bail!(
            "{}",
            language
                .message(MessageKey::VoiceToolsLeaveSummaryTooLong)
                .replace("{limit}", &MAX_VOICE_LEAVE_SUMMARY_CHARS.to_string())
        );
    }
    Ok(summary.to_owned())
}

async fn invoke_parent(
    endpoint: &Path,
    method: &str,
    args: Value,
    language: Language,
) -> std::result::Result<Value, Value> {
    let mut stream = UnixStream::connect(endpoint).await.map_err(|error| {
        ipc_error(
            language
                .message(MessageKey::VoiceToolsConnectIpc)
                .replace("{path}", &endpoint.display().to_string())
                .replace("{error}", &format!("{error:#}")),
        )
    })?;
    let request = json!({"jsonrpc":"2.0","id":1,"method":method,"params":args});
    let mut bytes = serde_json::to_vec(&request).map_err(|error| {
        ipc_error(
            language
                .message(MessageKey::VoiceToolsSerializeIpc)
                .replace("{error}", &error.to_string()),
        )
    })?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await.map_err(|error| {
        ipc_error(
            language
                .message(MessageKey::VoiceToolsSendIpc)
                .replace("{error}", &format!("{error:#}")),
        )
    })?;
    stream.flush().await.map_err(|error| {
        ipc_error(
            language
                .message(MessageKey::VoiceToolsFlushIpc)
                .replace("{error}", &format!("{error:#}")),
        )
    })?;

    let mut reader = BufReader::new(stream);
    let mut response = Vec::new();
    loop {
        let (consumed, found_newline) = {
            let available = reader.fill_buf().await.map_err(|error| {
                ipc_error(
                    language
                        .message(MessageKey::VoiceToolsReadIpc)
                        .replace("{error}", &format!("{error:#}")),
                )
            })?;
            if available.is_empty() {
                return Err(ipc_error(language.message(MessageKey::VoiceToolsIpcClosed)));
            }
            let content_len = available
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or(available.len());
            response.extend_from_slice(&available[..content_len]);
            let found_newline = content_len < available.len();
            (content_len + usize::from(found_newline), found_newline)
        };
        reader.consume(consumed);
        if found_newline {
            break;
        }
    }
    let envelope: Value = serde_json::from_slice(&response).map_err(|error| {
        ipc_error(
            language
                .message(MessageKey::VoiceToolsParseIpc)
                .replace("{error}", &error.to_string()),
        )
    })?;
    if envelope.get("id") != Some(&json!(1)) {
        return Err(ipc_error(
            language
                .message(MessageKey::VoiceToolsIpcIdMismatch)
                .replace(
                    "{id}",
                    &envelope
                        .get("id")
                        .map_or_else(|| "missing".to_owned(), |id| id.to_string()),
                ),
        ));
    }
    if let Some(error) = envelope.get("error") {
        return Err(error.clone());
    }
    envelope
        .get("result")
        .cloned()
        .ok_or_else(|| ipc_error(language.message(MessageKey::VoiceToolsIpcNoResult)))
}

fn tool_result(id: Value, result: std::result::Result<Value, Value>) -> Result<Value> {
    let (message, is_error) = match result {
        Ok(value) => (
            serde_json::to_string(&value)?,
            value.get("status").and_then(Value::as_str) == Some("partial"),
        ),
        Err(mut error) => {
            if let Some(object) = error.as_object_mut() {
                if let Some(Value::String(message)) = object.get_mut("message") {
                    *message = crate::diagnostics::sanitize_diagnostic(message);
                }
            }
            (serde_json::to_string(&error)?, true)
        }
    };
    Ok(json!({
        "jsonrpc":"2.0",
        "id":id,
        "result":{"content":[{"type":"text","text":message}],"isError":is_error}
    }))
}

fn ipc_error(message: impl Into<String>) -> Value {
    json!({"code":-32000,"message":crate::diagnostics::sanitize_diagnostic(&message.into())})
}

async fn write_response<W>(writer: &mut W, response: &Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let bytes = serde_json::to_vec(response)
        .context(Language::English.message(MessageKey::VoiceToolsSerializeError))?;
    writer
        .write_all(&bytes)
        .await
        .context(Language::English.message(MessageKey::VoiceToolsWriteError))?;
    writer
        .write_all(b"\n")
        .await
        .context(Language::English.message(MessageKey::VoiceToolsWriteError))?;
    writer
        .flush()
        .await
        .context(Language::English.message(MessageKey::VoiceToolsFlushError))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_leave_summary_accepts_trimmed_nonempty_text() {
        assert_eq!(
            parse_leave_summary(
                &json!({"summary":"  오늘 이야기와 다음 계획  "}),
                Language::English
            )
            .unwrap(),
            "오늘 이야기와 다음 계획"
        );
    }

    #[test]
    fn parse_leave_summary_rejects_invalid_values() {
        assert!(parse_leave_summary(&json!({}), Language::English).is_err());
        assert!(parse_leave_summary(&json!({"summary":"  "}), Language::English).is_err());
        assert!(parse_leave_summary(&json!({"summary":null}), Language::English).is_err());
        assert!(
            parse_leave_summary(&json!({"summary":"내용", "extra":true}), Language::English)
                .is_err()
        );
        assert!(
            parse_leave_summary(
                &json!({"summary":"x".repeat(MAX_VOICE_LEAVE_SUMMARY_CHARS + 1)}),
                Language::English
            )
            .is_err()
        );
    }

    #[test]
    fn voice_tool_metadata_and_validation_errors_follow_selected_language() {
        for (language, channel_description) in [
            (
                Language::English,
                "Voice channel name, optionally category/channel",
            ),
            (
                Language::Korean,
                "음성 채널 이름. 필요하면 카테고리/채널 형식으로 지정",
            ),
            (Language::Chinese, "语音频道名称，可使用分类/频道格式"),
            (
                Language::Japanese,
                "音声チャンネル名。必要に応じてカテゴリ/チャンネルで指定",
            ),
        ] {
            assert_eq!(
                voice_join_definition(language)["inputSchema"]["properties"]["channel"]["description"],
                channel_description
            );
        }
        let error = parse_leave_summary(&json!({"summary":" "}), Language::Korean)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "voice_leave summary가 비어 있으면 안 됩니다");
    }

    #[test]
    fn tool_error_keeps_native_code_and_message_while_redacting_credentials() {
        let result = tool_result(
            json!(7),
            Err(json!({"code":-32001,"message":"upstream failed; token=synthetic-secret"})),
        )
        .unwrap();
        assert_eq!(result["result"]["isError"], true);
        let returned: Value =
            serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(returned["code"], -32001);
        assert_eq!(
            returned["message"],
            "upstream failed; token=[secret redacted]"
        );
    }
}

use std::path::Path;

use anyhow::{Context, Result, bail};
use codex_uds::UnixStream;
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};

// The summary replaces one Discord message (no prefix is added).
pub(crate) const MAX_VOICE_LEAVE_SUMMARY_CHARS: usize = 2000;
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const DISCORD_TOOL_NAMES: [&str; 6] = [
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
        if !read_line(&mut reader, &mut line).await? {
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
                        "error":{"code":-32700,"message":format!("{}: {error}", language.text("Parse error", "구문 분석 오류", "解析错误", "解析エラー"))}
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
                    "error":{"code":-32600,"message":"Invalid Request"}
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
                    "description":format!("{} {}", language.text(
                        "End the active Discord voice call, then replace its original transcript-thread post with the concise summary you provide.",
                        "종료하려는 현재 Discord 음성 통화를 끝내고, 원래 전사 스레드 게시물을 제공한 간결한 요약으로 바꿉니다.",
                        "结束当前 Discord 语音通话，并用你提供的简明摘要替换原始转录线程帖子。",
                        "現在のDiscord音声通話を終了し、元の文字起こしスレッド投稿を指定した簡潔な要約に置き換えます。"
                    ), crate::instructions::voice_leave_policy(language)),
                    "inputSchema":{
                        "type":"object",
                        "properties":{
                            "summary":{
                                "type":"string",
                                "minLength":1,
                                "maxLength":MAX_VOICE_LEAVE_SUMMARY_CHARS,
                                "description":language.text(
                                    "A concise, natural-language summary of this voice conversation, grounded in what was said.",
                                    "실제 대화 내용에 근거한 간결하고 자연스러운 음성 대화 요약입니다.",
                                    "根据实际对话内容撰写的简洁自然的语音通话摘要。",
                                    "実際に話した内容に基づく、簡潔で自然な音声通話の要約です。"
                                )
                            }
                        },
                        "required":["summary"],
                        "additionalProperties":false
                    }
                })];
                tools.extend(crate::discord_tools::tool_definitions(language));
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
                    match parse_leave_summary(&args) {
                        Ok(_) => tool_result(id, invoke_parent(endpoint, name, args).await)?,
                        Err(error) => {
                            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":error.to_string()}})
                        }
                    }
                } else if DISCORD_TOOL_NAMES.contains(&name) {
                    if !args.is_object() {
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":language.text("Discord tool arguments must be an object", "Discord 도구 인수는 객체여야 합니다", "Discord 工具参数必须是对象", "Discordツールの引数はオブジェクトである必要があります")}})
                    } else {
                        tool_result(id, invoke_parent(endpoint, name, args).await)?
                    }
                } else {
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":format!("{}: {name}", language.text("Unknown tool", "알 수 없는 도구", "未知工具", "不明なツール"))}})
                }
            }
            _ => json!({
                "jsonrpc":"2.0",
                "id":id,
                "error":{"code":-32601,"message":format!("{}: {method}", language.text("Method not found", "메서드를 찾을 수 없습니다", "未找到方法", "メソッドが見つかりません"))}
            }),
        };
        write_response(&mut writer, &response).await?;
    }
}

async fn read_line<R>(reader: &mut R, line: &mut Vec<u8>) -> Result<bool>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let (consumed, found_newline) = {
            let available = reader
                .fill_buf()
                .await
                .context("read MCP request from stdin")?;
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

pub(crate) fn parse_leave_summary(args: &Value) -> Result<String> {
    let object = args
        .as_object()
        .context("voice_leave arguments must be an object")?;
    if object.len() != 1 || !object.contains_key("summary") {
        bail!("voice_leave arguments must contain only summary");
    }
    let summary = object["summary"]
        .as_str()
        .context("voice_leave summary must be a string")?
        .trim();
    if summary.is_empty() {
        bail!("voice_leave summary must not be empty");
    }
    if summary.chars().count() > MAX_VOICE_LEAVE_SUMMARY_CHARS {
        bail!("voice_leave summary exceeds {MAX_VOICE_LEAVE_SUMMARY_CHARS} characters");
    }
    Ok(summary.to_owned())
}

async fn invoke_parent(
    endpoint: &Path,
    method: &str,
    args: Value,
) -> std::result::Result<Value, Value> {
    let mut stream = UnixStream::connect(endpoint).await.map_err(|error| {
        ipc_error(format!(
            "connect to voice bridge IPC socket {}: {error:#}",
            endpoint.display()
        ))
    })?;
    let request = json!({"jsonrpc":"2.0","id":1,"method":method,"params":args});
    let mut bytes = serde_json::to_vec(&request)
        .map_err(|error| ipc_error(format!("serialize Discord tool IPC request: {error}")))?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .await
        .map_err(|error| ipc_error(format!("send Discord tool request to bridge: {error:#}")))?;
    stream
        .flush()
        .await
        .map_err(|error| ipc_error(format!("flush Discord tool request: {error:#}")))?;

    let mut reader = BufReader::new(stream);
    let mut response = Vec::new();
    loop {
        let (consumed, found_newline) = {
            let available = reader.fill_buf().await.map_err(|error| {
                ipc_error(format!("read Discord tool response from bridge: {error:#}"))
            })?;
            if available.is_empty() {
                return Err(ipc_error(
                    "bridge closed Discord tool IPC before returning a result",
                ));
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
    let envelope: Value = serde_json::from_slice(&response)
        .map_err(|error| ipc_error(format!("parse Discord tool IPC response: {error}")))?;
    if envelope.get("id") != Some(&json!(1)) {
        return Err(ipc_error(format!(
            "Discord tool IPC response ID does not match request: {envelope}"
        )));
    }
    if let Some(error) = envelope.get("error") {
        return Err(error.clone());
    }
    envelope
        .get("result")
        .cloned()
        .ok_or_else(|| ipc_error("Discord tool IPC response has no result"))
}

fn tool_result(id: Value, result: std::result::Result<Value, Value>) -> Result<Value> {
    let (message, is_error) = match result {
        Ok(value) => (
            serde_json::to_string(&value)?,
            value.get("status").and_then(Value::as_str) == Some("partial"),
        ),
        Err(error) => (serde_json::to_string(&error)?, true),
    };
    Ok(json!({
        "jsonrpc":"2.0",
        "id":id,
        "result":{"content":[{"type":"text","text":message}],"isError":is_error}
    }))
}

fn ipc_error(message: impl Into<String>) -> Value {
    json!({"code":-32000,"message":message.into()})
}

async fn write_response<W>(writer: &mut W, response: &Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let bytes = serde_json::to_vec(response).context("serialize MCP response")?;
    writer
        .write_all(&bytes)
        .await
        .context("write MCP response")?;
    writer
        .write_all(b"\n")
        .await
        .context("write MCP response")?;
    writer.flush().await.context("flush MCP response")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_leave_summary_accepts_trimmed_nonempty_text() {
        assert_eq!(
            parse_leave_summary(&json!({"summary":"  오늘 이야기와 다음 계획  "})).unwrap(),
            "오늘 이야기와 다음 계획"
        );
    }

    #[test]
    fn parse_leave_summary_rejects_invalid_values() {
        assert!(parse_leave_summary(&json!({})).is_err());
        assert!(parse_leave_summary(&json!({"summary":"  "})).is_err());
        assert!(parse_leave_summary(&json!({"summary":null})).is_err());
        assert!(parse_leave_summary(&json!({"summary":"내용", "extra":true})).is_err());
        assert!(
            parse_leave_summary(&json!({"summary":"x".repeat(MAX_VOICE_LEAVE_SUMMARY_CHARS + 1)}))
                .is_err()
        );
    }
}

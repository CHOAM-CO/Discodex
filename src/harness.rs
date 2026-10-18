//! Discodex transport adapter for Codex CLI task/worktree tools.
//! MCP task metadata parsing follows openai/codex tui/src/dynamic_tools_mcp.rs.
//! Adapted from commit 8c076942cb70fba8e5c4732c23f1faabc4aed484 (Apache-2.0).
use anyhow::{Context, Result, bail};
use codex_uds::UnixStream;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, broadcast},
};

pub(crate) type Rpc = Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<Value>> + Send + Sync>;
#[derive(Clone)]
pub(crate) struct ToolContext {
    pub rpc: Rpc,
    pub events: broadcast::Sender<Value>,
    pub thread_id: String,
    pub turn_id: String,
    pub thread_start: Value,
}

pub(crate) fn definitions() -> Vec<Value> {
    let mut tools = crate::task_tools::definitions();
    tools.extend(crate::worktree_tools::definitions());
    tools
}

/// Preserve the caller supplied by Codex, including calls from background tasks.
pub(crate) fn caller_thread(params: &Value) -> Result<String> {
    let metadata = params.get("_meta").context("missing task metadata")?;
    let turn_metadata = metadata.get("x-codex-turn-metadata").and_then(|v| {
        if v.is_object() {
            Some(v.clone())
        } else {
            v.as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
        }
    });
    metadata
        .get("threadId")
        .and_then(Value::as_str)
        .or_else(|| turn_metadata.as_ref()?.get("thread_id")?.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .context("missing task metadata")
}

fn caller_turn(params: &Value) -> String {
    let metadata = params.get("_meta").unwrap_or(&Value::Null);
    let turn_metadata = metadata.get("x-codex-turn-metadata").and_then(|v| {
        if v.is_object() {
            Some(v.clone())
        } else {
            v.as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
        }
    });
    metadata
        .get("turnId")
        .and_then(Value::as_str)
        .or_else(|| turn_metadata.as_ref()?.get("turn_id")?.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("mcp-turn-{}", uuid::Uuid::new_v4()))
}

fn call_envelope(params: &Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .context("missing tool name")?;
    if !crate::task_tools::handles(name) && !crate::worktree_tools::handles(name) {
        bail!("Unknown task/worktree tool: {name}");
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        bail!("Tool arguments must be an object");
    }
    Ok(
        json!({"threadId":caller_thread(params)?,"turnId":caller_turn(params),"tool":name,"arguments":arguments}),
    )
}

async fn invoke(endpoint: &Path, params: Value) -> Result<Value> {
    let mut stream = UnixStream::connect(endpoint)
        .await
        .context("connect task tool IPC")?;
    let request = json!({"jsonrpc":"2.0","id":1,"method":"harness/call","params":params});
    let mut bytes = serde_json::to_vec(&request)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        bail!("Task tool IPC closed before responding");
    }
    let response: Value = serde_json::from_str(&line)?;
    if let Some(error) = response.get("error") {
        bail!(
            "{}",
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Task tool failed")
        );
    }
    response
        .get("result")
        .cloned()
        .context("Task tool response has no result")
}

fn tool_result(id: Value, result: Result<Value>) -> Value {
    let (value, is_error) = match result {
        Ok(value) => (value, false),
        Err(error) => (json!({"error":format!("{error:#}")}), true),
    };
    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":value.to_string()}],"isError":is_error}})
}

async fn reply(endpoint: &Path, request: Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    Some(match method {
        "initialize" => json!({"jsonrpc":"2.0","id":id,"result":{
            "protocolVersion":"2025-06-18","capabilities":{"tools":{}},
            "serverInfo":{"name":"discodex-tasks","version":env!("CARGO_PKG_VERSION")}
        }}),
        "ping" | "shutdown" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
        "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools":definitions()}}),
        "tools/call" => {
            let result = match call_envelope(&params) {
                Ok(envelope) => invoke(endpoint, envelope).await,
                Err(error) => Err(error),
            };
            tool_result(id, result)
        }
        _ => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Method not found: {method}")}})
        }
    })
}

/// Independent calls must keep flowing while wait_threads waits for another task.
pub async fn run(endpoint: &Path) -> Result<()> {
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let output = Arc::new(Mutex::new(tokio::io::stdout()));
    let mut calls = tokio::task::JoinSet::new();
    while let Some(line) = input.next_line().await? {
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                let mut writer = output.lock().await;
                let mut bytes = serde_json::to_vec(
                    &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":error.to_string()}}),
                )?;
                bytes.push(b'\n');
                writer.write_all(&bytes).await?;
                writer.flush().await?;
                continue;
            }
        };
        if request.get("method").and_then(Value::as_str) == Some("exit") {
            break;
        }
        let endpoint = endpoint.to_owned();
        let output = Arc::clone(&output);
        calls.spawn(async move {
            if let Some(response) = reply(&endpoint, request).await {
                let mut bytes = serde_json::to_vec(&response)?;
                bytes.push(b'\n');
                let mut writer = output.lock().await;
                writer.write_all(&bytes).await?;
                writer.flush().await?;
            }
            Ok::<(), anyhow::Error>(())
        });
        while let Some(result) = calls.try_join_next() {
            result??;
        }
    }
    // EOF means the owning app-server ended; don't retain its pending wait calls.
    calls.abort_all();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caller_metadata_preserves_background_thread_and_both_official_encodings() {
        assert_eq!(
            caller_thread(&json!({"_meta":{"threadId":"child"}})).unwrap(),
            "child"
        );
        assert_eq!(
            caller_thread(&json!({"_meta":{"x-codex-turn-metadata":{"thread_id":"other"}}}))
                .unwrap(),
            "other"
        );
        assert_eq!(
            caller_thread(
                &json!({"_meta":{"x-codex-turn-metadata":"{\"thread_id\":\"string-child\"}"}})
            )
            .unwrap(),
            "string-child"
        );
        assert!(caller_thread(&json!({"arguments":{"threadId":"untrusted-argument"}})).is_err());
    }
    #[test]
    fn caller_turn_accepts_both_metadata_encodings() {
        assert_eq!(caller_turn(&json!({"_meta":{"turnId":"direct"}})), "direct");
        assert_eq!(
            caller_turn(&json!({"_meta":{"x-codex-turn-metadata":{"turn_id":"nested"}}})),
            "nested"
        );
        assert_eq!(
            caller_turn(&json!({"_meta":{"x-codex-turn-metadata":"{\"turn_id\":\"encoded\"}"}})),
            "encoded"
        );
        assert_ne!(caller_turn(&json!({})), caller_turn(&json!({})));
    }
    #[test]
    fn tool_envelope_keeps_target_arguments_separate_from_caller() {
        let envelope=call_envelope(&json!({"name":"read_thread","arguments":{"threadId":"target"},"_meta":{"threadId":"caller","turnId":"caller-turn"}})).unwrap();
        assert_eq!(
            envelope,
            json!({"threadId":"caller","turnId":"caller-turn","tool":"read_thread","arguments":{"threadId":"target"}})
        );
        assert!(call_envelope(&json!({"name":"unknown","_meta":{"threadId":"caller"}})).is_err());
    }
    #[test]
    fn mcp_failure_is_a_tool_error_and_success_keeps_result() {
        let value = tool_result(json!(7), Ok(json!({"operationId":"pending-op"})));
        assert_eq!(value["result"]["isError"], false);
        assert_eq!(
            serde_json::from_str::<Value>(value["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap(),
            json!({"operationId":"pending-op"})
        );
        assert_eq!(
            tool_result(json!(8), Err(anyhow::anyhow!("RPC unavailable")))["result"]["isError"],
            true
        );
    }
}

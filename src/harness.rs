//! Discodex MCP transports for Codex CLI task/worktree and native-agent tools.
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

type NativeBinding = crate::agy_mcp::NativeRunBinding;

fn bound_native_params(binding: &NativeBinding, mut params: Value) -> Value {
    params["discodexExternalRun"] = json!(binding);
    params
}

async fn invoke_native(
    endpoint: &Path,
    binding: &NativeBinding,
    method: &str,
    params: Value,
) -> Result<Value> {
    let mut stream = UnixStream::connect(endpoint)
        .await
        .context("connect native tool IPC")?;
    let request = json!({"jsonrpc":"2.0","id":1,"method":method,"params":bound_native_params(binding, params)});
    let mut bytes = serde_json::to_vec(&request)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        bail!("Native tool IPC closed before responding");
    }
    let response: Value = serde_json::from_str(&line)?;
    if let Some(error) = response.get("error") {
        bail!(
            "{}",
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Native tool failed")
        );
    }
    response
        .get("result")
        .cloned()
        .context("Native tool response has no result")
}

fn function_specs(specs: &[Value]) -> Vec<Value> {
    let mut result = Vec::new();
    for spec in specs {
        match spec.get("type").and_then(Value::as_str) {
            Some("function" | "custom") => result.push(spec.clone()),
            None if spec.get("name").is_some()
                && (spec.get("inputSchema").is_some() || spec.get("parameters").is_some()) =>
            {
                result.push(spec.clone())
            }
            Some("namespace") => {
                let namespace = spec.get("name").and_then(Value::as_str).unwrap_or_default();
                if let Some(tools) = spec.get("tools").and_then(Value::as_array) {
                    for tool in tools {
                        if matches!(
                            tool.get("type").and_then(Value::as_str),
                            Some("function" | "custom")
                        ) {
                            let mut function = tool.clone();
                            if let Some(object) = function.as_object_mut() {
                                object.insert("namespace".into(), json!(namespace));
                            }
                            result.push(function);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    result
}

fn mcp_tool_specs(specs: &[Value]) -> Vec<Value> {
    function_specs(specs)
        .into_iter()
        .filter_map(|spec| {
            let name = spec.get("name")?.as_str()?;
            let namespace = spec.get("namespace").and_then(Value::as_str);
            if is_ordinary_execution_overlap(namespace, name) {
                return None;
            }
            let custom = spec.get("type").and_then(Value::as_str) == Some("custom");
            let exposed_name = match (namespace, custom) {
                (Some(namespace), true) if !namespace.is_empty() => format!("{namespace}__custom__{name}"),
                (Some(namespace), false) if !namespace.is_empty() => format!("{namespace}__{name}"),
                (None, true) => format!("custom__{name}"),
                _ => name.to_owned(),
            };
            let input_schema = if custom {
                json!({
                    "type":"object",
                    "properties":{"input":{"type":"string","description":spec.pointer("/format/definition").and_then(Value::as_str).unwrap_or("Text input for this tool.")}},
                    "required":["input"],
                    "additionalProperties":false
                })
            } else {
                spec.get("inputSchema")
                    .or_else(|| spec.get("parameters"))
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object","properties":{}}))
            };
            Some(json!({
                "name": exposed_name,
                "description": spec.get("description").cloned().unwrap_or_else(|| json!("")),
                "inputSchema": input_schema,
                "_meta": {"discodex/nativeNamespace": namespace}
            }))
        })
        .collect()
}

fn combined_tool_specs(native_specs: &[Value]) -> Vec<Value> {
    let mut tools = mcp_tool_specs(native_specs);
    let mut names: std::collections::HashSet<String> = tools
        .iter()
        .filter_map(|tool| tool.get("name")?.as_str().map(str::to_owned))
        .collect();
    names.extend(
        function_specs(native_specs)
            .iter()
            .filter_map(|spec| spec.get("name")?.as_str().map(str::to_owned)),
    );
    let mut host_tools = crate::task_tools::definitions();
    host_tools.extend(crate::worktree_tools::definitions());
    host_tools.extend(crate::discord_tools::tool_definitions(
        crate::i18n::Language::default(),
    ));
    for tool in host_tools {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            continue;
        };
        if names.insert(name.to_owned()) {
            tools.push(tool);
        }
    }
    tools
}

/// AGY owns ordinary local execution and browsing. Retain the narrow upstream
/// Codex capability surface that adds task coordination, user interaction,
/// planning, clock, and app-specific functionality. Unknown future tools stay
/// visible so new native capabilities are not silently hidden.
fn is_ordinary_execution_overlap(namespace: Option<&str>, name: &str) -> bool {
    if matches!(namespace, Some("multi_agent_v1" | "codex_tui" | "clock"))
        || matches!(name, "update_plan" | "request_user_input")
    {
        return false;
    }
    matches!(
        name,
        "exec"
            | "exec_command"
            | "exec_command_output"
            | "write_stdin"
            | "apply_patch"
            | "read_file"
            | "write_file"
            | "search"
            | "file_search"
            | "find_files"
            | "web_search"
            | "web_fetch"
            | "browser"
            | "view_image"
    )
}

fn native_call_envelope(binding: &NativeBinding, params: &Value) -> Result<Value> {
    let exposed_name = params
        .get("name")
        .and_then(Value::as_str)
        .context("missing native tool name")?;
    let (namespace, name, custom) = if let Some(name) = exposed_name.strip_prefix("custom__") {
        (None, name, true)
    } else if let Some((namespace, rest)) = exposed_name.split_once("__") {
        if let Some(name) = rest.strip_prefix("custom__") {
            (Some(namespace), name, true)
        } else {
            (Some(namespace), rest, false)
        }
    } else {
        (None, exposed_name, false)
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let arguments = if custom {
        arguments
            .get("input")
            .and_then(Value::as_str)
            .map(|input| json!(input))
            .context("Native custom tool requires string `input`")?
    } else {
        if !arguments.is_object() {
            bail!("Native function arguments must be an object");
        }
        arguments
    };
    Ok(json!({
        "threadId": binding.thread_id,
        "turnId": binding.turn_id,
        "runId": binding.run_id,
        "callId": format!("agy-mcp-{}", uuid::Uuid::new_v4()),
        "discodexMcpCall": {"name": exposed_name, "arguments": params.get("arguments").cloned().unwrap_or_else(|| json!({}))},
        "namespace": namespace,
        "name": name,
        "arguments": arguments
    }))
}

fn native_output_content(output: &Value) -> Vec<Value> {
    let output = if matches!(
        output.get("type").and_then(Value::as_str),
        Some("function_call_output" | "custom_tool_call_output")
    ) {
        output.get("output").unwrap_or(output)
    } else {
        output
    };
    if let Some(text) = output.as_str() {
        return vec![json!({"type":"text","text":text})];
    }
    if let Some(content) = output
        .as_array()
        .or_else(|| output.get("content").and_then(Value::as_array))
    {
        return content
            .iter()
            .map(|block| {
                match block.get("type").and_then(Value::as_str) {
                    Some("input_text") => block
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|text| json!({"type":"text","text":text})),
                    Some("input_image") => block
                        .get("image_url")
                        .and_then(Value::as_str)
                        .and_then(|url| native_media_content("image", url)),
                    Some("input_audio") => block
                        .get("audio_url")
                        .and_then(Value::as_str)
                        .and_then(|url| native_media_content("audio", url)),
                    Some("text" | "image" | "audio" | "resource" | "resource_link") => {
                        Some(block.clone())
                    }
                    _ => None,
                }
                .unwrap_or_else(|| json!({"type":"text","text":block.to_string()}))
            })
            .collect();
    }
    vec![json!({"type":"text","text":output.to_string()})]
}

fn native_media_content(kind: &str, url: &str) -> Option<Value> {
    use base64::Engine;
    let (header, data) = url.strip_prefix("data:")?.split_once(',')?;
    let mime = header.split(';').next()?;
    let bytes = if header.ends_with(";base64") {
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()?
    } else {
        percent_encoding::percent_decode_str(data).collect()
    };
    Some(
        json!({"type":kind,"mimeType":mime,"data":base64::engine::general_purpose::STANDARD.encode(bytes)}),
    )
}

fn native_tool_result(id: Value, result: Result<Value>) -> Value {
    match result {
        Ok(value) => {
            let output = value.get("output").unwrap_or(&value);
            json!({"jsonrpc":"2.0","id":id,"result":{"content":native_output_content(output),"isError":value.get("isError").and_then(Value::as_bool).unwrap_or(false)}})
        }
        Err(error) => {
            json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":format!("{error:#}")}],"isError":true}})
        }
    }
}

async fn reply_native(endpoint: &Path, binding: &NativeBinding, request: Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    Some(match method {
        "initialize" => json!({"jsonrpc":"2.0","id":id,"result":{
            "protocolVersion":"2025-06-18","capabilities":{"tools":{}},
            "serverInfo":{"name":"discodex-native-tools","version":env!("CARGO_PKG_VERSION")}
        }}),
        "ping" | "shutdown" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
        "tools/list" => {
            let result = invoke_native(
                endpoint,
                binding,
                "thread/tool/list",
                json!({"threadId":binding.thread_id,"turnId":binding.turn_id,"runId":binding.run_id}),
            )
            .await;
            match result {
                Ok(value) => {
                    let specs = value
                        .get("tools")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    json!({"jsonrpc":"2.0","id":id,"result":{"tools":combined_tool_specs(&specs)}})
                }
                Err(error) => {
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":format!("{error:#}")}})
                }
            }
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let result = if crate::task_tools::handles(name) || crate::worktree_tools::handles(name)
            {
                if !arguments.is_object() {
                    Err(anyhow::anyhow!("Task/worktree arguments must be an object"))
                } else {
                    invoke_native(
                        endpoint,
                        binding,
                        "harness/call",
                        json!({"threadId":binding.thread_id,"turnId":binding.turn_id,"tool":name,"arguments":arguments}),
                    )
                    .await
                }
            } else if is_discord_host_tool(name) {
                if !arguments.is_object() {
                    Err(anyhow::anyhow!("Discord arguments must be an object"))
                } else {
                    invoke_native(endpoint, binding, name, arguments).await
                }
            } else {
                match native_call_envelope(binding, &params) {
                    Ok(envelope) => {
                        invoke_native(endpoint, binding, "thread/tool/call", envelope).await
                    }
                    Err(error) => Err(error),
                }
            };
            native_tool_result(id, result)
        }
        _ => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Method not found: {method}")}})
        }
    })
}

fn is_discord_host_tool(name: &str) -> bool {
    crate::discord_tools::tool_definitions(crate::i18n::Language::default())
        .iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
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
    run_server(endpoint, None).await
}

/// Serve Codex's native agent-tool catalog to AGY through a standard MCP
/// stdio transport. Thread identity is bound by the spawning application and
/// never accepted from the model's tool arguments.
pub async fn run_native(endpoint: &Path, binding: NativeBinding) -> Result<()> {
    run_server(endpoint, Some(binding)).await
}

async fn run_server(endpoint: &Path, native: Option<NativeBinding>) -> Result<()> {
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
        let native = native.clone();
        let output = Arc::clone(&output);
        calls.spawn(async move {
            let response = match native.as_ref() {
                Some(binding) => reply_native(&endpoint, binding, request).await,
                None => reply(&endpoint, request).await,
            };
            if let Some(response) = response {
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

    fn native_binding(thread_id: &str) -> NativeBinding {
        NativeBinding {
            thread_id: thread_id.into(),
            turn_id: format!("{thread_id}-turn"),
            run_id: format!("{thread_id}-run"),
            generation: 3,
        }
    }

    #[test]
    fn every_native_ipc_envelope_keeps_the_spawned_response_binding() {
        let binding = native_binding("thread-a");
        let input = json!({"name":"example","arguments":{"threadId":"target"},"discodexExternalRun":{"threadId":"model-supplied"}});
        let output = bound_native_params(&binding, input.clone());
        assert_eq!(
            output["discodexExternalRun"],
            json!({
                "threadId":"thread-a","turnId":"thread-a-turn","runId":"thread-a-run","generation":3
            })
        );
        let decoded: NativeBinding =
            serde_json::from_value(output["discodexExternalRun"].clone()).unwrap();
        assert_eq!(decoded, binding);
        assert_eq!(output["arguments"], input["arguments"]);
        assert_eq!(
            input["discodexExternalRun"],
            json!({"threadId":"model-supplied"})
        );
    }
    #[test]
    fn native_tool_schemas_flatten_namespaces_for_mcp() {
        let tools = mcp_tool_specs(&[
            json!({"type":"function","name":"read","description":"Read","inputSchema":{"type":"object"}}),
            json!({"type":"namespace","name":"codex","tools":[{"type":"function","name":"wait_threads","description":"Wait","parameters":{"type":"object","properties":{"timeoutMs":{"type":"integer"}}}}]}),
        ]);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "read");
        assert_eq!(tools[0]["inputSchema"]["type"], "object");
        assert_eq!(tools[1]["name"], "codex__wait_threads");
        assert_eq!(
            tools[1]["inputSchema"]["properties"]["timeoutMs"]["type"],
            "integer"
        );
    }

    #[test]
    fn catalog_prefers_agy_ordinary_tools_and_keeps_unknown_native_capabilities() {
        let tools = mcp_tool_specs(&[
            json!({"type":"function","name":"exec_command","description":"shell","parameters":{"type":"object"}}),
            json!({"type":"function","name":"future_native_capability","description":"new","parameters":{"type":"object"}}),
            json!({"type":"namespace","name":"multi_agent_v1","tools":[{"type":"function","name":"spawn_agent","description":"delegate","parameters":{"type":"object"}}]}),
        ]);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "future_native_capability");
        assert_eq!(tools[1]["name"], "multi_agent_v1__spawn_agent");
    }

    #[test]
    fn native_catalog_exposes_collaboration_tools_alongside_codex_guidance() {
        let native_specs = [json!({
            "type":"namespace",
            "name":"multi_agent_v1",
            "tools":[
                {"type":"function","name":"spawn_agent","description":"Start a parallel task","parameters":{"type":"object","properties":{"task_name":{"type":"string"}}}},
                {"type":"function","name":"wait_agent","description":"Wait for a task","parameters":{"type":"object","properties":{"task_name":{"type":"string"}}}}
            ]
        })];
        let output = combined_tool_specs(&native_specs);
        let names: Vec<&str> = output
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();

        assert!(names.contains(&"multi_agent_v1__spawn_agent"));
        assert!(names.contains(&"multi_agent_v1__wait_agent"));
        assert_eq!(
            output
                .iter()
                .find(|tool| tool["name"] == "multi_agent_v1__spawn_agent")
                .unwrap()["inputSchema"]["properties"]["task_name"]["type"],
            "string"
        );
        assert_eq!(
            output
                .iter()
                .find(|tool| tool["name"] == "multi_agent_v1__wait_agent")
                .unwrap()["inputSchema"]["properties"]["task_name"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn native_mcp_tools_list_returns_collaboration_specs() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = directory.path().join("native-tools.sock");
        let mut listener = codex_uds::UnixListener::bind(&endpoint).await.unwrap();
        let server = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "thread/tool/list");
            assert_eq!(request["params"]["threadId"], "thread-fixture");
            assert_eq!(request["params"]["runId"], "thread-fixture-run");
            let specs = json!([{
                "type":"namespace",
                "name":"multi_agent_v1",
                "tools":[
                    {"type":"function","name":"spawn_agent","description":"Start a parallel task","parameters":{"type":"object","properties":{"task_name":{"type":"string"}}}},
                    {"type":"function","name":"wait_agent","description":"Wait for a task","parameters":{"type":"object","properties":{"task_name":{"type":"string"}}}}
                ]
            }]);
            let mut response = serde_json::to_vec(&json!({
                "jsonrpc":"2.0","id":request["id"],"result":{"tools":specs}
            }))
            .unwrap();
            response.push(b'\n');
            stream.get_mut().write_all(&response).await.unwrap();
            stream.get_mut().flush().await.unwrap();
        });

        let response = reply_native(
            &endpoint,
            &native_binding("thread-fixture"),
            json!({"jsonrpc":"2.0","id":"mcp-1","method":"tools/list","params":{}}),
        )
        .await
        .unwrap();
        server.await.unwrap();

        let tools = response
            .pointer("/result/tools")
            .unwrap()
            .as_array()
            .unwrap();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"multi_agent_v1__spawn_agent"));
        assert!(names.contains(&"multi_agent_v1__wait_agent"));
    }

    #[test]
    fn combined_catalog_adds_host_tools_without_enabling_agy_voice() {
        let tools = combined_tool_specs(&[
            json!({"type":"function","name":"native_extra","description":"extra","parameters":{"type":"object"}}),
            json!({"type":"namespace","name":"codex_tui","tools":[{"type":"function","name":"list_threads","description":"native list","parameters":{"type":"object"}}]}),
        ]);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"native_extra"));
        assert!(names.contains(&"codex_tui__list_threads"));
        assert!(!names.contains(&"list_threads"));
        assert!(names.contains(&"discord_send_message"));
        assert!(!names.contains(&"voice_join"));
        assert!(!names.contains(&"voice_leave"));
    }

    #[test]
    fn native_call_binds_trusted_thread_and_preserves_namespace() {
        let binding = native_binding("trusted-thread");
        let envelope = native_call_envelope(
            &binding,
            &json!({"name":"codex__send_message_to_thread","arguments":{"threadId":"target"}}),
        )
        .unwrap();
        assert_eq!(envelope["threadId"], "trusted-thread");
        assert_eq!(envelope["namespace"], "codex");
        assert_eq!(envelope["name"], "send_message_to_thread");
        assert_eq!(envelope["arguments"]["threadId"], "target");
        assert!(native_call_envelope(&binding, &json!({"name":"read","arguments":[]})).is_err());
    }

    #[test]
    fn native_calls_from_two_parents_keep_binding_and_mcp_response_identity() {
        let arguments =
            json!({"threadId":"target-child","turnId":"tool-argument-turn","message":"continue"});
        let request = json!({
            "name":"agent__send_message", "threadId":"model-supplied-parent",
            "turnId":"model-supplied-turn", "arguments":arguments
        });
        for (parent, id) in [("parent-a", json!(7)), ("parent-b", json!("7"))] {
            let envelope = native_call_envelope(&native_binding(parent), &request).unwrap();
            assert_eq!(envelope["threadId"], parent);
            assert_eq!(envelope["turnId"], format!("{parent}-turn"));
            assert_eq!(envelope["runId"], format!("{parent}-run"));
            assert_eq!(envelope["namespace"], "agent");
            assert_eq!(envelope["name"], "send_message");
            assert_eq!(envelope["arguments"], arguments);
            let success = native_tool_result(
                id.clone(),
                Ok(json!({"output":{
                "type":"function_call_output", "call_id":envelope["callId"], "output":"delivered"
            }, "isError":false})),
            );
            assert_eq!(
                success,
                json!({"jsonrpc":"2.0","id":id,"result":{
                    "content":[{"type":"text","text":"delivered"}],"isError":false
                }})
            );
            let error = anyhow::anyhow!("target is unavailable").context("native tool failed");
            let failure = native_tool_result(id.clone(), Err(error));
            assert_eq!(
                failure,
                json!({"jsonrpc":"2.0","id":id,"result":{
                    "content":[{"type":"text","text":"native tool failed: target is unavailable"}],"isError":true
                }})
            );
        }
    }

    #[test]
    fn native_custom_tools_use_a_text_input_and_keep_freeform_payloads() {
        let spec = json!({
            "type":"namespace",
            "name":"agent",
            "tools":[{"type":"custom","name":"run","description":"run code","format":{"type":"text","syntax":"plain","definition":"Program text"}}]
        });
        let tools = mcp_tool_specs(&[spec]);
        assert_eq!(tools[0]["name"], "agent__custom__run");
        assert_eq!(
            tools[0]["inputSchema"]["properties"]["input"]["type"],
            "string"
        );
        let envelope = native_call_envelope(
            &native_binding("thread"),
            &json!({"name":"agent__custom__run","arguments":{"input":"print(1)"}}),
        )
        .unwrap();
        assert_eq!(envelope["namespace"], "agent");
        assert_eq!(envelope["name"], "run");
        assert_eq!(envelope["arguments"], "print(1)");
        let unnamespaced = native_call_envelope(
            &native_binding("thread"),
            &json!({"name":"custom__run","arguments":{"input":"print(2)"}}),
        )
        .unwrap();
        assert!(unnamespaced["namespace"].is_null());
        assert_eq!(unnamespaced["name"], "run");
        assert_eq!(unnamespaced["arguments"], "print(2)");
    }

    #[test]
    fn native_results_preserve_text_and_mark_failures() {
        let success = native_tool_result(
            json!(1),
            Ok(json!({"output":{"content":[{"type":"text","text":"done"}]}})),
        );
        assert_eq!(success["result"]["isError"], false);
        assert_eq!(success["result"]["content"][0]["text"], "done");
        let failure = native_tool_result(json!(2), Err(anyhow::anyhow!("inactive turn")));
        assert_eq!(failure["result"]["isError"], true);
        assert!(
            failure["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("inactive turn")
        );
    }

    #[test]
    fn native_response_items_keep_mixed_media_and_error_status_in_mcp_results() {
        let result = native_tool_result(
            json!(3),
            Ok(json!({"isError":true,"output":{
            "type":"function_call_output","call_id":"tool-call","output":[
                {"type":"input_text","text":"visible result"},
                {"type":"input_image","image_url":"data:image/png;base64,AQID"},
                {"type":"input_audio","audio_url":"data:audio/wav,%01%02"},
                {"type":"input_image","file_id":"unresolved-reference"}
            ]}})),
        );
        let content = result["result"]["content"].as_array().unwrap();
        assert_eq!(result["result"]["isError"], true);
        assert_eq!(content.len(), 4);
        assert_eq!(content[0], json!({"type":"text","text":"visible result"}));
        assert_eq!(
            content[1],
            json!({"type":"image","mimeType":"image/png","data":"AQID"})
        );
        assert_eq!(
            content[2],
            json!({"type":"audio","mimeType":"audio/wav","data":"AQI="})
        );
        let reference: Value = serde_json::from_str(content[3]["text"].as_str().unwrap()).unwrap();
        assert_eq!(reference["file_id"], "unresolved-reference");
        assert_eq!(
            native_output_content(
                &json!({"type":"custom_tool_call_output","call_id":"c","output":"done"})
            ),
            vec![json!({"type":"text","text":"done"})]
        );
    }

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

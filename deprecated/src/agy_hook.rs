//! Official AGY hook entrypoint. Its stdout is exclusively the hook JSON reply.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::agy_mcp::NativeRunBinding;
use crate::i18n::{Language, MessageKey};

pub const INSTRUCTIONS_ENV: &str = "DISCODEX_AGY_INSTRUCTIONS";
pub(crate) const MANAGED_HOOK: &str = "discodex-native-run-v1";

#[derive(Deserialize, Serialize)]
struct RunInstructions {
    binding: NativeRunBinding,
    instructions: String,
}

/// The run owns this file until CLI exit; each invocation reads the same snapshot.
pub(crate) fn stage_instructions(
    binding: &NativeRunBinding,
    instructions: &str,
) -> Result<tempfile::NamedTempFile> {
    let mut file = tempfile::Builder::new()
        .prefix("discodex-agy-instructions-")
        .suffix(".json")
        .tempfile()?;
    serde_json::to_writer(
        &mut file,
        &RunInstructions {
            binding: binding.clone(),
            instructions: instructions.to_owned(),
        },
    )?;
    file.flush()?;
    Ok(file)
}

pub fn run(args: &[String]) -> Result<()> {
    let option = |name: &str| -> Result<&str> {
        let index = args.iter().position(|arg| arg == name).with_context(|| {
            Language::English
                .message(MessageKey::AgyProtocolHookOptionRequired)
                .replace("{option}", name)
        })?;
        args.get(index + 1).map(String::as_str).with_context(|| {
            Language::English
                .message(MessageKey::AgyProtocolHookOptionValue)
                .replace("{option}", name)
        })
    };
    let language: Language = option("--language")?.parse()?;
    let event = option("--event")?;
    let binding: NativeRunBinding = serde_json::from_str(
        &std::env::var(crate::agy_mcp::NATIVE_RUN_ENV)
            .context(language.message(MessageKey::AgyProtocolHookBinding))?,
    )
    .context(language.message(MessageKey::AgyProtocolHookBinding))?;
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context(language.message(MessageKey::AgyProtocolHookInput))?;
    let input: Value =
        serde_json::from_str(&input).context(language.message(MessageKey::AgyProtocolHookInput))?;
    let response = match event {
        "PreInvocation" => {
            let path = PathBuf::from(
                std::env::var_os(INSTRUCTIONS_ENV)
                    .context(language.message(MessageKey::AgyProtocolHookInstructions))?,
            );
            let instructions: RunInstructions =
                serde_json::from_slice(&std::fs::read(&path).with_context(|| {
                    format!(
                        "{}: {}",
                        language.message(MessageKey::AgyProtocolHookInstructions),
                        path.display()
                    )
                })?)
                .context(language.message(MessageKey::AgyProtocolHookInstructions))?;
            anyhow::ensure!(
                instructions.binding == binding,
                "{}",
                language.message(MessageKey::AgyProtocolHookBinding)
            );
            json!({"injectSteps":[{"ephemeralMessage":instructions.instructions}]})
        }
        "PreToolUse" => {
            let name = input
                .pointer("/toolCall/name")
                .and_then(Value::as_str)
                .context(language.message(MessageKey::AgyProtocolHookToolName))?;
            if matches!(
                name,
                "invoke_subagent" | "define_subagent" | "send_message" | "manage_subagents"
            ) {
                json!({"decision":"deny", "reason":language.message(MessageKey::AgyProtocolNativeDelegationDenied)})
            } else {
                json!({"decision":"allow"})
            }
        }
        _ => bail!(
            "{}",
            language
                .message(MessageKey::AgyProtocolHookEventUnknown)
                .replace("{event}", event)
        ),
    };
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, &response)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn command(worker: &Path, event: &str, language: Language) -> Result<String> {
    let worker = worker
        .to_str()
        .context(language.message(MessageKey::AgySettingsHookExecutable))?;
    #[cfg(not(windows))]
    let worker = format!("'{}'", worker.replace('\'', "'\"'\"'"));
    #[cfg(windows)]
    let worker = {
        anyhow::ensure!(
            !worker.contains(['"', '\r', '\n']),
            "{}",
            language.message(MessageKey::AgySettingsHookExecutable)
        );
        format!("\"{worker}\"")
    };
    Ok(format!(
        "{worker} agy-hook --event {event} --language {}",
        language.code()
    ))
}

// SPDX-License-Identifier: Apache-2.0
//! Managed worktree MCP tools adapted from the official Codex TUI implementation.

use crate::harness::ToolContext;
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::Mutex, time::Instant};
use uuid::Uuid;

const NAMES: [&str; 3] = [
    "create_worktree",
    "get_worktree_creation_status",
    "list_worktrees",
];
fn tool_language() -> crate::i18n::Language {
    std::env::var("DISCODEX_LANGUAGE")
        .ok()
        .and_then(|value| value.parse::<crate::i18n::Language>().ok())
        .unwrap_or_default()
}

fn localized_detail(key: crate::i18n::MessageKey, detail: &str) -> String {
    localized_detail_with_language(key, detail, tool_language())
}

fn localized_detail_with_language(
    key: crate::i18n::MessageKey,
    detail: &str,
    language: crate::i18n::Language,
) -> String {
    let detail = crate::discord::sanitize_discord_diagnostic(detail);
    language.message(key).replace("{detail}", &detail)
}

pub(crate) fn handles(name: &str) -> bool {
    NAMES.contains(&name)
}
pub(crate) fn definitions() -> Vec<Value> {
    definitions_with_language(tool_language())
}
fn definitions_with_language(language: crate::i18n::Language) -> Vec<Value> {
    [
        ("create_worktree", crate::i18n::MessageKey::WorktreeToolsCreateDescription, json!({"ref":{"type":"string"},"allowAsync":{"type":"boolean","const":true}}), vec!["allowAsync"]),
        ("get_worktree_creation_status", crate::i18n::MessageKey::WorktreeToolsStatusDescription, json!({"operationId":{"type":"string","minLength":1}}), vec!["operationId"]),
        ("list_worktrees", crate::i18n::MessageKey::WorktreeToolsListDescription, json!({"cursor":{"type":"string"}}), vec![]),
    ].into_iter().map(|(name,description,properties,required)| json!({"name":name,"description":language.message(description),"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).collect()
}
#[derive(Clone)]
pub(crate) struct WorktreeTools {
    codex_home: PathBuf,
    backend: Arc<dyn WorktreeBackend>,
    operations: Arc<Mutex<HashMap<String, Operation>>>,
}
trait WorktreeBackend: Send + Sync {
    fn create(
        &self,
        root: &Path,
        cwd: &Path,
        revision: Option<&str>,
        owner: &str,
    ) -> Result<Checkout>;
}
struct GitWorktreeBackend;
impl WorktreeBackend for GitWorktreeBackend {
    fn create(
        &self,
        root: &Path,
        cwd: &Path,
        revision: Option<&str>,
        owner: &str,
    ) -> Result<Checkout> {
        create_checkout(root, cwd, revision, owner)
    }
}
struct Operation {
    thread_id: String,
    started: Instant,
    result: Option<std::result::Result<Value, String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    #[serde(rename = "ref")]
    revision: Option<String>,
    #[serde(rename = "allowAsync")]
    allow_async: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StatusArgs {
    operation_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    cursor: Option<String>,
}

impl WorktreeTools {
    pub(crate) fn new(codex_home: PathBuf) -> Result<Self> {
        Ok(Self {
            codex_home,
            backend: Arc::new(GitWorktreeBackend),
            operations: Arc::default(),
        })
    }
    pub(crate) async fn execute(&self, ctx: ToolContext, name: &str, args: Value) -> Result<Value> {
        match name {
            "get_worktree_creation_status" => {
                let a: StatusArgs = serde_json::from_value(args)?;
                let operations = self.operations.lock().await;
                let op = operations
                    .get(&a.operation_id)
                    .filter(|op| op.thread_id == ctx.thread_id)
                    .ok_or_else(|| {
                        anyhow!(
                            tool_language()
                                .message(crate::i18n::MessageKey::WorktreeToolsOperationUnknown)
                        )
                    })?;
                match &op.result {
                    Some(Ok(v)) => Ok(v.clone()),
                    Some(Err(e)) => bail!(
                        "{}",
                        localized_detail(crate::i18n::MessageKey::WorktreeToolsOperationFailed, e)
                    ),
                    None => Ok(json!({"type":"pending","operationId":a.operation_id})),
                }
            }
            "list_worktrees" => {
                let a: ListArgs = serde_json::from_value(args)?;
                let page = rpc(
                    &ctx,
                    "thread/attachment/list",
                    json!({"threadId":ctx.thread_id,"cursor":a.cursor}),
                )
                .await?;
                let mut data = Vec::new();
                for item in page
                    .get("data")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let kind = item
                        .get("attachmentType")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !matches!(kind, "worktree" | "archived_worktree") {
                        continue;
                    }
                    let p = if kind == "archived_worktree" {
                        item.pointer("/payload/worktree").unwrap_or(&Value::Null)
                    } else {
                        item.get("payload").unwrap_or(&Value::Null)
                    };
                    data.push(json!({"id":item.get("id"),"type":kind,"root":p.get("root"),"workspaceRoot":p.get("workspaceRoot"),"sourceCwd":p.get("sourceCwd")}));
                }
                Ok(json!({"data":data,"nextCursor":page.get("nextCursor")}))
            }
            "create_worktree" => self.create(ctx, args).await,
            _ => bail!("unsupported worktree tool: {name}"),
        }
    }
    async fn create(&self, ctx: ToolContext, args: Value) -> Result<Value> {
        let a: CreateArgs = serde_json::from_value(args)?;
        if !a.allow_async {
            bail!("Use allowAsync: true")
        }
        let response = rpc(
            &ctx,
            "thread/read",
            json!({"threadId":ctx.thread_id,"includeTurns":false}),
        )
        .await?;
        let thread = response.get("thread").unwrap_or(&response);
        if thread.get("ephemeral").and_then(Value::as_bool) == Some(true) {
            bail!(
                "Ephemeral side conversations cannot retain worktree attachments. Request a worktree in the main conversation."
            )
        }
        let envs = thread
            .get("environments")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("Server cannot verify the task's execution environment"))?;
        if envs.is_empty()
            || envs
                .iter()
                .any(|e| e.get("environmentId").and_then(Value::as_str) != Some("local"))
        {
            bail!("Managed worktree tools require a local execution environment")
        }
        let cwd = PathBuf::from(
            envs[0]
                .get("cwd")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Thread environment omitted cwd"))?,
        );
        let _ = rpc(
            &ctx,
            "thread/attachment/list",
            json!({"threadId":ctx.thread_id,"limit":1}),
        )
        .await?;
        let config = rpc(&ctx, "config/read", json!({"includeLayers":true,"cwd":cwd})).await?;
        let root = config
            .pointer("/config/desktop/git-worktree-root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.codex_home.join("worktrees"));
        if !root.is_absolute() {
            bail!("desktop.git-worktree-root must be an absolute path")
        }
        let mut ops = self.operations.lock().await;
        ops.retain(|_, op| op.result.is_none() || op.started.elapsed() < Duration::from_secs(3600));
        if let Some((id, _)) = ops
            .iter()
            .find(|(_, op)| op.thread_id == ctx.thread_id && op.result.is_none())
        {
            return Ok(
                json!({"type":"pending","operationId":id,"message":tool_language().message(crate::i18n::MessageKey::WorktreeToolsCreationAlreadyRunning)}),
            );
        }
        let id = Uuid::new_v4().to_string();
        ops.insert(
            id.clone(),
            Operation {
                thread_id: ctx.thread_id.clone(),
                started: Instant::now(),
                result: None,
            },
        );
        drop(ops);
        let registry = Arc::clone(&self.operations);
        let backend = Arc::clone(&self.backend);
        let op_id = id.clone();
        let owner = ctx.thread_id.clone();
        let worker_owner = owner.clone();
        let rpc_call = Arc::clone(&ctx.rpc);
        tokio::spawn(async move {
            let supervisor_registry = Arc::clone(&registry);
            let supervisor_id = op_id.clone();
            let worker = tokio::spawn(async move {
                let created = tokio::task::spawn_blocking(move || {
                    backend.create(&root, &cwd, a.revision.as_deref(), &worker_owner)
                })
                .await
                .map_err(|e| {
                    localized_detail(
                        crate::i18n::MessageKey::WorktreeToolsCreationInterrupted,
                        &e.to_string(),
                    )
                })
                .and_then(|r| r.map_err(|e| format!("{e:#}")));
                let result = match created {
                    Ok(w) => {
                        let payload =
                            json!({"root":w.root,"workspaceRoot":w.cwd,"sourceCwd":w.source_cwd});
                        if let Some(error) = w.owner_error {
                            Ok(
                                json!({"type":"registrationFailed","worktree":payload,"message":localized_detail(crate::i18n::MessageKey::WorktreeToolsCheckoutRetainedOwnerFailed, &error)}),
                            )
                        } else {
                            let key = w.root.to_string_lossy().into_owned();
                            match rpc_call("thread/attachment/add".to_owned(),json!({"threadId":owner,"attachmentType":"worktree","identityKey":key,"payload":payload})).await{
                            Ok(_)=>Ok(json!({"type":"created","worktree":payload})),
                            Err(e)=>Ok(json!({"type":"registrationFailed","worktree":payload,"message":localized_detail(crate::i18n::MessageKey::WorktreeToolsCheckoutRetainedRegistrationFailed, &e.to_string())}))
                        }
                        }
                    }
                    Err(e) => Err(e),
                };
                result
            });
            let result = worker.await.unwrap_or_else(|e| {
                Err(localized_detail(
                    crate::i18n::MessageKey::WorktreeToolsCreationUncertain,
                    &e.to_string(),
                ))
            });
            if let Some(op) = supervisor_registry.lock().await.get_mut(&supervisor_id) {
                op.result = Some(result);
            }
        });
        Ok(
            json!({"type":"pending","operationId":id,"message":tool_language().message(crate::i18n::MessageKey::WorktreeToolsCreationRunning)}),
        )
    }
}
async fn rpc(ctx: &ToolContext, method: &str, params: Value) -> Result<Value> {
    (ctx.rpc)(method.to_owned(), params).await
}

struct Checkout {
    root: PathBuf,
    cwd: PathBuf,
    source_cwd: PathBuf,
    owner_error: Option<String>,
}
fn create_checkout(
    root: &Path,
    source_cwd: &Path,
    revision: Option<&str>,
    owner: &str,
) -> Result<Checkout> {
    let source_cwd = fs::canonicalize(source_cwd)
        .with_context(|| format!("cannot resolve {}", source_cwd.display()))?;
    let source_root = fs::canonicalize(PathBuf::from(
        git_text(&source_cwd, ["rev-parse", "--show-toplevel"])?.trim(),
    ))
    .context("cannot resolve repository root")?;
    let relative = source_cwd
        .strip_prefix(&source_root)
        .context("working directory is outside repository root")?;
    let repo_name = source_root
        .file_name()
        .context("repository root has no name")?;
    let base = match revision {
        Some(v) => v.to_owned(),
        None => default_base(&source_root)?,
    };
    let head = git_text(
        &source_root,
        [
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ],
    )?
    .trim()
    .to_owned();
    fs::create_dir_all(root)?;
    #[cfg(target_os = "macos")]
    fs::write(root.join(".metadata_never_index"), b"")?;
    let mut allocated = None;
    for _ in 0..=u16::MAX {
        let id = Uuid::new_v4().simple().to_string();
        let bucket = root.join(&id[..4]);
        match fs::create_dir(&bucket) {
            Ok(()) => {
                allocated = Some(bucket.join(repo_name));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context("create worktree bucket"),
        }
    }
    let worktree =
        allocated.ok_or_else(|| anyhow!("all managed worktree identifiers are in use"))?;
    let add_args = [
        OsStr::new("worktree"),
        OsStr::new("add"),
        OsStr::new("--detach"),
        OsStr::new("--no-checkout"),
        worktree.as_os_str(),
        OsStr::new(&head),
    ];
    if let Err(e) = git_text_os(&source_root, add_args) {
        if let Some(b) = worktree.parent() {
            let _ = fs::remove_dir(b);
        }
        return Err(e).context("cannot create managed worktree");
    }
    let populated = (|| -> Result<()> {
        let config = git_text(
            &worktree,
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "config.worktree",
            ],
        )?;
        git_text_os(
            &worktree,
            [
                OsStr::new("config"),
                OsStr::new("--file"),
                OsStr::new(config.trim()),
                OsStr::new("core.worktree"),
                worktree.as_os_str(),
            ],
        )?;
        git_text(
            &worktree,
            [
                "--work-tree=.",
                "reset",
                "--hard",
                "--no-recurse-submodules",
                &head,
            ],
        )?;
        Ok(())
    })();
    if let Err(error) = populated {
        return match remove_worktree(&source_root, &worktree) {
            Ok(()) => Err(error).context("cannot populate managed worktree"),
            Err(cleanup) => Err(anyhow!(
                "cannot populate managed worktree: {error:#}; rollback also failed: {cleanup:#}. Checkout retained at {}",
                worktree.display()
            )),
        };
    }
    let cwd = worktree.join(relative);
    if !is_safe_worktree_cwd(&worktree, &cwd) {
        let error = anyhow!(
            "requested base does not contain a safe working directory {}",
            relative.display()
        );
        return match remove_worktree(&source_root, &worktree) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(anyhow!(
                "{error:#}; rollback also failed: {cleanup:#}. Checkout retained at {}",
                worktree.display()
            )),
        };
    }
    let owner_error = bind_owner(&worktree, owner).err().map(|e| format!("{e:#}"));
    Ok(Checkout {
        root: worktree,
        cwd,
        source_cwd,
        owner_error,
    })
}
fn is_safe_worktree_cwd(root: &Path, cwd: &Path) -> bool {
    let Ok(root) = fs::canonicalize(root) else {
        return false;
    };
    fs::canonicalize(cwd).is_ok_and(|resolved| resolved.is_dir() && resolved.starts_with(&root))
}

fn default_base(cwd: &Path) -> Result<String> {
    let out = git_output(
        cwd,
        [
            "for-each-ref",
            "--format=%(refname) %(symref)",
            "refs/remotes",
            "refs/heads",
        ],
    )?;
    let mut refs = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((n, t)) = line.split_once(' ') {
            refs.push((n.to_owned(), t.to_owned()))
        }
    }
    select_default_base(&refs)
}
fn select_default_base(refs: &[(String, String)]) -> Result<String> {
    if let Some((_, target)) = refs
        .iter()
        .filter(|(name, target)| name.ends_with("/HEAD") && !target.is_empty())
        .min_by_key(|(name, _)| (!name.starts_with("refs/remotes/origin/"), name.clone()))
    {
        return Ok(target.clone());
    }
    for candidate in [
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
        "refs/heads/main",
        "refs/heads/master",
    ] {
        if refs.iter().any(|(name, _)| name == candidate) {
            return Ok(candidate.to_owned());
        }
    }
    bail!(
        "{}",
        tool_language().message(crate::i18n::MessageKey::WorktreeToolsDefaultBranchUnknown)
    )
}
fn owner_record_matches(value: &Value, thread: &str) -> bool {
    value.get("version").and_then(Value::as_u64) == Some(1)
        && value.get("ownerThreadId").and_then(Value::as_str) == Some(thread)
}

fn bind_owner(root: &Path, thread: &str) -> Result<()> {
    let meta = git_text(
        root,
        [
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "codex-thread.json",
        ],
    )?;
    let path = PathBuf::from(meta.trim());
    if path.exists() {
        let v: Value = serde_json::from_slice(&fs::read(&path)?)?;
        if owner_record_matches(&v, thread) {
            return Ok(());
        }
        bail!("worktree already has an owner or invalid ownership metadata")
    }
    let parent = path.parent().context("worktree metadata has no parent")?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut tmp, &json!({"version":1,"ownerThreadId":thread}))?;
    tmp.as_file_mut().sync_all()?;
    tmp.persist_noclobber(&path)
        .map_err(|e| e.error)
        .context("cannot write worktree ownership metadata")?;
    Ok(())
}
fn git_text<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<String> {
    String::from_utf8(git_output(cwd, args)?.stdout).context("git output is not valid UTF-8")
}
fn git_text_os<I, S>(cwd: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    String::from_utf8(git_output(cwd, args)?.stdout).context("git output is not valid UTF-8")
}
fn remove_worktree(source: &Path, root: &Path) -> Result<()> {
    git_text_os(
        source,
        [
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--force"),
            root.as_os_str(),
        ],
    )
    .map(|_| ())
}
fn base_git_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    for key in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_OBJECT_DIRECTORY",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_GRAFT_FILE",
        "GIT_INDEX_FILE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_REPLACE_REF_BASE",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
        "GIT_COMMON_DIR",
    ] {
        command.env_remove(key);
    }
    command
        .current_dir(cwd)
        .arg("-c")
        .arg("safe.bareRepository=explicit")
        .arg("-c")
        .arg(if cfg!(windows) {
            "core.hooksPath=NUL"
        } else {
            "core.hooksPath=/dev/null"
        })
        .arg("-c")
        .arg("core.fsmonitor=")
        .arg("-c")
        .arg("attr.tree=")
        .arg("-c")
        .arg("core.attributesFile=")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn configured_filters(cwd: &Path) -> Result<BTreeSet<String>> {
    let output = base_git_command(cwd)
        .args([
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^filter\.",
        ])
        .output()
        .context("failed to inspect Git filter configuration")?;
    if !output.status.success() {
        if output.status.code() == Some(1) {
            return Ok(BTreeSet::new());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git command failed: {}", stderr.trim());
    }
    let names = String::from_utf8(output.stdout).context("Git filter names are not valid UTF-8")?;
    let mut filters = BTreeSet::new();
    for name in names.split('\0') {
        if let Some((driver, attribute)) = name
            .strip_prefix("filter.")
            .and_then(|name| name.rsplit_once('.'))
            && !driver.is_empty()
            && matches!(attribute, "clean" | "smudge" | "process" | "required")
        {
            filters.insert(driver.to_owned());
        }
    }
    Ok(filters)
}

fn git_output<I, S>(cwd: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let filters = configured_filters(cwd)?;
    let mut command = base_git_command(cwd);
    let mut index = 0usize;
    for filter in filters {
        for attribute in ["process", "clean", "smudge"] {
            command.env(
                format!("GIT_CONFIG_KEY_{index}"),
                format!("filter.{filter}.{attribute}"),
            );
            command.env(format!("GIT_CONFIG_VALUE_{index}"), "");
            index += 1;
        }
        command.env(
            format!("GIT_CONFIG_KEY_{index}"),
            format!("filter.{filter}.required"),
        );
        command.env(format!("GIT_CONFIG_VALUE_{index}"), "false");
        index += 1;
    }
    if index > 0 {
        command.env("GIT_CONFIG_COUNT", index.to_string());
    }
    let output = command.args(args).output().context("failed to start git")?;
    if !output.status.success() {
        bail!(
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn worktree_tool_definitions_use_the_selected_locale() {
        let descriptions = definitions_with_language(crate::i18n::Language::Korean);
        assert_eq!(
            descriptions[0]["description"],
            crate::i18n::Language::Korean
                .message(crate::i18n::MessageKey::WorktreeToolsCreateDescription)
        );
        assert_eq!(
            descriptions[1]["description"],
            crate::i18n::Language::Korean
                .message(crate::i18n::MessageKey::WorktreeToolsStatusDescription)
        );
        assert_eq!(
            descriptions[2]["description"],
            crate::i18n::Language::Korean
                .message(crate::i18n::MessageKey::WorktreeToolsListDescription)
        );
    }

    #[test]
    fn localized_worktree_diagnostic_keeps_context_and_redacts_credentials() {
        assert_eq!(
            localized_detail_with_language(
                crate::i18n::MessageKey::WorktreeToolsOperationFailed,
                "git failed with password=fixture-secret",
                crate::i18n::Language::Korean,
            ),
            "Worktree 작업에 실패했습니다: git failed with password=[secret redacted]"
        );
    }

    fn context(
        thread_id: &str,
        response: Value,
    ) -> (ToolContext, Arc<StdMutex<Vec<(String, Value)>>>) {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let saved = Arc::clone(&calls);
        let rpc = Arc::new(move |method: String, params: Value| {
            let saved = Arc::clone(&saved);
            let response = response.clone();
            Box::pin(async move {
                saved.lock().unwrap().push((method, params));
                Ok(response)
            }) as futures_util::future::BoxFuture<'static, Result<Value>>
        });
        let (events, _) = tokio::sync::broadcast::channel(4);
        (
            ToolContext {
                rpc,
                events,
                thread_id: thread_id.to_owned(),
                turn_id: "test-turn".to_owned(),
                thread_start: json!({}),
            },
            calls,
        )
    }

    fn creation_context(
        thread_id: &str,
        calls: Arc<StdMutex<Vec<String>>>,
        fail_add: bool,
    ) -> ToolContext {
        let thread_id = thread_id.to_owned();
        let rpc_thread_id = thread_id.clone();
        let rpc = Arc::new(move |method: String, _params: Value| {
            let calls = Arc::clone(&calls);
            let _tid = rpc_thread_id.clone();
            let fail_add = fail_add;
            Box::pin(async move {
                calls.lock().unwrap().push(method.clone());
                if method == "thread/attachment/add" && fail_add {
                    return Err(anyhow!("injected attachment failure"));
                }
                let response = match method.as_str() {
                    "thread/read" => {
                        json!({"thread":{"ephemeral":false,"environments":[{"environmentId":"local","cwd":std::env::temp_dir()}]}})
                    }
                    "thread/attachment/list" => json!({"data":[]}),
                    "config/read" => json!({"config":{}}),
                    _ => json!({}),
                };
                Ok(response)
            }) as futures_util::future::BoxFuture<'static, Result<Value>>
        });
        let (events, _) = tokio::sync::broadcast::channel(4);
        ToolContext {
            rpc,
            events,
            thread_id,
            turn_id: "test-turn".to_owned(),
            thread_start: json!({}),
        }
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    enum FakeOutcome {
        Success(Option<String>),
        Failure,
    }
    struct FakeBackend {
        outcome: FakeOutcome,
        calls: Arc<AtomicUsize>,
    }
    impl WorktreeBackend for FakeBackend {
        fn create(
            &self,
            _root: &Path,
            source: &Path,
            _revision: Option<&str>,
            _owner: &str,
        ) -> Result<Checkout> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.outcome {
                FakeOutcome::Failure => bail!("injected checkout failure"),
                FakeOutcome::Success(owner_error) => Ok(Checkout {
                    root: PathBuf::from("/mock/worktree"),
                    cwd: PathBuf::from("/mock/worktree"),
                    source_cwd: source.to_path_buf(),
                    owner_error: owner_error.clone(),
                }),
            }
        }
    }
    fn fake_tools(outcome: FakeOutcome, calls: Arc<AtomicUsize>) -> WorktreeTools {
        WorktreeTools {
            codex_home: std::env::temp_dir().join("discodex-codex-test"),
            backend: Arc::new(FakeBackend { outcome, calls }),
            operations: Arc::default(),
        }
    }
    async fn settled_status(tools: &WorktreeTools, ctx: ToolContext, id: &str) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match tools
                    .execute(
                        ctx.clone(),
                        "get_worktree_creation_status",
                        json!({"operationId":id}),
                    )
                    .await
                {
                    Ok(value) if value["type"] == "pending" => tokio::task::yield_now().await,
                    result => return result,
                }
            }
        })
        .await
        .context("injected worktree operation did not settle")?
    }

    #[test]
    fn definitions_dispatch_and_required_inputs() {
        let defs = definitions();
        assert_eq!(
            defs.iter()
                .map(|v| v["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "create_worktree",
                "get_worktree_creation_status",
                "list_worktrees"
            ]
        );
        assert!(NAMES.iter().all(|n| handles(n)));
        assert!(!handles("archive_worktree"));
        assert_eq!(defs[0]["inputSchema"]["required"], json!(["allowAsync"]));
    }

    #[test]
    fn argument_contracts_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<CreateArgs>(json!({"allowAsync":true,"ref":"origin/main"}))
                .is_ok()
        );
        assert!(
            serde_json::from_value::<CreateArgs>(json!({"allowAsync":true,"extra":1})).is_err()
        );
        assert_eq!(
            serde_json::from_value::<StatusArgs>(json!({"operationId":"op"}))
                .unwrap()
                .operation_id,
            "op"
        );
        assert!(serde_json::from_value::<ListArgs>(json!({"cursor":"next","extra":1})).is_err());
    }

    #[tokio::test]
    async fn list_worktrees_filters_attachments_and_preserves_cursor() {
        let response = json!({"data":[
            {"id":"a","attachmentType":"worktree","payload":{"root":"/tmp/w1","workspaceRoot":"/tmp/w1/src","sourceCwd":"/src"}},
            {"id":"b","attachmentType":"archived_worktree","payload":{"worktree":{"root":"/tmp/w2","workspaceRoot":"/tmp/w2","sourceCwd":"/src"}}},
            {"id":"c","attachmentType":"pull_request","payload":{"root":"ignored"}}
        ],"nextCursor":"next"});
        let (ctx, calls) = context("thread-a", response);
        let tools = WorktreeTools::new(std::env::temp_dir().join("discodex-codex-test")).unwrap();
        let result = tools
            .execute(ctx, "list_worktrees", json!({"cursor":"prior"}))
            .await
            .unwrap();
        assert_eq!(result["data"].as_array().unwrap().len(), 2);
        assert_eq!(result["data"][0]["workspaceRoot"], "/tmp/w1/src");
        assert_eq!(result["data"][1]["root"], "/tmp/w2");
        assert_eq!(result["nextCursor"], "next");
        assert_eq!(calls.lock().unwrap()[0].0, "thread/attachment/list");
        assert_eq!(calls.lock().unwrap()[0].1["cursor"], "prior");
    }

    #[tokio::test]
    async fn creation_rejects_sync_mode_before_rpc_or_operation_allocation() {
        let (ctx, calls) = context("thread-a", json!({}));
        let tools = WorktreeTools::new(std::env::temp_dir().join("discodex-codex-test")).unwrap();
        assert!(
            tools
                .execute(ctx, "create_worktree", json!({"allowAsync":false}))
                .await
                .is_err()
        );
        assert!(calls.lock().unwrap().is_empty());
        assert!(tools.operations.lock().await.is_empty());
    }

    #[tokio::test]
    async fn creation_deduplicates_pending_operation_before_spawning_checkout() {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let ctx = creation_context("thread-a", Arc::clone(&calls), false);
        let tools = WorktreeTools::new(std::env::temp_dir().join("discodex-codex-test")).unwrap();
        tools.operations.lock().await.insert(
            "existing-op".into(),
            Operation {
                thread_id: "thread-a".into(),
                started: Instant::now(),
                result: None,
            },
        );
        let result = tools
            .execute(ctx, "create_worktree", json!({"allowAsync":true}))
            .await
            .unwrap();
        assert_eq!(result["type"], "pending");
        assert_eq!(result["operationId"], "existing-op");
        assert_eq!(
            *calls.lock().unwrap(),
            ["thread/read", "thread/attachment/list", "config/read"]
        );
        assert_eq!(tools.operations.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn status_is_pending_only_for_its_own_thread() {
        let (ctx, _) = context("thread-a", json!({}));
        let tools = WorktreeTools::new(std::env::temp_dir().join("discodex-codex-test")).unwrap();
        tools.operations.lock().await.insert(
            "op-1".into(),
            Operation {
                thread_id: "thread-a".into(),
                started: Instant::now(),
                result: None,
            },
        );
        let status = tools
            .execute(
                ctx,
                "get_worktree_creation_status",
                json!({"operationId":"op-1"}),
            )
            .await
            .unwrap();
        assert_eq!(status, json!({"type":"pending","operationId":"op-1"}));
        let (other, _) = context("thread-b", json!({}));
        assert!(
            tools
                .execute(
                    other,
                    "get_worktree_creation_status",
                    json!({"operationId":"op-1"})
                )
                .await
                .is_err()
        );
        assert_eq!(tools.operations.lock().await.len(), 1);
    }
    #[test]
    fn default_base_prefers_origin_remote_head_and_owner_binding_is_thread_specific() {
        let refs = vec![
            (
                "refs/remotes/upstream/HEAD".to_owned(),
                "refs/remotes/upstream/main".to_owned(),
            ),
            (
                "refs/remotes/origin/HEAD".to_owned(),
                "refs/remotes/origin/trunk".to_owned(),
            ),
            ("refs/heads/main".to_owned(), String::new()),
        ];
        assert_eq!(
            select_default_base(&refs).unwrap(),
            "refs/remotes/origin/trunk"
        );
        let owner = json!({"version":1,"ownerThreadId":"thread-a"});
        assert!(owner_record_matches(&owner, "thread-a"));
        assert!(!owner_record_matches(&owner, "thread-b"));
        assert!(!owner_record_matches(
            &json!({"version":2,"ownerThreadId":"thread-a"}),
            "thread-a"
        ));
    }

    #[tokio::test]
    async fn async_create_publishes_completion_and_attaches_checkout() {
        let count = Arc::new(AtomicUsize::new(0));
        let tools = fake_tools(FakeOutcome::Success(None), Arc::clone(&count));
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let ctx = creation_context("thread-a", Arc::clone(&calls), false);
        let pending = tools
            .execute(ctx.clone(), "create_worktree", json!({"allowAsync":true}))
            .await
            .unwrap();
        assert_eq!(pending["type"], "pending");
        let status = settled_status(&tools, ctx, &pending["operationId"].as_str().unwrap())
            .await
            .unwrap();
        assert_eq!(status["type"], "created");
        assert_eq!(status["worktree"]["root"], "/mock/worktree");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|m| m == "thread/attachment/add")
        );
    }

    #[tokio::test]
    async fn checkout_and_attachment_failures_are_retained_as_status() {
        let failed_count = Arc::new(AtomicUsize::new(0));
        let failed = fake_tools(FakeOutcome::Failure, Arc::clone(&failed_count));
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let ctx = creation_context("thread-a", Arc::clone(&calls), false);
        let pending = failed
            .execute(ctx.clone(), "create_worktree", json!({"allowAsync":true}))
            .await
            .unwrap();
        let status = settled_status(&failed, ctx, &pending["operationId"].as_str().unwrap()).await;
        assert!(status.is_err());
        assert_eq!(failed_count.load(Ordering::SeqCst), 1);

        let bind_count = Arc::new(AtomicUsize::new(0));
        let binding = fake_tools(
            FakeOutcome::Success(Some("owner record collision".into())),
            Arc::clone(&bind_count),
        );
        let ctx = creation_context("thread-a", Arc::new(StdMutex::new(Vec::new())), false);
        let pending = binding
            .execute(ctx.clone(), "create_worktree", json!({"allowAsync":true}))
            .await
            .unwrap();
        let status = settled_status(&binding, ctx, &pending["operationId"].as_str().unwrap())
            .await
            .unwrap();
        assert_eq!(status["type"], "registrationFailed");
        assert_eq!(status["worktree"]["root"], "/mock/worktree");
        assert_eq!(bind_count.load(Ordering::SeqCst), 1);

        let registration_count = Arc::new(AtomicUsize::new(0));
        let registration = fake_tools(FakeOutcome::Success(None), Arc::clone(&registration_count));
        let ctx = creation_context("thread-a", Arc::new(StdMutex::new(Vec::new())), true);
        let pending = registration
            .execute(ctx.clone(), "create_worktree", json!({"allowAsync":true}))
            .await
            .unwrap();
        let status = settled_status(
            &registration,
            ctx,
            &pending["operationId"].as_str().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(status["type"], "registrationFailed");
        assert_eq!(status["worktree"]["root"], "/mock/worktree");
    }
}

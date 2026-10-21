use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=DISCODEX_BUILD_COMMIT");
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(head) = std::fs::read_to_string(".git/HEAD") {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed=.git/{reference}");
        }
    }
    let commit = std::env::var("DISCODEX_BUILD_COMMIT")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=DISCODEX_BUILD_COMMIT={commit}");

    // Release workers and runtime manifests are built from the same submodule.
    // Watch Git's resolved paths so detached checkouts, branches and worktrees
    // all rebuild the worker when its pinned core revision changes.
    assert!(
        codex_git(&["rev-parse", "--show-prefix"]).is_empty(),
        "initialize the Codex submodule before building; its directory is not a Git checkout"
    );
    if std::path::Path::new("third_party/codex/.git").is_file() {
        println!("cargo:rerun-if-changed=third_party/codex/.git");
    }
    for path in codex_git(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "HEAD",
        "--git-path",
        "packed-refs",
        "--git-path",
        "refs",
    ])
    .lines()
    {
        println!("cargo:rerun-if-changed={path}");
    }
    let codex_commit = codex_git(&["rev-parse", "--verify", "HEAD^{commit}"]);
    assert!(
        codex_commit.len() == 40 && codex_commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the Codex submodule must provide a full commit SHA"
    );
    println!("cargo:rustc-env=DISCODEX_CODEX_COMMIT={codex_commit}");
}

fn codex_git(args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-C", "third_party/codex"])
        .args(args)
        .output()
        .expect("Git is required to build Discodex from its pinned Codex submodule");
    assert!(
        output.status.success(),
        "read the pinned Codex submodule (initialize it before building): {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout)
        .expect("Git revision metadata must be UTF-8")
        .trim()
        .to_owned()
}

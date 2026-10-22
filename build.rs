use std::process::Command;
use std::{collections::BTreeMap, path::Path};

fn main() {
    generate_message_resources();
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

fn generate_message_resources() {
    let root = Path::new("resources/messages");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut messages = BTreeMap::<String, BTreeMap<String, String>>::new();
    if root.exists() {
        for entry in std::fs::read_dir(root).expect("read message resource directory") {
            let path = entry.expect("read message resource entry").path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            println!("cargo:rerun-if-changed={}", path.display());
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let file: BTreeMap<String, BTreeMap<String, String>> = serde_json::from_str(&content)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            for (key, translations) in file {
                assert!(
                    key.split('.').count() >= 2
                        && key.split('.').all(|part| !part.is_empty()
                            && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')),
                    "invalid message key {key:?} in {}",
                    path.display()
                );
                assert!(
                    ["en", "ko", "zh", "ja"]
                        .iter()
                        .all(|locale| translations.contains_key(*locale)),
                    "message {key:?} in {} must define en, ko, zh and ja",
                    path.display()
                );
                assert!(
                    messages.insert(key.clone(), translations).is_none(),
                    "duplicate message key {key:?}"
                );
            }
        }
    }

    let mut generated =
        String::from("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub enum MessageKey {\n");
    let mut variants = BTreeMap::new();
    for key in messages.keys() {
        let variant = message_variant(key);
        assert!(
            variants.insert(variant.clone(), key).is_none(),
            "message keys collide as MessageKey::{variant}; rename one resource key"
        );
        generated.push_str(&format!("    {variant},\n"));
    }
    generated.push_str("}\nimpl MessageKey {\n    pub const fn text(self, language: Language) -> &'static str {\n        match (self, language) {\n");
    for (key, translations) in &messages {
        let variant = message_variant(key);
        for (locale, language) in [
            ("en", "English"),
            ("ko", "Korean"),
            ("zh", "Chinese"),
            ("ja", "Japanese"),
        ] {
            let value = serde_json::to_string(&translations[locale])
                .expect("serialize message translation");
            generated.push_str(&format!(
                "            (Self::{variant}, Language::{language}) => {value},\n"
            ));
        }
    }
    generated.push_str("        }\n    }\n}\n");
    let out = Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR is set"))
        .join("message_resources.rs");
    std::fs::write(out, generated).expect("write generated message resources");
}

fn message_variant(key: &str) -> String {
    key.split(|character: char| character == '.' || character == '_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            let Some(first) = characters.next() else {
                return String::new();
            };
            first.to_ascii_uppercase().to_string() + characters.as_str()
        })
        .collect::<Vec<_>>()
        .join("")
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

#![forbid(unsafe_code)]

//! The third `unsafe` exception (AGENTS.md) covers only UniFFI-generated code.
//! The generated glue is compiled from `OUT_DIR` through `src/scaffolding.rs`,
//! so no file in this crate's sources may contain the `unsafe` keyword, and
//! only `lib.rs` may allow `unsafe_code`, once, on the scaffolding module.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// Whether a code line of `text` contains `unsafe` as a whole identifier.
/// Full-line comments are prose and `unsafe_code` in a lint attribute is not
/// the keyword; anything else (strings included) counts.
fn has_unsafe_keyword(text: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .any(|line| {
            line.match_indices(KEYWORD).any(|(at, word)| {
                let before = line[..at].chars().next_back();
                let after = line[at + word.len()..].chars().next();
                !before.is_some_and(ident) && !after.is_some_and(ident)
            })
        })
}

/// Spelled in two halves so this file passes its own scan.
const KEYWORD: &str = concat!("un", "safe");
/// The lint allowance, split the same way so it is only found where it is used.
const ALLOW: &str = concat!("allow(un", "safe_code)");

#[test]
fn no_hand_written_unsafe() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["src", "tests"] {
        rust_sources(&root.join(dir), &mut files);
    }
    files.push(root.join("build.rs"));
    let mut allows = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("read source");
        let name = file
            .strip_prefix(root)
            .unwrap_or(file)
            .display()
            .to_string()
            .replace('\\', "/");
        assert!(
            !has_unsafe_keyword(&text),
            "{name}: hand-written `{KEYWORD}` is forbidden in agendar-ffi"
        );
        for (line, content) in text.lines().enumerate() {
            if content.contains(ALLOW) {
                allows.push(format!("{name}:{}: {}", line + 1, content.trim()));
            }
        }
    }
    assert_eq!(
        allows,
        [format!("src/lib.rs:11: #[{ALLOW}]")],
        "only the scaffolding module may allow unsafe_code"
    );
    let lib = fs::read_to_string(root.join("src/lib.rs")).expect("read lib.rs");
    assert!(
        lib.contains(&format!("#[{ALLOW}]\nmod scaffolding;")),
        "the single allow must sit on `mod scaffolding;`"
    );
    let scaffolding = fs::read_to_string(root.join("src/scaffolding.rs")).expect("read");
    let code: Vec<&str> = scaffolding
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect();
    assert_eq!(
        code,
        [
            "#![allow(clippy::all, clippy::pedantic)]",
            "use crate::facade::*;",
            "uniffi::include_scaffolding!(\"agendar\");",
        ],
        "scaffolding.rs only includes the generated glue"
    );
}

#[test]
fn the_keyword_scan_sees_unsafe_but_not_lint_names() {
    let keyword = |code: &str| has_unsafe_keyword(&code.replace("KW", KEYWORD));
    assert!(keyword("fn f() { KW { g() } }"));
    assert!(keyword("#[KW(no_mangle)]"));
    assert!(keyword("let s = \"//\"; KW { g() }"));
    assert!(!keyword("#![forbid(KW_code)]"));
    assert!(!keyword("let not_KW = 1;"));
    assert!(!keyword("/// The `KW` exception."));
}

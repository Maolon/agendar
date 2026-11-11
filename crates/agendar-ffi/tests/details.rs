#![forbid(unsafe_code)]

//! `FfiError.Core.detail` is a closed set: the list in
//! `agendar_core::error::details`. Three things hold it closed. The facade's
//! conversion drops a detail the list does not have (unit tests in
//! `facade/mod.rs`). Every detail the facade itself writes is in the list
//! (the scan below, so none of its own is dropped by mistake). And every
//! listed code is still raised somewhere, so the list does not keep names
//! nothing uses.

use std::fs;
use std::path::{Path, PathBuf};

use agendar_core::error::details;

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

/// The sources of a crate of this workspace, as (name, text).
fn sources(krate: &str) -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .join(krate)
        .join("src");
    let mut files = Vec::new();
    rust_sources(&root, &mut files);
    assert!(!files.is_empty(), "{krate} has sources");
    files
        .into_iter()
        .map(|file| {
            let name = file
                .strip_prefix(&root)
                .unwrap_or(&file)
                .display()
                .to_string();
            (name, fs::read_to_string(&file).expect("read source"))
        })
        .collect()
}

/// The calls that take a detail, and which argument it is.
const CALLS: [(&str, usize); 4] = [
    ("with_detail(", 0),
    ("validation_at(", 1),
    // The facade's own two helpers (`write.rs`, `shared.rs`).
    ("invalid(", 1),
    ("named(", 2),
];

/// Argument `index` of the call whose arguments start at `rest`, trimmed.
/// Splits at top-level commas; strings and nested brackets are skipped.
fn argument(rest: &str, index: usize) -> &str {
    let (mut depth, mut in_string, mut escaped) = (0_i32, false, false);
    let (mut start, mut seen) = (0, 0);
    for (at, c) in rest.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => return rest[start..at].trim(),
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                if seen == index {
                    return rest[start..at].trim();
                }
                seen += 1;
                start = at + 1;
            }
            _ => {}
        }
    }
    rest[start..].trim()
}

/// The name of a constant of `details` as its code: the two are the same
/// word (`OWN_ANSWER_TOO_LONG` is `own_answer_too_long`), checked below.
fn constant(argument: &str) -> Option<String> {
    argument
        .strip_prefix("details::")
        .map(str::to_ascii_lowercase)
}

#[test]
fn every_detail_the_facade_writes_is_listed() {
    let mut written = 0;
    for (name, text) in sources("agendar-ffi") {
        // What a file's own unit tests build is not what the facade emits.
        let code = text.split("#[cfg(test)]").next().unwrap_or_default();
        for (call, index) in CALLS {
            for (at, _) in code.match_indices(call) {
                let before = &code[..at];
                // The helper's definition, or a longer name ending the same.
                if before.ends_with("fn ")
                    || before.ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                {
                    continue;
                }
                let detail = argument(&code[at + call.len()..], index);
                // A helper passing its own parameter on: its callers are
                // scanned by its name.
                if detail == "detail" {
                    continue;
                }
                let code = match detail.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                    Some(literal) => literal.to_owned(),
                    None => constant(detail).unwrap_or_else(|| {
                        panic!("{name}: `{call}` takes `{detail}`, not a detail the list can check")
                    }),
                };
                assert_eq!(
                    details::listed(&code),
                    Some(code.as_str()),
                    "{name}: `{code}` is not in agendar_core::error::details"
                );
                written += 1;
            }
        }
    }
    // The scan found the facade's details (it would pass on nothing found).
    assert!(written >= 15, "{written}");
}

#[test]
fn the_scan_reads_a_call_as_rust_does() {
    assert_eq!(argument(r#""a_b") .into()"#, 0), r#""a_b""#);
    assert_eq!(
        argument(
            "\n    \"/x\",\n    details::ECHO_MISSING,\n    \"m\",\n)",
            1
        ),
        "details::ECHO_MISSING"
    );
    assert_eq!(
        argument(
            r#"format!("{a}, {b}", a, b), "second, not this", third)"#,
            1
        ),
        r#""second, not this""#
    );
    assert_eq!(
        argument(r#"Id::parse(v), field, details::X)"#, 2),
        "details::X"
    );
    assert_eq!(constant("details::NOT_FOUND").as_deref(), Some("not_found"));
}

#[test]
fn every_listed_detail_is_still_raised() {
    let core = sources("agendar-core");
    let facade = sources("agendar-ffi");
    let list = core
        .iter()
        .find(|(name, _)| name.ends_with("details.rs"))
        .map(|(_, text)| text.as_str())
        .expect("the list");
    for code in details::CORE {
        let literal = format!("\"{code}\"");
        assert!(
            core.iter()
                .any(|(name, text)| !name.ends_with("details.rs") && text.contains(&literal)),
            "`{code}` is listed and the core no longer raises it"
        );
    }
    for code in details::HOST {
        // Declared under its own name, and used by the facade under it.
        let constant = code.to_ascii_uppercase();
        assert!(
            list.contains(&format!("pub const {constant}: &str = \"{code}\";")),
            "`{code}` has no constant of its name"
        );
        assert!(
            facade
                .iter()
                .any(|(_, text)| text.contains(&format!("details::{constant}"))),
            "`{code}` is listed and the facade no longer sets it"
        );
    }
}

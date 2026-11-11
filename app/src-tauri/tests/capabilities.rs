//! Static build-contract tests: the command list, invoke registrations and the
//! capability files must agree, and no broad plugin permission may sneak in.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn quoted_words(source: &str, marker: &str) -> BTreeSet<String> {
    let start = source
        .find(marker)
        .unwrap_or_else(|| panic!("marker {marker} not found"))
        + marker.len();
    let rest = &source[start..];
    let end = rest.find(']').expect("list terminator");
    let mut words = BTreeSet::new();
    for piece in rest[..end].split('"') {
        let word: String = piece
            .chars()
            .filter(|character| character.is_ascii_lowercase() || *character == '_')
            .collect();
        let original_non_space = piece
            .trim()
            .trim_matches(|character: char| character == ',' || character.is_whitespace());
        if !word.is_empty() && word == original_non_space {
            words.insert(word);
        }
    }
    words
}

#[test]
fn build_manifest_invoke_registrations_and_capabilities_agree() {
    let build = read(&manifest_dir().join("build.rs"));
    assert!(build.contains("tauri_build::try_build"));
    assert!(build.contains("AppManifest::new().commands(COMMANDS)"));
    let build_commands = quoted_words(&build, "const COMMANDS: &[&str] = &[");

    let lib = read(&manifest_dir().join("src/lib.rs"));
    let handler_start =
        lib.find("generate_handler![").expect("invoke handler") + "generate_handler![".len();
    let handler = &lib[handler_start..];
    let handler_end = handler.find(']').expect("handler list end");
    let mut registered = BTreeSet::new();
    for piece in handler[..handler_end].split("commands::") {
        let name: String = piece
            .chars()
            .take_while(|character| character.is_ascii_lowercase() || *character == '_')
            .collect();
        if !name.is_empty() {
            registered.insert(name);
        }
    }

    let commands = read(&manifest_dir().join("src/commands.rs"));
    let declared = quoted_words(&commands, "pub const COMMANDS: &[&str] = &[");

    assert_eq!(build_commands, declared, "build and command list agree");
    assert_eq!(declared, registered, "every command is registered once");
    assert_eq!(declared.len(), 36);

    let main: Value = serde_json::from_str(&read(&manifest_dir().join("capabilities/main.json")))
        .expect("main capability");
    let popover: Value =
        serde_json::from_str(&read(&manifest_dir().join("capabilities/popover.json")))
            .expect("popover capability");

    let expected_main: BTreeSet<String> = declared
        .iter()
        .map(|command| format!("allow-{}", command.replace('_', "-")))
        .chain(
            [
                "core:event:allow-listen",
                "core:event:allow-unlisten",
                // The toolbar's drag region moves the window; nothing else
                // from the window plugin is granted.
                "core:window:allow-start-dragging",
            ]
            .iter()
            .map(|permission| permission.to_string()),
        )
        .collect();
    let actual_main: BTreeSet<String> = main["permissions"]
        .as_array()
        .expect("permissions")
        .iter()
        .map(|value| value.as_str().expect("permission").to_owned())
        .collect();
    assert_eq!(actual_main, expected_main);
    assert_eq!(main["webviews"], serde_json::json!(["main"]));
    assert_eq!(main["local"], true);
    assert_eq!(main["identifier"], "main");

    let expected_popover: BTreeSet<String> = [
        "core:event:allow-listen",
        "core:event:allow-unlisten",
        "allow-get-snapshot",
        "allow-prepare-answer",
        "allow-answer-decision",
        // Quick triage includes setting an item aside: the owner's snooze event.
        "allow-snooze-need",
        // ...or closing a hand-off item for good, leaving the step untouched.
        "allow-dismiss-need",
        "allow-show-main",
        "allow-hide-popover",
        "allow-window-chrome",
        "allow-set-glass-regions",
    ]
    .iter()
    .map(|permission| permission.to_string())
    .collect();
    let actual_popover: BTreeSet<String> = popover["permissions"]
        .as_array()
        .expect("permissions")
        .iter()
        .map(|value| value.as_str().expect("permission").to_owned())
        .collect();
    assert_eq!(actual_popover, expected_popover);
    assert_eq!(popover["webviews"], serde_json::json!(["popover"]));
    assert_eq!(popover["identifier"], "popover");

    for capability in [&main, &popover] {
        for permission in capability["permissions"].as_array().expect("permissions") {
            let permission = permission.as_str().expect("permission");
            assert!(
                !permission.starts_with("shell:")
                    && !permission.starts_with("fs:")
                    && !permission.starts_with("opener:")
                    && !permission.contains("notification")
                    && (!permission.contains("window:allow-")
                        || permission == "core:window:allow-start-dragging"),
                "broad plugin permission {permission} must not be granted"
            );
        }
        assert!(capability.get("remote").is_none(), "no remote capability");
    }
}

#[test]
fn the_capability_directory_contains_only_main_and_popover() {
    let directory = manifest_dir().join("capabilities");
    let mut files: Vec<String> = std::fs::read_dir(&directory)
        .expect("capabilities")
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect();
    files.sort();
    assert_eq!(
        files,
        vec!["main.json".to_owned(), "popover.json".to_owned()]
    );
}

#[test]
fn the_tauri_config_matches_the_frozen_window_csp_and_bundle_contract() {
    let config: Value =
        serde_json::from_str(&read(&manifest_dir().join("tauri.conf.json"))).expect("tauri config");
    assert_eq!(config["identifier"], "dev.agendar.app");
    assert_eq!(config["productName"], "Agendar");
    assert_eq!(config["build"]["frontendDist"], "../dist");
    assert_eq!(config["build"]["devUrl"], "http://localhost:1420");
    assert_eq!(
        config["app"]["security"]["capabilities"],
        serde_json::json!(["main", "popover"])
    );
    let csp = config["app"]["security"]["csp"]
        .as_str()
        .expect("production csp");
    assert!(csp.starts_with("default-src 'none'; script-src 'self';"));
    assert!(!csp.contains('*'), "no wildcard CSP source");
    assert!(!csp.contains("unsafe-eval"));
    assert_eq!(
        config["bundle"]["externalBin"],
        serde_json::json!(["binaries/agendar"])
    );

    let windows = config["app"]["windows"].as_array().expect("windows");
    let labels: Vec<&str> = windows
        .iter()
        .map(|window| window["label"].as_str().expect("window label"))
        .collect();
    assert_eq!(labels, vec!["main", "popover"]);
    let main = &windows[0];
    assert_eq!(main["width"], 1120);
    assert_eq!(main["height"], 740);
    assert_eq!(main["minWidth"], 860);
    assert_eq!(main["minHeight"], 600);
    assert_eq!(main["resizable"], true);
    let popover = &windows[1];
    assert_eq!(popover["width"], 380);
    assert!(popover["height"].as_u64().expect("height") <= 560);
    assert_eq!(popover["resizable"], false);
    assert_eq!(popover["decorations"], false);
    assert_eq!(popover["visible"], false);
    assert_eq!(popover["skipTaskbar"], true);
    assert_eq!(popover["url"], "index.html?surface=popover");
}

#[test]
fn the_app_crate_forbids_unsafe_and_has_no_extra_plugin_entries() {
    let lib = read(&manifest_dir().join("src/lib.rs"));
    assert!(lib.starts_with("#![forbid(unsafe_code)]"));
    let manifest = read(&manifest_dir().join("Cargo.toml"));
    assert!(!manifest.contains("tauri-plugin-shell"));
    assert!(!manifest.contains("tauri-plugin-fs"));
    assert!(!manifest.contains("tauri-plugin-opener"));
}

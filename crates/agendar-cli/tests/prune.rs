//! `agendar prune`: the usage and reclaimable listing. It lists; it never
//! deletes and never writes to the Vault.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use agendar_core::model::{ProjectDeclared, ProjectSlug, UtcTime, project_field_revisions};
use support::{CliEnv, expect_error, expect_success};

const PROJECT: &str = "demo";
const NODE: &str = "core/wake";

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).expect("json")).expect("write json");
}

/// A project with one delivery (a transcript demo and one criterion's
/// evidence) and one attachment no event cites.
fn delivered() -> CliEnv {
    let mut env = CliEnv::new(PROJECT);
    env.init();
    let project = env.root.path().join("project.json");
    write_json(
        &project,
        &json!({"title": "Delivery", "brief": "Delivery board.", "goal": "Prove delivery."}),
    );
    expect_success(
        &env.run(&["project", "declare", "--json", &project.to_string_lossy()]),
        "project declare",
    );
    let feature = env.root.path().join("feature.json");
    write_json(
        &feature,
        &json!({"title": "Wake delivery", "slice": "core", "demo_kind": "cli_transcript"}),
    );
    expect_success(
        &env.run(&[
            "feature",
            "declare",
            "--node",
            NODE,
            "--json",
            &feature.to_string_lossy(),
        ]),
        "feature declare",
    );
    let started = expect_success(
        &env.run(&["progress", "started", "--node", NODE, "--eta", "10m..20m"]),
        "progress",
    );
    let attempt = started["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();
    let card = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agendar-core/tests/oracle/examples/ask.json");
    let asked = expect_success(
        &env.run(&[
            "ask",
            "--node",
            NODE,
            "--json",
            &card.to_string_lossy(),
            "--mode",
            "ask",
        ]),
        "ask",
    );
    let receipt = env.answer(
        asked["data"]["ask_id"].as_str().expect("ask id"),
        "adapters",
    );

    let demo = env.repo.join("demo.txt");
    let evidence = env.repo.join("evidence.txt");
    fs::write(&demo, b"demo transcript\n").expect("demo");
    fs::write(&evidence, b"aggregate: passed\n").expect("evidence");
    let input = env.root.path().join("delivery.json");
    write_json(
        &input,
        &json!({
            "name": "Wake delivery",
            "demos": [{"kind": "cli_transcript", "body": {"file": demo.to_string_lossy(), "ext": "txt"}}],
            "criteria": [{
                "criterion": "continues", "status": "passed", "claim": "agent_says",
                "evidence": {"file": evidence.to_string_lossy(), "ext": "txt"}
            }],
            "disclosure": {"proven": ["Agent compared bytes"], "not_proven": [], "faked": []}
        }),
    );
    expect_success(
        &env.run(&[
            "deliver",
            "--node",
            NODE,
            "--attempt",
            &attempt,
            "--receipt",
            &receipt,
            "--json",
            &input.to_string_lossy(),
        ]),
        "deliver",
    );
    let stray = env.repo.join("stray.txt");
    fs::write(&stray, b"nobody cites this\n").expect("stray");
    expect_success(
        &env.run(&["blob", "put", &stray.to_string_lossy(), "--ext", "txt"]),
        "blob put",
    );
    env
}

/// `prune` without `--project`: every project.
fn prune_all(env: &CliEnv, extra: &[&str]) -> Value {
    let mut command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .args(extra)
        .arg("prune");
    expect_success(&command.output().expect("run agendar"), "prune")
}

/// The operator archives the project (an app action, not an agent command).
fn archive(env: &CliEnv) {
    let service = env.service();
    let project = ProjectSlug::parse(PROJECT).expect("project");
    let fresh = service
        .refresh(
            UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time"),
            false,
        )
        .expect("refresh");
    let summary = &fresh.state.projects[&project].summary;
    let expected: BTreeMap<_, _> = project_field_revisions(&project, summary)
        .into_iter()
        .filter(|(field, _)| field == "archived")
        .collect();
    let patch: ProjectDeclared = serde_json::from_value(json!({"archived": true})).expect("patch");
    service
        .update_project(&project, patch, &expected)
        .expect("archive");
}

/// Every file below `root`: relative path to bytes.
fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(directory).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.insert(
                    path.strip_prefix(root).expect("below root").join(""),
                    Vec::new(),
                );
                walk(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).expect("below root").to_owned(),
                    fs::read(&path).expect("read file"),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

fn blobs_with<'a>(project: &'a Value, status: &str) -> Vec<&'a Value> {
    project["blobs"]
        .as_array()
        .expect("blobs")
        .iter()
        .filter(|blob| blob["status"] == status)
        .collect()
}

#[test]
fn prune_lists_usage_and_what_an_archived_project_could_free() {
    let env = delivered();
    let listed = prune_all(&env, &[]);
    let data = &listed["data"];
    assert_eq!(data["superseded_after_days"], 30);
    let projects = data["projects"].as_array().expect("projects");
    assert_eq!(projects.len(), 1);
    let project = &projects[0];
    assert_eq!(project["project"], PROJECT);
    assert_eq!(project["title"], "Delivery");
    assert_eq!(project["archived"], false);

    // "demo transcript\n" 16, "aggregate: passed\n" 18, "nobody cites this\n" 18.
    assert_eq!(
        project["attachments"],
        json!({"count": 3, "bytes": 52, "local_bytes": 52})
    );
    assert_eq!(
        project["categories"]["delivery"],
        json!({"count": 2, "bytes": 34, "local_bytes": 34})
    );
    assert_eq!(
        project["unreferenced"],
        json!({"count": 1, "bytes": 18, "local_bytes": 18})
    );
    assert_eq!(
        project["reclaimable"],
        json!({"count": 0, "bytes": 0, "local_bytes": 0})
    );
    let event_bytes: u64 = env
        .event_lines()
        .iter()
        .map(|line| line.len() as u64 + 1)
        .sum();
    assert_eq!(project["event_bytes"], event_bytes);
    assert_eq!(project["total_bytes"], event_bytes + 52);
    assert_eq!(data["total"]["total_bytes"], event_bytes + 52);

    // A live project: its delivery is current, so both attachments are kept.
    let kept = blobs_with(project, "kept");
    assert_eq!(kept.len(), 2);
    assert!(kept.iter().all(|blob| blob["reason"] == "current_delivery"));
    let stray = blobs_with(project, "unreferenced");
    assert_eq!(stray.len(), 1);
    assert_eq!(stray[0]["bytes"], 18);
    for blob in project["blobs"].as_array().expect("blobs") {
        let path = env.vault.join(blob["path"].as_str().expect("path"));
        assert!(path.is_file(), "listed blob exists: {}", path.display());
    }

    // Archived: the delivery's attachments could go; the stray is still only listed.
    archive(&env);
    let listed = expect_success(&env.run(&["prune"]), "prune");
    let project = &listed["data"]["projects"][0];
    assert_eq!(project["archived"], true);
    assert_eq!(
        project["reclaimable"],
        json!({"count": 2, "bytes": 34, "local_bytes": 34})
    );
    assert_eq!(listed["data"]["total"]["reclaimable"]["bytes"], 34);
    let reclaimable = blobs_with(project, "reclaimable");
    assert_eq!(reclaimable.len(), 2);
    for blob in &reclaimable {
        assert_eq!(blob["reason"], "archived_project_delivery");
        assert_eq!(blob["category"], "delivery");
        assert_eq!(blob["referrers"].as_array().expect("referrers").len(), 1);
    }
    let uses: Vec<&str> = reclaimable
        .iter()
        .map(|blob| blob["referrers"][0]["use"].as_str().expect("use"))
        .collect();
    assert!(
        uses.contains(&"delivery_demo") && uses.contains(&"delivery_evidence"),
        "{uses:?}"
    );
    assert_eq!(blobs_with(project, "unreferenced").len(), 1);

    // An unknown project is an error, not an empty listing.
    let mut command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .args(["--project", "missing", "prune"]);
    expect_error(&command.output().expect("run agendar"), "E_NOT_FOUND", 3);
}

#[test]
fn prune_deletes_nothing_and_leaves_the_vault_byte_identical() {
    let env = delivered();
    archive(&env);
    let before = tree(&env.vault);
    assert!(
        before
            .keys()
            .any(|path| path.components().any(|part| part.as_os_str() == "blobs"))
    );

    let listed = prune_all(&env, &[]);
    assert_eq!(listed["data"]["total"]["reclaimable"]["count"], 2);
    expect_success(&env.run(&["prune"]), "prune");
    prune_all(&env, &["--cached"]);
    assert_eq!(tree(&env.vault), before, "prune must not change the Vault");

    // There is nothing to delete with: no flag, no subcommand.
    for flag in ["--delete", "--yes", "--apply", "--force"] {
        let output = env.run(&["prune", flag]);
        assert_ne!(output.status.code(), Some(0), "{flag} must not be accepted");
    }
    let help = env.run(&["prune", "--help"]);
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("deletes nothing"), "{text}");
    assert_eq!(tree(&env.vault), before);
}

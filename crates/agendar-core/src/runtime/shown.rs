//! The local cursor of planned steps `agendar settle` showed, per project, so
//! a shown step is not shown again for a week and the whole backlog is looked
//! at over a few sessions. Local only (data dir): looking is not a Vault fact.
//! A missing or unreadable file reads as nothing shown.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::VaultId;
use crate::model::ids::{NodePath, ProjectSlug, UtcTime};
use crate::runtime::local_state::{acquire_lock, read_json_file, write_json_atomic};
use crate::runtime::paths::LocalPaths;
use crate::vault::Vault;

const VERSION: u8 = 1;
const FILE: &str = "settle-shown.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct ShownFile {
    v: u8,
    #[serde(default)]
    projects: BTreeMap<ProjectSlug, BTreeMap<NodePath, UtcTime>>,
}

fn path(local: &LocalPaths, vault: &VaultId) -> std::path::PathBuf {
    local.vault_local_dir(vault).join(FILE)
}

fn load(local: &LocalPaths, vault: &VaultId) -> ShownFile {
    match read_json_file::<ShownFile>(&path(local, vault)) {
        Ok(Some(file)) if file.v == VERSION => file,
        _ => ShownFile {
            v: VERSION,
            ..ShownFile::default()
        },
    }
}

/// When each planned step of `project` was last shown.
pub fn shown(
    local: &LocalPaths,
    vault: &VaultId,
    project: &ProjectSlug,
) -> BTreeMap<NodePath, UtcTime> {
    load(local, vault)
        .projects
        .remove(project)
        .unwrap_or_default()
}

/// Remember that `nodes` were shown at `at`; entries older than `keep_s` are
/// dropped.
pub fn mark_shown(
    local: &LocalPaths,
    vault: &Vault,
    project: &ProjectSlug,
    nodes: &[NodePath],
    at: UtcTime,
    keep_s: i64,
) -> Result<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    let layout = local.ensure_layout(vault.root(), vault.id())?;
    let _lock = acquire_lock(&layout.locks.join("settle-shown.lock"))?;
    let mut file = load(local, vault.id());
    for entries in file.projects.values_mut() {
        entries.retain(|_, shown| shown.add_seconds(keep_s) > at);
    }
    file.projects.retain(|_, entries| !entries.is_empty());
    let entries = file.projects.entry(project.clone()).or_default();
    for node in nodes {
        entries.insert(node.clone(), at);
    }
    file.v = VERSION;
    write_json_atomic(&path(local, vault.id()), &file)
}

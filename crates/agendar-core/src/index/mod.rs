//! Local SQLite index: connection lifecycle, offset ingestion, transactional
//! rebuild and projection storage.
//!
//! The index is a disposable cache of Vault truth plus local-only mirrors. It
//! lives outside the Vault at `<data>/vaults/<vault-id>/index.sqlite`, shares one
//! cross-process ingest lock, preserves conflicting raw variants and rebuilds in
//! place. `schema.sql` is a byte-identical copy of the normative `index.sql`.

pub mod ingest;
pub mod query;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::{Connection, OpenFlags, OptionalExtension as _, TransactionBehavior, params};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::{
    Event, EventId, ProjectSlug, Sha256, State, UtcTime, canonical_json, parse_event,
};

use crate::reduce::order::{Resolution, event_digest, resolve};
use crate::reduce::{ReduceContext, reduce_with_resolution};
use crate::runtime::LocalPaths;
use crate::vault::Vault;

/// Initial DDL of the local index.
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

pub const SCHEMA_VERSION: i64 = 1;
pub const REDUCER_VERSION: i64 = 1;

/// Result of one ingest transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    pub added: u64,
    pub pending: u64,
    pub invalid: u64,
    pub generation: u64,
    pub changed_projects: Vec<ProjectSlug>,
    pub pulse_ids: Vec<EventId>,
}

/// One open index connection.
pub struct Index {
    connection: Connection,
    path: PathBuf,
    local: LocalPaths,
}

/// Every table the normative DDL creates.
pub const REQUIRED_TABLES: [&str; 17] = [
    "meta",
    "source_files",
    "raw_events",
    "event_sources",
    "diagnostics",
    "projects",
    "features",
    "edges",
    "decisions",
    "receipts",
    "deliveries",
    "needs_you",
    "rules",
    "sessions",
    "local_sessions",
    "delivery_attempts",
    "ui_local",
];

impl Index {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    /// Exclusive cross-process ingest lock for this Vault's local cache.
    pub fn acquire_ingest_lock(&self, vault: &Vault) -> Result<std::fs::File> {
        acquire_ingest_lock_file(&self.local, vault)
    }

    /// The same lock, never waiting past `deadline` (the Stop-hook budget).
    pub fn acquire_ingest_lock_until(
        &self,
        vault: &Vault,
        deadline: Option<Instant>,
    ) -> Result<std::fs::File> {
        acquire_ingest_lock_file_until(&self.local, vault, deadline)
    }
}

fn acquire_ingest_lock_file(local: &LocalPaths, vault: &Vault) -> Result<std::fs::File> {
    acquire_ingest_lock_file_until(local, vault, None)
}

fn acquire_ingest_lock_file_until(
    local: &LocalPaths,
    vault: &Vault,
    deadline: Option<Instant>,
) -> Result<std::fs::File> {
    let locks = local.locks_dir(vault.id());
    std::fs::create_dir_all(&locks)?;
    crate::runtime::local_state::acquire_lock_until(&locks.join("ingest.lock"), deadline)
}

/// Reject a database leaf or sidecar that is a symlink or resolves into the
/// Vault before SQLite is allowed to create or open it.
fn validate_index_leaves(path: &Path, vault: &Vault) -> Result<()> {
    let vault_root = std::fs::canonicalize(vault.root())?;
    let Some(parent) = path.parent() else {
        return Err(index_error_message(
            "The local index path has no parent directory.",
            "index_path",
        ));
    };
    let parent = std::fs::canonicalize(parent)?;
    if parent.starts_with(&vault_root) {
        return Err(index_error_message(
            "The local index would live inside the Vault; choose a data directory outside it.",
            "index_inside_vault",
        ));
    }
    let mut leaves = vec![path.to_path_buf()];
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "index.sqlite".to_owned());
    for sidecar in ["-wal", "-shm"] {
        leaves.push(parent.join(format!("{name}{sidecar}")));
    }
    for leaf in leaves {
        validate_index_leaf(&leaf, parent.as_path(), &vault_root)?;
    }
    Ok(())
}

/// Validate one index leaf: the database itself or one of its SQLite sidecars.
///
/// SQLite unlinks `-wal`/`-shm` when its last connection closes, which happens
/// outside the ingest lock. Another opener can therefore legitimately observe a
/// sidecar in one filesystem call and find it gone in the next; that is not an
/// alias, a symlink or an I/O failure, so the check retries instead of failing.
/// The loop keeps every security guarantee: an observed symlink, a resolution
/// into the Vault and a resolution outside the data directory are all refused,
/// and a leaf that is gone simply does not exist yet (SQLite recreates it).
fn validate_index_leaf(leaf: &Path, parent: &Path, vault_root: &Path) -> Result<()> {
    // A leaf that keeps changing under us is refused rather than validated on a
    // best-effort basis; the bound only exists to terminate the retry loop.
    const ATTEMPTS: usize = 8;
    for _ in 0..ATTEMPTS {
        match std::fs::symlink_metadata(leaf) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(index_error_message(
                    "The local index leaf is a symbolic link; refusing to open it.",
                    "index_symlink",
                ));
            }
            Ok(_) => {
                match std::fs::canonicalize(leaf) {
                    // Windows moves a file deleted while still open into the
                    // volume's hidden `$Extend\$Deleted` folder; resolving it then
                    // points there. It is vanishing, like the cases below.
                    Ok(resolved) if posix_deleted(&resolved) => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    Ok(resolved) => {
                        if resolved.starts_with(vault_root) {
                            return Err(index_error_message(
                                "The local index leaf resolves inside the Vault; refusing to open it.",
                                "index_inside_vault",
                            ));
                        }
                        if resolved.parent() != Some(parent) {
                            return Err(index_error_message(
                                "The local index leaf resolves outside its data directory.",
                                "index_alias",
                            ));
                        }
                        return Ok(());
                    }
                    // The leaf was unlinked between the two calls; look again.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) if delete_pending(&error) => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if delete_pending(&error) => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(index_error_message(
        "The local index leaf kept changing while it was validated; refusing to open it.",
        "index_leaf_unstable",
    ))
}

/// Windows reports a file whose deletion is still pending (SQLite closing its
/// last connection) as access denied rather than not found. It is gone a moment
/// later, so the leaf check looks again; a leaf that stays denied is still
/// refused when the attempts run out.
fn delete_pending(error: &std::io::Error) -> bool {
    cfg!(windows) && error.kind() == std::io::ErrorKind::PermissionDenied
}

/// A Windows path inside the hidden folder that holds files deleted while open.
fn posix_deleted(resolved: &Path) -> bool {
    cfg!(windows)
        && resolved
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains(r"\$extend\$deleted\")
}

/// Open (creating on first use) the index for one Vault.
pub fn open_index(local: &LocalPaths, vault: &Vault) -> Result<Index> {
    open_index_until(local, vault, None)
}

/// Open the index without waiting past `deadline` for the first-open lock.
pub fn open_index_until(
    local: &LocalPaths,
    vault: &Vault,
    deadline: Option<Instant>,
) -> Result<Index> {
    local.validate_against_vault_root(vault.root())?;
    local.ensure_layout(vault.root(), vault.id())?;
    // First-open initialization is serialized with the ingest lock so two
    // processes can never observe a versioned but incomplete schema.
    let _lock = acquire_ingest_lock_file_until(local, vault, deadline)?;
    open_locked(local, vault)
}

fn open_locked(local: &LocalPaths, vault: &Vault) -> Result<Index> {
    local.validate_against_vault_root(vault.root())?;
    let path = local.index_path(vault.id());
    validate_index_leaves(&path, vault)?;
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(index_error)?;
    configure(&connection)?;
    initialize(&connection, vault)?;
    let stored_vault = meta(&connection, "vault_id")?;
    if stored_vault.as_deref() != Some(vault.id().as_str()) {
        return Err(index_error_message(
            "The index belongs to a different Vault; rebuild instead of reusing it.",
            "vault_id_mismatch",
        ));
    }
    Ok(Index {
        connection,
        path,
        local: local.clone(),
    })
}

/// Create or complete the schema in one atomic step.
///
/// DDL, `user_version` and identity metadata commit together, so a crash or a
/// concurrent opener can never observe a versioned index without its tables. An
/// existing cache with tables is completed, never dropped or replaced.
fn initialize(connection: &Connection, vault: &Vault) -> Result<()> {
    let version = user_version(connection)?;
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    if version != 0 {
        return Err(index_error_message(
            "The index schema version is unsupported; stop clients and rebuild explicitly.",
            "schema_version_unsupported",
        ));
    }
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(index_error)?;
    let result = (|| -> Result<()> {
        let existing = table_names(connection)?;
        if existing.is_empty() {
            connection
                .execute_batch(&schema_without_pragmas())
                .map_err(index_error)?;
        } else {
            for table in REQUIRED_TABLES {
                if !existing.iter().any(|name| name == table) {
                    return Err(index_error_message(
                        "The index contains partial schema state; preserve it and rebuild explicitly.",
                        "schema_incomplete",
                    ));
                }
            }
        }
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(index_error)?;
        let created = user_version(connection)?;
        if created != SCHEMA_VERSION {
            return Err(index_error_message(
                "The index schema did not record its version.",
                "schema_version",
            ));
        }
        set_meta(connection, "vault_id", vault.id().as_str())?;
        set_meta(connection, "schema_version", &SCHEMA_VERSION.to_string())?;
        set_meta(connection, "reducer_version", &REDUCER_VERSION.to_string())?;
        set_meta(connection, "generation", "0")?;
        set_meta(connection, "epoch", &ulid::Ulid::generate().to_string())?;
        Ok(())
    })();
    match result {
        Ok(()) => connection.execute_batch("COMMIT").map_err(index_error),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// The normative DDL without its connection-level PRAGMAs, which cannot run
/// inside the initialization transaction.
fn schema_without_pragmas() -> String {
    SCHEMA_SQL
        .lines()
        .filter(|line| !line.trim_start().starts_with("PRAGMA "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn table_names(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .map_err(index_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(index_error)?;
    let mut names = Vec::new();
    for row in rows {
        names.push(row.map_err(index_error)?);
    }
    Ok(names)
}

/// Rebuild the index in place from Vault truth at one fixed evaluation time.
///
/// Cached event variants that are missing from the Vault abort the rebuild with
/// `E_INDEX` and keep the old database: the Vault may be unhydrated or damaged,
/// and the cache may be the last surviving copy.
pub fn rebuild(vault: &Vault, local: &LocalPaths, now: UtcTime) -> Result<Index> {
    local.validate_against_vault_root(vault.root())?;
    local.ensure_layout(vault.root(), vault.id())?;
    // The rebuild lock is acquired before preflight so a concurrent ingest can
    // never interleave with the scan/compare/replace sequence.
    let _lock = acquire_ingest_lock_file(local, vault)?;
    let path = local.index_path(vault.id());
    validate_index_leaves(&path, vault)?;
    let scan = ingest::preflight_scan(vault)?;
    let mut index = open_locked(local, vault)?;
    let cached = cached_variants(&index)?;
    for variant in &cached {
        if !scan
            .variants
            .contains_key(&(variant.0.clone(), variant.1.clone()))
        {
            return Err(index_error_message(
                "The Vault no longer contains an event the index already observed; rebuild aborted without deleting the cache.",
                "cached_event_missing",
            )
            .with_hint("Check Vault hydration or restore the missing source file, then rebuild again."));
        }
    }
    let output = reduce_with_resolution(&scan.events, &ReduceContext { as_of: now }, None);
    let resolution = output.resolution;
    let state = output.state;
    let generation = next_generation(&index)?;
    let transaction = index
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(index_error)?;
    clear_projections(&transaction)?;
    transaction
        .execute_batch(
            "DELETE FROM event_sources; DELETE FROM raw_events; DELETE FROM source_files;
             DELETE FROM diagnostics;",
        )
        .map_err(index_error)?;
    for source in &scan.sources {
        upsert_source(&transaction, source, now)?;
    }
    for invalid in &scan.invalid_lines {
        insert_invalid_line(&transaction, invalid, now)?;
    }
    let mut seen: BTreeMap<(EventId, Sha256), ()> = BTreeMap::new();
    for event in &scan.events {
        let digest = event_digest(event);
        if seen
            .insert((event.id.clone(), digest.clone()), ())
            .is_some()
        {
            continue;
        }
        insert_raw_event(&transaction, event, &digest)?;
    }
    for occurrence in &scan.occurrences {
        ingest::insert_occurrence(&transaction, occurrence)?;
    }
    apply_resolution(&transaction, &resolution)?;
    store_projection_tx(&transaction, &state, generation, now)?;
    transaction
        .execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES('epoch', ?1)",
            params![ulid::Ulid::generate().to_string()],
        )
        .map_err(index_error)?;
    integrity_ok(&transaction)?;
    transaction.commit().map_err(index_error)?;
    Ok(index)
}

fn configure(connection: &Connection) -> Result<()> {
    connection
        .busy_timeout(std::time::Duration::from_millis(5_000))
        .map_err(index_error)?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(index_error)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(index_error)?;
    Ok(())
}

fn user_version(connection: &Connection) -> Result<i64> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(index_error)
}

fn integrity_ok(connection: &Connection) -> Result<()> {
    let foreign: String = connection
        .query_row("PRAGMA foreign_key_check", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .map_err(index_error)?
        .unwrap_or_else(|| "ok".to_owned());
    if foreign != "ok" {
        return Err(index_error_message(
            "Index rebuild failed the foreign key check.",
            "foreign_key_check",
        ));
    }
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(index_error)?;
    if integrity != "ok" {
        return Err(index_error_message(
            "Index rebuild failed the integrity check.",
            "integrity_check",
        ));
    }
    Ok(())
}

fn cached_variants(index: &Index) -> Result<Vec<(EventId, Sha256)>> {
    let mut statement = index
        .connection
        .prepare("SELECT id, digest FROM raw_events")
        .map_err(index_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(index_error)?;
    let mut variants = Vec::new();
    for row in rows {
        let (id, digest) = row.map_err(index_error)?;
        variants.push((
            EventId::parse(&id)
                .map_err(|_| index_error_message("Invalid event id in index.", "index_id"))?,
            Sha256::parse(&digest).map_err(|_| {
                index_error_message("Invalid event digest in index.", "index_digest")
            })?,
        ));
    }
    Ok(variants)
}

fn next_generation(index: &Index) -> Result<u64> {
    let current = meta(&index.connection, "generation")?
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    Ok(current.saturating_add(1))
}

pub fn meta(connection: &Connection, key: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension as _;
    connection
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(index_error)
}

pub fn set_meta(connection: &Connection, key: &str, value: &str) -> Result<()> {
    connection
        .execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(index_error)?;
    Ok(())
}

/// Serialize one projection and store it in the project row.
fn insert_project(
    transaction: &rusqlite::Transaction<'_>,
    slug: &ProjectSlug,
    state: &State,
) -> Result<()> {
    let project = &state.projects[slug];
    let summary = serde_json::to_string(&project.summary).expect("summary serializes");
    transaction
        .execute(
            "INSERT INTO projects(slug, archived, variance_rank, active, json)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                slug.as_str(),
                i32::from(project.summary.archived),
                project.summary.variance.rank(),
                i32::from(project.summary.variance == crate::model::Variance::Active),
                summary
            ],
        )
        .map_err(index_error)?;
    Ok(())
}

pub(crate) fn clear_projections(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    transaction
        .execute_batch(
            "DELETE FROM edges; DELETE FROM needs_you; DELETE FROM deliveries;
             DELETE FROM receipts; DELETE FROM decisions; DELETE FROM features;
             DELETE FROM rules; DELETE FROM sessions; DELETE FROM projects;",
        )
        .map_err(index_error)?;
    for row in list_projects(transaction)? {
        transaction
            .execute(
                "DELETE FROM meta WHERE key = ?1",
                params![format!("policy:{}", row.as_str())],
            )
            .map_err(index_error)?;
    }
    Ok(())
}

fn list_projects(transaction: &rusqlite::Transaction<'_>) -> Result<Vec<ProjectSlug>> {
    let mut statement = transaction
        .prepare("SELECT slug FROM projects")
        .map_err(index_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(index_error)?;
    let mut projects = Vec::new();
    for row in rows {
        let slug = row.map_err(index_error)?;
        projects
            .push(ProjectSlug::parse(&slug).map_err(|_| {
                index_error_message("Invalid project slug in index.", "index_slug")
            })?);
    }
    Ok(projects)
}

pub(crate) fn store_projection_tx(
    transaction: &rusqlite::Transaction<'_>,
    state: &State,
    generation: u64,
    now: UtcTime,
) -> Result<()> {
    for slug in state.projects.keys() {
        insert_project(transaction, slug, state)?;
        let project = &state.projects[slug];
        for feature in project.features.values() {
            transaction
                .execute(
                    "INSERT INTO features(project, node, progress, gate, basis, liveness, json)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        slug.as_str(),
                        feature.node.as_str(),
                        feature.progress.as_str(),
                        feature.gate.as_str(),
                        basis_str(feature.basis),
                        feature.liveness.as_str(),
                        serde_json::to_string(feature).expect("feature serializes"),
                    ],
                )
                .map_err(index_error)?;
        }
        let adjacency: BTreeMap<&crate::model::NodePath, Vec<&crate::model::NodePath>> = {
            let mut map: BTreeMap<&crate::model::NodePath, Vec<&crate::model::NodePath>> =
                BTreeMap::new();
            for feature in project.features.values() {
                map.insert(&feature.node, feature.accepted_depends_on.iter().collect());
            }
            map
        };
        for (downstream, upstreams) in adjacency {
            let source = project.features[downstream]
                .field_sources
                .get("depends_on")
                .cloned();
            let Some(source) = source else {
                continue;
            };
            for upstream in upstreams {
                transaction
                    .execute(
                        "INSERT INTO edges(project, upstream, downstream, source_event)
                         VALUES(?1, ?2, ?3, ?4)",
                        params![
                            slug.as_str(),
                            upstream.as_str(),
                            downstream.as_str(),
                            source.as_str()
                        ],
                    )
                    .map_err(index_error)?;
            }
        }
        for decision in project.decisions.values() {
            transaction
                .execute(
                    "INSERT INTO decisions(ask_id, project, node, lifecycle, effective_answer, json)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        decision.id.as_str(),
                        slug.as_str(),
                        decision.node.as_str(),
                        decision.lifecycle.as_str(),
                        decision
                            .effective_receipt
                            .as_ref()
                            .map(EventId::as_str)
                            .unwrap_or(""),
                        serde_json::to_string(decision).expect("decision serializes"),
                    ],
                )
                .map_err(index_error)?;
        }
        for receipt in project.receipts.values() {
            transaction
                .execute(
                    "INSERT INTO receipts(id, project, ask_id, json) VALUES(?1, ?2, ?3, ?4)",
                    params![
                        receipt.id.as_str(),
                        slug.as_str(),
                        receipt.ask.as_str(),
                        serde_json::to_string(receipt).expect("receipt serializes")
                    ],
                )
                .map_err(index_error)?;
        }
        for delivery in project.deliveries.values() {
            transaction
                .execute(
                    "INSERT INTO deliveries(id, project, node, review_status, json)
                     VALUES(?1, ?2, ?3, ?4, ?5)",
                    params![
                        delivery.id.as_str(),
                        slug.as_str(),
                        delivery.node.as_str(),
                        review_status_str(delivery.review),
                        serde_json::to_string(delivery).expect("delivery serializes")
                    ],
                )
                .map_err(index_error)?;
        }
        for row in &project.needs_you {
            transaction
                .execute(
                    "INSERT INTO needs_you(id, project, kind, score_x2, since, json)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        row.id,
                        slug.as_str(),
                        row.kind.as_str(),
                        i64::try_from(row.score_x2).unwrap_or(i64::MAX),
                        row.since.canonical(),
                        serde_json::to_string(row).expect("need serializes")
                    ],
                )
                .map_err(index_error)?;
        }
        for rule in &project.rules {
            transaction
                .execute(
                    "INSERT INTO rules(project, rule_id, enabled, json) VALUES(?1, ?2, ?3, ?4)",
                    params![
                        slug.as_str(),
                        rule.value.rule_id.as_str(),
                        i32::from(rule.value.enabled),
                        serde_json::to_string(rule).expect("rule serializes")
                    ],
                )
                .map_err(index_error)?;
        }
        for session in project.sessions.values() {
            transaction
                .execute(
                    "INSERT INTO sessions(session_id, project, host, harness, last_event_ts, liveness, json)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        session.id.as_str(),
                        slug.as_str(),
                        session.host.as_str(),
                        harness_str(session.harness),
                        session.last_event_ts.canonical(),
                        session.liveness.as_str(),
                        serde_json::to_string(session).expect("session serializes")
                    ],
                )
                .map_err(index_error)?;
        }
        transaction
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES(?1, ?2)",
                params![
                    format!("policy:{}", slug.as_str()),
                    serde_json::to_string(&project.policy).expect("policy serializes")
                ],
            )
            .map_err(index_error)?;
    }
    for diagnostic in &state.diagnostics {
        transaction
            .execute(
                "INSERT OR REPLACE INTO diagnostics(id, path, byte_start, project, event_id, code, message, resolved)
                 VALUES(?1, NULL, NULL, ?2, ?3, ?4, ?5, 0)",
                params![
                    diagnostic.id,
                    diagnostic.project.as_ref().map(ProjectSlug::as_str),
                    diagnostic.event.as_ref().map(EventId::as_str),
                    diagnostic.code.as_deref().unwrap_or("diagnostic"),
                    diagnostic.message
                ],
            )
            .map_err(index_error)?;
    }
    set_meta(
        transaction,
        "event_set_digest",
        state.event_set_digest.as_str(),
    )?;
    set_meta(transaction, "reducer_version", &REDUCER_VERSION.to_string())?;
    set_meta(transaction, "evaluated_at", &state.evaluated_at.canonical())?;
    set_meta(transaction, "generation", &generation.to_string())?;
    set_meta(transaction, "last_scan_at", &now.canonical())?;
    Ok(())
}

pub fn insert_raw_event(
    transaction: &rusqlite::Transaction<'_>,
    event: &Event,
    digest: &Sha256,
) -> Result<bool> {
    let json = canonical_json(&serde_json::to_value(event).expect("event serializes"));
    let changed = transaction
        .execute(
            "INSERT INTO raw_events(id, digest, writer, seq, ts, project, kind, json, status, reason)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'valid', NULL)
             ON CONFLICT(id, digest) DO NOTHING",
            params![
                event.id.as_str(),
                digest.as_str(),
                event.writer.to_string(),
                i64::try_from(event.seq).unwrap_or(i64::MAX),
                event.ts.canonical(),
                event.project.as_str(),
                event.kind(),
                json
            ],
        )
        .map_err(index_error)?;
    Ok(changed == 1)
}

pub fn apply_resolution(
    transaction: &rusqlite::Transaction<'_>,
    resolution: &Resolution,
) -> Result<()> {
    for ((id, digest), variant) in &resolution.variants {
        transaction
            .execute(
                "UPDATE raw_events SET status = ?1, reason = ?2 WHERE id = ?3 AND digest = ?4",
                params![
                    variant.status.as_str(),
                    variant.reason,
                    id.as_str(),
                    digest.as_str()
                ],
            )
            .map_err(index_error)?;
    }
    Ok(())
}

/// One stored raw variant.
#[derive(Debug, Clone)]
pub struct RawVariant {
    pub id: EventId,
    pub digest: Sha256,
    pub event: Option<Event>,
}

/// Load every raw variant, decoding the ones that still decode.
pub fn load_variants(index: &Index) -> Result<Vec<RawVariant>> {
    let mut statement = index
        .connection
        .prepare("SELECT id, digest, json, writer FROM raw_events ORDER BY id, digest")
        .map_err(index_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(index_error)?;
    let mut variants = Vec::new();
    for row in rows {
        let (id, digest, json, writer) = row.map_err(index_error)?;
        let id = EventId::parse(&id)
            .map_err(|_| index_error_message("Invalid event id in index.", "index_id"))?;
        let digest = Sha256::parse(&digest)
            .map_err(|_| index_error_message("Invalid event digest in index.", "index_digest"))?;
        let event = decode_stored(&id, &digest, &json, &writer);
        variants.push(RawVariant { id, digest, event });
    }
    Ok(variants)
}

pub(crate) fn decode_stored(
    id: &EventId,
    digest: &Sha256,
    json: &str,
    writer: &str,
) -> Option<Event> {
    let _ = digest;
    let project = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|value| value["project"].as_str().map(str::to_owned))?;
    let relative = format!("projects/{project}/{writer}.jsonl");
    let source = crate::model::SourcePath::from_relative_path(&relative).ok()?;
    let mut text = json.as_bytes().to_vec();
    text.push(b'\n');
    let event = parse_event(&text, &source).ok()?;
    (event.id == *id).then_some(event)
}

/// All eligible events plus the resolution of the current raw set.
pub fn load_resolution(index: &Index) -> Result<(Vec<Event>, Resolution)> {
    let variants = load_variants(index)?;
    let events: Vec<Event> = variants
        .into_iter()
        .filter_map(|variant| variant.event)
        .collect();
    let resolution = resolve(&events);
    Ok((events, resolution))
}

pub(crate) fn insert_invalid_line(
    transaction: &rusqlite::Transaction<'_>,
    invalid: &ingest::InvalidLine,
    now: UtcTime,
) -> Result<()> {
    let _ = now;
    transaction
        .execute(
            "INSERT OR REPLACE INTO diagnostics(id, path, byte_start, project, event_id, code, message, resolved)
             VALUES(?1, ?2, ?3, NULL, NULL, ?4, ?5, 0)",
            params![
                invalid.id,
                invalid.path,
                i64::try_from(invalid.byte_start).unwrap_or(i64::MAX),
                invalid.code,
                invalid.message
            ],
        )
        .map_err(index_error)?;
    Ok(())
}

pub(crate) fn upsert_source(
    transaction: &rusqlite::Transaction<'_>,
    source: &ingest::SourceRecord,
    now: UtcTime,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO source_files(path, offset, prefix_sha256, observed_size, last_seen_at, health)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
               offset = excluded.offset,
               prefix_sha256 = excluded.prefix_sha256,
               observed_size = excluded.observed_size,
               last_seen_at = excluded.last_seen_at,
               health = excluded.health",
            params![
                source.path,
                i64::try_from(source.offset).unwrap_or(i64::MAX),
                source.prefix_sha256.as_str(),
                i64::try_from(source.observed_size).unwrap_or(i64::MAX),
                now.canonical(),
                source.health
            ],
        )
        .map_err(index_error)?;
    Ok(())
}

pub(crate) fn index_error(error: rusqlite::Error) -> AgendarError {
    AgendarError::new(ErrorCode::Index, "Cannot read or update the local index.")
        .with_detail(format!("sqlite: {error}"))
        .with_hint("Stop other clients, preserve the index, and retry; do not delete Vault events.")
}

pub(crate) fn index_error_message(message: &str, detail: &str) -> AgendarError {
    AgendarError::new(ErrorCode::Index, message)
        .with_detail(detail)
        .with_hint("Preserve both the Vault and the local index, then retry the rebuild.")
}

fn basis_str(basis: crate::model::Basis) -> &'static str {
    match basis {
        crate::model::Basis::Firm => "firm",
        crate::model::Basis::Assumed => "assumed",
        crate::model::Basis::Tainted => "tainted",
    }
}

fn harness_str(harness: crate::model::Harness) -> &'static str {
    match harness {
        crate::model::Harness::ClaudeCode => "claude_code",
        crate::model::Harness::Codex => "codex",
        crate::model::Harness::Generic => "generic",
        crate::model::Harness::Pi => "pi",
        crate::model::Harness::Herdr => "herdr",
        crate::model::Harness::OpenCode => "opencode",
        crate::model::Harness::Cursor => "cursor",
        crate::model::Harness::Dsh => "dsh",
        crate::model::Harness::Agy => "agy",
        crate::model::Harness::Gemini => "gemini",
    }
}

fn review_status_str(status: crate::model::ReviewStatus) -> &'static str {
    match status {
        crate::model::ReviewStatus::Pending => "pending",
        crate::model::ReviewStatus::Accepted => "accepted",
        crate::model::ReviewStatus::SentBack => "sent_back",
    }
}

//! Vault tests: containment, locked appends with crash recovery,
//! complete-line reads, local data-root layout and verified blob publication.
//!
//! Multi-process cases spawn this same test binary in worker mode (see
//! `worker_process`) so two real OS processes contend on the same locks.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agendar_core::error::ErrorCode;
use agendar_core::model::{
    Agent, BlobExt, BlobRef, Event, EventId, EventPayload, Harness, HostId, NodePath, ProjectSlug,
    Refs, RuleId, RuleSet, SessionId, Sha256, SourcePath, UtcTime, WriterId, WriterRole,
    canonical_json, parse_event,
};
use agendar_core::runtime::{HostIdentity, LocalPaths, acquire_lock};
use agendar_core::vault::{
    AppendResult, Clock, EventDraft, IdSource, MAX_BATCH_BYTES, MAX_LINE_BYTES,
    SEGMENT_SEAL_THRESHOLD, SealReason, SystemClock, UlidIdSource, Vault, append_event,
    blob_journal_path, blob_lock_path, blob_relative_path, put_blob, read_blob,
    read_complete_lines, recover_writer, seal_writer, segment_would_seal, writer_journal_path,
    writer_lock_path, writer_relative_path,
};

const HOST: &str = "h-00000000000000000000000001";
const SESSION: &str = "00000000000000000000000101";
const SESSION2: &str = "00000000000000000000000102";
const PROJECT: &str = "example";
const FIXED_TS: &str = "2026-01-01T00:00:00.000Z";

fn project() -> ProjectSlug {
    ProjectSlug::parse(PROJECT).expect("fixture project")
}

fn host() -> HostId {
    HostId::parse(HOST).expect("fixture host")
}

fn session() -> SessionId {
    SessionId::parse(SESSION).expect("fixture session")
}

fn session2() -> SessionId {
    SessionId::parse(SESSION2).expect("fixture session")
}

fn writer(role: WriterRole) -> WriterId {
    WriterId::new(host(), role)
}

fn operator() -> WriterId {
    writer(WriterRole::Operator)
}

fn agent() -> WriterId {
    writer(WriterRole::Agent(session()))
}

fn agent2() -> WriterId {
    writer(WriterRole::Agent(session2()))
}

fn steward() -> WriterId {
    writer(WriterRole::Steward)
}

fn key(writer: WriterId) -> agendar_core::vault::WriterKey {
    agendar_core::vault::WriterKey {
        project: project(),
        writer,
    }
}

fn fixed_time(text: &str) -> UtcTime {
    UtcTime::parse(text).expect("fixture timestamp")
}

struct FixedClock(UtcTime);

impl Clock for FixedClock {
    fn now(&self) -> UtcTime {
        self.0
    }
}

struct FixedIds {
    next: u64,
}

impl FixedIds {
    fn new(next: u64) -> Self {
        Self { next }
    }
}

impl IdSource for FixedIds {
    fn next_event_id(&mut self) -> EventId {
        let value = self.next;
        self.next += 1;
        EventId::parse(&format!("{value:026}")).expect("fixture event id")
    }
}

fn fixed_clock() -> FixedClock {
    FixedClock(fixed_time(FIXED_TS))
}

fn rule_draft(text: &str) -> EventDraft {
    EventDraft {
        refs: Refs::default(),
        payload: EventPayload::RuleSet(RuleSet {
            rule_id: RuleId::parse("house_rule").expect("fixture rule id"),
            text: text.to_owned(),
            enabled: true,
        }),
    }
}

fn pulse_draft(message: &str) -> EventDraft {
    EventDraft {
        refs: Refs::default(),
        payload: EventPayload::Pulse(agendar_core::model::Pulse {
            agent: Agent {
                session: session(),
                harness: Harness::Generic,
            },
            message: message.to_owned(),
            boundary: false,
            eta: None,
        }),
    }
}

fn long_node(index: u64, n: u64) -> NodePath {
    let a = format!("{}_{}", "a".repeat(50), index);
    let b = format!("{}_{}", "b".repeat(50), n);
    let c = format!("{}_{}", "c".repeat(50), n);
    NodePath::parse(&format!("core/{a}/{b}/{c}")).expect("fixture node")
}

fn big_feature_draft(index: u64) -> EventDraft {
    let depends_on: Vec<NodePath> = (0..64).map(|n| long_node(index, n)).collect();
    EventDraft {
        refs: Refs {
            node: Some(NodePath::parse("core/big").expect("fixture node")),
            ..Refs::default()
        },
        payload: EventPayload::PlanFeature(agendar_core::model::PlanFeature {
            agent: Some(Agent {
                session: session(),
                harness: Harness::Generic,
            }),
            title: Some("Big feature".to_owned()),
            depends_on: Some(depends_on),
            slice: Some("core".to_owned()),
            demo_kind: Some(agendar_core::model::DemoKind::CliTranscript),
            milestone: None,
            deadline: None,
            priority: None,
            paused: None,
            handoff: None,
        }),
    }
}

struct Scratch {
    _temp: tempfile::TempDir,
    vault: Vault,
    local: LocalPaths,
    root: PathBuf,
    data: PathBuf,
}

fn scratch() -> Scratch {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("vault");
    let data = temp.path().join("data");
    let local = LocalPaths::new(&data).expect("local paths");
    let mut ids = FixedIds::new(1);
    let vault = Vault::init(&root, &fixed_clock(), &mut ids).expect("vault init");
    let root = vault.root().to_path_buf();
    let data = local.data_root().to_path_buf();
    Scratch {
        _temp: temp,
        vault,
        local,
        root,
        data,
    }
}

/// Set a file or directory mode for a permission-fault fixture. Every use is
/// paired with a restore so the temporary directory can always be removed.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions).expect("set permissions");
}

fn append(scratch: &Scratch, writer: WriterId, draft: EventDraft) -> AppendResult {
    let mut ids = UlidIdSource;
    append_event(
        &scratch.vault,
        &scratch.local,
        &key(writer),
        draft,
        &SystemClock,
        &mut ids,
    )
    .expect("append")
}

fn segment_path(scratch: &Scratch, writer: &WriterId, segment: u32) -> PathBuf {
    scratch
        .root
        .join(writer_relative_path(&project(), writer, segment).expect("writer relative path"))
}

fn read_all(path: &Path) -> Vec<u8> {
    fs::read(path).expect("read file")
}

fn lines_of(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect()
}

fn parse_file(scratch: &Scratch, writer: &WriterId, segment: u32) -> Vec<Event> {
    let relative = writer_relative_path(&project(), writer, segment).expect("writer relative path");
    let source = SourcePath::from_relative_path(&relative).expect("source path");
    let bytes = read_all(&scratch.root.join(&relative));
    lines_of(&bytes)
        .into_iter()
        .map(|line| parse_event(line, &source).expect("parse event"))
        .collect()
}

fn journal_path(scratch: &Scratch, writer: &WriterId) -> PathBuf {
    writer_journal_path(&scratch.local, &scratch.vault, &key(writer.clone()))
}

fn assert_pending_cleared(scratch: &Scratch, writer: &WriterId) {
    let value: serde_json::Value =
        serde_json::from_slice(&read_all(&journal_path(scratch, writer))).expect("journal");
    assert!(
        value.get("pending").is_none() || value["pending"].is_null(),
        "the pending append is cleared after recovery"
    );
}

fn assert_pending_retained(scratch: &Scratch, writer: &WriterId) {
    let value: serde_json::Value =
        serde_json::from_slice(&read_all(&journal_path(scratch, writer))).expect("journal");
    assert!(
        !value["pending"].is_null(),
        "the pending append is preserved, not discarded"
    );
}

fn operator_event(id: &str, seq: u64, text: &str) -> Event {
    Event {
        v: 1,
        id: EventId::parse(id).expect("id"),
        seq,
        writer: operator(),
        ts: fixed_time(FIXED_TS),
        project: project(),
        refs: Refs::default(),
        payload: EventPayload::RuleSet(RuleSet {
            rule_id: RuleId::parse("house_rule").expect("rule"),
            text: text.to_owned(),
            enabled: true,
        }),
    }
}

fn pending_payload_path(scratch: &Scratch, writer: &WriterId) -> PathBuf {
    let journal = journal_path(scratch, writer);
    journal.with_extension("pending")
}

fn write_pending_state(
    scratch: &Scratch,
    writer: &WriterId,
    event: &Event,
    segment: u32,
    prefix_bytes: Option<usize>,
) -> Vec<u8> {
    let relative = writer_relative_path(&project(), writer, segment).expect("writer relative path");
    let value = serde_json::to_value(event).expect("serialize event");
    let mut line = canonical_json(&value).into_bytes();
    line.push(b'\n');
    // Create the local layout and segment parent through a normal append of a
    // different writer, then write the journal and pending bytes directly, as
    // a crash between journal fsync and append would leave them.
    scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let target = scratch.root.join(&relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).expect("segment parent");
    }
    match prefix_bytes {
        Some(prefix) => fs::write(&target, &line[..prefix]).expect("partial segment"),
        None => {
            if target.exists() {
                fs::remove_file(&target).expect("clear segment");
            }
        }
    }
    let digest = Sha256::from_bytes(&line);
    let journal = serde_json::json!({
        "v": 1,
        "project": project().as_str(),
        "writer": writer.to_string(),
        "next_seq": event.seq,
        "pending": {
            "path": relative,
            "offset": 0,
            "bytes": line.len(),
            "digest": digest.as_str(),
        },
    });
    let journal_path = journal_path(scratch, writer);
    fs::write(
        &journal_path,
        serde_json::to_vec(&journal).expect("journal"),
    )
    .expect("journal");
    fs::write(pending_payload_path(scratch, writer), &line).expect("pending bytes");
    line
}

fn workspace_manifest(scratch: &Scratch) -> serde_json::Value {
    serde_json::from_slice(&read_all(&scratch.root.join("vault.json"))).expect("manifest")
}

// ---------------------------------------------------------------------------
// Vault manifest and project paths
// ---------------------------------------------------------------------------

#[test]
fn init_creates_a_create_once_manifest_and_projects_directory() {
    let scratch = scratch();
    assert!(scratch.vault.created());
    assert!(scratch.root.join("projects").is_dir());
    let manifest = workspace_manifest(&scratch);
    assert_eq!(manifest["v"], 1);
    assert_eq!(manifest["id"], "00000000000000000000000001");
    assert_eq!(manifest["created_at"], FIXED_TS);
    assert_eq!(
        manifest.as_object().expect("object").len(),
        3,
        "the manifest has exactly v, id and created_at"
    );
    let raw = read_all(&scratch.root.join("vault.json"));
    assert_eq!(raw.last(), Some(&b'\n'));

    let mut ids = FixedIds::new(99);
    let reopened = Vault::init(
        &scratch.root,
        &FixedClock(fixed_time("2030-01-01T00:00:00.000Z")),
        &mut ids,
    )
    .expect("second init opens");
    assert!(!reopened.created());
    assert_eq!(reopened.id(), scratch.vault.id());
    assert_eq!(
        reopened.manifest().created_at,
        scratch.vault.manifest().created_at
    );
}

#[test]
fn open_rejects_missing_unknown_and_malformed_manifests() {
    let temp = tempfile::tempdir().expect("temp");
    let missing = temp.path().join("missing");
    fs::create_dir_all(&missing).expect("dir");
    let error = Vault::open(&missing).expect_err("missing manifest");
    assert_eq!(error.code(), ErrorCode::NotFound);

    let unknown = temp.path().join("unknown");
    fs::create_dir_all(&unknown).expect("dir");
    fs::write(unknown.join("vault.json"), b"{\"v\":2,\"id\":\"00000000000000000000000001\",\"created_at\":\"2026-01-01T00:00:00.000Z\"}\n")
        .expect("manifest");
    let error = Vault::open(&unknown).expect_err("unknown version");
    assert_eq!(error.code(), ErrorCode::Unsupported);

    let malformed = temp.path().join("malformed");
    fs::create_dir_all(&malformed).expect("dir");
    fs::write(malformed.join("vault.json"), b"{\"v\":1}\n").expect("manifest");
    let error = Vault::open(&malformed).expect_err("malformed manifest");
    assert_eq!(error.code(), ErrorCode::Validation);

    let extra = temp.path().join("extra");
    fs::create_dir_all(&extra).expect("dir");
    fs::write(
        extra.join("vault.json"),
        b"{\"v\":1,\"id\":\"00000000000000000000000001\",\"created_at\":\"2026-01-01T00:00:00.000Z\",\"extra\":true}\n",
    )
    .expect("manifest");
    let error = Vault::open(&extra).expect_err("unknown manifest field");
    assert_eq!(error.code(), ErrorCode::Validation);
}

#[cfg(unix)]
#[test]
fn open_rejects_a_symlinked_manifest() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().join("vault");
    fs::create_dir_all(&root).expect("dir");
    let outside = temp.path().join("outside.json");
    fs::write(
        &outside,
        b"{\"v\":1,\"id\":\"00000000000000000000000001\",\"created_at\":\"2026-01-01T00:00:00.000Z\"}\n",
    )
    .expect("manifest");
    symlink(&outside, root.join("vault.json")).expect("symlink");
    let error = Vault::open(&root).expect_err("symlinked manifest");
    assert_eq!(error.code(), ErrorCode::Validation);
}

#[test]
fn project_directory_is_contained_and_not_created_by_the_lookup() {
    let scratch = scratch();
    let directory = scratch
        .vault
        .project_dir(&project())
        .expect("project directory");
    assert_eq!(directory, scratch.root.join("projects").join(PROJECT));
    assert!(!directory.exists());
}

// ---------------------------------------------------------------------------
// Local path separation
// ---------------------------------------------------------------------------

fn separation(scratch_root: &Path, data: &Path) -> agendar_core::Result<LocalPaths> {
    let local = LocalPaths::new(data)?;
    local.validate_against_vault_root(scratch_root)?;
    Ok(local)
}

#[test]
fn disjoint_data_and_vault_roots_are_accepted() {
    let temp = tempfile::tempdir().expect("temp");
    let vault = temp.path().join("vault");
    fs::create_dir_all(&vault).expect("vault");
    let data = temp.path().join("data");
    let local = separation(&vault, &data).expect("disjoint");
    let layout = local
        .ensure_layout(&vault, &VaultIdFixture::id())
        .expect("layout");
    assert!(layout.locks.is_dir());
    assert!(layout.pending.is_dir());
    assert!(layout.tmp.is_dir());
    assert!(!layout.locks.starts_with(&vault));
    assert!(!layout.pending.starts_with(&vault));
    assert!(!layout.tmp.starts_with(&vault));
}

#[test]
fn a_new_project_directory_creates_every_component() {
    let scratch = scratch();
    let project = ProjectSlug::parse("another").expect("slug");
    let directory = agendar_core::vault::ensure_host_directory(&scratch.root, &project, &host())
        .expect("host directory");
    assert!(directory.is_dir());
    assert!(scratch.root.join("projects").join("another").is_dir());
    assert!(directory.starts_with(&scratch.root));
}

#[test]
fn a_local_layout_creates_every_missing_component() {
    let temp = tempfile::tempdir().expect("temp");
    let vault = temp.path().join("vault");
    fs::create_dir_all(&vault).expect("vault");
    let data = temp.path().join("deeply").join("nested").join("data");
    let local = LocalPaths::new(&data).expect("local paths");
    let layout = local
        .ensure_layout(&vault, &VaultIdFixture::id())
        .expect("layout");
    for directory in [
        &layout.vault_local,
        &layout.locks,
        &layout.pending,
        &layout.tmp,
    ] {
        assert!(directory.is_dir(), "{} is created", directory.display());
    }
    assert!(data.is_dir());
}

#[cfg(unix)]
#[test]
fn an_interrupted_project_directory_creation_is_retried_until_its_parent_sync_succeeds() {
    let scratch = scratch();
    let projects = scratch.root.join("projects");
    // The Vault root and `projects` are already published by `Vault::init`;
    // withholding read access here makes `sync_dir(projects)` fail while a new
    // child entry can still be created inside it.
    set_mode(&projects, 0o333);

    let first = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("interrupted directory"),
        &SystemClock,
        &mut UlidIdSource,
    );
    let created = projects.join(PROJECT);
    let created_before_failure = created.is_dir();
    let error = first.expect_err("the new project entry cannot be published");
    assert_eq!(error.code(), ErrorCode::Io);
    assert!(
        created_before_failure,
        "the project directory exists but its parent entry was never synced"
    );

    // A retry must refresh the already-visible component instead of assuming
    // its entry is durable, so it fails again while `projects` cannot be synced.
    let retry = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("interrupted directory"),
        &SystemClock,
        &mut UlidIdSource,
    );
    let error = retry.expect_err("the retry syncs the visible component's parent");
    assert_eq!(error.code(), ErrorCode::Io);

    set_mode(&projects, 0o755);
    let appended = append(&scratch, operator(), rule_draft("interrupted directory"));
    assert_eq!(appended.event.seq, 1);
    assert_eq!(parse_file(&scratch, &operator(), 1).len(), 1);
}

#[cfg(unix)]
#[test]
fn an_interrupted_local_layout_creation_is_retried_until_its_parent_sync_succeeds() {
    let temp = tempfile::tempdir().expect("temp");
    let vault = temp.path().join("vault");
    fs::create_dir_all(&vault).expect("vault");
    let data = temp.path().join("data");
    fs::create_dir_all(&data).expect("data");
    let local = LocalPaths::new(&data).expect("local paths");
    let data = local.data_root().to_path_buf();
    set_mode(&data, 0o333);

    let first = local.ensure_layout(&vault, &VaultIdFixture::id());
    let vaults = data.join("vaults");
    let created_before_failure = vaults.is_dir();
    let error = first.expect_err("the new `vaults` entry cannot be published");
    assert_eq!(error.code(), ErrorCode::Io);
    assert!(
        created_before_failure,
        "`vaults` exists but its entry in the data root was never synced"
    );

    let retry = local.ensure_layout(&vault, &VaultIdFixture::id());
    let error = retry.expect_err("the retry syncs the visible component's parent");
    assert_eq!(error.code(), ErrorCode::Io);

    set_mode(&data, 0o755);
    let layout = local
        .ensure_layout(&vault, &VaultIdFixture::id())
        .expect("layout");
    for directory in [
        &layout.vault_local,
        &layout.locks,
        &layout.pending,
        &layout.tmp,
    ] {
        assert!(directory.is_dir(), "{} exists", directory.display());
    }
}

struct VaultIdFixture;

impl VaultIdFixture {
    fn id() -> agendar_core::model::VaultId {
        agendar_core::model::VaultId::parse("00000000000000000000000001").expect("vault id")
    }
}

#[test]
fn data_inside_vault_is_rejected() {
    let temp = tempfile::tempdir().expect("temp");
    let vault = temp.path().join("vault");
    fs::create_dir_all(&vault).expect("vault");
    let data = vault.join("local-data");
    let error = separation(&vault, &data).expect_err("data inside vault");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert!(
        !data.exists(),
        "rejection must happen before directory creation"
    );
}

#[test]
fn vault_inside_data_is_rejected() {
    let temp = tempfile::tempdir().expect("temp");
    let data = temp.path().join("data");
    let vault = data.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    let error = separation(&vault, &data).expect_err("vault inside data");
    assert_eq!(error.code(), ErrorCode::Validation);
}

#[test]
fn equal_data_and_vault_roots_are_rejected() {
    let temp = tempfile::tempdir().expect("temp");
    let vault = temp.path().join("same");
    fs::create_dir_all(&vault).expect("vault");
    let error = separation(&vault, &vault).expect_err("equal roots");
    assert_eq!(error.code(), ErrorCode::Validation);
}

#[test]
fn data_vaults_ancestor_case_is_rejected_before_local_state_exists() {
    let temp = tempfile::tempdir().expect("temp");
    let data = temp.path().join("data");
    let vault = data.join("vaults");
    fs::create_dir_all(&vault).expect("vault");
    let local = LocalPaths::new(&data).expect("paths");
    let error = local
        .validate_against_vault_root(&vault)
        .expect_err("vaults ancestor");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert!(
        fs::read_dir(&vault).expect("vault dir").next().is_none(),
        "no local state may be created in the Vault"
    );
}

#[cfg(unix)]
#[test]
fn a_vaults_symlink_alias_into_the_vault_is_rejected_before_writing() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temp");
    let vault = temp.path().join("vault");
    let data = temp.path().join("data");
    fs::create_dir_all(&vault).expect("vault");
    fs::create_dir_all(&data).expect("data");
    let inner = vault.join("inside");
    fs::create_dir_all(&inner).expect("inside");
    symlink(&inner, data.join("vaults")).expect("alias");

    let local = LocalPaths::new(&data).expect("paths");
    let error = local
        .ensure_layout(&vault, &VaultIdFixture::id())
        .expect_err("alias into vault");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert!(
        fs::read_dir(&inner).expect("inside dir").next().is_none(),
        "rejection must happen before any local state is written"
    );
}

// ---------------------------------------------------------------------------
// Writer appends
// ---------------------------------------------------------------------------

#[test]
fn append_writes_one_canonical_line_and_advances_the_sequence() {
    let scratch = scratch();
    let first = append(&scratch, operator(), rule_draft("First rule"));
    assert_eq!(first.event.seq, 1);
    assert_eq!(first.event.v, 1);
    assert_eq!(first.event.project, project());
    assert_eq!(first.event.writer, operator());
    assert_eq!(
        first.relative_path,
        "projects/example/h-00000000000000000000000001/operator.jsonl"
    );

    let bytes = read_all(&segment_path(&scratch, &operator(), 1));
    assert_eq!(bytes.last(), Some(&b'\n'));
    let events = parse_file(&scratch, &operator(), 1);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0], first.event);
    assert_eq!(first.byte_end, bytes.len() as u64);

    let second = append(&scratch, operator(), rule_draft("Second rule"));
    assert_eq!(second.event.seq, 2);
    let events = parse_file(&scratch, &operator(), 1);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], first.event);
    assert_eq!(events[1], second.event);
}

#[test]
fn writer_streams_are_independent_per_writer_and_project() {
    let scratch = scratch();
    let op = append(&scratch, operator(), rule_draft("Operator rule"));
    assert_eq!(op.event.seq, 1);
    let agent_event = append(&scratch, agent(), pulse_draft("Agent one"));
    assert_eq!(agent_event.event.seq, 1);
    let steward_event = append(
        &scratch,
        steward(),
        EventDraft {
            refs: Refs::default(),
            payload: EventPayload::SessionLost(agendar_core::model::SessionLost {
                session: session(),
                process_start: 1,
                reason: agendar_core::model::LostReason::Exited,
                exit_code: None,
            }),
        },
    );
    assert_eq!(steward_event.event.seq, 1);
    assert!(segment_path(&scratch, &operator(), 1).exists());
    assert!(segment_path(&scratch, &agent(), 1).exists());
    assert!(segment_path(&scratch, &steward(), 1).exists());
}

#[test]
fn append_rejects_an_invalid_draft_before_writing_anything() {
    let scratch = scratch();
    let mut ids = UlidIdSource;
    let bad = EventDraft {
        refs: Refs::default(),
        payload: EventPayload::Pulse(agendar_core::model::Pulse {
            agent: Agent {
                session: session2(),
                harness: Harness::Generic,
            },
            message: "wrong session".to_owned(),
            boundary: false,
            eta: None,
        }),
    };
    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(agent()),
        bad,
        &SystemClock,
        &mut ids,
    )
    .expect_err("agent session must match the writer");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert!(!segment_path(&scratch, &agent(), 1).exists());
    assert!(!journal_path(&scratch, &agent()).exists());
    assert!(!pending_payload_path(&scratch, &agent()).exists());
}

#[test]
fn append_rejects_a_role_violation_before_writing() {
    let scratch = scratch();
    let mut ids = UlidIdSource;
    // rule.set is human-only; the agent writer must be rejected.
    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(agent()),
        rule_draft("Agent may not set a house rule"),
        &SystemClock,
        &mut ids,
    )
    .expect_err("role violation");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert!(!segment_path(&scratch, &agent(), 1).exists());
}

#[test]
fn session_end_seal_refuses_later_appends_and_is_idempotent() {
    let scratch = scratch();
    append(&scratch, agent(), pulse_draft("Started"));
    seal_writer(
        &scratch.vault,
        &scratch.local,
        &key(agent()),
        SealReason::SessionEnded,
    )
    .expect("seal");
    seal_writer(
        &scratch.vault,
        &scratch.local,
        &key(agent()),
        SealReason::SessionEnded,
    )
    .expect("second seal");
    let bytes_before = read_all(&segment_path(&scratch, &agent(), 1));

    let mut ids = UlidIdSource;
    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(agent()),
        pulse_draft("After the session ended"),
        &SystemClock,
        &mut ids,
    )
    .expect_err("sealed writer");
    assert_eq!(error.code(), ErrorCode::WriterFork);
    assert_eq!(error.detail(), Some("writer_sealed"));
    assert_eq!(read_all(&segment_path(&scratch, &agent(), 1)), bytes_before);

    // Another writer is unaffected.
    append(&scratch, operator(), rule_draft("Still writable"));
}

#[test]
fn only_agent_sessions_may_be_sealed_at_session_end() {
    let scratch = scratch();
    append(&scratch, operator(), rule_draft("human stream"));
    let error = seal_writer(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        SealReason::SessionEnded,
    )
    .expect_err("operator session seal");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(error.detail(), Some("seal_reason"));
    // The human stream is still writable.
    let next = append(&scratch, operator(), rule_draft("still writable"));
    assert_eq!(next.event.seq, 2);
}

#[test]
fn size_seal_forces_the_next_segment() {
    let scratch = scratch();
    append(&scratch, operator(), rule_draft("Before size seal"));
    seal_writer(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        SealReason::Size,
    )
    .expect("size seal");
    let result = append(&scratch, operator(), rule_draft("After size seal"));
    assert_eq!(result.event.seq, 2);
    assert!(result.relative_path.ends_with("operator.2.jsonl"));
    assert_eq!(parse_file(&scratch, &operator(), 1).len(), 1);
    assert_eq!(parse_file(&scratch, &operator(), 2), vec![result.event]);
}

#[test]
fn segment_threshold_opens_the_next_numeric_segment() {
    let scratch = scratch();
    let mut last_segment = 1;
    let mut appends = 0_u64;
    while appends < 120 {
        let result = append(&scratch, agent(), big_feature_draft(appends));
        appends += 1;
        let segment = result
            .relative_path
            .rsplit_once(".jsonl")
            .expect("suffix")
            .0
            .rsplit_once('.')
            .and_then(|(_, value)| value.parse::<u32>().ok())
            .unwrap_or(1);
        if segment > last_segment {
            last_segment = segment;
            break;
        }
    }
    assert!(last_segment >= 2, "the stream must seal after 1 MiB");
    let first = segment_path(&scratch, &agent(), 1);
    assert!(fs::metadata(&first).expect("segment 1").len() as u64 <= SEGMENT_SEAL_THRESHOLD);
    // The events in both segments parse and stay in numeric order.
    let events1 = parse_file(&scratch, &agent(), 1);
    let events2 = parse_file(&scratch, &agent(), 2);
    assert_eq!(events1.len() + events2.len(), appends as usize);
    let mut previous = 0;
    for event in events1.iter().chain(events2.iter()) {
        assert_eq!(event.seq, previous + 1);
        previous = event.seq;
    }
}

#[test]
fn segment_seal_boundary_allows_an_exact_fit() {
    assert!(!segment_would_seal(0, SEGMENT_SEAL_THRESHOLD));
    assert!(!segment_would_seal(SEGMENT_SEAL_THRESHOLD - 10, 10));
    assert!(segment_would_seal(SEGMENT_SEAL_THRESHOLD - 10, 11));
    assert!(segment_would_seal(SEGMENT_SEAL_THRESHOLD, 1));
}

#[test]
fn sequence_recovery_uses_the_maximum_across_numeric_segments() {
    let scratch = scratch();
    append(&scratch, operator(), rule_draft("one"));
    // Move the first segment to segment 10 and craft a larger sequence there.
    let first = segment_path(&scratch, &operator(), 1);
    let tenth = segment_path(&scratch, &operator(), 10);
    fs::create_dir_all(tenth.parent().expect("parent")).expect("dirs");
    fs::rename(&first, &tenth).expect("rename segment");

    let mut ids = UlidIdSource;
    let next = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("after ten"),
        &SystemClock,
        &mut ids,
    )
    .expect("append");
    assert_eq!(next.event.seq, 2, "scan sees segment 10 numerically");
    assert!(next.relative_path.ends_with("operator.10.jsonl"));
}

#[test]
fn sequence_recovery_uses_the_maximum_across_segments_that_both_exist() {
    let scratch = scratch();
    append(&scratch, operator(), rule_draft("one"));
    append(&scratch, operator(), rule_draft("two"));
    // A later numeric segment arrives beside segment 1 (for example from sync)
    // and holds a competing higher sequence maximum.
    let tenth = segment_path(&scratch, &operator(), 10);
    fs::create_dir_all(tenth.parent().expect("parent")).expect("dirs");
    let later = operator_event("00000000000000000000000060", 100, "Later segment");
    let value = serde_json::to_value(&later).expect("value");
    let mut line = canonical_json(&value).into_bytes();
    line.push(b'\n');
    fs::write(&tenth, &line).expect("segment 10");
    assert!(segment_path(&scratch, &operator(), 1).exists());

    let next = append(&scratch, operator(), rule_draft("after ten"));
    assert_eq!(next.event.seq, 101, "both segments are scanned");
    assert!(next.relative_path.ends_with("operator.10.jsonl"));
    let events = parse_file(&scratch, &operator(), 10);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], later);
    assert_eq!(events[1], next.event);
}

#[test]
fn recover_completes_a_full_pending_line_and_returns_the_original_event() {
    let scratch = scratch();
    let event = operator_event("00000000000000000000000042", 7, "Recovered rule");
    let line = write_pending_state(&scratch, &operator(), &event, 1, None);
    // A crash after `write_all` but before the journal update leaves the whole
    // line in the segment while the pending record is still durable.
    fs::write(segment_path(&scratch, &operator(), 1), &line).expect("full pending line");

    let mut ids = UlidIdSource;
    let recovered = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("Recovered rule"),
        &SystemClock,
        &mut ids,
    )
    .expect("recovery returns the durable event");
    assert_eq!(recovered.event, event);
    assert_eq!(
        recovered.relative_path,
        "projects/example/h-00000000000000000000000001/operator.jsonl"
    );
    assert_eq!(read_all(&segment_path(&scratch, &operator(), 1)), line);
    assert_pending_cleared(&scratch, &operator());
    assert!(!pending_payload_path(&scratch, &operator()).exists());
}

#[test]
fn recover_writer_returns_a_full_pending_line_without_consuming_a_draft() {
    let scratch = scratch();
    let event = operator_event("00000000000000000000000046", 4, "Startup recovery");
    let line = write_pending_state(&scratch, &operator(), &event, 1, None);
    fs::write(segment_path(&scratch, &operator(), 1), &line).expect("full pending line");

    let recovered = recover_writer(&scratch.vault, &scratch.local, &key(operator()))
        .expect("recovery")
        .expect("pending append");
    assert_eq!(recovered.event, event);
    assert_eq!(read_all(&segment_path(&scratch, &operator(), 1)), line);
    assert_pending_cleared(&scratch, &operator());
    assert!(!pending_payload_path(&scratch, &operator()).exists());
}

#[test]
fn a_pending_line_followed_by_extra_bytes_freezes_the_writer() {
    let scratch = scratch();
    let event = operator_event("00000000000000000000000047", 1, "Suffix plus garbage");
    let line = write_pending_state(&scratch, &operator(), &event, 1, None);
    let mut conflicting = line.clone();
    conflicting.extend_from_slice(b"CONFLICTING SUFFIX");
    fs::write(segment_path(&scratch, &operator(), 1), &conflicting).expect("segment");

    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("new"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("bytes beyond the journalled suffix");
    assert_eq!(error.code(), ErrorCode::WriterFork);
    assert_eq!(
        error.detail(),
        Some("writer segment is longer than its pending append")
    );
    assert_eq!(
        read_all(&segment_path(&scratch, &operator(), 1)),
        conflicting,
        "the whole suffix is left untouched"
    );
    assert_pending_retained(&scratch, &operator());
    assert!(pending_payload_path(&scratch, &operator()).exists());

    // A retried append cannot admit a new draft after the freeze.
    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("retry"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("still frozen");
    assert_eq!(error.code(), ErrorCode::WriterFork);
}

#[test]
fn a_pending_line_followed_by_a_second_line_freezes_the_writer() {
    let scratch = scratch();
    let event = operator_event("00000000000000000000000048", 1, "Two lines");
    let line = write_pending_state(&scratch, &operator(), &event, 1, None);
    let mut conflicting = line.clone();
    conflicting.extend_from_slice(&line);
    fs::write(segment_path(&scratch, &operator(), 1), &conflicting).expect("segment");

    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("new"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("a second complete line is a conflicting suffix");
    assert_eq!(error.code(), ErrorCode::WriterFork);
    assert_eq!(
        read_all(&segment_path(&scratch, &operator(), 1)),
        conflicting
    );
    assert_pending_retained(&scratch, &operator());
}

#[cfg(unix)]
#[test]
fn a_full_pending_line_that_cannot_be_flushed_keeps_its_recovery_material() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = scratch();
    let event = operator_event("00000000000000000000000049", 1, "Unflushed line");
    let line = write_pending_state(&scratch, &operator(), &event, 1, None);
    let segment = segment_path(&scratch, &operator(), 1);
    fs::write(&segment, &line).expect("full pending line");
    let mut permissions = fs::metadata(&segment).expect("metadata").permissions();
    permissions.set_mode(0o444);
    fs::set_permissions(&segment, permissions).expect("read-only segment");

    let result = recover_writer(&scratch.vault, &scratch.local, &key(operator()));

    let mut permissions = fs::metadata(&segment).expect("metadata").permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&segment, permissions).expect("restore");

    // Recovery flushes a verified suffix before retiring its journal; a segment
    // that cannot be flushed keeps the journal and payload for a later retry.
    let error = result.expect_err("the verified suffix cannot be flushed");
    assert_eq!(error.code(), ErrorCode::Io);
    assert_pending_retained(&scratch, &operator());
    assert!(pending_payload_path(&scratch, &operator()).exists());

    let recovered = recover_writer(&scratch.vault, &scratch.local, &key(operator()))
        .expect("retry recovery")
        .expect("pending append");
    assert_eq!(recovered.event, event);
    assert_pending_cleared(&scratch, &operator());
    assert!(!pending_payload_path(&scratch, &operator()).exists());
}

#[cfg(unix)]
#[test]
fn a_pending_partial_suffix_that_cannot_sync_its_directory_keeps_its_recovery_material() {
    let scratch = scratch();
    let event = operator_event("00000000000000000000000052", 1, "Unsynced segment entry");
    let line = write_pending_state(&scratch, &operator(), &event, 1, Some(40));
    let segment = segment_path(&scratch, &operator(), 1);
    let host_dir = segment.parent().expect("host directory").to_path_buf();
    // A segment created by the dead process may be readable while its directory
    // entry was never published, so recovery must sync that entry even when it
    // only appends to an existing segment. Withholding access to the host
    // directory makes exactly that sync fail.
    set_mode(&host_dir, 0o333);

    let result = recover_writer(&scratch.vault, &scratch.local, &key(operator()));

    set_mode(&host_dir, 0o755);

    let error = result.expect_err("the verified segment entry cannot be synced");
    assert_eq!(error.code(), ErrorCode::Io);
    assert_pending_retained(&scratch, &operator());
    assert!(pending_payload_path(&scratch, &operator()).exists());
    assert_eq!(
        read_all(&segment),
        line,
        "the appended suffix stays, but the event is not acknowledged"
    );

    let recovered = recover_writer(&scratch.vault, &scratch.local, &key(operator()))
        .expect("recovery after restoring access")
        .expect("pending append");
    assert_eq!(recovered.event, event);
    assert_eq!(read_all(&segment), line);
    assert_pending_cleared(&scratch, &operator());
    assert!(!pending_payload_path(&scratch, &operator()).exists());
}

#[test]
fn recover_appends_a_partial_prefix_without_truncating_it() {
    let scratch = scratch();
    let event = Event {
        v: 1,
        id: EventId::parse("00000000000000000000000043").expect("id"),
        seq: 3,
        writer: agent(),
        ts: fixed_time(FIXED_TS),
        project: project(),
        refs: Refs::default(),
        payload: EventPayload::Pulse(agendar_core::model::Pulse {
            agent: Agent {
                session: session(),
                harness: Harness::Generic,
            },
            message: "Suffix recovery".to_owned(),
            boundary: false,
            eta: None,
        }),
    };
    let line = write_pending_state(&scratch, &agent(), &event, 1, Some(40));
    assert_eq!(read_all(&segment_path(&scratch, &agent(), 1)), &line[..40]);
    let recovered = recover_writer(&scratch.vault, &scratch.local, &key(agent()))
        .expect("recovery")
        .expect("pending append");
    assert_eq!(recovered.event, event);
    assert_eq!(read_all(&segment_path(&scratch, &agent(), 1)), line);
    assert_pending_cleared(&scratch, &agent());
    assert!(!pending_payload_path(&scratch, &agent()).exists());
}

#[test]
fn recover_crash_before_append_writes_the_journalled_line() {
    let scratch = scratch();
    let event = Event {
        v: 1,
        id: EventId::parse("00000000000000000000000044").expect("id"),
        seq: 1,
        writer: agent2(),
        ts: fixed_time(FIXED_TS),
        project: project(),
        refs: Refs::default(),
        payload: EventPayload::Pulse(agendar_core::model::Pulse {
            agent: Agent {
                session: session2(),
                harness: Harness::Generic,
            },
            message: "Never appended".to_owned(),
            boundary: false,
            eta: None,
        }),
    };
    let line = write_pending_state(&scratch, &agent2(), &event, 1, None);
    assert!(!segment_path(&scratch, &agent2(), 1).exists());
    let recovered = recover_writer(&scratch.vault, &scratch.local, &key(agent2()))
        .expect("recovery")
        .expect("pending append");
    assert_eq!(recovered.event, event);
    assert_eq!(read_all(&segment_path(&scratch, &agent2(), 1)), line);
}

#[test]
fn a_conflicting_suffix_freezes_the_writer() {
    let scratch = scratch();
    let event = Event {
        v: 1,
        id: EventId::parse("00000000000000000000000045").expect("id"),
        seq: 1,
        writer: operator(),
        ts: fixed_time(FIXED_TS),
        project: project(),
        refs: Refs::default(),
        payload: EventPayload::RuleSet(RuleSet {
            rule_id: RuleId::parse("house_rule").expect("rule"),
            text: "Conflicting".to_owned(),
            enabled: true,
        }),
    };
    let line = write_pending_state(&scratch, &operator(), &event, 1, Some(20));
    // Replace the plausible prefix with different bytes of the same length.
    let mut conflicting = line[..20].to_vec();
    conflicting[0] = b'[';
    fs::write(segment_path(&scratch, &operator(), 1), &conflicting).expect("conflict");
    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("new"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("conflicting suffix");
    assert_eq!(error.code(), ErrorCode::WriterFork);
    assert_eq!(
        read_all(&segment_path(&scratch, &operator(), 1)),
        conflicting
    );
    assert!(journal_path(&scratch, &operator()).exists());

    // The writer stays frozen.
    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("later"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("frozen writer");
    assert_eq!(error.code(), ErrorCode::WriterFork);
}

#[test]
fn a_partial_tail_without_a_journal_is_abandoned_for_the_next_segment() {
    let scratch = scratch();
    append(&scratch, operator(), rule_draft("Complete"));
    let first = segment_path(&scratch, &operator(), 1);
    let mut bytes = read_all(&first);
    let complete_len = bytes.len();
    bytes.extend_from_slice(b"{\"v\":1,\"id\":\"torn");
    fs::write(&first, &bytes).expect("torn tail");

    let next = append(&scratch, operator(), rule_draft("After the tear"));
    assert_eq!(next.event.seq, 2);
    assert!(next.relative_path.ends_with("operator.2.jsonl"));
    assert_eq!(read_all(&first), bytes, "the torn tail is never truncated");
    assert_eq!(read_all(&first)[..complete_len], bytes[..complete_len]);
    assert_eq!(parse_file(&scratch, &operator(), 2), vec![next.event]);
}

#[cfg(unix)]
#[test]
fn a_symlinked_project_directory_is_rejected_without_writing_outside() {
    use std::os::unix::fs::symlink;

    let scratch = scratch();
    let outside = scratch.data.parent().expect("parent").join("outside");
    fs::create_dir_all(&outside).expect("outside");
    fs::create_dir_all(scratch.root.join("projects")).expect("projects");
    symlink(&outside, scratch.root.join("projects").join(PROJECT)).expect("symlink project");

    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("Must not escape"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("symlinked project");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert!(
        fs::read_dir(&outside).expect("outside").next().is_none(),
        "nothing may be written through the symlink"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_segment_file_is_rejected() {
    use std::os::unix::fs::symlink;

    let scratch = scratch();
    append(&scratch, operator(), rule_draft("First"));
    let segment = segment_path(&scratch, &operator(), 1);
    let outside = scratch.data.parent().expect("parent").join("outside.jsonl");
    fs::write(&outside, read_all(&segment)).expect("copy");
    fs::remove_file(&segment).expect("remove");
    symlink(&outside, &segment).expect("symlink");

    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("Second"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("symlinked segment");
    assert_eq!(error.code(), ErrorCode::Validation);
}

#[cfg(unix)]
#[test]
fn a_read_only_vault_fails_the_append_without_losing_journal_state() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = scratch();
    append(&scratch, operator(), rule_draft("Before read-only"));
    let segment = segment_path(&scratch, &operator(), 1);
    let mut permissions = fs::metadata(&segment).expect("metadata").permissions();
    permissions.set_mode(0o444);
    fs::set_permissions(&segment, permissions).expect("read-only file");

    let result = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("Blocked"),
        &SystemClock,
        &mut UlidIdSource,
    );

    let mut permissions = fs::metadata(&segment).expect("metadata").permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&segment, permissions).expect("restore");

    let error = result.expect_err("read-only host directory");
    assert_eq!(error.code(), ErrorCode::Io);
    // The pending journal remains for crash recovery rather than pretending
    // success, and the intended event becomes durable once writing is possible
    // again.
    let journal: serde_json::Value =
        serde_json::from_slice(&read_all(&journal_path(&scratch, &operator()))).expect("journal");
    assert!(!journal["pending"].is_null());
    let recovered = recover_writer(&scratch.vault, &scratch.local, &key(operator()))
        .expect("recovery")
        .expect("pending append");
    assert_eq!(recovered.event.seq, 2);
    match &recovered.event.payload {
        EventPayload::RuleSet(rule) => assert_eq!(rule.text, "Blocked"),
        other => panic!("unexpected recovered payload: {other:?}"),
    }
    let third = append(&scratch, operator(), rule_draft("After recovery"));
    assert_eq!(third.event.seq, 3);
    assert_eq!(parse_file(&scratch, &operator(), 1).len(), 3);
}

// ---------------------------------------------------------------------------
// Complete-line reader
// ---------------------------------------------------------------------------

#[test]
fn reader_returns_complete_lines_and_offsets() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    fs::write(&path, b"one\ntwo\r\nthree\n").expect("write");
    let batch = read_complete_lines(&path, 0, 4 * 1024 * 1024).expect("read");
    assert_eq!(batch.observed_size, 15);
    assert_eq!(batch.next_offset, 15);
    assert!(!batch.trailing_partial);
    assert_eq!(
        batch
            .lines
            .iter()
            .map(|line| line.bytes.clone())
            .collect::<Vec<_>>(),
        vec![b"one\n".to_vec(), b"two\r\n".to_vec(), b"three\n".to_vec()]
    );
    assert_eq!(batch.lines[0].start, 0);
    assert_eq!(batch.lines[0].end, 4);
    assert_eq!(batch.lines[2].end, 15);
}

#[test]
fn reader_skips_an_unfinished_final_line() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    fs::write(&path, b"one\npart").expect("write");
    let batch = read_complete_lines(&path, 0, 1024).expect("read");
    assert_eq!(batch.lines.len(), 1);
    assert_eq!(batch.next_offset, 4);
    assert!(batch.trailing_partial);
    let second = read_complete_lines(&path, batch.next_offset, 1024).expect("read");
    assert_eq!(second.lines.len(), 0);
    assert_eq!(second.next_offset, 4);
    assert!(second.trailing_partial);
}

#[test]
fn reader_does_not_reject_a_split_utf8_character_before_the_newline() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    let text = "é\n".as_bytes();
    // Write only the first byte of the two-byte character.
    fs::write(&path, &text[..1]).expect("write");
    let batch = read_complete_lines(&path, 0, 1024).expect("partial UTF-8 is not an error");
    assert!(batch.lines.is_empty());
    assert!(batch.trailing_partial);
    fs::write(&path, text).expect("complete");
    let batch = read_complete_lines(&path, 0, 1024).expect("read");
    assert_eq!(batch.lines.len(), 1);
    assert_eq!(batch.lines[0].bytes, b"\xc3\xa9\n".to_vec());
}

#[test]
fn reader_drains_an_oversized_line_and_keeps_going() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    let oversized = vec![b'x'; 70_000];
    let mut content = oversized.clone();
    content.push(b'\n');
    content.extend_from_slice(b"good\n");
    fs::write(&path, &content).expect("write");
    let batch = read_complete_lines(&path, 0, 1024).expect("read");
    assert_eq!(batch.lines.len(), 1, "one oversized line per batch");
    assert!(batch.lines[0].oversized);
    assert!(batch.lines[0].bytes.len() <= 65_536);
    assert_eq!(batch.next_offset, 70_001);
    let second = read_complete_lines(&path, batch.next_offset, 1024).expect("read");
    assert_eq!(second.lines.len(), 1);
    assert_eq!(second.lines[0].bytes, b"good\n".to_vec());
    assert!(!second.trailing_partial);
}

#[test]
fn reader_advances_over_a_malformed_complete_line() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    fs::write(&path, b"not json\n{\"v\":1}\n").expect("write");
    let batch = read_complete_lines(&path, 0, 1024).expect("read");
    assert_eq!(batch.lines.len(), 2);
    assert_eq!(batch.next_offset, 17);
}

#[test]
fn reader_honours_the_batch_budget() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    fs::write(&path, b"a\nb\nc\n").expect("write");
    let batch = read_complete_lines(&path, 0, 2).expect("read");
    assert_eq!(batch.lines.len(), 1, "one line per two-byte budget");
    assert_eq!(batch.next_offset, 2);
    let next = read_complete_lines(&path, batch.next_offset, 2).expect("read");
    assert_eq!(next.lines.len(), 1);
    assert_eq!(next.next_offset, 4);
}

#[test]
fn reader_never_exceeds_the_batch_budget_for_ordinary_lines() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    // 65 ordinary 65,536-byte lines (the exact per-line maximum).
    let mut contents = Vec::new();
    for _ in 0..65 {
        contents.extend(std::iter::repeat_n(b'a', 65_535));
        contents.push(b'\n');
    }
    fs::write(&path, &contents).expect("write");
    let batch = read_complete_lines(&path, 0, MAX_BATCH_BYTES).expect("read");
    assert_eq!(batch.lines.len(), 64, "the budget fits 64 maximal lines");
    assert!(batch.lines.iter().all(|line| !line.oversized));
    assert_eq!(batch.next_offset, 4 * 1024 * 1024);
    assert_eq!(batch.next_offset, batch.lines[63].end);
    assert!(!batch.trailing_partial);

    // The next batch starts exactly at the unconsumed line.
    let next = read_complete_lines(&path, batch.next_offset, MAX_BATCH_BYTES).expect("read");
    assert_eq!(next.lines.len(), 1);
    assert_eq!(next.next_offset, 65 * 65_536);
    assert!(!next.trailing_partial);
}

#[test]
fn reader_returns_one_line_when_the_budget_is_below_one_line() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    fs::write(&path, b"first\nsecond\n").expect("write");
    let batch = read_complete_lines(&path, 0, 1).expect("read");
    assert_eq!(batch.lines.len(), 1, "a batch always makes progress");
    assert_eq!(batch.next_offset, 6);
    let next = read_complete_lines(&path, batch.next_offset, 1).expect("read");
    assert_eq!(next.lines.len(), 1);
    assert_eq!(next.next_offset, 13);
}

#[test]
fn reader_mixes_ordinary_and_oversized_lines_within_the_budget() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    let mut contents = Vec::new();
    contents.extend(std::iter::repeat_n(b'c', 200_000));
    contents.push(b'\n');
    contents.extend(std::iter::repeat_n(b'd', 65_535));
    contents.push(b'\n');
    fs::write(&path, &contents).expect("write");

    // The oversized line keeps its explicit bounded-drain exception: it is
    // drained and returned even though it alone exceeds the budget, while the
    // ordinary line after it is left for the next batch.
    let drained = read_complete_lines(&path, 0, 65_536).expect("read");
    assert_eq!(drained.lines.len(), 1);
    assert!(drained.lines[0].oversized);
    assert_eq!(drained.lines[0].bytes.len(), MAX_LINE_BYTES);
    assert_eq!(drained.next_offset, 200_001);
    assert!(!drained.trailing_partial);

    let next = read_complete_lines(&path, drained.next_offset, 65_536).expect("read");
    assert_eq!(
        next.lines.len(),
        1,
        "an exactly fitting ordinary line is read"
    );
    assert!(!next.lines[0].oversized);
    assert_eq!(next.next_offset, 265_537);
    assert!(!next.trailing_partial);
}

#[test]
fn reader_bounds_consecutive_oversized_lines_by_the_batch_budget() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    // 128 consecutive oversized lines: retaining one 64-KiB prefix per line
    // would grow the batch with the file size (about 8 MiB here).
    const LINES: usize = 128;
    const LINE_SOURCE: usize = 70_001;
    let mut contents = Vec::new();
    for _ in 0..LINES {
        contents.extend(std::iter::repeat_n(b'z', LINE_SOURCE - 1));
        contents.push(b'\n');
    }
    fs::write(&path, &contents).expect("write");

    let budget = 300_000;
    let batch = read_complete_lines(&path, 0, budget).expect("read");
    let retained: usize = batch.lines.iter().map(|line| line.bytes.len()).sum();
    assert!(batch.lines.iter().all(|line| line.oversized));
    assert_eq!(
        batch.lines.len(),
        5,
        "the drain stops once the batch budget is spent"
    );
    assert!(
        retained <= budget + MAX_LINE_BYTES,
        "retained {retained} bytes stay within the budget plus one bounded prefix"
    );
    assert_eq!(batch.next_offset, 5 * LINE_SOURCE as u64);
    assert!(!batch.trailing_partial);

    // Successive batches make forward progress over the same bounded window.
    let mut offset = 0_u64;
    let mut seen = 0_usize;
    let mut batches = 0_usize;
    while offset < contents.len() as u64 {
        let batch = read_complete_lines(&path, offset, budget).expect("read");
        let retained: usize = batch.lines.iter().map(|line| line.bytes.len()).sum();
        assert!(
            retained <= budget + MAX_LINE_BYTES,
            "batch {batches} retains {retained} bytes"
        );
        assert!(batch.next_offset > offset, "the batch makes progress");
        offset = batch.next_offset;
        seen += batch.lines.len();
        batches += 1;
        assert!(batches <= LINES, "the reader terminates");
    }
    assert_eq!(seen, LINES, "every oversized line is drained exactly once");
    assert!(
        !read_complete_lines(&path, offset, budget)
            .expect("eof")
            .trailing_partial
    );
}

#[test]
fn reader_keeps_an_exact_budget_batch_before_oversized_data() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    let mut contents = Vec::new();
    contents.extend(std::iter::repeat_n(b'o', 99));
    contents.push(b'\n');
    contents.extend(std::iter::repeat_n(b'p', 99));
    contents.push(b'\n');
    contents.extend(std::iter::repeat_n(b'q', 70_000));
    contents.push(b'\n');
    fs::write(&path, &contents).expect("write");

    // Two ordinary 100-byte lines fill a 200-byte budget exactly; the oversized
    // line that follows waits for the next batch instead of being drained into
    // a batch whose budget is already spent.
    let batch = read_complete_lines(&path, 0, 200).expect("read");
    assert_eq!(batch.lines.len(), 2);
    assert!(batch.lines.iter().all(|line| !line.oversized));
    assert_eq!(batch.next_offset, 200);
    assert!(!batch.trailing_partial);

    let next = read_complete_lines(&path, batch.next_offset, 200).expect("read");
    assert_eq!(next.lines.len(), 1);
    assert!(next.lines[0].oversized);
    assert_eq!(next.lines[0].bytes.len(), MAX_LINE_BYTES);
    assert_eq!(next.next_offset, 70_201);
    assert!(!next.trailing_partial);

    let eof = read_complete_lines(&path, next.next_offset, 200).expect("read");
    assert!(eof.lines.is_empty());
    assert_eq!(eof.next_offset, 70_201);
}

#[test]
fn reader_reports_a_shrunk_source_and_a_missing_file() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("stream.jsonl");
    fs::write(&path, b"one\n").expect("write");
    let error = read_complete_lines(&path, 99, 1024).expect_err("shrink");
    assert_eq!(error.code(), ErrorCode::Io);
    assert_eq!(error.detail(), Some("source_shrunk"));

    let missing = temp.path().join("missing.jsonl");
    let error = read_complete_lines(&missing, 0, 1024).expect_err("missing");
    assert_eq!(error.code(), ErrorCode::Io);
    assert_eq!(error.detail(), Some("source_missing"));
    assert!(error.retryable());
}

#[test]
fn reader_returns_an_empty_batch_for_an_empty_file() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("empty.jsonl");
    fs::write(&path, b"").expect("write");
    let batch = read_complete_lines(&path, 0, 1024).expect("read");
    assert!(batch.lines.is_empty());
    assert_eq!(batch.next_offset, 0);
    assert_eq!(batch.observed_size, 0);
    assert!(!batch.trailing_partial);
}

// ---------------------------------------------------------------------------
// Local journal and host identity
// ---------------------------------------------------------------------------

#[test]
fn host_identity_is_created_once_and_reused() {
    let temp = tempfile::tempdir().expect("temp");
    let data = temp.path().join("data");
    let first = HostIdentity::load_or_create(&data, fixed_time(FIXED_TS)).expect("identity");
    assert!(first.host.as_str().starts_with("h-"));
    let second =
        HostIdentity::load_or_create(&data, fixed_time("2030-01-01T00:00:00.000Z")).expect("reuse");
    assert_eq!(first, second);
    let raw = fs::read(data.join("identity.json")).expect("identity file");
    assert_eq!(raw.last(), Some(&b'\n'));

    let loaded = HostIdentity::load(&data).expect("load").expect("present");
    assert_eq!(loaded, first);
}

#[test]
fn host_identity_rejects_unknown_versions_and_malformed_files() {
    let temp = tempfile::tempdir().expect("temp");
    let data = temp.path().join("data");
    fs::create_dir_all(&data).expect("data");
    fs::write(
        data.join("identity.json"),
        b"{\"v\":2,\"host\":\"h-00000000000000000000000001\",\"created_at\":\"2026-01-01T00:00:00.000Z\"}\n",
    )
    .expect("identity");
    let error = HostIdentity::load(&data).expect_err("version");
    assert_eq!(error.code(), ErrorCode::Unsupported);

    fs::write(data.join("identity.json"), b"{\"v\":1}\n").expect("identity");
    let error = HostIdentity::load(&data).expect_err("shape");
    assert_eq!(error.code(), ErrorCode::Validation);
}

#[test]
fn concurrent_identity_creation_agrees_on_one_host() {
    let temp = tempfile::tempdir().expect("temp");
    let data = temp.path().join("data");
    let mut handles = Vec::new();
    for _ in 0..8 {
        let data = data.clone();
        handles.push(std::thread::spawn(move || {
            HostIdentity::load_or_create(&data, fixed_time(FIXED_TS))
        }));
    }
    let hosts: Vec<HostId> = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread").expect("identity").host)
        .collect();
    assert!(hosts.windows(2).all(|pair| pair[0] == pair[1]));
}

// ---------------------------------------------------------------------------
// Blobs
// ---------------------------------------------------------------------------

fn staged_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut bytes = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.push((state & 0xff) as u8);
    }
    bytes
}

fn put_bytes(scratch: &Scratch, ext: BlobExt, bytes: &[u8]) -> BlobRef {
    let mut input = bytes;
    put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        ext,
        &mut input,
    )
    .expect("put blob")
}

#[test]
fn blob_round_trip_uses_the_content_addressed_target() {
    let scratch = scratch();
    let bytes = b"a small transcript\n";
    let blob = put_bytes(&scratch, BlobExt::Txt, bytes);
    assert_eq!(blob.host, host());
    assert_eq!(blob.ext, BlobExt::Txt);
    assert_eq!(blob.bytes, bytes.len() as u64);
    assert_eq!(blob.sha256, Sha256::from_bytes(bytes));
    let target = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &blob.sha256,
        BlobExt::Txt,
    ));
    assert_eq!(read_all(&target), bytes);
    assert_eq!(
        read_blob(&scratch.vault, &project(), &blob).expect("read blob"),
        bytes
    );
    let mut no_bytes = &bytes[..];
    let again = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut no_bytes,
    )
    .expect("reuse");
    assert_eq!(again, blob);
}

#[test]
fn empty_and_oversized_blobs_are_rejected_without_creating_targets() {
    let scratch = scratch();
    let mut empty: &[u8] = b"";
    let error = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut empty,
    )
    .expect_err("empty");
    assert_eq!(error.code(), ErrorCode::Validation);

    let too_large = vec![0_u8; 52_428_801];
    let mut input = &too_large[..];
    let error = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Mp4,
        &mut input,
    )
    .expect_err("too large");
    assert_eq!(error.code(), ErrorCode::Validation);
    let host_dir = scratch.root.join("projects").join(PROJECT).join(HOST);
    assert!(!host_dir.join("blobs").exists());
}

#[test]
fn existing_identical_bytes_are_reused_and_conflicting_bytes_are_never_overwritten() {
    let scratch = scratch();
    let bytes = staged_bytes(4096, 7);
    let blob = put_bytes(&scratch, BlobExt::Md, &bytes);
    let target = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &blob.sha256,
        BlobExt::Md,
    ));
    let mut conflict = bytes.clone();
    conflict[0] ^= 0xff;
    fs::write(&target, &conflict).expect("conflict");
    let mut input = &bytes[..];
    let error = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Md,
        &mut input,
    )
    .expect_err("digest conflict");
    assert_eq!(error.code(), ErrorCode::BlobCorrupt);
    assert_eq!(read_all(&target), conflict, "never overwritten");
}

fn craft_completed_publication_journal(
    scratch: &Scratch,
    bytes: &[u8],
    staged_name: &str,
) -> PathBuf {
    let digest = Sha256::from_bytes(bytes);
    scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let journal = blob_journal_path(
        &scratch.local,
        &scratch.vault,
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    );
    let value = serde_json::json!({
        "v": 1,
        "project": project().as_str(),
        "host": HOST,
        "sha256": digest.as_str(),
        "ext": "txt",
        "bytes": bytes.len(),
        "staged_name": staged_name,
        "target_path": blob_relative_path(&project(), &host(), &digest, BlobExt::Txt),
        "published": bytes.len(),
        "complete": true,
    });
    fs::write(&journal, serde_json::to_vec(&value).expect("journal")).expect("journal");
    journal
}

#[test]
fn a_completed_publication_journal_cannot_delete_bytes_outside_its_staging_directory() {
    let scratch = scratch();
    append(&scratch, operator(), rule_draft("Event truth"));
    let segment = segment_path(&scratch, &operator(), 1);
    let before = read_all(&segment);

    // A real file beside (not inside) the staging directory: `../outside.stage`
    // from `tmp` resolves exactly here, so a traversal would delete it.
    let layout = scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let outside = layout.vault_local.join("outside.stage");
    fs::write(&outside, b"external bytes").expect("outside");

    let bytes = b"journal bytes";
    let digest = Sha256::from_bytes(bytes);
    let target = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    ));
    fs::create_dir_all(target.parent().expect("blobs")).expect("blobs");
    fs::write(&target, bytes).expect("complete target");

    let relative = writer_relative_path(&project(), &operator(), 1).expect("relative");
    for staged_name in [relative.as_str(), "../outside.stage", "nested/blob.stage"] {
        // A damaged local journal must not authorize deleting event truth or an
        // external file, even when it claims the publication is complete.
        let journal = craft_completed_publication_journal(&scratch, bytes, staged_name);
        let mut input = &bytes[..];
        let error = put_blob(
            &scratch.vault,
            &scratch.local,
            &project(),
            &host(),
            BlobExt::Txt,
            &mut input,
        )
        .expect_err("foreign staging name");
        assert_eq!(error.code(), ErrorCode::Validation);
        assert_eq!(error.detail(), Some("blob_stage_name"));
        assert_eq!(read_all(&segment), before, "event truth is preserved");
        assert_eq!(read_all(&outside), b"external bytes");
        assert_eq!(read_all(&target), bytes, "the verified target is preserved");
        assert!(
            journal.exists(),
            "the unreadable journal is preserved for inspection"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_publication_journal_rejects_a_symlinked_staged_file() {
    use std::os::unix::fs::symlink;

    let scratch = scratch();
    let bytes = b"symlinked staged bytes";
    let digest = Sha256::from_bytes(bytes);
    let layout = scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let outside = scratch
        .data
        .parent()
        .expect("parent")
        .join("outside-stage.bin");
    fs::write(&outside, bytes).expect("outside");
    symlink(&outside, layout.tmp.join("blob-evil.stage")).expect("staged symlink");
    let journal = blob_journal_path(
        &scratch.local,
        &scratch.vault,
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    );
    let value = serde_json::json!({
        "v": 1,
        "project": project().as_str(),
        "host": HOST,
        "sha256": digest.as_str(),
        "ext": "txt",
        "bytes": bytes.len(),
        "staged_name": "blob-evil.stage",
        "target_path": blob_relative_path(&project(), &host(), &digest, BlobExt::Txt),
        "published": 0,
        "complete": false,
    });
    fs::write(&journal, serde_json::to_vec(&value).expect("journal")).expect("journal");

    let mut input = &bytes[..];
    let error = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut input,
    )
    .expect_err("symlinked staged bytes");
    assert_eq!(error.code(), ErrorCode::BlobCorrupt);
    assert_eq!(read_all(&outside), bytes, "nothing outside tmp is touched");
    let target = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    ));
    assert!(!target.exists());
}

#[cfg(unix)]
#[test]
fn a_completed_publication_flushes_the_target_before_retiring_its_journal() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = scratch();
    let bytes = b"already complete";
    let digest = Sha256::from_bytes(bytes);
    let target = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    ));
    fs::create_dir_all(target.parent().expect("blobs")).expect("blobs");
    fs::write(&target, bytes).expect("complete target");
    let mut permissions = fs::metadata(&target).expect("metadata").permissions();
    permissions.set_mode(0o444);
    fs::set_permissions(&target, permissions).expect("read-only target");
    let journal = craft_completed_publication_journal(&scratch, bytes, "blob-flushed.stage");

    let mut input = &bytes[..];
    let result = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut input,
    );

    let mut permissions = fs::metadata(&target).expect("metadata").permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&target, permissions).expect("restore");

    // A verified full-length target is flushed before its journal is retired; a
    // target that cannot be flushed keeps the journal for a later retry.
    let error = result.expect_err("the verified target cannot be flushed");
    assert_eq!(error.code(), ErrorCode::Io);
    assert!(journal.exists());

    let mut input = &bytes[..];
    let blob = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut input,
    )
    .expect("recovered publication");
    assert_eq!(blob.sha256, digest);
    assert!(!journal.exists(), "the recovered journal is retired");
    assert_eq!(read_all(&target), bytes);
}

#[cfg(unix)]
#[test]
fn a_completed_publication_that_cannot_sync_its_directory_keeps_its_journal() {
    let scratch = scratch();
    let bytes = b"directory sync";
    let digest = Sha256::from_bytes(bytes);
    let target = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    ));
    let blobs = target.parent().expect("blobs directory").to_path_buf();
    fs::create_dir_all(&blobs).expect("blobs");
    fs::write(&target, bytes).expect("complete target");
    let journal = craft_completed_publication_journal(&scratch, bytes, "blob-unsynced.stage");
    // The target bytes are readable but its directory entry is not durable; a
    // directory that cannot be opened for syncing is a failure, not a pass.
    set_mode(&blobs, 0o333);

    let mut input = &bytes[..];
    let result = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut input,
    );

    set_mode(&blobs, 0o755);

    let error = result.expect_err("the published entry cannot be synced");
    assert_eq!(error.code(), ErrorCode::Io);
    assert!(
        journal.exists(),
        "the journal is retired only after the sync"
    );
    assert_eq!(read_all(&target), bytes);

    let mut input = &bytes[..];
    let blob = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut input,
    )
    .expect("recovered publication");
    assert_eq!(blob.sha256, digest);
    assert!(!journal.exists(), "the recovered journal is retired");
    assert_eq!(
        read_blob(&scratch.vault, &project(), &blob).expect("read"),
        bytes
    );
}

#[cfg(unix)]
#[test]
fn a_blob_stage_that_cannot_sync_its_directory_is_retried() {
    let scratch = scratch();
    let bytes = staged_bytes(4096, 21);
    let digest = Sha256::from_bytes(&bytes);
    let layout = scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let target = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    ));
    // The staged file is written and synced, but its `tmp` entry is not: the
    // journal may only reference a durable staging name, so the upload fails.
    set_mode(&layout.tmp, 0o333);

    let mut input = &bytes[..];
    let error = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut input,
    )
    .expect_err("the staged entry cannot be published");
    assert_eq!(error.code(), ErrorCode::Io);
    assert!(
        !target.exists(),
        "nothing is published without a durable stage"
    );

    let mut input = &bytes[..];
    let error = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut input,
    )
    .expect_err("the retry is still refused");
    assert_eq!(error.code(), ErrorCode::Io);

    set_mode(&layout.tmp, 0o755);
    let mut input = &bytes[..];
    let blob = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut input,
    )
    .expect("publication after restoring access");
    assert_eq!(blob.sha256, digest);
    assert_eq!(
        read_blob(&scratch.vault, &project(), &blob).expect("read"),
        bytes
    );
}

#[test]
fn a_partial_target_without_a_journal_is_corruption() {
    let scratch = scratch();
    let bytes = staged_bytes(2048, 11);
    let digest = Sha256::from_bytes(&bytes);
    let blobs = scratch
        .root
        .join("projects")
        .join(PROJECT)
        .join(HOST)
        .join("blobs");
    fs::create_dir_all(&blobs).expect("blobs");
    let target = blobs.join(format!("{digest}.txt"));
    fs::write(&target, &bytes[..100]).expect("partial");
    let mut input = &bytes[..];
    let error = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut input,
    )
    .expect_err("partial target");
    assert_eq!(error.code(), ErrorCode::BlobCorrupt);
    assert_eq!(read_all(&target), &bytes[..100]);
}

#[test]
fn read_blob_reports_partial_media_as_retryable_pending_media() {
    let scratch = scratch();
    let bytes = b"verified bytes";
    let blob = put_bytes(&scratch, BlobExt::Txt, bytes);
    let target = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &blob.sha256,
        BlobExt::Txt,
    ));
    // A lock-free reader sees a remote partial hydration exactly like this; it
    // may not claim nonretryable corruption.
    fs::write(&target, b"short").expect("truncate");
    let error = read_blob(&scratch.vault, &project(), &blob).expect_err("length");
    assert_eq!(error.code(), ErrorCode::NotFound);
    assert_eq!(error.detail(), Some("blob_pending_length"));
    assert!(
        error.retryable(),
        "pending media is retryable, never a Seen claim"
    );

    let mut altered = bytes.to_vec();
    altered[0] ^= 0xff;
    fs::write(&target, &altered).expect("altered bytes");
    let error = read_blob(&scratch.vault, &project(), &blob).expect_err("digest");
    assert_eq!(error.code(), ErrorCode::NotFound);
    assert_eq!(error.detail(), Some("blob_pending_digest"));
    assert!(error.retryable());

    // The completed bytes then read normally; nothing was marked corrupt.
    fs::write(&target, bytes).expect("complete bytes");
    assert_eq!(
        read_blob(&scratch.vault, &project(), &blob).expect("read"),
        bytes
    );
}

#[test]
fn read_blob_reports_missing_media_as_retryable_pending_media() {
    let scratch = scratch();
    let digest = Sha256::from_bytes(b"not yet synced");
    let blob = BlobRef {
        host: host(),
        sha256: digest,
        ext: BlobExt::Md,
        bytes: 14,
    };
    let error = read_blob(&scratch.vault, &project(), &blob).expect_err("missing media");
    assert_eq!(error.code(), ErrorCode::NotFound);
    assert_eq!(error.detail(), Some("blob_missing"));
    assert!(error.retryable());
}

#[cfg(unix)]
#[test]
fn a_symlinked_blob_target_is_rejected() {
    use std::os::unix::fs::symlink;

    let scratch = scratch();
    let bytes = b"outside bytes";
    let digest = Sha256::from_bytes(bytes);
    let blobs = scratch
        .root
        .join("projects")
        .join(PROJECT)
        .join(HOST)
        .join("blobs");
    fs::create_dir_all(&blobs).expect("blobs");
    let outside = scratch.data.parent().expect("parent").join("outside.bin");
    fs::write(&outside, bytes).expect("outside");
    let target = blobs.join(format!("{digest}.txt"));
    symlink(&outside, &target).expect("symlink");
    let mut input = &bytes[..];
    let error = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut input,
    )
    .expect_err("symlinked target");
    assert_eq!(error.code(), ErrorCode::Validation);
    let blob = BlobRef {
        host: host(),
        sha256: digest,
        ext: BlobExt::Txt,
        bytes: bytes.len() as u64,
    };
    let error = read_blob(&scratch.vault, &project(), &blob).expect_err("read symlink");
    assert_eq!(error.code(), ErrorCode::BlobCorrupt);
}

#[test]
fn no_local_lock_journal_or_temp_file_is_written_inside_the_vault() {
    let scratch = scratch();
    append(&scratch, operator(), rule_draft("rule"));
    put_bytes(&scratch, BlobExt::Txt, b"blob bytes");
    HostIdentity::load_or_create(&scratch.data, fixed_time(FIXED_TS)).expect("identity");

    let mut files = Vec::new();
    collect_files(&scratch.root, &mut files);
    for file in files {
        let relative = file
            .strip_prefix(&scratch.root)
            .expect("relative")
            .to_string_lossy()
            .replace('\\', "/");
        assert!(
            relative == "vault.json"
                || (relative.starts_with("projects/") && relative.contains('/')),
            "unexpected file inside the Vault: {relative}"
        );
        assert!(!relative.contains("locks"));
        assert!(!relative.contains("pending"));
        assert!(!relative.contains("tmp"));
        assert!(!relative.ends_with(".lock"));
        assert!(!relative.ends_with(".sqlite"));
    }
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("read dir") {
        let entry = entry.expect("entry");
        let file_type = entry.file_type().expect("type");
        if file_type.is_dir() {
            collect_files(&entry.path(), files);
        } else {
            files.push(entry.path());
        }
    }
}

#[cfg(unix)]
#[test]
fn a_symlinked_writer_lock_is_rejected_before_it_creates_vault_state() {
    use std::os::unix::fs::symlink;

    let scratch = scratch();
    scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let lock_path = writer_lock_path(&scratch.local, &scratch.vault, &key(operator()));
    // The alias names an existing, otherwise valid host directory inside the
    // Vault: following it would create a local lock file there.
    let alias = scratch.root.join("projects").join(PROJECT).join("host");
    fs::create_dir_all(&alias).expect("host dir");
    let alias_target = alias.join("local-state.lock");
    symlink(&alias_target, &lock_path).expect("lock alias");

    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("must not follow the lock alias"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("aliased writer lock");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(error.detail(), Some("local_state_symlink"));
    assert!(
        !alias_target.exists(),
        "no local state may be created inside the Vault"
    );
    assert!(!segment_path(&scratch, &operator(), 1).exists());
}

#[cfg(unix)]
#[test]
fn a_symlinked_blob_lock_is_rejected_before_it_creates_vault_state() {
    use std::os::unix::fs::symlink;

    let scratch = scratch();
    let bytes = b"locked bytes";
    let digest = Sha256::from_bytes(bytes);
    let layout = scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    assert!(layout.locks.is_dir());
    let lock_path = blob_lock_path(
        &scratch.local,
        &scratch.vault,
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    );
    // The alias names a file inside the Vault; following it would create a
    // local lock file there.
    let mirror = scratch
        .root
        .join("projects")
        .join(PROJECT)
        .join("alias.lock");
    fs::create_dir_all(mirror.parent().expect("parent")).expect("projects");
    symlink(&mirror, &lock_path).expect("lock alias");

    let mut input = &bytes[..];
    let error = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut input,
    )
    .expect_err("aliased blob lock");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(error.detail(), Some("local_state_symlink"));
    assert!(
        !mirror.exists(),
        "no local state may be created in the Vault"
    );
    let target = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    ));
    assert!(!target.exists());
}

#[cfg(unix)]
#[test]
fn a_symlinked_identity_lock_is_rejected_before_it_creates_state() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temp");
    let data = temp.path().join("data");
    fs::create_dir_all(&data).expect("data");
    let vault = temp.path().join("vault");
    fs::create_dir_all(&vault).expect("vault");
    let alias_target = vault.join("identity.lock");
    symlink(&alias_target, data.join("identity.lock")).expect("lock alias");

    let error = HostIdentity::load_or_create(&data, fixed_time(FIXED_TS))
        .expect_err("aliased identity lock");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(error.detail(), Some("local_state_symlink"));
    assert!(!alias_target.exists());
    assert!(!data.join("identity.json").exists());
}

#[test]
fn lock_contention_times_out_with_a_retryable_busy_error() {
    let scratch = scratch();
    scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let lock_path = writer_lock_path(&scratch.local, &scratch.vault, &key(operator()));
    let _held = acquire_lock(&lock_path).expect("hold writer lock");
    let started = Instant::now();
    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("blocked"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("lock contention");
    assert_eq!(error.code(), ErrorCode::Busy);
    assert!(error.retryable());
    assert_eq!(error.detail(), Some("lock_timeout"));
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(4),
        "the wait is bounded but not immediate: {elapsed:?}"
    );
}

#[test]
fn a_pending_event_is_completed_before_a_different_draft_is_appended() {
    let scratch = scratch();
    let event = Event {
        v: 1,
        id: EventId::parse("00000000000000000000000050").expect("id"),
        seq: 1,
        writer: operator(),
        ts: fixed_time(FIXED_TS),
        project: project(),
        refs: Refs::default(),
        payload: EventPayload::RuleSet(RuleSet {
            rule_id: RuleId::parse("house_rule").expect("rule"),
            text: "Old durable event".to_owned(),
            enabled: true,
        }),
    };
    let old_line = write_pending_state(&scratch, &operator(), &event, 1, None);
    let new_event = append(&scratch, operator(), rule_draft("Different new event"));
    assert_ne!(new_event.event.id, event.id);
    assert_eq!(new_event.event.seq, 2);
    let bytes = read_all(&segment_path(&scratch, &operator(), 1));
    assert_eq!(&bytes[..old_line.len()], &old_line[..]);
    assert_pending_cleared(&scratch, &operator());
    let events = parse_file(&scratch, &operator(), 1);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], event);
    assert_eq!(events[1], new_event.event);
}

#[test]
fn conflict_copy_file_names_are_ignored() {
    let scratch = scratch();
    append(&scratch, operator(), rule_draft("one"));
    let segment = segment_path(&scratch, &operator(), 1);
    let copy = segment.with_file_name("operator 2.jsonl");
    fs::write(&copy, read_all(&segment)).expect("conflict copy");
    let copy_dash = segment.with_file_name("operator-copy.jsonl");
    fs::write(
        &copy_dash,
        b"not an event
",
    )
    .expect("conflict copy");
    let copy_bytes = read_all(&copy);
    let dash_bytes = read_all(&copy_dash);

    let second = append(&scratch, operator(), rule_draft("two"));
    assert_eq!(second.event.seq, 2);
    assert_eq!(read_all(&copy), copy_bytes);
    assert_eq!(read_all(&copy_dash), dash_bytes);
    assert_eq!(parse_file(&scratch, &operator(), 1).len(), 2);
}

#[test]
fn an_abandoned_partial_tail_is_recorded_in_the_local_journal() {
    let scratch = scratch();
    append(&scratch, operator(), rule_draft("complete"));
    let segment = segment_path(&scratch, &operator(), 1);
    let complete_len = read_all(&segment).len() as u64;
    let mut torn = read_all(&segment);
    torn.extend_from_slice(b"{\"v\":1");
    fs::write(&segment, &torn).expect("torn");

    append(&scratch, operator(), rule_draft("next segment"));
    let journal: serde_json::Value =
        serde_json::from_slice(&read_all(&journal_path(&scratch, &operator()))).expect("journal");
    let abandoned = journal["abandoned"].as_array().expect("abandoned list");
    assert_eq!(abandoned.len(), 1);
    assert!(
        abandoned[0]["path"]
            .as_str()
            .expect("path")
            .ends_with("operator.jsonl")
    );
    assert_eq!(abandoned[0]["offset"], complete_len);
}

#[test]
fn a_damaged_oversized_line_still_advances_the_sequence() {
    let scratch = scratch();
    let segment = segment_path(&scratch, &operator(), 1);
    fs::create_dir_all(segment.parent().expect("parent")).expect("dirs");
    let mut damaged = b"{\"seq\":5,".to_vec();
    damaged.resize(70_000, b' ');
    damaged.push(b'\n');
    fs::write(&segment, &damaged).expect("damaged line");

    let result = append(&scratch, operator(), rule_draft("after damage"));
    assert_eq!(result.event.seq, 6);
    let bytes = read_all(&segment);
    let last = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .next_back()
        .expect("last line");
    let relative = writer_relative_path(&project(), &operator(), 1).expect("source");
    let source = SourcePath::from_relative_path(&relative).expect("source");
    assert_eq!(parse_event(last, &source).expect("valid line").seq, 6);
}

#[test]
fn a_local_writer_journal_with_an_unknown_version_is_unsupported() {
    let scratch = scratch();
    let layout = scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let journal = writer_journal_path(&scratch.local, &scratch.vault, &key(operator()));
    fs::write(
        &journal,
        b"{\"v\":2,\"project\":\"example\",\"writer\":\"h-00000000000000000000000001/operator\",\"next_seq\":0}\n",
    )
    .expect("journal");
    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("blocked"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("version");
    assert_eq!(error.code(), ErrorCode::Unsupported);
    assert!(layout.pending.exists());
}

#[test]
fn a_local_writer_journal_for_another_writer_is_rejected() {
    let scratch = scratch();
    scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let journal = writer_journal_path(&scratch.local, &scratch.vault, &key(operator()));
    fs::write(
        &journal,
        b"{\"v\":1,\"project\":\"example\",\"writer\":\"h-00000000000000000000000001/steward\",\"next_seq\":0}\n",
    )
    .expect("journal");
    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("blocked"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("mismatch");
    assert_eq!(error.code(), ErrorCode::Io);
    assert_eq!(error.detail(), Some("writer_journal_mismatch"));
}

#[test]
fn failed_blob_uploads_leave_no_staged_temporary_files() {
    let scratch = scratch();
    let layout = scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let mut empty: &[u8] = b"";
    let _ = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Txt,
        &mut empty,
    );
    let too_large = vec![0_u8; 52_428_801];
    let mut input = &too_large[..];
    let _ = put_blob(
        &scratch.vault,
        &scratch.local,
        &project(),
        &host(),
        BlobExt::Mp4,
        &mut input,
    );
    let remaining = fs::read_dir(&layout.tmp)
        .expect("tmp")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("entries");
    assert!(remaining.is_empty(), "staged temp files are cleaned up");
}

#[test]
fn default_data_root_ends_with_the_app_identifier() {
    if let Ok(path) = LocalPaths::default_data_root() {
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("dev.agendar.app")
        );
    }
}

#[cfg(unix)]
#[test]
fn reader_rejects_a_symlinked_source() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temp");
    let real = temp.path().join("real.jsonl");
    fs::write(&real, b"one\n").expect("real");
    let link = temp.path().join("link.jsonl");
    symlink(&real, &link).expect("symlink");
    let error = read_complete_lines(&link, 0, 1024).expect_err("symlink source");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(error.detail(), Some("vault_symlink"));
}

#[test]
fn a_missing_pending_payload_is_preserved_and_freezes_the_writer() {
    let scratch = scratch();
    scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let journal = writer_journal_path(&scratch.local, &scratch.vault, &key(operator()));
    let relative = writer_relative_path(&project(), &operator(), 1).expect("path");
    let record = serde_json::json!({
        "v": 1,
        "project": project().as_str(),
        "writer": operator().to_string(),
        "next_seq": 0,
        "pending": {
            "path": relative,
            "offset": 0,
            "bytes": 100,
            "digest": Sha256::from_bytes(b"never written").as_str(),
        },
    });
    fs::write(&journal, serde_json::to_vec(&record).expect("journal")).expect("journal");

    // A committed reservation whose bytes are gone is not abandoned intent: it
    // freezes the writer instead of admitting a replacement draft.
    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("recovered without bytes"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("missing committed bytes");
    assert_eq!(error.code(), ErrorCode::WriterFork);
    assert_eq!(error.detail(), Some("pending append bytes are missing"));
    assert_pending_retained(&scratch, &operator());
    assert!(!segment_path(&scratch, &operator(), 1).exists());
    assert_eq!(parse_file_len(&scratch, &operator()), 0);
}

fn parse_file_len(scratch: &Scratch, writer: &WriterId) -> usize {
    let relative = writer_relative_path(&project(), writer, 1).expect("path");
    let path = scratch.root.join(&relative);
    if !path.exists() {
        0
    } else {
        let source = SourcePath::from_relative_path(&relative).expect("source");
        lines_of(&read_all(&path))
            .into_iter()
            .filter(|line| parse_event(line, &source).is_ok())
            .count()
    }
}

#[test]
fn a_pending_payload_with_a_different_digest_freezes_the_writer() {
    let scratch = scratch();
    let event = operator_event("00000000000000000000000050", 1, "Digest mismatch");
    let line = write_pending_state(&scratch, &operator(), &event, 1, Some(0));
    let payload = pending_payload_path(&scratch, &operator());
    let mut altered = line.clone();
    altered[0] = b'[';
    fs::write(&payload, &altered).expect("altered payload");

    let error = recover_writer(&scratch.vault, &scratch.local, &key(operator()))
        .expect_err("digest mismatch");
    assert_eq!(error.code(), ErrorCode::WriterFork);
    assert_eq!(
        error.detail(),
        Some("pending append bytes do not match the journal digest")
    );
    assert_pending_retained(&scratch, &operator());
    assert_eq!(read_all(&payload), altered, "corrupt bytes are preserved");
}

#[cfg(unix)]
#[test]
fn a_temporarily_unreadable_pending_payload_preserves_its_reservation() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = scratch();
    let event = operator_event("00000000000000000000000051", 1, "Unreadable payload");
    let line = write_pending_state(&scratch, &operator(), &event, 1, Some(40));
    let payload = pending_payload_path(&scratch, &operator());
    let mut permissions = fs::metadata(&payload).expect("metadata").permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(&payload, permissions).expect("unreadable payload");

    let result = recover_writer(&scratch.vault, &scratch.local, &key(operator()));

    let mut permissions = fs::metadata(&payload).expect("metadata").permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&payload, permissions).expect("restore");

    let error = result.expect_err("unreadable pending bytes");
    assert_eq!(error.code(), ErrorCode::Io);
    assert_eq!(error.detail(), Some("pending_bytes_unreadable"));
    assert!(error.retryable());
    assert_pending_retained(&scratch, &operator());
    assert_eq!(read_all(&payload), line);
    assert_eq!(
        read_all(&segment_path(&scratch, &operator(), 1)),
        &line[..40],
        "the partial suffix is untouched"
    );

    let recovered = recover_writer(&scratch.vault, &scratch.local, &key(operator()))
        .expect("recovery after restoring access")
        .expect("pending append");
    assert_eq!(recovered.event, event);
    assert_pending_cleared(&scratch, &operator());
    assert!(!payload.exists());
}

#[test]
fn a_pending_record_with_a_partial_suffix_and_no_bytes_freezes_the_writer() {
    let scratch = scratch();
    scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let segment = segment_path(&scratch, &operator(), 1);
    fs::create_dir_all(segment.parent().expect("parent")).expect("dirs");
    fs::write(&segment, b"partial bytes").expect("partial");
    let journal = writer_journal_path(&scratch.local, &scratch.vault, &key(operator()));
    let relative = writer_relative_path(&project(), &operator(), 1).expect("path");
    let record = serde_json::json!({
        "v": 1,
        "project": project().as_str(),
        "writer": operator().to_string(),
        "next_seq": 0,
        "pending": {
            "path": relative,
            "offset": 0,
            "bytes": 100,
            "digest": Sha256::from_bytes(b"never written").as_str(),
        },
    });
    fs::write(&journal, serde_json::to_vec(&record).expect("journal")).expect("journal");
    let error = append_event(
        &scratch.vault,
        &scratch.local,
        &key(operator()),
        rule_draft("blocked"),
        &SystemClock,
        &mut UlidIdSource,
    )
    .expect_err("missing bytes with a suffix");
    assert_eq!(error.code(), ErrorCode::WriterFork);
    assert_eq!(read_all(&segment), b"partial bytes");
}

// ---------------------------------------------------------------------------
// Multi-process cases
// ---------------------------------------------------------------------------

const WORKER_ENV: &str = "AGENDAR_VAULT_WORKER";

#[derive(serde::Serialize, serde::Deserialize)]
struct WorkerSpec {
    mode: String,
    vault: String,
    data: String,
    #[serde(default)]
    file: String,
    #[serde(default)]
    writer: String,
    #[serde(default)]
    count: u64,
    #[serde(default)]
    session: String,
    /// Blob extension for `blob` mode; empty means `txt`.
    #[serde(default)]
    ext: String,
}

#[test]
fn worker_process() {
    let Ok(raw) = std::env::var(WORKER_ENV) else {
        return;
    };
    let spec: WorkerSpec = serde_json::from_str(&raw).expect("worker spec");
    match run_worker(&spec) {
        Ok(output) => {
            println!("WORKER_OK {output}");
        }
        Err(error) => {
            eprintln!("WORKER_ERR {} {}", error.code(), error.message());
            std::process::exit(1);
        }
    }
}

fn run_worker(spec: &WorkerSpec) -> agendar_core::Result<String> {
    let vault = Vault::open(Path::new(&spec.vault))?;
    let local = LocalPaths::new(&spec.data)?;
    let project = ProjectSlug::parse(PROJECT)?;
    match spec.mode.as_str() {
        "append" => {
            let writer = WriterId::parse(&spec.writer)?;
            let key = agendar_core::vault::WriterKey {
                project: project.clone(),
                writer: writer.clone(),
            };
            let mut seqs = Vec::new();
            for index in 0..spec.count {
                let draft = EventDraft {
                    refs: Refs::default(),
                    payload: EventPayload::RuleSet(RuleSet {
                        rule_id: RuleId::parse("shared_rule")?,
                        text: format!("{} event {index}", spec.session),
                        enabled: true,
                    }),
                };
                let result =
                    append_event(&vault, &local, &key, draft, &SystemClock, &mut UlidIdSource)?;
                seqs.push(result.event.seq);
            }
            Ok(serde_json::to_string(&seqs).expect("seqs"))
        }
        "blob" => {
            let bytes = fs::read(&spec.file)?;
            let mut input = &bytes[..];
            let ext = match spec.ext.as_str() {
                "" | "txt" => BlobExt::Txt,
                "md" => BlobExt::Md,
                other => {
                    return Err(agendar_core::error::AgendarError::new(
                        ErrorCode::Validation,
                        format!("unknown worker blob extension {other}"),
                    ));
                }
            };
            let blob = put_blob(&vault, &local, &project, &host(), ext, &mut input)?;
            Ok(serde_json::to_string(&blob).expect("blob"))
        }
        other => Err(agendar_core::error::AgendarError::new(
            ErrorCode::Validation,
            format!("unknown worker mode {other}"),
        )),
    }
}

fn worker_command(spec: &WorkerSpec) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    // a child binary gets a disposable HOME and data home,
    // never the real user configuration, and never an inherited Agendar
    // selection variable.
    let home = Path::new(&spec.data)
        .parent()
        .map(|parent| parent.join("worker-home"))
        .unwrap_or_else(|| PathBuf::from(&spec.data));
    fs::create_dir_all(&home).expect("worker home");
    let inherited: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| name.starts_with("AGENDAR_"))
        .collect();
    for name in inherited {
        command.env_remove(name);
    }
    command
        .arg("--exact")
        .arg("worker_process")
        .arg("--nocapture")
        .env(WORKER_ENV, serde_json::to_string(spec).expect("spec"))
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env("APPDATA", home.join("appdata"))
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env_remove("RUST_TEST_THREADS")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command
}

// The crash worker reads from the caller's file, not the staged file, so the
// publication target never becomes partial during staging. A real partial
// target is produced by interrupting a publication in the parent test, which
// can inspect the durable journal and target between writes.
#[test]
fn two_process_identical_blob_uploads_produce_one_identical_target() {
    let scratch = scratch();
    let bytes = staged_bytes(1_500_000, 21);
    let file = scratch.data.parent().expect("parent").join("blob.bin");
    fs::write(&file, &bytes).expect("blob file");
    let spec = WorkerSpec {
        mode: "blob".to_owned(),
        vault: scratch.root.to_string_lossy().into_owned(),
        data: scratch.data.to_string_lossy().into_owned(),
        file: file.to_string_lossy().into_owned(),
        writer: String::new(),
        count: 0,
        session: String::new(),
        ext: String::new(),
    };
    let mut first = worker_command(&spec);
    let mut second = worker_command(&spec);
    let first_child = first.spawn().expect("first");
    let second_child = second.spawn().expect("second");
    let first_output = first_child.wait_with_output().expect("first output");
    let second_output = second_child.wait_with_output().expect("second output");
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let stdout_of = |output: &std::process::Output| -> BlobRef {
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text
            .lines()
            .find_map(|line| line.strip_prefix("WORKER_OK "))
            .expect("worker output");
        serde_json::from_str(line).expect("blob json")
    };
    let first_blob = stdout_of(&first_output);
    let second_blob = stdout_of(&second_output);
    assert_eq!(first_blob, second_blob);
    assert_eq!(first_blob.sha256, Sha256::from_bytes(&bytes));
    let target = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &first_blob.sha256,
        BlobExt::Txt,
    ));
    assert_eq!(read_all(&target), bytes);
    let blobs = target.parent().expect("blobs");
    assert_eq!(
        fs::read_dir(blobs).expect("read").count(),
        1,
        "one target for identical bytes"
    );
    assert!(
        !blob_journal_path(
            &scratch.local,
            &scratch.vault,
            &project(),
            &host(),
            &first_blob.sha256,
            BlobExt::Txt
        )
        .exists(),
        "the publication journal is cleared"
    );
}

#[test]
fn two_process_same_writer_appends_a_contiguous_sequence() {
    let scratch = scratch();
    let spec = |session: &str| WorkerSpec {
        mode: "append".to_owned(),
        vault: scratch.root.to_string_lossy().into_owned(),
        data: scratch.data.to_string_lossy().into_owned(),
        file: String::new(),
        writer: format!("{HOST}/operator"),
        count: 40,
        session: session.to_owned(),
        ext: String::new(),
    };
    let mut first = worker_command(&spec("A"));
    let mut second = worker_command(&spec("B"));
    let first_child = first.spawn().expect("first");
    let second_child = second.spawn().expect("second");
    let first_output = first_child.wait_with_output().expect("first");
    let second_output = second_child.wait_with_output().expect("second");
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let reported = |output: &std::process::Output| -> Vec<u64> {
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text
            .lines()
            .find_map(|line| line.strip_prefix("WORKER_OK "))
            .expect("worker seqs");
        serde_json::from_str(line).expect("seqs")
    };
    let mut reported_seqs = reported(&first_output);
    reported_seqs.extend(reported(&second_output));
    reported_seqs.sort_unstable();
    assert_eq!(reported_seqs, (1..=80).collect::<Vec<u64>>());

    let events = parse_file(&scratch, &operator(), 1);
    assert_eq!(events.len(), 80);
    let mut seqs: Vec<u64> = events.iter().map(|event| event.seq).collect();
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=80).collect::<Vec<u64>>());
    let mut ids: Vec<&EventId> = events.iter().map(|event| &event.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 80);
}

#[test]
fn a_crashed_publication_at_a_prefix_is_completed_by_a_waiting_contender() {
    let scratch = scratch();
    let bytes = staged_bytes(2 * 1024 * 1024, 33);
    let file = scratch.data.parent().expect("parent").join("crash.bin");
    fs::write(&file, &bytes).expect("blob file");
    let digest = Sha256::from_bytes(&bytes);
    let target = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    ));
    let journal = blob_journal_path(
        &scratch.local,
        &scratch.vault,
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    );
    let lock_path = blob_lock_path(
        &scratch.local,
        &scratch.vault,
        &project(),
        &host(),
        &digest,
        BlobExt::Txt,
    );
    let layout = scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let staged = layout.tmp.join("blob-crashed.stage");
    fs::write(&staged, &bytes).expect("staged bytes");
    fs::create_dir_all(target.parent().expect("blobs")).expect("blobs");
    let prefix = 700_000;
    fs::write(&target, &bytes[..prefix]).expect("partial target");
    let journal_value = serde_json::json!({
        "v": 1,
        "project": project().as_str(),
        "host": HOST,
        "sha256": digest.as_str(),
        "ext": "txt",
        "bytes": bytes.len(),
        "staged_name": "blob-crashed.stage",
        "target_path": blob_relative_path(&project(), &host(), &digest, BlobExt::Txt),
        "published": prefix,
        "complete": false,
    });
    fs::write(
        &journal,
        serde_json::to_vec(&journal_value).expect("journal"),
    )
    .expect("journal");

    // Simulate the crashed owner still holding the publication lock.
    let held = acquire_lock(&lock_path).expect("hold publication lock");
    let spec = WorkerSpec {
        mode: "blob".to_owned(),
        vault: scratch.root.to_string_lossy().into_owned(),
        data: scratch.data.to_string_lossy().into_owned(),
        file: file.to_string_lossy().into_owned(),
        writer: String::new(),
        count: 0,
        session: String::new(),
        ext: String::new(),
    };
    let mut command = worker_command(&spec);
    let mut contender = command.spawn().expect("contender");
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        contender.try_wait().expect("poll").is_none(),
        "the contender must wait for the publication lock"
    );
    drop(held);
    let output = contender.wait_with_output().expect("contender");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Exactly one suffix was appended; the completed target is the full blob.
    assert_eq!(read_all(&target), bytes);
    assert!(!journal.exists());
    assert!(
        !staged.exists(),
        "terminal publication removes its staged bytes"
    );
    let blob = BlobRef {
        host: host(),
        sha256: digest,
        ext: BlobExt::Txt,
        bytes: bytes.len() as u64,
    };
    assert_eq!(
        read_blob(&scratch.vault, &project(), &blob).expect("read"),
        bytes
    );
}

#[test]
fn a_process_killed_mid_copy_is_recovered_by_a_started_contender() {
    for attempt in 0..3_u64 {
        let scratch = scratch();
        let bytes = staged_bytes(16 * 1024 * 1024, 300 + attempt);
        let file = scratch.data.parent().expect("parent").join("kill.bin");
        fs::write(&file, &bytes).expect("blob file");
        let digest = Sha256::from_bytes(&bytes);
        let target = scratch.root.join(blob_relative_path(
            &project(),
            &host(),
            &digest,
            BlobExt::Txt,
        ));
        let journal = blob_journal_path(
            &scratch.local,
            &scratch.vault,
            &project(),
            &host(),
            &digest,
            BlobExt::Txt,
        );
        let spec = WorkerSpec {
            mode: "blob".to_owned(),
            vault: scratch.root.to_string_lossy().into_owned(),
            data: scratch.data.to_string_lossy().into_owned(),
            file: file.to_string_lossy().into_owned(),
            writer: String::new(),
            count: 0,
            session: String::new(),
            ext: String::new(),
        };
        let mut owner_command = worker_command(&spec);
        let mut owner = owner_command.spawn().expect("owner");
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut partial = false;
        while Instant::now() < deadline {
            if journal.exists()
                && let Ok(metadata) = fs::metadata(&target)
                && metadata.len() > 0
                && metadata.len() < bytes.len() as u64
            {
                partial = true;
                break;
            }
            if owner.try_wait().expect("owner").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if partial {
            let mut contender_command = worker_command(&spec);
            let mut contender = contender_command.spawn().expect("contender");
            std::thread::sleep(Duration::from_millis(150));
            let still_partial = fs::metadata(&target)
                .map(|metadata| metadata.len() < bytes.len() as u64)
                .unwrap_or(false);
            if still_partial && owner.try_wait().expect("owner").is_none() {
                owner.kill().expect("kill mid-copy");
                let _ = owner.wait();
                let output = contender.wait_with_output().expect("contender");
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(read_all(&target), bytes);
                assert!(!journal.exists());
                return;
            }
            let _ = contender.kill();
            let _ = contender.wait();
        }
        let _ = owner.kill();
        let _ = owner.wait();
    }
    panic!("could not interrupt the publication mid-copy in three attempts");
}

#[test]
fn distinct_extensions_create_independent_targets_for_the_same_bytes() {
    let scratch = scratch();
    let bytes = staged_bytes(3000, 5);
    let text = put_bytes(&scratch, BlobExt::Txt, &bytes);
    let markdown = put_bytes(&scratch, BlobExt::Md, &bytes);
    assert_eq!(text.sha256, markdown.sha256);
    assert_eq!(text.bytes, markdown.bytes);
    assert_ne!(text.ext, markdown.ext);
    let txt_path = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &text.sha256,
        BlobExt::Txt,
    ));
    let md_path = scratch.root.join(blob_relative_path(
        &project(),
        &host(),
        &markdown.sha256,
        BlobExt::Md,
    ));
    assert_eq!(read_all(&txt_path), bytes);
    assert_eq!(read_all(&md_path), bytes);
    assert_eq!(
        read_blob(&scratch.vault, &project(), &text).expect("txt read"),
        bytes
    );
    assert_eq!(
        read_blob(&scratch.vault, &project(), &markdown).expect("md read"),
        bytes
    );
}

#[test]
fn two_processes_publish_the_same_bytes_with_different_extensions() {
    let scratch = scratch();
    let bytes = staged_bytes(1_200_000, 41);
    let file = scratch.data.parent().expect("parent").join("ext-race.bin");
    fs::write(&file, &bytes).expect("blob file");
    let spec = |ext: &str| WorkerSpec {
        mode: "blob".to_owned(),
        vault: scratch.root.to_string_lossy().into_owned(),
        data: scratch.data.to_string_lossy().into_owned(),
        file: file.to_string_lossy().into_owned(),
        writer: String::new(),
        count: 0,
        session: String::new(),
        ext: ext.to_owned(),
    };
    let mut first = worker_command(&spec("txt"));
    let mut second = worker_command(&spec("md"));
    let first_child = first.spawn().expect("txt child");
    let second_child = second.spawn().expect("md child");
    let first_output = first_child.wait_with_output().expect("txt output");
    let second_output = second_child.wait_with_output().expect("md output");
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let blob_of = |output: &std::process::Output| -> BlobRef {
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text
            .lines()
            .find_map(|line| line.strip_prefix("WORKER_OK "))
            .expect("worker output");
        serde_json::from_str(line).expect("blob json")
    };
    let text_blob = blob_of(&first_output);
    let markdown_blob = blob_of(&second_output);
    assert_eq!(text_blob.sha256, Sha256::from_bytes(&bytes));
    assert_eq!(text_blob.sha256, markdown_blob.sha256);
    assert_eq!(text_blob.bytes, markdown_blob.bytes);
    assert_eq!(text_blob.ext, BlobExt::Txt);
    assert_eq!(markdown_blob.ext, BlobExt::Md);

    for (blob, ext) in [(&text_blob, BlobExt::Txt), (&markdown_blob, BlobExt::Md)] {
        let target = scratch
            .root
            .join(blob_relative_path(&project(), &host(), &blob.sha256, ext));
        assert_eq!(read_all(&target), bytes, "each extension is complete");
        assert!(
            !blob_journal_path(
                &scratch.local,
                &scratch.vault,
                &project(),
                &host(),
                &blob.sha256,
                ext
            )
            .exists(),
            "each independent journal is terminal"
        );
        assert_eq!(
            read_blob(&scratch.vault, &project(), blob).expect("read"),
            bytes
        );
    }
    let blobs = scratch
        .root
        .join("projects")
        .join(PROJECT)
        .join(HOST)
        .join("blobs");
    assert_eq!(
        fs::read_dir(&blobs).expect("read").count(),
        2,
        "independent extensions keep two complete targets"
    );
}

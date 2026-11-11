//! A Vault file a sync app has not downloaded yet (any product) never stalls a
//! refresh or holds the ingest lock: it counts with its stored cursor, keeps
//! its events, shows as waiting in local health, and is read once its single
//! background read finishes. Files whose names are not writer files are named
//! in local health and never ingested.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use agendar_core::index::source_reads::{ReadRequest, SourceRead, SourceReads, read_source};
use agendar_core::model::EventId;
use agendar_core::runtime::LocalPaths;
use agendar_core::service::Service;
use agendar_core::vault::{SystemClock, UlidIdSource, Vault};

const BUDGET: Duration = Duration::from_millis(100);

struct Scratch {
    _temp: tempfile::TempDir,
    vault: Vault,
    local: LocalPaths,
}

fn scratch() -> (Scratch, support::Fixture) {
    let fixture = support::load_fixture("one-question");
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("vault");
    let local = LocalPaths::new(temp.path().join("data")).expect("local paths");
    let vault = Vault::init(&root, &SystemClock, &mut UlidIdSource).expect("vault init");
    for event in &fixture.events {
        support::append_event_to_vault(&vault, event, 1).expect("append event");
    }
    let (service, _) = Service::init(vault.root(), local.data_root()).expect("service");
    service
        .refresh(fixture.as_of, false)
        .expect("first refresh");
    (
        Scratch {
            _temp: temp,
            vault,
            local,
        },
        fixture,
    )
}

impl Scratch {
    fn agent_file(&self) -> PathBuf {
        self.vault.root().join(format!(
            "projects/{}/{}.jsonl",
            support::PROJECT,
            support::AGENT
        ))
    }

    fn lock_path(&self) -> PathBuf {
        self.local
            .ensure_layout(self.vault.root(), self.vault.id())
            .expect("layout")
            .locks
            .join("ingest.lock")
    }

    fn service_with(&self, reads: Arc<SourceReads>) -> Service {
        let mut service =
            Service::open(self.vault.root(), self.local.data_root()).expect("open service");
        service.set_source_reads(reads);
        service
    }

    fn append_pulse(&self, seq: u64) -> EventId {
        let mut pulse = support::valid_event("pulse.session");
        pulse["id"] = serde_json::json!(ulid::Ulid::generate().to_string());
        pulse["seq"] = serde_json::json!(seq);
        let pulse = support::parse_value(&pulse).expect("pulse");
        support::append_event_to_vault(&self.vault, &pulse, 1).expect("append pulse");
        pulse.id
    }

    fn stored_copies(&self, id: &EventId) -> i64 {
        let index = agendar_core::index::open_index(&self.local, &self.vault).expect("index");
        index
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM event_sources WHERE event_id = ?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .expect("count")
    }
}

/// A reader that blocks on one file until released, like the OS does for a
/// file whose content is still downloading. Each call to it records whether
/// the ingest lock was free at that moment.
struct Gate {
    blocked: PathBuf,
    lock: PathBuf,
    released: Mutex<bool>,
    wake: Condvar,
    calls: AtomicUsize,
    lock_free_at_read: Mutex<Vec<bool>>,
}

impl Gate {
    fn new(blocked: PathBuf, lock: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            blocked,
            lock,
            released: Mutex::new(false),
            wake: Condvar::new(),
            calls: AtomicUsize::new(0),
            lock_free_at_read: Mutex::new(Vec::new()),
        })
    }

    fn reads(self: &Arc<Self>) -> Arc<SourceReads> {
        let gate = Arc::clone(self);
        Arc::new(SourceReads::new(
            BUDGET,
            Arc::new(
                move |path: &Path, request: &ReadRequest| -> agendar_core::Result<SourceRead> {
                    let free = agendar_core::runtime::local_state::acquire_lock_until(
                        &gate.lock,
                        Some(Instant::now()),
                    )
                    .is_ok();
                    gate.lock_free_at_read.lock().unwrap().push(free);
                    if path == gate.blocked {
                        gate.calls.fetch_add(1, Ordering::SeqCst);
                        let mut released = gate.released.lock().unwrap();
                        while !*released {
                            released = gate.wake.wait(released).unwrap();
                        }
                    }
                    read_source(path, request)
                },
            ),
        ))
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

fn diagnostics(service: &Service) -> Vec<serde_json::Value> {
    service.status(false).expect("status")["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .clone()
}

fn wait_until(mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < until, "condition not reached in time");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_blocked_source_counts_as_unchanged_and_is_read_once_it_arrives() {
    let (scratch, fixture) = scratch();
    let gate = Gate::new(scratch.agent_file(), scratch.lock_path());
    let reads = gate.reads();
    let service = scratch.service_with(Arc::clone(&reads));
    let known = Service::open(scratch.vault.root(), scratch.local.data_root())
        .expect("open")
        .refresh(fixture.as_of, false)
        .expect("read before the append")
        .events
        .len();

    let pulse = scratch.append_pulse(1000);
    let started = Instant::now();
    let read = service
        .refresh(fixture.as_of, false)
        .expect("a waiting source does not fail the refresh");
    let elapsed = started.elapsed();
    assert!(
        elapsed < BUDGET * 2 + Duration::from_millis(400),
        "the refresh returns near its deadline, not when the file arrives: {elapsed:?}"
    );
    assert_eq!(read.events.len(), known, "earlier events are kept");
    assert!(!read.events.iter().any(|event| event.id == pulse));
    drop(read);

    // While the read is blocked nobody holds the ingest lock.
    let held = agendar_core::runtime::local_state::acquire_lock_until(
        &scratch.lock_path(),
        Some(Instant::now()),
    )
    .expect("the ingest lock is free while the source read is blocked");
    drop(held);

    let notes = diagnostics(&service);
    let relative = format!("projects/{}/{}.jsonl", support::PROJECT, support::AGENT);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0]["code"], "source_waiting");
    assert_eq!(notes[0]["path"], relative.as_str());
    assert_eq!(notes[0]["retryable"], true);

    // Repeated refreshes reuse the one outstanding read and do not wait for it.
    for _ in 0..5 {
        let started = Instant::now();
        let read = service.refresh(fixture.as_of, false).expect("refresh");
        assert!(
            started.elapsed() < BUDGET,
            "a known-slow source is not waited for"
        );
        assert!(!read.events.iter().any(|event| event.id == pulse));
    }
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1, "one read per path");
    assert_eq!(reads.in_flight(), 1);

    gate.release();
    wait_until(|| reads.in_flight() == 0);
    let read = service
        .refresh(fixture.as_of, false)
        .expect("refresh after arrival");
    assert!(
        read.events.iter().any(|event| event.id == pulse),
        "the next refresh picks the finished read up"
    );
    assert_eq!(
        gate.calls.load(Ordering::SeqCst),
        1,
        "it used the finished background read instead of reading again"
    );
    let generation = read.generation;
    drop(read);
    assert_eq!(scratch.stored_copies(&pulse), 1, "ingested exactly once");
    assert!(diagnostics(&service).is_empty(), "the waiting note clears");
    let again = service.refresh(fixture.as_of, false).expect("refresh");
    assert_eq!(again.generation, generation, "nothing left to ingest");
    drop(again);
    assert_eq!(scratch.stored_copies(&pulse), 1);

    assert!(
        gate.lock_free_at_read
            .lock()
            .unwrap()
            .iter()
            .all(|free| *free),
        "no source read ever started under the ingest lock"
    );
}

#[test]
fn concurrent_services_share_one_outstanding_read_per_path() {
    let (scratch, fixture) = scratch();
    let gate = Gate::new(scratch.agent_file(), scratch.lock_path());
    let reads = gate.reads();
    scratch.append_pulse(1000);
    let services: Vec<Arc<Service>> = (0..4)
        .map(|_| Arc::new(scratch.service_with(Arc::clone(&reads))))
        .collect();
    for _ in 0..3 {
        let handles: Vec<_> = services
            .iter()
            .map(|service| {
                let service = Arc::clone(service);
                let now = fixture.as_of;
                std::thread::spawn(move || service.refresh(now, false).expect("refresh").generation)
            })
            .collect();
        for handle in handles {
            handle.join().expect("reader");
        }
    }
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
    assert_eq!(reads.in_flight(), 1);
    gate.release();
    wait_until(|| reads.in_flight() == 0);
}

#[test]
fn stray_file_names_are_named_in_health_and_never_ingested() {
    let (scratch, fixture) = scratch();
    let host =
        scratch
            .vault
            .root()
            .join(format!("projects/{}/{}", support::PROJECT, support::HOST));
    let mut pulse = support::valid_event("pulse.session");
    let stray_id = ulid::Ulid::generate().to_string();
    pulse["id"] = serde_json::json!(stray_id);
    pulse["seq"] = serde_json::json!(2000);
    let line = support::line(&pulse);
    let names = [
        "s-00000000000000000000000101 2.jsonl",
        "operator (conflicted copy 2026-11-09).jsonl",
        "operator (1).jsonl",
        "operator-MACBOOK.jsonl",
        "operator.sync-conflict-20261109-120000-ABCDEFG.jsonl",
        "notes.txt",
    ];
    for name in names {
        std::fs::write(host.join(name), &line).expect("stray file");
    }
    // Hidden bookkeeping files of the OS or a sync app are left alone.
    std::fs::write(host.join(".DS_Store"), b"x").expect("hidden file");

    let service = Service::open(scratch.vault.root(), scratch.local.data_root()).expect("open");
    let read = service.refresh(fixture.as_of, false).expect("refresh");
    assert!(
        !read
            .events
            .iter()
            .any(|event| event.id.as_str() == stray_id)
    );
    drop(read);
    let notes = diagnostics(&service);
    let mut named: Vec<String> = notes
        .iter()
        .map(|note| {
            assert_eq!(note["code"], "source_stray");
            assert_eq!(note["retryable"], false);
            note["path"].as_str().expect("path").to_owned()
        })
        .collect();
    named.sort();
    let mut expected: Vec<String> = names
        .iter()
        .map(|name| format!("projects/{}/{}/{name}", support::PROJECT, support::HOST))
        .collect();
    expected.sort();
    assert_eq!(named, expected);
    let index = agendar_core::index::open_index(&scratch.local, &scratch.vault).expect("index");
    let stored: i64 = index
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM raw_events WHERE id = ?1",
            [stray_id.as_str()],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(stored, 0, "a stray file's events are not ingested");
    drop(index);

    // Removing a stray file clears its note on the next refresh.
    std::fs::remove_file(host.join("notes.txt")).expect("remove");
    service.refresh(fixture.as_of, false).expect("refresh");
    assert_eq!(diagnostics(&service).len(), names.len() - 1);
}

#[test]
fn a_source_ingested_by_another_process_after_the_read_is_skipped() {
    use agendar_core::index::ingest::{ingest_prescanned_until, prescan, stored_sources};
    let (scratch, fixture) = scratch();
    let first = scratch.append_pulse(1000);
    // This process reads the Vault (without the lock) ...
    let mut index = agendar_core::index::open_index(&scratch.local, &scratch.vault).expect("index");
    let stored = stored_sources(&index).expect("stored");
    let scan = prescan(&scratch.vault, &SourceReads::shared(), &stored, &[]).expect("prescan");
    // ... another process appends and ingests before this one takes the lock.
    let second = scratch.append_pulse(1001);
    Service::open(scratch.vault.root(), scratch.local.data_root())
        .expect("open")
        .refresh(fixture.as_of, false)
        .expect("other process");
    let moved = stored_sources(&index).expect("stored");

    ingest_prescanned_until(&scratch.vault, &mut index, &scan, fixture.as_of, None)
        .expect("commit");
    assert_eq!(
        stored_sources(&index).expect("stored"),
        moved,
        "the moved cursor is kept, not replaced by the older read"
    );
    drop(index);
    assert_eq!(scratch.stored_copies(&first), 1);
    assert_eq!(scratch.stored_copies(&second), 1);
    let read = Service::open(scratch.vault.root(), scratch.local.data_root())
        .expect("open")
        .refresh(fixture.as_of, false)
        .expect("refresh");
    assert!(read.events.iter().any(|event| event.id == second));
}

#[test]
fn an_icloud_stub_of_a_writer_file_counts_as_waiting_not_missing() {
    let (scratch, fixture) = scratch();
    let agent = scratch.agent_file();
    let relative = format!("projects/{}/{}.jsonl", support::PROJECT, support::AGENT);
    let name = agent
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    let stub = agent.with_file_name(format!(".{name}.icloud"));
    let service = Service::open(scratch.vault.root(), scratch.local.data_root()).expect("open");
    let before = service
        .refresh(fixture.as_of, false)
        .expect("refresh")
        .events
        .len();

    // iCloud Drive evicted the file: only its hidden stub is left.
    std::fs::rename(&agent, &stub).expect("evict");
    let read = service.refresh(fixture.as_of, false).expect("refresh");
    assert_eq!(read.events.len(), before, "the stored events stand in");
    drop(read);
    let notes = diagnostics(&service);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0]["code"], "source_waiting");
    assert_eq!(notes[0]["path"], relative);
    assert_eq!(notes[0]["retryable"], true);
    let health = service.status(false).expect("status");
    let row = health["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|row| row["path"] == relative)
        .expect("row kept");
    assert_ne!(row["health"], "missing");

    // A stub of a writer file this host never read is waiting too, and a stub
    // of something that is not a writer file is left alone.
    let host = agent.parent().expect("host");
    std::fs::write(
        host.join(".s-00000000000000000000000777.jsonl.icloud"),
        b"x",
    )
    .expect("stub");
    std::fs::write(host.join(".notes.txt.icloud"), b"x").expect("stub");
    service.refresh(fixture.as_of, false).expect("refresh");
    let mut waiting: Vec<String> = diagnostics(&service)
        .iter()
        .map(|note| note["path"].as_str().expect("path").to_owned())
        .collect();
    waiting.sort();
    assert_eq!(waiting.len(), 2, "{waiting:?}");
    assert!(waiting.contains(&relative));

    // The download finishes: the file is read again and the notes go.
    std::fs::rename(&stub, &agent).expect("download");
    std::fs::remove_file(host.join(".s-00000000000000000000000777.jsonl.icloud")).expect("rm");
    let added = scratch.append_pulse(3000);
    let read = service.refresh(fixture.as_of, false).expect("refresh");
    assert!(read.events.iter().any(|event| event.id == added));
    drop(read);
    assert!(diagnostics(&service).is_empty());
}

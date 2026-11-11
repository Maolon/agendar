//! The index's stored source cursor is a cache shared by every process: a new
//! Service reads a current index without the ingest lock, and after one append
//! only one process ingests.

mod support;

use std::sync::Arc;
use std::time::Instant;

use agendar_core::index::ingest::{current_source_digest, ingest_unless_current_until};
use agendar_core::index::{meta, open_index};
use agendar_core::model::{EventId, ProjectSlug, UtcTime};
use agendar_core::runtime::LocalPaths;
use agendar_core::service::Service;
use agendar_core::vault::{SystemClock, UlidIdSource, Vault};

struct Scratch {
    _temp: tempfile::TempDir,
    vault: Vault,
    local: LocalPaths,
}

fn scratch_with(fixture: &support::Fixture) -> Scratch {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("vault");
    let data = temp.path().join("data");
    let local = LocalPaths::new(&data).expect("local paths");
    let mut ids = UlidIdSource;
    let vault = Vault::init(&root, &SystemClock, &mut ids).expect("vault init");
    for event in &fixture.events {
        support::append_event_to_vault(&vault, event, 1).expect("append event");
    }
    Scratch {
        _temp: temp,
        vault,
        local,
    }
}

impl Scratch {
    /// A new process: a Service with an empty in-memory cache.
    fn new_service(&self) -> Service {
        Service::open(self.vault.root(), self.local.data_root()).expect("open service")
    }

    fn hold_ingest_lock(&self) -> std::fs::File {
        let lock = self
            .local
            .ensure_layout(self.vault.root(), self.vault.id())
            .expect("layout")
            .locks
            .join("ingest.lock");
        agendar_core::runtime::acquire_lock(&lock).expect("hold ingest lock")
    }

    fn generation(&self) -> u64 {
        let index = open_index(&self.local, &self.vault).expect("index");
        meta(index.connection(), "generation")
            .expect("meta")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    fn append_pulse(&self, seq: u64) -> EventId {
        let mut pulse = support::valid_event("pulse.session");
        pulse["id"] = serde_json::json!(ulid::Ulid::generate().to_string());
        pulse["seq"] = serde_json::json!(seq);
        let pulse = support::parse_value(&pulse).expect("pulse");
        support::append_event_to_vault(&self.vault, &pulse, 1).expect("append pulse");
        pulse.id
    }
}

fn first_read(scratch: &Scratch, now: UtcTime) -> u64 {
    let (service, _) =
        Service::init(scratch.vault.root(), scratch.local.data_root()).expect("service");
    service
        .refresh(now, false)
        .expect("first refresh")
        .generation
}

#[test]
fn a_new_service_reads_an_unchanged_vault_without_the_ingest_lock() {
    let fixture = support::load_fixture("attempts");
    let scratch = scratch_with(&fixture);
    let project = ProjectSlug::parse("example").expect("project");
    let (service, _) =
        Service::init(scratch.vault.root(), scratch.local.data_root()).expect("service");
    let first = service
        .refresh(fixture.as_of, false)
        .expect("first refresh");
    let before = first.state.projects[&project].needs_you[0].waiting_minutes;
    drop(first);

    let held = scratch.hold_ingest_lock();
    let later = fixture.as_of.add_seconds(3600);
    let fresh = scratch.new_service();
    let read = fresh
        .refresh_until(later, false, Some(Instant::now()))
        .expect("a current index is read without the ingest lock");
    assert_eq!(
        read.generation, 1,
        "an unchanged read never bumps the generation"
    );
    assert_eq!(read.state.evaluated_at, later);
    assert_eq!(
        read.state.projects[&project].needs_you[0].waiting_minutes,
        before + 60,
        "time-dependent state is evaluated at the new clock"
    );
    assert!(
        !agendar_core::index::query::recent_project_events(&read.index, &project, 20)
            .expect("the read-only index supports extras")
            .is_empty()
    );
    drop(read);
    assert!(
        fresh.has_committed_read(),
        "the lock-free read fills the process cache"
    );

    // A cached read (freshness, status) opens a current index without the lock too.
    let cached = scratch
        .new_service()
        .refresh_until(later, true, Some(Instant::now()))
        .expect("a cached read opens a current index without the lock");
    assert_eq!(cached.generation, 1);
    Service::freshness(&cached, true).expect("freshness");
    drop(cached);
    drop(held);
    assert_eq!(scratch.generation(), 1);
}

#[test]
fn eight_new_services_after_one_append_ingest_once() {
    let fixture = support::load_fixture("one-question");
    let scratch = Arc::new(scratch_with(&fixture));
    let generation = first_read(&scratch, fixture.as_of);
    assert_eq!(generation, 1);
    let services: Vec<Arc<Service>> = (0..8)
        .map(|_| {
            let service = Arc::new(scratch.new_service());
            let read = service.refresh(fixture.as_of, false).expect("warm read");
            assert_eq!(
                read.generation, 1,
                "warming never ingests an unchanged Vault"
            );
            service
        })
        .collect();

    let pulse = scratch.append_pulse(1000);
    let barrier = Arc::new(std::sync::Barrier::new(services.len()));
    let handles: Vec<_> = services
        .iter()
        .map(|service| {
            let service = Arc::clone(service);
            let barrier = Arc::clone(&barrier);
            let pulse = pulse.clone();
            let now = fixture.as_of;
            std::thread::spawn(move || {
                barrier.wait();
                let read = service.refresh(now, false).expect("herd read");
                assert!(
                    read.events.iter().any(|event| event.id == pulse),
                    "every reader sees the appended event"
                );
                read.generation
            })
        })
        .collect();
    for handle in handles {
        assert_eq!(handle.join().expect("reader"), 2);
    }
    assert_eq!(scratch.generation(), 2, "exactly one ingest for one append");
}

#[test]
fn a_second_ingester_after_the_same_append_writes_nothing() {
    let fixture = support::load_fixture("one-question");
    let scratch = scratch_with(&fixture);
    first_read(&scratch, fixture.as_of);
    scratch.append_pulse(1000);

    // Both ingesters observed the new Vault cursor before taking the lock.
    let observed = current_source_digest(&scratch.vault).expect("cursor");
    let mut first = open_index(&scratch.local, &scratch.vault).expect("first index");
    let mut second = open_index(&scratch.local, &scratch.vault).expect("second index");
    let report =
        ingest_unless_current_until(&scratch.vault, &mut first, &observed, fixture.as_of, None)
            .expect("first ingest")
            .expect("the first ingester writes");
    assert_eq!(report.generation, 2);
    assert_eq!(report.added, 1);

    let data_version = |index: &agendar_core::index::Index| -> i64 {
        index
            .connection()
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .expect("data version")
    };
    let before = data_version(&first);
    let skipped = ingest_unless_current_until(
        &scratch.vault,
        &mut second,
        &observed,
        fixture.as_of.add_seconds(60),
        None,
    )
    .expect("second ingest");
    assert!(skipped.is_none(), "the index already holds this cursor");
    assert_eq!(
        data_version(&first),
        before,
        "the second ingester committed nothing"
    );
    assert_eq!(scratch.generation(), 2);
}

#[test]
fn an_index_without_a_stored_cursor_uses_the_locked_path() {
    let fixture = support::load_fixture("one-question");
    let scratch = scratch_with(&fixture);
    first_read(&scratch, fixture.as_of);
    let index = open_index(&scratch.local, &scratch.vault).expect("index");
    index
        .connection()
        .execute("DELETE FROM meta WHERE key = 'source_digest'", [])
        .expect("forget the cursor");
    drop(index);

    let held = scratch.hold_ingest_lock();
    let busy = scratch
        .new_service()
        .refresh_until(fixture.as_of, false, Some(Instant::now()));
    match busy {
        Ok(_) => panic!("an index without a cursor must not be served lock-free"),
        Err(error) => assert!(error.retryable(), "busy, not a failure: {error:?}"),
    }
    drop(held);

    let read = scratch
        .new_service()
        .refresh(fixture.as_of, false)
        .expect("locked refresh");
    assert_eq!(read.generation, 2, "the locked path ingests once");
    assert_eq!(
        read.source_digest,
        Some(current_source_digest(&scratch.vault).expect("cursor")),
        "the ingest stores the cursor again"
    );
    drop(read);
    let again = scratch
        .new_service()
        .refresh(fixture.as_of, false)
        .expect("lock-free again");
    assert_eq!(again.generation, 2);
}

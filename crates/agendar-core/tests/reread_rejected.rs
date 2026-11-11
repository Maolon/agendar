//! A line an older version rejects because it names something it does not
//! know is kept in the index; after the upgrade the first refresh reads it
//! again, without `index rebuild`, and the index equals a rebuild by the new
//! version. The older reader is simulated on the test thread
//! (`simulate_older_reader`): it reports another decoder version and rejects
//! one kind and one body field the way the real decoder rejects unknown ones.
//! It also rejects one kind (`rule.set`) by a rule, which is not a curable
//! rejection: that line is not kept, and the upgrade still reads it, because
//! a version change also rereads the files with rejected lines.

mod support;

use std::collections::BTreeMap;
use std::path::Path;

use agendar_core::index::decode::{
    DECODER_VERSION, OlderReader, OlderReaderGuard, simulate_older_reader,
};
use agendar_core::index::{meta, open_index, rebuild};
use agendar_core::model::{Event, ProjectSlug, UtcTime, WriterId};
use agendar_core::runtime::HostIdentity;
use agendar_core::service::{Fresh, Service};
use agendar_core::vault::{SystemClock, UlidIdSource, Vault};

const SEED: &str = "delivery-seen-review";
const CORRUPT: &str = "{\"v\":1,\n";

fn older() -> OlderReaderGuard {
    simulate_older_reader(OlderReader {
        version: "0.0.0-older".to_owned(),
        unknown_kinds: vec!["review.result".to_owned()],
        unknown_body_fields: vec![("plan.feature".to_owned(), "slice".to_owned())],
        rejected_kinds: vec!["rule.set".to_owned()],
    })
}

/// Lines the older reader keeps for a newer version in the seed: 3 reviews,
/// 2 features.
const KEPT: i64 = 5;
/// Lines it rejects without keeping them: the seed's one `rule.set`.
const DROPPED: i64 = 1;

fn open_service(vault: &Path, data: &Path, now: UtcTime) -> Service {
    HostIdentity::load_or_create(data, now).expect("host identity");
    Service::open(vault, data).expect("open service")
}

/// The seed's events plus one corrupt line, which no version reads.
fn seed_vault(root: &Path) -> (Vault, Vec<Event>, UtcTime) {
    let seed = support::load_seed(SEED);
    let vault = Vault::init(root, &SystemClock, &mut UlidIdSource).expect("vault init");
    for event in &seed.events {
        support::append_event_to_vault(&vault, event, 1).expect("append event");
    }
    support::append_raw_line(
        &vault,
        &ProjectSlug::parse(support::PROJECT).expect("project"),
        &WriterId::parse(support::OPERATOR).expect("writer"),
        1,
        CORRUPT,
    )
    .expect("append corrupt line");
    (vault, seed.events, seed.as_of)
}

fn count(service: &Service, sql: &str) -> i64 {
    let index = open_index(service.local(), service.vault()).expect("index");
    index
        .connection()
        .query_row(sql, [], |row| row.get(0))
        .expect("count")
}

fn undecodable(service: &Service) -> i64 {
    count(
        service,
        "SELECT COUNT(*) FROM raw_events WHERE reason = 'undecodable'",
    )
}

fn invalid_lines(service: &Service) -> i64 {
    count(
        service,
        "SELECT COUNT(*) FROM diagnostics WHERE code = 'invalid_line'",
    )
}

fn notes(service: &Service) -> Vec<serde_json::Value> {
    service.status(true).expect("status")["diagnostics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Every Vault-derived table and the decoder stamp, rows sorted. Local
/// timestamps (`source_files.last_seen_at`) and per-rebuild values (epoch,
/// generation) are left out.
fn dump(service: &Service) -> BTreeMap<String, Vec<String>> {
    let index = open_index(service.local(), service.vault()).expect("index");
    let connection = index.connection();
    let mut tables = BTreeMap::new();
    for (table, sql) in [
        (
            "source_files",
            "SELECT path, offset, prefix_sha256, observed_size, health FROM source_files",
        ),
        ("raw_events", "SELECT * FROM raw_events"),
        ("event_sources", "SELECT * FROM event_sources"),
        ("diagnostics", "SELECT * FROM diagnostics"),
        ("projects", "SELECT * FROM projects"),
        ("features", "SELECT * FROM features"),
        ("edges", "SELECT * FROM edges"),
        ("decisions", "SELECT * FROM decisions"),
        ("receipts", "SELECT * FROM receipts"),
        ("deliveries", "SELECT * FROM deliveries"),
        ("needs_you", "SELECT * FROM needs_you"),
        ("rules", "SELECT * FROM rules"),
        ("sessions", "SELECT * FROM sessions"),
        (
            "meta",
            "SELECT key, value FROM meta WHERE key IN
             ('event_set_digest', 'decoder_version', 'source_digest', 'evaluated_at')
             OR key LIKE 'policy:%'",
        ),
    ] {
        let mut statement = connection.prepare(sql).expect("prepare");
        let columns = statement.column_count();
        let mut rows: Vec<String> = statement
            .query_map([], |row| {
                let mut cells = Vec::with_capacity(columns);
                for column in 0..columns {
                    cells.push(format!("{:?}", row.get_ref(column)?));
                }
                Ok(cells.join(" | "))
            })
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        rows.sort();
        tables.insert(table.to_owned(), rows);
    }
    tables
}

/// A rebuild by the running (new) version, on its own data dir.
fn rebuilt(vault: &Vault, data: &Path, now: UtcTime) -> Service {
    let service = open_service(vault.root(), data, now);
    rebuild(vault, service.local(), now).expect("rebuild");
    service
}

fn assert_same_read(left: &Fresh, right: &Fresh, label: &str) {
    assert_eq!(left.events, right.events, "{label}: events");
    assert_eq!(left.resolution, right.resolution, "{label}: resolution");
    assert_eq!(left.state, right.state, "{label}: state");
    assert_eq!(
        Service::freshness(left, false)
            .expect("freshness")
            .event_set_digest,
        Service::freshness(right, false)
            .expect("freshness")
            .event_set_digest,
        "{label}: event set digest"
    );
}

#[test]
fn an_older_reader_keeps_the_lines_and_the_new_one_reads_them_without_a_rebuild() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (vault, events, now) = seed_vault(&temp.path().join("vault"));
    let service = open_service(vault.root(), &temp.path().join("data"), now);

    let old = {
        let _older = older();
        let old = service.refresh(now, false).expect("older refresh");
        assert_eq!(undecodable(&service), KEPT, "kept for a newer version");
        assert_eq!(
            invalid_lines(&service),
            1 + DROPPED,
            "the corrupt line and the line that fails the older rule"
        );
        // A second older refresh does no work: nothing was committed.
        let again = service.refresh(now, false).expect("older refresh again");
        assert_eq!(again.generation, old.generation);
        old
    };
    assert_eq!(old.events.len(), events.len() - (KEPT + DROPPED) as usize);
    let waiting: Vec<_> = notes(&service)
        .into_iter()
        .filter(|note| note["code"] == "awaiting_newer_version")
        .collect();
    assert_eq!(waiting.len(), 1, "one count");
    assert_eq!(waiting[0]["path"], "projects");
    assert!(
        waiting[0]["message"]
            .as_str()
            .expect("message")
            .starts_with("5 event lines"),
        "{waiting:?}"
    );

    // The upgrade: the Vault did not change, yet the first refresh decodes
    // the kept lines again and rereads the file with rejected lines, which
    // brings back the line the older rule rejected. One call, two commits.
    let new = service.refresh(now, false).expect("new refresh");
    assert_eq!(
        new.generation,
        old.generation + 2,
        "the pass, then the reread"
    );
    assert_eq!(new.events.len(), events.len());
    assert_eq!(undecodable(&service), 0);
    assert_eq!(invalid_lines(&service), 1, "corrupt JSON stays rejected");
    assert!(
        notes(&service)
            .iter()
            .all(|note| note["code"] != "awaiting_newer_version")
    );
    // Once per version change: the next refresh takes the unchanged path.
    let again = service.refresh(now, false).expect("second new refresh");
    assert_eq!(again.generation, new.generation, "no second pass");
    let index = open_index(service.local(), service.vault()).expect("index");
    assert_eq!(
        meta(index.connection(), "decoder_version")
            .expect("meta")
            .as_deref(),
        Some(DECODER_VERSION)
    );
    drop(index);

    // Identical to a rebuild and a cold ingest by the new version.
    let rebuilt = rebuilt(&vault, &temp.path().join("rebuilt"), now);
    assert_eq!(dump(&service), dump(&rebuilt), "index equals a rebuild");
    let reloaded = service.refresh(now, true).expect("reload");
    let rebuilt_read = rebuilt.refresh(now, true).expect("rebuilt read");
    assert_same_read(&new, &rebuilt_read, "reused vs rebuild");
    assert_same_read(&reloaded, &rebuilt_read, "reload vs rebuild");
    let cold = open_service(vault.root(), &temp.path().join("cold"), now)
        .refresh(now, false)
        .expect("cold");
    assert_same_read(&new, &cold, "reused vs cold");
}

#[test]
fn two_hosts_on_different_versions_converge_once_both_are_new() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (vault, _, now) = seed_vault(&temp.path().join("vault"));
    let old_host = open_service(vault.root(), &temp.path().join("old-host"), now);
    let new_host = open_service(vault.root(), &temp.path().join("new-host"), now);
    let behind = {
        let _older = older();
        old_host.refresh(now, false).expect("older host")
    };
    let ahead = new_host.refresh(now, false).expect("newer host");
    assert_ne!(behind.state, ahead.state, "the older host lags");
    let caught_up = old_host.refresh(now, false).expect("upgraded host");
    assert_same_read(&caught_up, &ahead, "converged");
    assert_eq!(dump(&old_host), dump(&new_host), "same index");
}

#[test]
fn a_downgrade_keeps_the_newer_rows_and_does_not_loop() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (vault, events, now) = seed_vault(&temp.path().join("vault"));
    let service = open_service(vault.root(), &temp.path().join("data"), now);
    let new = service.refresh(now, false).expect("new");
    assert_eq!(new.events.len(), events.len());
    {
        let _older = older();
        let old = service.refresh(now, false).expect("older on a newer index");
        assert_eq!(old.generation, new.generation + 2, "one pass, one reread");
        assert_eq!(old.events.len(), events.len() - (KEPT + DROPPED) as usize);
        assert_eq!(
            undecodable(&service),
            KEPT + DROPPED,
            "rows kept, not deleted"
        );
        let again = service.refresh(now, false).expect("older again");
        assert_eq!(again.generation, old.generation, "no loop");
        // An explicit rebuild by the older version is not refused, although
        // it rejects the `rule.set` line for good: the line is still in the
        // Vault, so its cached row is not a missing event.
        rebuild(&vault, service.local(), now).expect("older rebuild");
        assert_eq!(undecodable(&service), KEPT);
        assert_eq!(invalid_lines(&service), 1 + DROPPED);
    }
    let back = service.refresh(now, false).expect("upgraded again");
    let rebuilt = rebuilt(&vault, &temp.path().join("rebuilt"), now);
    assert_eq!(dump(&service), dump(&rebuilt));
    assert_same_read(
        &back,
        &rebuilt.refresh(now, true).expect("rebuilt read"),
        "after the round trip",
    );
}

/// An index last written by a binary that predates the decoder stamp kept no
/// raw bytes for the lines it rejected: the files with rejected lines are
/// read again from the start.
#[test]
fn an_index_from_before_the_stamp_rereads_files_with_rejected_lines() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (vault, events, now) = seed_vault(&temp.path().join("vault"));
    let service = open_service(vault.root(), &temp.path().join("data"), now);
    {
        let _older = older();
        service.refresh(now, false).expect("older refresh");
    }
    // Turn the kept lines into what a pre-stamp binary stores: line
    // diagnostics without raw bytes, and no decoder stamp.
    {
        let index = open_index(service.local(), service.vault()).expect("index");
        index
            .connection()
            .execute_batch(
                "INSERT INTO diagnostics(id, path, byte_start, project, event_id, code, message, resolved)
                   SELECT 'invalid_line:' || s.path || ':' || s.byte_start, s.path, s.byte_start,
                          NULL, NULL, 'invalid_line', 'rejected', 0
                   FROM event_sources s JOIN raw_events r ON r.id = s.event_id AND r.digest = s.digest
                   WHERE r.reason = 'undecodable';
                 DELETE FROM event_sources WHERE (event_id, digest) IN
                   (SELECT id, digest FROM raw_events WHERE reason = 'undecodable');
                 DELETE FROM raw_events WHERE reason = 'undecodable';
                 DELETE FROM meta WHERE key IN ('decoder_version', 'decoder_generation');",
            )
            .expect("simulate a pre-stamp index");
    }
    assert_eq!(invalid_lines(&service), 1 + DROPPED + KEPT);
    // One refresh suffices: it rereads those files before it returns.
    let read = service.refresh(now, false).expect("first new refresh");
    assert_eq!(read.events.len(), events.len());
    assert_eq!(invalid_lines(&service), 1);
    let rebuilt = rebuilt(&vault, &temp.path().join("rebuilt"), now);
    assert_eq!(dump(&service), dump(&rebuilt));
    assert_same_read(
        &read,
        &rebuilt.refresh(now, true).expect("rebuilt read"),
        "first refresh vs rebuild",
    );
    let second = service.refresh(now, false).expect("second");
    assert_eq!(second.generation, read.generation, "settled");
}

/// Without any rejected line there is nothing to reread: the version change
/// costs one commit over the stored rows.
#[test]
fn a_version_change_without_rejected_lines_is_one_commit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let seed = support::load_seed(SEED);
    let vault =
        Vault::init(&temp.path().join("vault"), &SystemClock, &mut UlidIdSource).expect("vault");
    for event in &seed.events {
        support::append_event_to_vault(&vault, event, 1).expect("append event");
    }
    let now = seed.as_of;
    let service = open_service(vault.root(), &temp.path().join("data"), now);
    let old = {
        let _older = simulate_older_reader(OlderReader {
            version: "0.0.0-older".to_owned(),
            unknown_kinds: vec!["review.result".to_owned()],
            ..OlderReader::default()
        });
        service.refresh(now, false).expect("older")
    };
    assert_eq!(invalid_lines(&service), 0);
    let new = service.refresh(now, false).expect("new");
    assert_eq!(new.generation, old.generation + 1, "one commit");
    assert_eq!(new.events.len(), seed.events.len());
    let rebuilt = rebuilt(&vault, &temp.path().join("rebuilt"), now);
    assert_eq!(dump(&service), dump(&rebuilt));
}

/// A rebuild still refuses to drop a cached row whose line is gone from the
/// Vault, also when this binary cannot read the row.
#[test]
fn a_rebuild_still_aborts_when_a_kept_line_is_gone_from_the_vault() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (vault, _, now) = seed_vault(&temp.path().join("vault"));
    let project = ProjectSlug::parse(support::PROJECT).expect("project");
    let writer = WriterId::parse(support::OPERATOR).expect("writer");
    // One kept line in a file of its own (the writer's second segment).
    support::append_raw_line(
        &vault,
        &project,
        &writer,
        2,
        "{\"body\":{\"demo_kind\":\"cli_transcript\",\"depends_on\":[],\"slice\":\"example\",\"title\":\"z\"},\"id\":\"0000000000000000000000ZZ02\",\"kind\":\"plan.feature\",\"project\":\"example\",\"refs\":{\"node\":\"z\"},\"seq\":901,\"ts\":\"2026-01-01T00:00:00.000Z\",\"v\":1,\"writer\":\"h-00000000000000000000000001/operator\"}\n",
    )
    .expect("append line");
    let service = open_service(vault.root(), &temp.path().join("data"), now);
    let _older = older();
    service.refresh(now, false).expect("older");
    assert_eq!(undecodable(&service), KEPT + 1);
    rebuild(&vault, service.local(), now).expect("the line is still there");
    let segment = agendar_core::vault::ensure_host_directory(vault.root(), &project, writer.host())
        .expect("host directory")
        .join(agendar_core::vault::writer_file_name(&writer, 2).expect("file name"));
    std::fs::remove_file(segment).expect("remove the segment");
    let error = match rebuild(&vault, service.local(), now) {
        Ok(_) => panic!("a rebuild without the kept line must abort"),
        Err(error) => error,
    };
    assert_eq!(error.detail(), Some("cached_event_missing"));
    assert_eq!(undecodable(&service), KEPT + 1, "the cache is kept");
}

/// A kept line whose own JSON is not the canonical form of the event the new
/// version decodes (an empty `refs.causes` is dropped on serialization) changes its
/// stored digest when it is read again; the index still equals a rebuild,
/// and an older version can still rebuild over the newer rows.
#[test]
fn a_line_kept_under_one_digest_is_stored_under_the_new_ones_digest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (vault, events, now) = seed_vault(&temp.path().join("vault"));
    support::append_raw_line(
        &vault,
        &ProjectSlug::parse(support::PROJECT).expect("project"),
        &WriterId::parse(support::OPERATOR).expect("writer"),
        1,
        "{\"body\":{\"demo_kind\":\"cli_transcript\",\"depends_on\":[],\"slice\":\"example\",\"title\":\"y\"},\"id\":\"0000000000000000000000ZZ01\",\"kind\":\"plan.feature\",\"project\":\"example\",\"refs\":{\"causes\":[],\"node\":\"y\"},\"seq\":900,\"ts\":\"2026-01-01T00:00:00.000Z\",\"v\":1,\"writer\":\"h-00000000000000000000000001/operator\"}\n",
    )
    .expect("append line");
    let service = open_service(vault.root(), &temp.path().join("data"), now);
    let digests = |service: &Service| {
        let index = open_index(service.local(), service.vault()).expect("index");
        index
            .connection()
            .query_row(
                "SELECT digest FROM raw_events WHERE id = '0000000000000000000000ZZ01'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("row")
    };
    let kept = {
        let _older = older();
        service.refresh(now, false).expect("older");
        assert_eq!(undecodable(&service), KEPT + 1);
        digests(&service)
    };
    let new = service.refresh(now, false).expect("new");
    assert_eq!(new.events.len(), events.len() + 1);
    assert_ne!(digests(&service), kept, "stored under the decoded digest");
    let rebuilt = rebuilt(&vault, &temp.path().join("rebuilt"), now);
    assert_eq!(dump(&service), dump(&rebuilt));
    {
        let _older = older();
        rebuild(&vault, service.local(), now).expect("older rebuild over newer rows");
        assert_eq!(digests(&service), kept);
    }
    service.refresh(now, false).expect("new again");
    assert_eq!(dump(&service), dump(&rebuilt));
}

/// Prints the demo transcript:
/// `cargo test -p agendar-core --test reread_rejected demo_transcript -- --ignored --nocapture`
#[test]
#[ignore = "demo transcript"]
fn demo_transcript() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (vault, events, now) = seed_vault(&temp.path().join("vault"));
    println!(
        "temp Vault: seed {SEED}, {} events + 1 corrupt line; this binary's decoder {DECODER_VERSION}",
        events.len()
    );
    let service = open_service(vault.root(), &temp.path().join("data"), now);
    let show = |label: &str, fresh: &Fresh| {
        println!(
            "{label}: generation {}, events folded {}, rows kept for a newer version {}, invalid_line diagnostics {}",
            fresh.generation,
            fresh.events.len(),
            undecodable(&service),
            invalid_lines(&service)
        );
    };
    {
        let _older = older();
        println!(
            "\n$ refresh as the older reader (decoder 0.0.0-older: review.result and plan.feature.slice unknown; rule.set fails an older rule)"
        );
        let fresh = service.refresh(now, false).expect("older");
        show("older refresh", &fresh);
        let again = service.refresh(now, false).expect("older again");
        show("older refresh again", &again);
        println!("status diagnostics:");
        for note in notes(&service) {
            println!("  {note}");
        }
    }
    println!("\n$ refresh after the upgrade (no Vault change, no rebuild)");
    let fresh = service.refresh(now, false).expect("new");
    show("first refresh", &fresh);
    let again = service.refresh(now, false).expect("new again");
    show("second refresh", &again);
    println!("status diagnostics: {:?}", notes(&service));
    let rebuilt = rebuilt(&vault, &temp.path().join("rebuilt"), now);
    println!(
        "\nindex equals a rebuild by this version (all Vault tables, meta digests): {}",
        dump(&service) == dump(&rebuilt)
    );
    let rebuilt_read = rebuilt.refresh(now, true).expect("rebuilt read");
    println!(
        "state equal: {}, resolution equal: {}, event_set_digest {}",
        fresh.state == rebuilt_read.state,
        fresh.resolution == rebuilt_read.resolution,
        Service::freshness(&fresh, false)
            .expect("freshness")
            .event_set_digest
    );
}

// ---------------------------------------------------------------------
// The owner's drop (`plan.dropped`) across versions
// ---------------------------------------------------------------------

/// The owner at another computer drops the seed's planned step `y`.
const OWNER_DROP: &str = "{\"body\":{\"note\":\"Not needed any more.\"},\"id\":\"0000000000000000000000ZZ09\",\"kind\":\"plan.dropped\",\"project\":\"example\",\"refs\":{\"node\":\"y\"},\"seq\":1,\"ts\":\"2026-01-01T00:20:00.000Z\",\"v\":1,\"writer\":\"h-00000000000000000000000009/operator\"}\n";
const OWNER_AT_9: &str = "h-00000000000000000000000009/operator";

/// A reader from before `plan.dropped` existed.
fn before_owner_drop() -> OlderReaderGuard {
    simulate_older_reader(OlderReader {
        version: "0.0.0-older".to_owned(),
        unknown_kinds: vec!["plan.dropped".to_owned()],
        ..OlderReader::default()
    })
}

fn vault_with_owner_drop(root: &Path, line: &str) -> (Vault, Vec<Event>, UtcTime) {
    let (vault, events, now) = seed_vault(root);
    support::append_raw_line(
        &vault,
        &ProjectSlug::parse(support::PROJECT).expect("project"),
        &WriterId::parse(OWNER_AT_9).expect("writer"),
        1,
        line,
    )
    .expect("append the owner's drop");
    (vault, events, now)
}

fn step_y(read: &Fresh) -> &agendar_core::model::FeatureState {
    let project = ProjectSlug::parse(support::PROJECT).expect("project");
    let node = agendar_core::model::NodePath::parse("y").expect("node");
    &read.state.projects[&project].features[&node]
}

fn awaiting(service: &Service) -> Vec<String> {
    notes(service)
        .into_iter()
        .filter(|note| note["code"] == "awaiting_newer_version")
        .map(|note| note["message"].as_str().expect("message").to_owned())
        .collect()
}

#[test]
fn an_owner_drop_waits_on_an_older_version_and_folds_after_the_upgrade() {
    use agendar_core::model::Progress;
    let temp = tempfile::tempdir().expect("tempdir");
    let (vault, events, now) = vault_with_owner_drop(&temp.path().join("vault"), OWNER_DROP);
    let service = open_service(vault.root(), &temp.path().join("data"), now);

    let old = {
        let _older = before_owner_drop();
        let old = service.refresh(now, false).expect("older refresh");
        assert_eq!(undecodable(&service), 1, "kept for a newer version");
        assert_eq!(invalid_lines(&service), 1, "only the corrupt line");
        let again = service.refresh(now, false).expect("older refresh again");
        assert_eq!(again.generation, old.generation, "no work while it waits");
        old
    };
    assert_eq!(old.events.len(), events.len());
    assert_eq!(step_y(&old).progress, Progress::Planned);
    let waiting = awaiting(&service);
    assert_eq!(waiting.len(), 1);
    assert!(waiting[0].starts_with("1 event line was written by a newer Agendar version"));

    // The upgrade: no rebuild, the first refresh folds the drop.
    let new = service.refresh(now, false).expect("new refresh");
    assert_eq!(new.events.len(), events.len() + 1);
    assert_eq!(undecodable(&service), 0);
    assert!(awaiting(&service).is_empty());
    let dropped = step_y(&new);
    assert_eq!(dropped.progress, Progress::Dropped);
    let owner = dropped.owner_drop.as_ref().expect("dropped by the owner");
    assert_eq!(owner.event.as_str(), "0000000000000000000000ZZ09");
    assert_eq!(owner.note.as_deref(), Some("Not needed any more."));
    let again = service.refresh(now, false).expect("second new refresh");
    assert_eq!(again.generation, new.generation, "once per version change");

    let rebuilt = rebuilt(&vault, &temp.path().join("rebuilt"), now);
    assert_eq!(dump(&service), dump(&rebuilt), "index equals a rebuild");
    let rebuilt_read = rebuilt.refresh(now, true).expect("rebuilt read");
    assert_same_read(&new, &rebuilt_read, "reused vs rebuild");
    assert_same_read(
        &service.refresh(now, true).expect("reload"),
        &rebuilt_read,
        "reload vs rebuild",
    );
    let cold = open_service(vault.root(), &temp.path().join("cold"), now)
        .refresh(now, false)
        .expect("cold");
    assert_same_read(&new, &cold, "reused vs cold");
}

/// No simulation: the decoder of this build meets a line whose kind it does
/// not know (the owner's drop line under a kind no version has yet). It is
/// rejected as `unknown_kind` and kept through the cheap path, which is what
/// a build from before `plan.dropped` does with the owner's drop.
#[test]
fn the_real_decoder_keeps_an_operator_line_of_a_kind_it_does_not_know() {
    let line = OWNER_DROP.replace("plan.dropped", "plan.not-yet-known");
    let source = agendar_core::model::SourcePath::from_relative_path(&format!(
        "projects/{}/h-00000000000000000000000009/operator.jsonl",
        support::PROJECT
    ))
    .expect("source");
    let error = agendar_core::model::parse_event(line.as_bytes(), &source)
        .expect_err("this build does not know the kind");
    assert_eq!(error.detail(), Some("unknown_kind"));
    agendar_core::model::parse_event(OWNER_DROP.as_bytes(), &source)
        .expect("this build reads the owner's drop");

    let temp = tempfile::tempdir().expect("tempdir");
    let (vault, events, now) = vault_with_owner_drop(&temp.path().join("vault"), &line);
    let service = open_service(vault.root(), &temp.path().join("data"), now);
    let read = service.refresh(now, false).expect("refresh");
    assert_eq!(read.events.len(), events.len(), "not folded");
    assert_eq!(undecodable(&service), 1, "stored, not only diagnosed");
    assert_eq!(invalid_lines(&service), 1, "only the corrupt line");
    assert_eq!(awaiting(&service).len(), 1, "counted in health");
    let again = service.refresh(now, false).expect("again");
    assert_eq!(again.generation, read.generation);
    // A rebuild by the same build keeps it the same way.
    let rebuilt = rebuilt(&vault, &temp.path().join("rebuilt"), now);
    assert_eq!(dump(&service), dump(&rebuilt));
}

#[test]
fn two_hosts_converge_on_an_owner_drop_once_both_are_new() {
    use agendar_core::model::Progress;
    let temp = tempfile::tempdir().expect("tempdir");
    let (vault, _, now) = vault_with_owner_drop(&temp.path().join("vault"), OWNER_DROP);
    let old_host = open_service(vault.root(), &temp.path().join("old-host"), now);
    let new_host = open_service(vault.root(), &temp.path().join("new-host"), now);
    let behind = {
        let _older = before_owner_drop();
        old_host.refresh(now, false).expect("older host")
    };
    let ahead = new_host.refresh(now, false).expect("newer host");
    assert_eq!(step_y(&behind).progress, Progress::Planned);
    assert_eq!(step_y(&ahead).progress, Progress::Dropped);
    let caught_up = old_host.refresh(now, false).expect("upgraded host");
    assert_same_read(&caught_up, &ahead, "converged");
    assert_eq!(dump(&old_host), dump(&new_host), "same index");
}

// ---------------------------------------------------------------------
// The owner's snooze (`need.snoozed`) across versions
// ---------------------------------------------------------------------

/// A reader from before `need.snoozed` existed.
fn before_snooze() -> OlderReaderGuard {
    simulate_older_reader(OlderReader {
        version: "0.0.0-older".to_owned(),
        unknown_kinds: vec!["need.snoozed".to_owned()],
        ..OlderReader::default()
    })
}

/// An agent at the other computer asks on the seed's planned step `y` (a
/// line every version reads), and the owner there snoozes that question for
/// an hour: the row's id and the snooze line.
fn snooze_of_a_seed_row(vault: &Vault, data: &Path, now: UtcTime) -> (String, String) {
    let asker = "h-00000000000000000000000009/s-00000000000000000000000909";
    let ask = serde_json::json!({
        "v": 1, "id": "0000000000000000000000ZZ0A", "seq": 1, "writer": asker,
        "ts": now.add_seconds(-120), "project": support::PROJECT, "refs": {"node": "y"},
        "kind": "decision.ask",
        "body": {
            "agent": {"session": "00000000000000000000000909", "harness": "generic"},
            "card": support::card(), "policy": support::policy_eval(),
        },
    });
    support::append_raw_line(
        vault,
        &ProjectSlug::parse(support::PROJECT).expect("project"),
        &WriterId::parse(asker).expect("writer"),
        1,
        &format!("{ask}\n"),
    )
    .expect("append the ask");
    let read = open_service(vault.root(), data, now)
        .refresh(now, false)
        .expect("read the seed");
    let project = &read.state.projects[&ProjectSlug::parse(support::PROJECT).expect("project")];
    let need = project
        .needs_you
        .iter()
        .find(|row| row.id == "decide:0000000000000000000000ZZ0A")
        .expect("the question is a row");
    let revision = agendar_core::guard::need_revision(
        need,
        project.decisions.get(&need.source),
        &project.policy.revision,
    );
    let line = serde_json::json!({
        "v": 1, "id": "0000000000000000000000ZZ10", "seq": 1, "writer": OWNER_AT_9,
        "ts": now.add_seconds(-60), "project": support::PROJECT, "refs": {},
        "kind": "need.snoozed",
        "body": {"need": need.id, "revision": revision, "until": now.add_seconds(3_600)},
    });
    (need.id.clone(), format!("{line}\n"))
}

fn snoozed_until(read: &Fresh, need: &str) -> Option<UtcTime> {
    read.state.projects[&ProjectSlug::parse(support::PROJECT).expect("project")]
        .needs_you
        .iter()
        .find(|row| row.id == need)
        .expect("the row")
        .snoozed_until
}

#[test]
fn a_snooze_waits_on_an_older_version_and_hides_the_row_after_the_upgrade() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (vault, events, now) = seed_vault(&temp.path().join("vault"));
    let (need, line) = snooze_of_a_seed_row(&vault, &temp.path().join("probe"), now);
    support::append_raw_line(
        &vault,
        &ProjectSlug::parse(support::PROJECT).expect("project"),
        &WriterId::parse(OWNER_AT_9).expect("writer"),
        1,
        &line,
    )
    .expect("append the snooze");
    let service = open_service(vault.root(), &temp.path().join("data"), now);

    // The older build keeps the line as a placeholder and shows the row.
    let old = {
        let _older = before_snooze();
        let old = service.refresh(now, false).expect("older refresh");
        assert_eq!(undecodable(&service), 1, "kept for a newer version");
        assert_eq!(invalid_lines(&service), 1, "only the corrupt line");
        let again = service.refresh(now, false).expect("older refresh again");
        assert_eq!(again.generation, old.generation, "no work while it waits");
        old
    };
    assert_eq!(old.events.len(), events.len() + 1, "the seed and the ask");
    assert_eq!(snoozed_until(&old, &need), None);
    assert_eq!(awaiting(&service).len(), 1);

    // The upgrade: no rebuild, the first refresh reads the snooze.
    let new = service.refresh(now, false).expect("new refresh");
    assert_eq!(new.events.len(), events.len() + 2);
    assert_eq!(undecodable(&service), 0);
    assert!(awaiting(&service).is_empty());
    assert_eq!(snoozed_until(&new, &need), Some(now.add_seconds(3_600)));
    let again = service.refresh(now, false).expect("second new refresh");
    assert_eq!(again.generation, new.generation, "once per version change");

    let rebuilt = rebuilt(&vault, &temp.path().join("rebuilt"), now);
    assert_eq!(dump(&service), dump(&rebuilt), "index equals a rebuild");
    let rebuilt_read = rebuilt.refresh(now, true).expect("rebuilt read");
    assert_same_read(&new, &rebuilt_read, "reused vs rebuild");
    assert_same_read(
        &service.refresh(now, true).expect("reload"),
        &rebuilt_read,
        "reload vs rebuild",
    );

    // A host still on the older build and an upgraded one agree once both
    // are new.
    let other = open_service(vault.root(), &temp.path().join("other"), now);
    let behind = {
        let _older = before_snooze();
        other.refresh(now, false).expect("older host")
    };
    assert_eq!(snoozed_until(&behind, &need), None);
    let caught_up = other.refresh(now, false).expect("upgraded host");
    assert_same_read(&caught_up, &new, "converged");
    assert_eq!(dump(&other), dump(&service), "same index");
}

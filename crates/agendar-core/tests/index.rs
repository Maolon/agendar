//! Local index tests: DDL equality, offset ingestion, projection parity,
//! rebuild equality and query contracts.

mod support;

use agendar_core::index::ingest::ingest;
use agendar_core::index::query::{Query, QueryResult, query};
use agendar_core::index::{open_index, rebuild};
use agendar_core::model::{Event, EventPayload, ProjectSlug, State, UtcTime, canonical_json};
use agendar_core::reduce::{ReduceContext, reduce};
use agendar_core::runtime::LocalPaths;
use agendar_core::vault::{SystemClock, UlidIdSource, Vault};
struct Scratch {
    _temp: tempfile::TempDir,
    vault: Vault,
    local: LocalPaths,
}

fn scratch() -> Scratch {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("vault");
    let data = temp.path().join("data");
    let local = LocalPaths::new(&data).expect("local paths");
    let mut ids = UlidIdSource;
    let vault = Vault::init(&root, &SystemClock, &mut ids).expect("vault init");
    Scratch {
        _temp: temp,
        vault,
        local,
    }
}

fn fixture(name: &str) -> support::Fixture {
    support::load_fixture(name)
}

fn as_of(fixture: &support::Fixture) -> UtcTime {
    fixture.as_of
}

fn state_at(events: &[Event], as_of: UtcTime) -> State {
    reduce(events, &ReduceContext { as_of })
}

fn project_state<'a>(state: &'a State, slug: &str) -> &'a agendar_core::model::ProjectState {
    state
        .projects
        .get(&ProjectSlug::parse(slug).expect("slug"))
        .expect("project exists")
}

fn query_project(
    index: &agendar_core::index::Index,
    slug: &str,
    now: UtcTime,
) -> agendar_core::model::ProjectState {
    match query(
        index,
        Query::Project {
            project: ProjectSlug::parse(slug).expect("slug"),
        },
        now,
    )
    .expect("project query")
    {
        QueryResult::Project { state, .. } => state,
        other => panic!("unexpected query result {other:?}"),
    }
}

#[test]
fn the_schema_opens_at_its_declared_version() {
    let scratch = scratch();
    let index = open_index(&scratch.local, &scratch.vault).expect("index opens");
    let version: i64 = index
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user version");
    assert_eq!(version, 1);
    let schema: String = index
        .connection()
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("schema_version meta");
    assert_eq!(schema, "1");
    for table in [
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
    ] {
        let count: i64 = index
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .expect("table lookup");
        assert_eq!(count, 1, "table {table} exists");
    }
    let foreign_keys: i64 = index
        .connection()
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .expect("foreign keys");
    assert_eq!(foreign_keys, 1);
}

#[test]
fn ingest_matches_the_reducer_projection_and_reports_the_batch() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    for event in &fixture.events {
        support::append_event_to_vault(&scratch.vault, event, 1).expect("append event");
    }
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    let report = ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    assert_eq!(report.added, fixture.events.len() as u64);
    assert_eq!(report.pending, 0);
    assert_eq!(report.invalid, 0);
    assert_eq!(report.generation, 1);
    assert_eq!(
        report.changed_projects,
        vec![ProjectSlug::parse("example").expect("slug")]
    );
    assert!(report.pulse_ids.is_empty());

    let expected = state_at(&fixture.events, as_of(&fixture));
    let indexed = query_project(&index, "example", as_of(&fixture));
    assert_eq!(
        canonical_json(&serde_json::to_value(&indexed).expect("project")),
        canonical_json(
            &serde_json::to_value(project_state(&expected, "example")).expect("project")
        )
    );
    let digest: String = index
        .connection()
        .query_row(
            "SELECT value FROM meta WHERE key = 'event_set_digest'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("digest");
    assert_eq!(digest, expected.event_set_digest.as_str());

    // A second ingest folds again (generation advances) but adds no variants.
    let report = ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("second ingest");
    assert_eq!(report.added, 0);
    assert_eq!(report.generation, 2);
    assert!(report.changed_projects.is_empty());
}

#[test]
fn every_prefix_matches_full_reduce_and_the_index() {
    for name in support::fixture_names() {
        let scratch = scratch();
        let fixture = fixture(&name);
        let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
        let now = fixture.as_of;
        let mut seen: Vec<Event> = Vec::new();
        for (position, event) in fixture.events.iter().enumerate() {
            // Two interleaved hosts are emulated by alternating append order.
            support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
            seen.push(event.clone());
            ingest(&scratch.vault, &mut index, &[], now).expect("ingest prefix");
            let expected = state_at(&seen, now);
            for slug in expected.projects.keys() {
                let indexed = query_project(&index, slug.as_str(), now);
                assert_eq!(
                    canonical_json(&serde_json::to_value(&indexed).expect("project")),
                    canonical_json(
                        &serde_json::to_value(&expected.projects[slug]).expect("project")
                    ),
                    "fixture {name} prefix {} project {} drifted",
                    position + 1,
                    slug.as_str()
                );
            }
            let digest: String = index
                .connection()
                .query_row(
                    "SELECT value FROM meta WHERE key = 'event_set_digest'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .expect("digest");
            assert_eq!(digest, expected.event_set_digest.as_str());
        }
    }
}

#[test]
fn a_pending_reference_resolves_on_a_later_ingest() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    let ask = fixture
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::DecisionAsk(_)))
        .expect("ask")
        .clone();
    let answer = fixture
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::DecisionAnswer(_)))
        .expect("answer")
        .clone();
    // The answer arrives before its ask: pending, not lost.
    support::append_event_to_vault(&scratch.vault, &answer, 1).expect("answer");
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    let report = ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    assert_eq!(report.pending, 1);
    let indexed = query_project(&index, "example", fixture.as_of);
    assert!(indexed.decisions.is_empty());
    // The ask completes the causal chain.
    support::append_event_to_vault(&scratch.vault, &ask, 1).expect("ask");
    let report = ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    assert_eq!(report.pending, 0);
    let indexed = query_project(&index, "example", fixture.as_of);
    assert_eq!(indexed.decisions.len(), 1);
    assert!(indexed.receipts.contains_key(&answer.id));
}

#[test]
fn conflicting_variants_are_preserved_and_quarantined() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    let plan = fixture
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::PlanFeature(_)))
        .expect("plan")
        .clone();
    support::append_event_to_vault(&scratch.vault, &plan, 1).expect("first variant");
    // A same-id, different-payload variant in a second segment file.
    let mut mutated = plan.clone();
    if let EventPayload::PlanFeature(body) = &mut mutated.payload {
        body.title = Some("mutated".to_owned());
    }
    support::append_event_to_vault(&scratch.vault, &mutated, 2).expect("second variant");
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    let report = ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    let rows: i64 = index
        .connection()
        .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
        .expect("count");
    assert_eq!(rows, 2, "both conflicting variants are preserved");
    let quarantined: i64 = index
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM raw_events WHERE status = 'quarantined'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(quarantined, 2);
    assert_eq!(report.invalid, 2);
    let indexed = query_project(&index, "example", fixture.as_of);
    assert!(indexed.features.is_empty());
}

#[test]
fn recent_project_events_reads_the_newest_valid_rows_oldest_first() {
    use agendar_core::index::query::recent_project_events;
    let ids = |events: &[&Event]| -> Vec<String> {
        events.iter().map(|event| event.id.to_string()).collect()
    };
    let scratch = scratch();
    let fixture = fixture("one-question");
    for event in &fixture.events {
        support::append_event_to_vault(&scratch.vault, event, 1).expect("append event");
    }
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    let slug = ProjectSlug::parse("example").expect("slug");
    let mut expected: Vec<&Event> = fixture.events.iter().collect();
    expected.sort_by(|left, right| (left.ts, left.id.as_str()).cmp(&(right.ts, right.id.as_str())));
    assert!(expected.len() > 2, "the fixture has enough events");

    let all = recent_project_events(&index, &slug, 400).expect("recent events");
    assert_eq!(ids(&all.iter().collect::<Vec<_>>()), ids(&expected));
    let tail = recent_project_events(&index, &slug, 2).expect("recent tail");
    assert_eq!(
        ids(&tail.iter().collect::<Vec<_>>()),
        ids(&expected[expected.len() - 2..])
    );
    let other = ProjectSlug::parse("other").expect("slug");
    assert!(
        recent_project_events(&index, &other, 400)
            .expect("other project")
            .is_empty()
    );

    // Quarantined variants never reach the feed.
    let forked = self::scratch();
    let plan = fixture
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::PlanFeature(_)))
        .expect("plan")
        .clone();
    let mut mutated = plan.clone();
    if let EventPayload::PlanFeature(body) = &mut mutated.payload {
        body.title = Some("mutated".to_owned());
    }
    support::append_event_to_vault(&forked.vault, &plan, 1).expect("first variant");
    support::append_event_to_vault(&forked.vault, &mutated, 2).expect("second variant");
    let mut index = open_index(&forked.local, &forked.vault).expect("index");
    ingest(&forked.vault, &mut index, &[], fixture.as_of).expect("ingest");
    assert!(
        recent_project_events(&index, &slug, 400)
            .expect("recent events")
            .iter()
            .all(|event| event.id != plan.id)
    );
}

#[test]
fn an_invalid_complete_line_is_a_diagnostic_and_advances_the_offset() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    let declared = &fixture.events[0];
    support::append_event_to_vault(&scratch.vault, declared, 1).expect("declaration");
    support::append_raw_line(
        &scratch.vault,
        &declared.project,
        &declared.writer,
        1,
        "{\"v\":1,\"not\":\"an event\"}\n",
    )
    .expect("invalid line");
    let plan = fixture
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::PlanFeature(_)))
        .expect("plan");
    support::append_event_to_vault(&scratch.vault, plan, 1).expect("plan");
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    let report = ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    assert_eq!(report.added, 2);
    let indexed = query_project(&index, "example", fixture.as_of);
    assert!(
        indexed
            .features
            .contains_key(&agendar_core::model::NodePath::parse("x").expect("node"))
    );
    let diagnostics: i64 = index
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM diagnostics WHERE code = 'invalid_line'",
            [],
            |row| row.get(0),
        )
        .expect("diagnostics");
    assert_eq!(diagnostics, 1);
    let health = query(&index, Query::Health, fixture.as_of).expect("health");
    match health {
        QueryResult::Health { files, .. } => {
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].health, "ok");
            assert!(files[0].offset > 0);
        }
        other => panic!("unexpected result {other:?}"),
    }
}

#[test]
fn a_rewritten_prefix_resets_the_cursor_and_retains_observed_events() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    let declared = &fixture.events[0];
    let plan = fixture
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::PlanFeature(_)))
        .expect("plan");
    support::append_event_to_vault(&scratch.vault, declared, 1).expect("declared");
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("first ingest");

    // Replace the file with a shorter, different prefix.
    let directory = agendar_core::vault::ensure_host_directory(
        scratch.vault.root(),
        &declared.project,
        declared.writer.host(),
    )
    .expect("host directory");
    let name = agendar_core::vault::writer_file_name(&declared.writer, 1).expect("file name");
    std::fs::write(directory.join(name), b"{\"partial\":true}\n").expect("rewrite");
    support::append_event_to_vault(&scratch.vault, plan, 1).expect("plan");
    let report = ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    assert!(report.added >= 1);
    let rows: i64 = index
        .connection()
        .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
        .expect("count");
    assert_eq!(rows, 2, "the earlier observed declaration is retained");
    let rewritten: i64 = index
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM diagnostics WHERE code = 'source_rewritten'",
            [],
            |row| row.get(0),
        )
        .expect("diagnostics");
    assert_eq!(rewritten, 1);

    // A rebuild now aborts because the Vault no longer holds the declaration.
    let error = match rebuild(&scratch.vault, &scratch.local, fixture.as_of) {
        Ok(_) => panic!("missing cached event must abort the rebuild"),
        Err(error) => error,
    };
    assert_eq!(error.code(), agendar_core::ErrorCode::Index);
    assert_eq!(error.detail(), Some("cached_event_missing"));
    let rows: i64 = index
        .connection()
        .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
        .expect("cache preserved");
    assert_eq!(rows, 2);
}

#[test]
fn rebuild_equals_incremental_at_a_fixed_time_and_preserves_local_rows() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    for event in &fixture.events {
        support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
    }
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    index
        .connection()
        .execute(
            "INSERT INTO local_sessions(session_id, project, harness, harness_session_id, pid, process_start, registered_at, repo_root)
             VALUES('00000000000000000000000101','example','generic','harness-1',4242,7,'2026-01-01T00:00:00.000Z','/tmp/repo')",
            [],
        )
        .expect("local session mirror");
    index
        .connection()
        .execute(
            "INSERT INTO delivery_attempts(packet_id, session_id, status, attempt_count)
             VALUES('packet-1','00000000000000000000000101','pending',1)",
            [],
        )
        .expect("delivery attempt mirror");
    index
        .connection()
        .execute(
            "INSERT INTO ui_local(key, json) VALUES('theme','\"dark\"')",
            [],
        )
        .expect("ui local");

    let before = query_project(&index, "example", fixture.as_of);
    let generation_before: i64 = index
        .connection()
        .query_row(
            "SELECT value FROM meta WHERE key = 'generation'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("generation")
        .parse()
        .expect("number");
    let epoch_before: String = index
        .connection()
        .query_row("SELECT value FROM meta WHERE key = 'epoch'", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("epoch");

    let rebuilt = rebuild(&scratch.vault, &scratch.local, fixture.as_of).expect("rebuild");
    let after = query_project(&rebuilt, "example", fixture.as_of);
    assert_eq!(
        canonical_json(&serde_json::to_value(&before).expect("state")),
        canonical_json(&serde_json::to_value(&after).expect("state"))
    );
    let expected = state_at(&fixture.events, fixture.as_of);
    assert_eq!(
        query_project(&rebuilt, "example", fixture.as_of)
            .needs_you
            .len(),
        project_state(&expected, "example").needs_you.len()
    );
    let generation_after: i64 = rebuilt
        .connection()
        .query_row(
            "SELECT value FROM meta WHERE key = 'generation'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("generation")
        .parse()
        .expect("number");
    assert!(generation_after > generation_before);
    let epoch_after: String = rebuilt
        .connection()
        .query_row("SELECT value FROM meta WHERE key = 'epoch'", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("epoch");
    assert_ne!(epoch_after, epoch_before, "rebuild stores a new epoch ULID");
    let local_sessions: i64 = rebuilt
        .connection()
        .query_row("SELECT COUNT(*) FROM local_sessions", [], |row| row.get(0))
        .expect("local sessions");
    assert_eq!(local_sessions, 1);
    let delivery_attempts: i64 = rebuilt
        .connection()
        .query_row("SELECT COUNT(*) FROM delivery_attempts", [], |row| {
            row.get(0)
        })
        .expect("delivery attempts");
    assert_eq!(delivery_attempts, 1);
    let ui_local: i64 = rebuilt
        .connection()
        .query_row("SELECT COUNT(*) FROM ui_local", [], |row| row.get(0))
        .expect("ui local");
    assert_eq!(ui_local, 1);
}

#[test]
fn a_missing_source_retains_the_cache_and_reports_pending_file_health() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    for event in &fixture.events {
        support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
    }
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    let declared = &fixture.events[0];
    let directory = agendar_core::vault::ensure_host_directory(
        scratch.vault.root(),
        &declared.project,
        declared.writer.host(),
    )
    .expect("host directory");
    std::fs::remove_dir_all(directory).expect("remove host directory");
    let report = ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    assert_eq!(report.added, 0);
    match query(&index, Query::Health, fixture.as_of).expect("health") {
        QueryResult::Health { files, .. } => {
            assert_eq!(files.len(), 2, "both writer files of host 01 disappeared");
            for file in &files {
                assert_eq!(file.health, "missing");
                assert!(file.offset > 0, "the recorded offset is retained");
            }
        }
        other => panic!("unexpected result {other:?}"),
    }
}

#[test]
fn an_index_for_another_vault_is_rejected_instead_of_reused() {
    let scratch = scratch();
    let index = open_index(&scratch.local, &scratch.vault).expect("index");
    index
        .connection()
        .execute(
            "UPDATE meta SET value = '00000000000000000000000099' WHERE key = 'vault_id'",
            [],
        )
        .expect("tamper");
    drop(index);
    let error = match open_index(&scratch.local, &scratch.vault) {
        Ok(_) => panic!("a foreign index must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), agendar_core::ErrorCode::Index);
    assert_eq!(error.detail(), Some("vault_id_mismatch"));
}

#[test]
fn needs_decision_history_and_search_queries_are_derived() {
    let scratch = scratch();
    let seed = support::load_seed("ranking-two-asks");
    for event in &seed.events {
        support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
    }
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest(&scratch.vault, &mut index, &[], seed.as_of).expect("ingest");
    let expected = state_at(&seed.events, seed.as_of);
    let expected_project = project_state(&expected, "example");

    match query(
        &index,
        Query::Needs {
            project: None,
            include_snoozed: false,
        },
        seed.as_of,
    )
    .expect("needs")
    {
        QueryResult::Needs { items } => {
            assert_eq!(items.len(), expected_project.needs_you.len());
            for (row, want) in items.iter().zip(&expected_project.needs_you) {
                assert_eq!(row.id, want.id);
                assert_eq!(row.score_x2, want.score_x2);
                assert_eq!(row.stalled_agents, want.stalled_agents);
            }
        }
        other => panic!("unexpected result {other:?}"),
    }

    let ask = agendar_core::model::EventId::parse("00000000000000000000000006").expect("ask id");
    match query(&index, Query::Decision { ask: ask.clone() }, seed.as_of).expect("decision") {
        QueryResult::Decision { state, receipts } => {
            assert_eq!(state.id, ask);
            assert!(state.effective_receipt.is_none());
            assert!(receipts.is_empty());
        }
        other => panic!("unexpected result {other:?}"),
    }

    match query(
        &index,
        Query::History {
            project: ProjectSlug::parse("example").expect("slug"),
            after: None,
            limit: 50,
        },
        seed.as_of,
    )
    .expect("history")
    {
        QueryResult::History { events } => {
            assert_eq!(events.len(), seed.events.len());
            let texts: Vec<&str> = events.iter().map(|event| event.text.as_str()).collect();
            assert!(texts.iter().any(|text| text.starts_with("Asked: ")));
            assert!(texts.iter().any(|text| text.contains("started")));
            // History uses the causal order, not lexical event order.
            assert_eq!(events[1].id.as_str(), "00000000000000000000000002");
        }
        other => panic!("unexpected result {other:?}"),
    }

    match query(
        &index,
        Query::Search {
            text: "answers".to_owned(),
        },
        seed.as_of,
    )
    .expect("search")
    {
        QueryResult::Search { needs, projects } => {
            assert_eq!(needs.len(), 2);
            assert!(projects.is_empty(), "no project field contains `answers`");
        }
        other => panic!("unexpected result {other:?}"),
    }
    match query(
        &index,
        Query::Search {
            text: "Example".to_owned(),
        },
        seed.as_of,
    )
    .expect("search")
    {
        QueryResult::Search { needs, projects } => {
            assert!(needs.is_empty());
            assert_eq!(projects.len(), 1);
            assert_eq!(projects[0].slug.as_str(), "example");
        }
        other => panic!("unexpected result {other:?}"),
    }

    match query(
        &index,
        Query::Rules {
            project: ProjectSlug::parse("example").expect("slug"),
        },
        seed.as_of,
    )
    .expect("rules")
    {
        QueryResult::Rules { rules } => assert!(rules.is_empty()),
        other => panic!("unexpected result {other:?}"),
    }
}

#[test]
fn snapshot_reports_a_missing_later_capability() {
    let scratch = scratch();
    let index = open_index(&scratch.local, &scratch.vault).expect("index");
    let error = query(
        &index,
        Query::Snapshot,
        UtcTime::parse("2026-01-01T12:00:00.000Z").expect("time"),
    )
    .expect_err("snapshot needs local status");
    assert_eq!(error.code(), agendar_core::ErrorCode::NotImplemented);
    assert_eq!(error.detail(), Some("snapshot_not_implemented"));
}

#[test]
fn project_and_decision_queries_report_not_found() {
    let scratch = scratch();
    let index = open_index(&scratch.local, &scratch.vault).expect("index");
    let now = UtcTime::parse("2026-01-01T12:00:00.000Z").expect("time");
    let error = query(
        &index,
        Query::Project {
            project: ProjectSlug::parse("missing").expect("slug"),
        },
        now,
    )
    .expect_err("missing project");
    assert_eq!(error.code(), agendar_core::ErrorCode::NotFound);
    let error = query(
        &index,
        Query::Decision {
            ask: agendar_core::model::EventId::parse("00000000000000000000000001").expect("id"),
        },
        now,
    )
    .expect_err("missing decision");
    assert_eq!(error.code(), agendar_core::ErrorCode::NotFound);
}

// ---------------------------------------------------------------------------
// Fix round 1: isolation, atomic initialization, stable reads, snapshots,
// provenance and the typed query contract.
// ---------------------------------------------------------------------------

fn indexed_full_state(
    index: &agendar_core::index::Index,
    now: UtcTime,
) -> agendar_core::model::State {
    use agendar_core::model::{Diagnostic, EventId, Sha256};
    let mut projects = std::collections::BTreeMap::new();
    for slug in list_slugs(index) {
        projects.insert(slug.clone(), query_project(index, slug.as_str(), now));
    }
    let mut statement = index
        .connection()
        .prepare(
            "SELECT id, project, event_id, code, message FROM diagnostics
             WHERE path IS NULL ORDER BY id",
        )
        .expect("diagnostics");
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .expect("diagnostics");
    let mut diagnostics = Vec::new();
    for row in rows {
        let (id, project, event, code, message) = row.expect("row");
        diagnostics.push(Diagnostic::new(
            id,
            code,
            project.map(|slug| ProjectSlug::parse(&slug).expect("slug")),
            event.map(|event| EventId::parse(&event).expect("event")),
            message,
        ));
    }
    let digest = index
        .connection()
        .query_row(
            "SELECT value FROM meta WHERE key = 'event_set_digest'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("digest");
    let evaluated_at = index
        .connection()
        .query_row(
            "SELECT value FROM meta WHERE key = 'evaluated_at'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("evaluated_at");
    agendar_core::model::State {
        projects,
        diagnostics,
        event_set_digest: Sha256::parse(&digest).expect("digest"),
        evaluated_at: UtcTime::parse(&evaluated_at).expect("ts"),
    }
}

fn list_slugs(index: &agendar_core::index::Index) -> Vec<ProjectSlug> {
    let mut statement = index
        .connection()
        .prepare("SELECT slug FROM projects ORDER BY slug")
        .expect("projects");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("rows");
    rows.map(|row| ProjectSlug::parse(&row.expect("slug")).expect("slug"))
        .collect()
}

#[cfg(unix)]
#[test]
fn an_index_leaf_may_not_be_a_symlink() {
    use agendar_core::ErrorCode;
    let scratch = scratch();
    let directory = scratch.local.vault_local_dir(scratch.vault.id());
    std::fs::create_dir_all(&directory).expect("index directory");
    let in_vault = scratch.vault.root().join("cache.sqlite");
    std::os::unix::fs::symlink(&in_vault, directory.join("index.sqlite")).expect("symlink");
    let error = expect_index_error(open_index(&scratch.local, &scratch.vault));
    assert_eq!(error.code(), ErrorCode::Index);
    assert!(
        !in_vault.exists(),
        "SQLite must never create a database inside the Vault"
    );

    // The WAL/SHM sidecars are validated the same way.
    std::fs::remove_file(directory.join("index.sqlite")).expect("remove symlink");
    std::os::unix::fs::symlink(&in_vault, directory.join("index.sqlite-wal")).expect("wal symlink");
    let error = expect_index_error(open_index(&scratch.local, &scratch.vault));
    assert_eq!(error.code(), ErrorCode::Index);
    assert!(!in_vault.exists());
}

#[test]
fn concurrent_open_and_close_tolerates_vanishing_wal_sidecars() {
    // SQLite deletes `-wal`/`-shm` when its last connection to the index
    // closes. That happens outside the ingest lock, so another opener can
    // observe a sidecar in one filesystem call and find it gone in the next.
    // Opening must still succeed: a legitimately vanished sidecar is not an
    // alias, a symlink or an I/O failure.
    let scratch = scratch();
    let local = scratch.local.clone();
    let vault = scratch.vault.clone();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let local = &local;
            let vault = &vault;
            scope.spawn(move || {
                for round in 0..120 {
                    let index = open_index(local, vault)
                        .unwrap_or_else(|error| panic!("open round {round}: {error}"));
                    let version: i64 = index
                        .connection()
                        .query_row("PRAGMA user_version", [], |row| row.get(0))
                        .expect("version");
                    assert_eq!(version, 1);
                    drop(index);
                }
            });
        }
    });

    // The index is still intact after the churn.
    let index = open_index(&local, &vault).expect("open after churn");
    let version: i64 = index
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version");
    assert_eq!(version, 1);
}

#[test]
fn first_open_initializes_the_schema_atomically_and_serially() {
    use agendar_core::ErrorCode;
    let concurrent = scratch();
    let local = concurrent.local.clone();
    let vault = concurrent.vault.clone();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let local = &local;
            let vault = &vault;
            scope.spawn(move || {
                let index = open_index(local, vault).expect("concurrent first open");
                let version: i64 = index
                    .connection()
                    .query_row("PRAGMA user_version", [], |row| row.get(0))
                    .expect("version");
                assert_eq!(version, 1);
                // A versioned index is never observable without its tables.
                let tables: i64 = index
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                        [],
                        |row| row.get(0),
                    )
                    .expect("tables");
                assert_eq!(tables, 17);
                let meta: i64 = index
                    .connection()
                    .query_row("SELECT COUNT(*) FROM meta WHERE key = 'vault_id'", [], |row| {
                        row.get(0)
                    })
                    .expect("meta");
                assert_eq!(meta, 1);
            });
        }
    });

    // A partial legacy schema is preserved and refused, never replaced.
    let partial = scratch();
    let directory = partial.local.vault_local_dir(partial.vault.id());
    std::fs::create_dir_all(&directory).expect("directory");
    let path = directory.join("index.sqlite");
    {
        let connection = rusqlite::Connection::open(&path).expect("manual db");
        connection
            .execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;")
            .expect("partial schema");
    }
    let error = expect_index_error(open_index(&partial.local, &partial.vault));
    assert_eq!(error.code(), ErrorCode::Index);
    assert_eq!(error.detail(), Some("schema_incomplete"));
    let connection = rusqlite::Connection::open(&path).expect("preserved");
    let tables: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |row| row.get(0),
        )
        .expect("tables");
    assert_eq!(tables, 1, "the existing cache is preserved");
}

fn same_length_feature(id: &str, node: &str) -> serde_json::Value {
    serde_json::json!({
        "v": 1,
        "id": id,
        "seq": 2,
        "writer": support::OPERATOR,
        "ts": "2026-01-01T00:00:00.000Z",
        "project": support::PROJECT,
        "refs": {"node": node},
        "kind": "plan.feature",
        "body": {"title": node, "slice": "example", "demo_kind": "cli_transcript", "depends_on": []},
    })
}

fn write_writer_file(
    vault: &Vault,
    project: &ProjectSlug,
    writer: &agendar_core::model::WriterId,
    text: &str,
) -> std::path::PathBuf {
    let directory =
        agendar_core::vault::ensure_host_directory(vault.root(), project, writer.host())
            .expect("host directory");
    let name = agendar_core::vault::writer_file_name(writer, 1).expect("file name");
    let path = directory.join(name);
    std::fs::write(&path, text).expect("write source");
    path
}

/// Ingest with the documented retry for transient `E_BUSY` contention.
fn ingest_retrying(
    vault: &Vault,
    index: &mut agendar_core::index::Index,
    now: UtcTime,
) -> agendar_core::index::IngestReport {
    let mut last = None;
    for _ in 0..5 {
        match ingest(vault, index, &[], now) {
            Ok(report) => return report,
            Err(error) if error.retryable() => last = Some(error),
            Err(error) => panic!("ingest failed: {error:?}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("ingest stayed busy: {:?}", last);
}

fn expect_index_error<T>(result: agendar_core::Result<T>) -> agendar_core::AgendarError {
    match result {
        Ok(_) => panic!("the operation must be refused"),
        Err(error) => error,
    }
}

#[test]
fn the_source_cursor_changes_on_append_and_same_length_rewrite() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    for event in &fixture.events {
        support::append_event_to_vault(&scratch.vault, event, 1).expect("append event");
    }
    let (service, _) =
        agendar_core::service::Service::init(scratch.vault.root(), scratch.local.data_root())
            .expect("service");
    let first = service
        .refresh(fixture.as_of, false)
        .expect("first refresh");
    let first_digest = first.source_digest.clone().expect("source cursor");
    assert_eq!(
        agendar_core::index::ingest::current_source_digest(&scratch.vault).expect("current cursor"),
        first_digest
    );

    let mut pulse = support::valid_event("pulse.session");
    pulse["id"] = serde_json::json!(ulid::Ulid::generate().to_string());
    pulse["seq"] = serde_json::json!(1000);
    let pulse = support::parse_value(&pulse).expect("pulse");
    support::append_event_to_vault(&scratch.vault, &pulse, 1).expect("append pulse");
    let second = service
        .refresh(fixture.as_of, false)
        .expect("append refresh");
    let second_digest = second.source_digest.clone().expect("source cursor");
    assert_ne!(second_digest, first_digest, "an append changes the cursor");
    assert_eq!(
        agendar_core::index::ingest::current_source_digest(&scratch.vault).expect("current cursor"),
        second_digest
    );

    let path = scratch
        .vault
        .root()
        .join(format!("projects/{}/{}", support::PROJECT, support::AGENT))
        .with_extension("jsonl");
    let before = std::fs::read_to_string(&path).expect("source text");
    let after = before.replacen("Started.", "Started?", 1);
    assert_eq!(before.len(), after.len(), "the rewrite keeps its length");
    std::fs::write(&path, after).expect("same-length rewrite");
    let third = service
        .refresh(fixture.as_of, false)
        .expect("rewrite refresh");
    let third_digest = third.source_digest.clone().expect("source cursor");
    assert_ne!(third_digest, second_digest, "a rewrite changes the cursor");
    assert_eq!(
        agendar_core::index::ingest::current_source_digest(&scratch.vault).expect("current cursor"),
        third_digest
    );
}

#[test]
fn an_unchanged_refresh_skips_the_ingest_lock_and_reevaluates_time() {
    let scratch = scratch();
    let fixture = fixture("attempts");
    for event in &fixture.events {
        support::append_event_to_vault(&scratch.vault, event, 1).expect("append event");
    }
    let (service, _) =
        agendar_core::service::Service::init(scratch.vault.root(), scratch.local.data_root())
            .expect("service");
    let first = service
        .refresh(fixture.as_of, false)
        .expect("first refresh");
    let project = ProjectSlug::parse("example").expect("project");
    let before = first.state.projects[&project].needs_you[0].waiting_minutes;

    let lock = scratch
        .local
        .ensure_layout(scratch.vault.root(), scratch.vault.id())
        .expect("layout")
        .locks
        .join("ingest.lock");
    let held = agendar_core::runtime::acquire_lock(&lock).expect("hold ingest lock");
    let later = fixture.as_of.add_seconds(3600);
    let second = service
        .refresh_until(later, false, Some(std::time::Instant::now()))
        .expect("the unchanged committed read does not need the ingest lock");
    assert_eq!(second.generation, first.generation);
    assert_eq!(second.state.evaluated_at, later);
    assert_eq!(
        second.state.projects[&project].needs_you[0].waiting_minutes,
        before + 60,
        "the cached events are reduced again at the later clock"
    );
    assert!(
        !agendar_core::index::query::recent_project_events(&second.index, &project, 20)
            .expect("the read-only index supports extras")
            .is_empty()
    );
    drop(held);
}

#[test]
fn a_same_length_rewrite_is_never_skipped() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    let declared = support::line(&serde_json::to_value(&fixture.events[0]).expect("declaration"));
    let first = same_length_feature("00000000000000000000000002", "x");
    let second = same_length_feature("00000000000000000000000003", "q");
    let first_line = support::line(&first);
    let second_line = support::line(&second);
    assert_eq!(
        first_line.len(),
        second_line.len(),
        "the replacement must keep the exact byte length"
    );
    let project = ProjectSlug::parse(support::PROJECT).expect("slug");
    let writer = fixture.events[0].writer.clone();
    write_writer_file(
        &scratch.vault,
        &project,
        &writer,
        &format!("{declared}{first_line}"),
    );
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("first ingest");
    let count = |index: &agendar_core::index::Index, id: &str| -> i64 {
        index
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM raw_events WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .expect("count")
    };
    assert_eq!(count(&index, "00000000000000000000000002"), 1);

    // Replace the file with a same-length version carrying a different event.
    write_writer_file(
        &scratch.vault,
        &project,
        &writer,
        &format!("{declared}{second_line}"),
    );
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("second ingest");
    assert_eq!(
        count(&index, "00000000000000000000000003"),
        1,
        "the replacement event is ingested, never skipped by a stale cursor"
    );
    assert_eq!(
        count(&index, "00000000000000000000000002"),
        1,
        "append-only truth keeps the earlier variant"
    );
    let offset: i64 = index
        .connection()
        .query_row("SELECT MAX(offset) FROM source_files", [], |row| row.get(0))
        .expect("offset");
    assert_eq!(
        offset as usize,
        declared.len() + second_line.len(),
        "the committed cursor covers exactly the decoded bytes"
    );
    let rewritten: i64 = index
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM diagnostics WHERE code = 'source_rewritten'",
            [],
            |row| row.get(0),
        )
        .expect("rewrite diagnostic");
    assert_eq!(rewritten, 1);
}

#[test]
fn rebuild_restores_provenance_offsets_and_diagnostics() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    for event in &fixture.events {
        support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
    }
    support::append_raw_line(
        &scratch.vault,
        &fixture.events[0].project,
        &fixture.events[0].writer,
        2,
        "{\"v\":1,\"not\":\"an event\"}\n",
    )
    .expect("invalid line");
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    let sources_before: i64 = index
        .connection()
        .query_row("SELECT COUNT(*) FROM event_sources", [], |row| row.get(0))
        .expect("sources");
    assert_eq!(sources_before, fixture.events.len() as i64);
    let offsets_before: Vec<(String, i64)> = {
        let mut statement = index
            .connection()
            .prepare("SELECT path, offset FROM source_files ORDER BY path")
            .expect("sources");
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("rows");
        rows.map(|row| row.expect("row")).collect()
    };
    let diagnostics_before: i64 = index
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM diagnostics WHERE path IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("diagnostics");
    assert_eq!(diagnostics_before, 1);

    let rebuilt = rebuild(&scratch.vault, &scratch.local, fixture.as_of).expect("rebuild");
    let sources_after: i64 = rebuilt
        .connection()
        .query_row("SELECT COUNT(*) FROM event_sources", [], |row| row.get(0))
        .expect("sources");
    assert_eq!(
        sources_after,
        fixture.events.len() as i64,
        "rebuild repopulates every source association"
    );
    let offsets_after: Vec<(String, i64)> = {
        let mut statement = rebuilt
            .connection()
            .prepare("SELECT path, offset FROM source_files ORDER BY path")
            .expect("sources");
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("rows");
        rows.map(|row| row.expect("row")).collect()
    };
    assert_eq!(offsets_before, offsets_after, "offsets are rebuilt exactly");
    let diagnostics_after: i64 = rebuilt
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM diagnostics WHERE path IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("diagnostics");
    assert_eq!(diagnostics_after, 1, "path diagnostics are regenerated");
    let state = indexed_full_state(&rebuilt, fixture.as_of);
    assert_eq!(state.projects.len(), 1);
}

#[test]
fn a_query_never_mixes_projection_generations() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    let project = fixture.events[0].project.clone();
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    // A second connection stands in for another process.
    let mut writer = open_index(&scratch.local, &scratch.vault).expect("second index");
    for event in &fixture.events {
        support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
    }
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("initial ingest");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_stop = stop.clone();
    let slug = ProjectSlug::parse(support::PROJECT).expect("slug");
    std::thread::scope(|scope| {
        scope.spawn(move || {
            // Bounded so a reader panic can never leave the scope spinning.
            for position in 0..100 {
                if reader_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let event = &fixture.events[position % fixture.events.len()];
                support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
                ingest_retrying(&scratch.vault, &mut writer, fixture.as_of);
            }
        });
        for _ in 0..100 {
            let state = match query(
                &index,
                Query::Project {
                    project: slug.clone(),
                },
                fixture.as_of,
            )
            .expect("reader query")
            {
                QueryResult::Project { state, .. } => state,
                other => panic!("unexpected {other:?}"),
            };
            let counted: u32 = state.summary.feature_counts.values().sum();
            assert_eq!(
                counted as usize,
                state.features.len(),
                "summary and children come from one generation"
            );
            assert_eq!(state.summary.needs_count as usize, state.needs_you.len());
            for decision in state.decisions.values() {
                if let Some(receipt) = &decision.effective_receipt {
                    assert!(
                        state.receipts.contains_key(receipt),
                        "a decision and its receipt are never split across generations"
                    );
                }
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let _ = project;
}

#[test]
fn typed_queries_expose_the_documented_shapes() {
    use agendar_core::index::query::{HISTORY_DEFAULT_LIMIT, RuleRecord};
    let scratch = scratch();
    let fixture = fixture("cycle");
    for event in &fixture.events {
        support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
    }
    // A human house rule with a lexical identity.
    let rule = support::parse_value(&serde_json::json!({
        "v": 1,
        "id": "00000000000000000000000045",
        "seq": 5,
        "writer": support::OPERATOR,
        "ts": "2026-01-01T00:02:00.000Z",
        "project": support::PROJECT,
        "refs": {},
        "kind": "rule.set",
        "body": {"rule_id": "house-rule", "text": "Always check the plan.", "enabled": true},
    }))
    .expect("rule");
    support::append_event_to_vault(&scratch.vault, &rule, 1).expect("append rule");
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");

    let slug = ProjectSlug::parse(support::PROJECT).expect("slug");
    match query(
        &index,
        Query::Project {
            project: slug.clone(),
        },
        fixture.as_of,
    )
    .expect("project")
    {
        QueryResult::Project { state, graph } => {
            assert_eq!(state.features.len(), 2);
            assert!(
                !graph.nodes.is_empty(),
                "the project query carries its graph"
            );
            assert!(!graph.edges.is_empty());
        }
        other => panic!("unexpected {other:?}"),
    }
    match query(
        &index,
        Query::Rules {
            project: slug.clone(),
        },
        fixture.as_of,
    )
    .expect("rules")
    {
        QueryResult::Rules { rules } => {
            assert_eq!(rules.len(), 1);
            let RuleRecord { rule, event_id } = &rules[0];
            assert_eq!(rule.rule_id.as_str(), "house-rule");
            assert!(rule.enabled);
            assert_eq!(event_id.as_str(), "00000000000000000000000045");
        }
        other => panic!("unexpected {other:?}"),
    }

    // An omitted history limit deserializes as 50.
    let omitted: Query = serde_json::from_value(serde_json::json!({
        "kind": "history",
        "project": support::PROJECT,
    }))
    .expect("history without a limit");
    match omitted {
        Query::History { limit, .. } => assert_eq!(limit, HISTORY_DEFAULT_LIMIT),
        other => panic!("unexpected {other:?}"),
    }
    let error = query(
        &index,
        Query::History {
            project: slug.clone(),
            after: None,
            limit: 201,
        },
        fixture.as_of,
    )
    .expect_err("out-of-range limit");
    assert_eq!(error.code(), agendar_core::ErrorCode::Validation);

    // Search validates its bound and matches feature titles.
    let too_long = "a".repeat(201);
    let error = query(&index, Query::Search { text: too_long }, fixture.as_of)
        .expect_err("over-long search");
    assert_eq!(error.code(), agendar_core::ErrorCode::Validation);
    match query(
        &index,
        Query::Search {
            text: "y".to_owned(),
        },
        fixture.as_of,
    )
    .expect("search by feature title")
    {
        QueryResult::Search { projects, .. } => {
            assert_eq!(projects.len(), 1, "feature titles match their project");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn the_recorded_prefix_digest_is_a_single_sha256() {
    use agendar_core::index::ingest::{ConsumedSource, hash_prefix, read_consumed};
    use agendar_core::model::Sha256;
    let scratch = scratch();
    let path = scratch.vault.root().join("prefix-probe.bin");
    std::fs::write(&path, b"abc\n").expect("write");
    // Literals computed independently (coreutils/python hashlib), not derived
    // from the implementation.
    assert_eq!(
        hash_prefix(&path, 0).expect("empty prefix").as_str(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    let consumed: ConsumedSource = read_consumed(&path, 0).expect("read");
    assert_eq!(consumed.next_offset, 4);
    assert_eq!(
        consumed.prefix_sha256.as_str(),
        "edeaaff3f1774ad2888673770c6d64097e391bc362d7d6fb34982ddf0efd18cb",
        "prefix_sha256 is SHA-256 of the bytes, not of their digest"
    );
    assert_eq!(
        hash_prefix(&path, 4).expect("whole file").as_str(),
        consumed.prefix_sha256.as_str()
    );
    let _ = Sha256::from_digest(&[0_u8; 32]);
}

#[test]
fn a_corrupt_cache_is_preserved_not_replaced() {
    let scratch = scratch();
    let directory = scratch.local.vault_local_dir(scratch.vault.id());
    std::fs::create_dir_all(&directory).expect("directory");
    let path = directory.join("index.sqlite");
    let garbage = b"this is not a sqlite database".to_vec();
    std::fs::write(&path, &garbage).expect("write garbage");
    let error = expect_index_error(open_index(&scratch.local, &scratch.vault));
    assert!(matches!(
        error.code(),
        agendar_core::ErrorCode::Index | agendar_core::ErrorCode::Io
    ));
    assert_eq!(
        std::fs::read(&path).expect("read back"),
        garbage,
        "a corrupt cache is never silently deleted or replaced"
    );
}

#[test]
fn a_rescan_drops_stale_invalid_line_diagnostics() {
    let scratch = scratch();
    let fixture = fixture("one-question");
    let declared = &fixture.events[0];
    let plan = &fixture.events[1];
    let declared_line = support::line(&serde_json::to_value(declared).expect("declaration"));
    let plan_line = support::line(&serde_json::to_value(plan).expect("plan"));
    let project = ProjectSlug::parse(support::PROJECT).expect("slug");
    let writer = declared.writer.clone();
    write_writer_file(
        &scratch.vault,
        &project,
        &writer,
        &format!("{declared_line}{{\"v\":1,\"not\":\"an event\"}}\n{plan_line}"),
    );
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    let invalid_count = |index: &agendar_core::index::Index| -> i64 {
        index
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM diagnostics WHERE code = 'invalid_line'",
                [],
                |row| row.get(0),
            )
            .expect("count")
    };
    assert_eq!(invalid_count(&index), 1);

    // Rewriting the source without the damaged line regenerates diagnostics.
    write_writer_file(
        &scratch.vault,
        &project,
        &writer,
        &format!("{declared_line}{plan_line}"),
    );
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("rescan");
    assert_eq!(
        invalid_count(&index),
        0,
        "stale path diagnostics are replaced"
    );
    assert!(
        query_project(&index, "example", fixture.as_of)
            .features
            .contains_key(&agendar_core::model::NodePath::parse("x").expect("node"))
    );
}

#[test]
fn a_rotation_during_ingest_never_skips_the_final_content() {
    // A provider-style temp+rename rotation runs while ingest reads the source.
    // The single-pass reader must always commit a cursor for bytes it decoded,
    // so the final content can never be skipped.
    let scratch = scratch();
    let fixture = fixture("one-question");
    let declared = support::line(&serde_json::to_value(&fixture.events[0]).expect("declaration"));
    let first = same_length_feature("00000000000000000000000002", "x");
    let second = same_length_feature("00000000000000000000000003", "q");
    let first_line = support::line(&first);
    let second_line = support::line(&second);
    assert_eq!(first_line.len(), second_line.len());
    let project = ProjectSlug::parse(support::PROJECT).expect("slug");
    let writer = fixture.events[0].writer.clone();
    let directory =
        agendar_core::vault::ensure_host_directory(scratch.vault.root(), &project, writer.host())
            .expect("host directory");
    let name = agendar_core::vault::writer_file_name(&writer, 1).expect("file name");
    let path = directory.join(name);
    std::fs::write(&path, format!("{declared}{first_line}")).expect("initial");
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("initial ingest");

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_stop = stop.clone();
    let path_for_writer = path.clone();
    let declared_for_writer = declared.clone();
    let first_for_writer = first_line.clone();
    let second_for_writer = second_line.clone();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let mut flip = false;
            for _ in 0..50 {
                if writer_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let body = if flip {
                    &second_for_writer
                } else {
                    &first_for_writer
                };
                let temporary = path_for_writer.with_extension("tmp");
                std::fs::write(&temporary, format!("{declared_for_writer}{body}")).expect("temp");
                std::fs::rename(&temporary, &path_for_writer).expect("rename");
                flip = !flip;
                std::thread::yield_now();
            }
        });
        for _ in 0..50 {
            ingest_retrying(&scratch.vault, &mut index, fixture.as_of);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    // Settle the file on the second variant and ingest once more.
    std::fs::write(&path, format!("{declared}{second_line}")).expect("final");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("final ingest");
    let present = |id: &str| -> i64 {
        index
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM raw_events WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .expect("count")
    };
    assert_eq!(
        present("00000000000000000000000003"),
        1,
        "final event ingested"
    );
    let offset: i64 = index
        .connection()
        .query_row("SELECT MAX(offset) FROM source_files", [], |row| row.get(0))
        .expect("offset");
    assert_eq!(offset as usize, declared.len() + second_line.len());
}

/// One wire event for the multi-project search fixture.
#[allow(clippy::too_many_arguments)]
fn search_event(
    project: &str,
    writer: &str,
    id: u64,
    seq: u64,
    ts: &str,
    kind: &str,
    refs: serde_json::Value,
    body: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "v": 1,
        "id": format!("{id:026}"),
        "seq": seq,
        "writer": writer,
        "ts": ts,
        "project": project,
        "refs": refs,
        "kind": kind,
        "body": body,
    })
}

#[test]
fn search_orders_projects_by_every_project_need() {
    // N3 regression: the project tie-break reads every project's earliest need
    // without consuming it, and does not depend on which rows matched the search
    // text. All three projects match, so the order is earliest need then slug.
    let scratch = scratch();
    let as_of = UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time");
    let plan = [
        ("alpha", "Answers alpha", 20_u64),
        ("beta", "Answers beta", 10_u64),
        ("gamma", "Answers gamma", 30_u64),
    ];
    for (index, (project, title, ask_minute)) in plan.iter().enumerate() {
        let session = format!("{:026}", 700 + index);
        let agent = format!("{}/s-{session}", support::HOST);
        let base = 900 + index as u64 * 10;
        let declared = search_event(
            project,
            support::OPERATOR,
            base,
            1,
            "2026-01-01T00:00:00.000Z",
            "project.declared",
            serde_json::json!({}),
            serde_json::json!({"title": title, "brief": "Search fixture", "goal": "Rank"}),
        );
        let feature = search_event(
            project,
            support::OPERATOR,
            base + 1,
            2,
            "2026-01-01T00:00:00.000Z",
            "plan.feature",
            serde_json::json!({"node": "a"}),
            serde_json::json!({
                "title": "a",
                "slice": "search",
                "demo_kind": "cli_transcript",
                "depends_on": [],
            }),
        );
        let start = search_event(
            project,
            &agent,
            base + 2,
            1,
            "2026-01-01T00:00:00.000Z",
            "work.progress",
            serde_json::json!({"node": "a"}),
            serde_json::json!({
                "agent": {"session": session, "harness": "generic"},
                "status": "started",
                "eta": {"min_s": 600, "max_s": 600},
                "evidence": [],
            }),
        );
        let ask_ts = format!("2026-01-01T00:{ask_minute:02}:00.000Z");
        let ask = search_event(
            project,
            &agent,
            base + 3,
            2,
            &ask_ts,
            "decision.ask",
            serde_json::json!({"node": "a"}),
            serde_json::json!({
                "agent": {"session": session, "harness": "generic"},
                "card": support::card(),
                "policy": support::policy_eval(),
            }),
        );
        let project_slug = ProjectSlug::parse(project).expect("slug");
        let operator = agendar_core::model::WriterId::parse(support::OPERATOR).expect("operator");
        let agent_writer = agendar_core::model::WriterId::parse(&agent).expect("agent");
        write_writer_file(
            &scratch.vault,
            &project_slug,
            &operator,
            &format!("{}{}", support::line(&declared), support::line(&feature)),
        );
        write_writer_file(
            &scratch.vault,
            &project_slug,
            &agent_writer,
            &format!("{}{}", support::line(&start), support::line(&ask)),
        );
    }
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest_retrying(&scratch.vault, &mut index, as_of);

    match query(
        &index,
        Query::Search {
            text: "answers".to_owned(),
        },
        as_of,
    )
    .expect("search")
    {
        QueryResult::Search { needs, projects } => {
            assert_eq!(needs.len(), 3, "the card question matches every project");
            let order: Vec<&str> = projects
                .iter()
                .map(|project| project.slug.as_str())
                .collect();
            assert_eq!(
                order,
                vec!["beta", "alpha", "gamma"],
                "earliest need first, then slug; never a consumed timestamp"
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

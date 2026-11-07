//! Convergence: arrival order, duplicate collapse, prefix parity and rebuild
//! equality for every reviewed fixture and approved seed.

mod support;

use agendar_core::index::ingest::ingest;
use agendar_core::index::query::{Query, QueryResult, query};
use agendar_core::index::{Index, open_index, rebuild};
use agendar_core::model::{Event, EventPayload, ProjectSlug, State, UtcTime, canonical_json};
use agendar_core::reduce::{ReduceContext, reduce};
use agendar_core::runtime::LocalPaths;
use agendar_core::vault::{Clock as _, SystemClock, UlidIdSource, Vault};
use proptest::prelude::*;

fn canonical(state: &State) -> String {
    canonical_json(&serde_json::to_value(state).expect("state serializes"))
}

fn reduced(events: &[Event], as_of: UtcTime) -> State {
    reduce(events, &ReduceContext { as_of })
}

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

fn permute(events: &[Event], keys: &[u64], duplicates: &[bool]) -> Vec<Event> {
    let mut order: Vec<usize> = (0..events.len()).collect();
    order.sort_by_key(|index| (keys[index % keys.len()], *index));
    let mut permuted = Vec::new();
    for index in order {
        permuted.push(events[index].clone());
        if duplicates[index % duplicates.len()] {
            permuted.push(events[index].clone());
        }
    }
    permuted
}

#[test]
fn every_fixture_and_seed_uses_the_same_set_based_reducer() {
    // A fixed, hand-picked set of "arbitrary" arrival schedules per fixture,
    // on top of the proptest permutations below.
    let schedules: &[&[u64]] = &[
        &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        &[15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
        &[7, 2, 9, 0, 5, 13, 1, 15, 3, 11, 6, 14, 4, 10, 8, 12],
        &[3, 3, 1, 8, 8, 0, 12, 6, 6, 2, 14, 9, 5, 13, 11, 4],
    ];
    for name in support::fixture_names() {
        let fixture = support::load_fixture(&name);
        let baseline = canonical(&reduced(&fixture.events, fixture.as_of));
        for schedule in schedules {
            let permuted = permute(&fixture.events, schedule, &[false; 16]);
            assert_eq!(
                canonical(&reduced(&permuted, fixture.as_of)),
                baseline,
                "fixture {name} depends on arrival order"
            );
            // Duplicate a subset: exact duplicates must collapse.
            let mut duplicated = permuted.clone();
            for (index, event) in permuted.iter().enumerate().take(3) {
                if index % 2 == 0 {
                    duplicated.insert(0, event.clone());
                }
            }
            assert_eq!(
                canonical(&reduced(&duplicated, fixture.as_of)),
                baseline,
                "fixture {name} changed under exact duplicates"
            );
        }
    }
    for name in [
        "answer-conflict",
        "plan-cycle",
        "ranking-two-asks",
        "concurrent-consumption",
        "delivery-seen-review",
    ] {
        let seed = support::load_seed(name);
        let baseline = canonical(&reduced(&seed.events, seed.as_of));
        for schedule in schedules {
            let permuted = permute(&seed.events, schedule, &[false; 16]);
            assert_eq!(
                canonical(&reduced(&permuted, seed.as_of)),
                baseline,
                "seed {name} depends on arrival order"
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 100, ..ProptestConfig::default() })]

    #[test]
    fn reducer_permutations_and_duplicate_subsets_converge(
        keys in prop::collection::vec(any::<u64>(), 16),
        duplicates in prop::collection::vec(any::<bool>(), 16),
    ) {
        for name in support::fixture_names() {
            let fixture = support::load_fixture(&name);
            if fixture.events.is_empty() { continue; }
            let baseline = canonical(&reduced(&fixture.events, fixture.as_of));
            let permuted = permute(&fixture.events, &keys, &duplicates);
            prop_assert_eq!(
                canonical(&reduced(&permuted, fixture.as_of)),
                baseline,
                "fixture {} did not converge (keys {:?}, duplicates {:?})",
                name, keys, duplicates
            );
        }
        for name in ["answer-conflict", "plan-cycle", "ranking-two-asks"] {
            let seed = support::load_seed(name);
            let baseline = canonical(&reduced(&seed.events, seed.as_of));
            let permuted = permute(&seed.events, &keys, &duplicates);
            prop_assert_eq!(
                canonical(&reduced(&permuted, seed.as_of)),
                baseline,
                "seed {} did not converge",
                name
            );
        }
    }

    #[test]
    fn index_ingest_converges_for_permuted_arrivals(
        keys in prop::collection::vec(any::<u64>(), 16),
        duplicates in prop::collection::vec(any::<bool>(), 16),
    ) {
        let fixture = support::load_fixture("one-question");
        let permuted = permute(&fixture.events, &keys, &duplicates);
        let scratch = scratch();
        for event in &permuted {
            support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
        }
        let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
        ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
        let expected = reduced(&fixture.events, fixture.as_of);
        let indexed = query_project(&index, fixture.as_of);
        prop_assert_eq!(
            canonical_json(&serde_json::to_value(&indexed).expect("project")),
            canonical_json(
                &serde_json::to_value(&expected.projects[&ProjectSlug::parse("example").expect("slug")])
                    .expect("project")
            )
        );
        prop_assert_eq!(
            digest(&index),
            expected.event_set_digest.as_str()
        );
    }
}

fn query_project(index: &Index, as_of: UtcTime) -> agendar_core::model::ProjectState {
    match query(
        index,
        Query::Project {
            project: ProjectSlug::parse("example").expect("slug"),
        },
        as_of,
    )
    .expect("project query")
    {
        QueryResult::Project { state, .. } => state,
        other => panic!("unexpected result {other:?}"),
    }
}

fn digest(index: &Index) -> String {
    index
        .connection()
        .query_row(
            "SELECT value FROM meta WHERE key = 'event_set_digest'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("digest")
}

#[test]
fn incremental_index_reopen_and_rebuild_agree_at_a_fixed_time() {
    for name in support::fixture_names() {
        let fixture = support::load_fixture(&name);
        let scratch = scratch();
        for event in &fixture.events {
            support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
        }
        let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
        ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
        let incremental = query_project(&index, fixture.as_of);
        let expected = reduced(&fixture.events, fixture.as_of);
        assert_eq!(
            canonical_json(&serde_json::to_value(&incremental).expect("project")),
            canonical_json(
                &serde_json::to_value(
                    &expected.projects[&ProjectSlug::parse("example").expect("slug")]
                )
                .expect("project")
            ),
            "fixture {name} incremental projection"
        );
        drop(index);

        // Reopen the same cache and ingest again: the projection is unchanged.
        let mut reopened = open_index(&scratch.local, &scratch.vault).expect("reopen");
        ingest(&scratch.vault, &mut reopened, &[], fixture.as_of).expect("re-ingest");
        let after_reopen = query_project(&reopened, fixture.as_of);
        assert_eq!(
            canonical_json(&serde_json::to_value(&after_reopen).expect("project")),
            canonical_json(&serde_json::to_value(&incremental).expect("project")),
            "fixture {name} reopen projection"
        );
        drop(reopened);

        // A full rebuild at the same fixed time is byte-identical.
        let rebuilt = rebuild(&scratch.vault, &scratch.local, fixture.as_of).expect("rebuild");
        let after_rebuild = query_project(&rebuilt, fixture.as_of);
        assert_eq!(
            canonical_json(&serde_json::to_value(&after_rebuild).expect("project")),
            canonical_json(&serde_json::to_value(&incremental).expect("project")),
            "fixture {name} rebuild projection"
        );
        assert_eq!(digest(&rebuilt), expected.event_set_digest.as_str());
    }
}

#[test]
fn writer_fork_fixture_keeps_quarantined_variants_out_of_every_projection() {
    let fixture = support::load_fixture("writer-fork");
    let state = reduced(&fixture.events, fixture.as_of);
    assert!(
        state.projects[&ProjectSlug::parse("example").expect("slug")]
            .features
            .is_empty()
    );
    for event in &fixture.events {
        if let EventPayload::PlanFeature(_) = event.payload {
            // The feature patches themselves never become features here.
            assert!(event.id.as_str().starts_with("0000000000000000000000007"));
        }
    }
}

// ---------------------------------------------------------------------------
// Fix round 1: every randomized ingest prefix, delayed/duplicate schedules and
// the transaction rollback contract.
// ---------------------------------------------------------------------------

/// Deterministic 64-bit LCG so a failing schedule is reproducible.
fn next_seed(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state >> 11
}

fn shuffled(len: usize, seed: u64) -> Vec<usize> {
    let mut state = seed | 1;
    let mut order: Vec<usize> = (0..len).collect();
    for index in (1..len).rev() {
        let swap = (next_seed(&mut state) % (index as u64 + 1)) as usize;
        order.swap(index, swap);
    }
    order
}

/// The full State rebuilt from the stored projections, diagnostics and meta.
fn indexed_state(index: &agendar_core::index::Index, now: UtcTime) -> State {
    use agendar_core::model::{Diagnostic, EventId, Sha256};
    let mut projects = std::collections::BTreeMap::new();
    let mut statement = index
        .connection()
        .prepare("SELECT slug FROM projects ORDER BY slug")
        .expect("projects");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("rows");
    for row in rows {
        let slug = ProjectSlug::parse(&row.expect("slug")).expect("slug");
        match query(
            index,
            Query::Project {
                project: slug.clone(),
            },
            now,
        )
        .expect("project query")
        {
            QueryResult::Project { state, .. } => {
                projects.insert(slug, state);
            }
            other => panic!("unexpected {other:?}"),
        }
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
        .expect("rows");
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
    let digest = meta(index, "event_set_digest");
    let evaluated_at = meta(index, "evaluated_at");
    State {
        projects,
        diagnostics,
        event_set_digest: Sha256::parse(&digest).expect("digest"),
        evaluated_at: UtcTime::parse(&evaluated_at).expect("ts"),
    }
}

fn meta(index: &agendar_core::index::Index, key: &str) -> String {
    index
        .connection()
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
        .expect("meta")
}

#[test]
fn every_randomized_ingest_prefix_matches_the_full_state() {
    for name in support::fixture_names() {
        let fixture = support::load_fixture(&name);
        let scratch = scratch();
        let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
        let mut arrived: Vec<Event> = Vec::new();
        let mut present: std::collections::BTreeSet<agendar_core::model::EventId> =
            std::collections::BTreeSet::new();
        for seed in 0..100_u64 {
            for (position, index_in_fixture) in
                shuffled(fixture.events.len(), seed).into_iter().enumerate()
            {
                let event = &fixture.events[index_in_fixture];
                support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
                if present.insert(event.id.clone()) {
                    arrived.push(event.clone());
                }
                // A duplicate arrival is folded again but must not add variants.
                let before = meta(&index, "generation");
                let report =
                    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest prefix");
                assert!(
                    report.generation > before.parse::<u64>().expect("generation"),
                    "fixture {name} seed {seed} position {position}"
                );
                let expected = reduced(&arrived, fixture.as_of);
                let actual = indexed_state(&index, fixture.as_of);
                assert_eq!(
                    canonical(&actual),
                    canonical(&expected),
                    "fixture {name} seed {seed} position {position} drifted"
                );
                assert_eq!(
                    meta(&index, "event_set_digest"),
                    expected.event_set_digest.as_str(),
                    "fixture {name} seed {seed} position {position} digest"
                );
            }
        }
        // The whole set has arrived: reopen and rebuild agree with the reducer.
        let expected = reduced(&fixture.events, fixture.as_of);
        drop(index);
        let reopened = open_index(&scratch.local, &scratch.vault).expect("reopen");
        assert_eq!(
            canonical(&indexed_state(&reopened, fixture.as_of)),
            canonical(&expected),
            "fixture {name} reopen"
        );
        drop(reopened);
        let rebuilt = rebuild(&scratch.vault, &scratch.local, fixture.as_of).expect("rebuild");
        assert_eq!(
            canonical(&indexed_state(&rebuilt, fixture.as_of)),
            canonical(&expected),
            "fixture {name} rebuild"
        );
    }
}

#[test]
fn a_failed_ingest_transaction_leaves_every_commit_point_unchanged() {
    let scratch = scratch();
    let fixture = support::load_fixture("one-question");
    for event in &fixture.events {
        support::append_event_to_vault(&scratch.vault, event, 1).expect("append");
    }
    let mut index = open_index(&scratch.local, &scratch.vault).expect("index");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("ingest");
    fn count(index: &agendar_core::index::Index, sql: &str) -> i64 {
        index
            .connection()
            .query_row(sql, [], |row| row.get(0))
            .expect("count")
    }
    let raw_before = count(&index, "SELECT COUNT(*) FROM raw_events");
    let sources_before = count(&index, "SELECT COUNT(*) FROM event_sources");
    let offsets_before: Vec<(String, i64, String)> = {
        let mut statement = index
            .connection()
            .prepare("SELECT path, offset, prefix_sha256 FROM source_files ORDER BY path")
            .expect("sources");
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("rows");
        rows.map(|row| row.expect("row")).collect()
    };
    let state_before = canonical(&indexed_state(&index, fixture.as_of));
    let generation_before = meta(&index, "generation");
    let digest_before = meta(&index, "event_set_digest");

    // A new valid event that only fails inside the write transaction.
    let pulse = support::parse_value(&serde_json::json!({
        "v": 1,
        "id": "00000000000000000000000009",
        "seq": 5,
        "writer": support::AGENT,
        "ts": "2026-01-01T00:06:00.000Z",
        "project": support::PROJECT,
        "refs": {},
        "kind": "pulse",
        "body": {"agent": support::agent(), "message": "Session started.", "boundary": true},
    }))
    .expect("pulse");
    support::append_event_to_vault(&scratch.vault, &pulse, 1).expect("append pulse");
    index
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_ingest BEFORE INSERT ON raw_events
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .expect("trigger");
    let error = ingest(&scratch.vault, &mut index, &[], fixture.as_of)
        .expect_err("the injected failure aborts the transaction");
    assert_eq!(error.code(), agendar_core::ErrorCode::Index);

    assert_eq!(count(&index, "SELECT COUNT(*) FROM raw_events"), raw_before);
    assert_eq!(
        count(&index, "SELECT COUNT(*) FROM event_sources"),
        sources_before
    );
    let offsets_after: Vec<(String, i64, String)> = {
        let mut statement = index
            .connection()
            .prepare("SELECT path, offset, prefix_sha256 FROM source_files ORDER BY path")
            .expect("sources");
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("rows");
        rows.map(|row| row.expect("row")).collect()
    };
    assert_eq!(
        offsets_before, offsets_after,
        "cursors are not acknowledged"
    );
    assert_eq!(
        canonical(&indexed_state(&index, fixture.as_of)),
        state_before,
        "projections are untouched"
    );
    assert_eq!(meta(&index, "generation"), generation_before);
    assert_eq!(meta(&index, "event_set_digest"), digest_before);

    // Removing the fault lets the same batch commit.
    index
        .connection()
        .execute_batch("DROP TRIGGER fail_ingest;")
        .expect("drop trigger");
    ingest(&scratch.vault, &mut index, &[], fixture.as_of).expect("retry");
    assert!(count(&index, "SELECT COUNT(*) FROM raw_events") > raw_before);
}

// ---------------------------------------------------------------------------
// Two hosts on one machine share one Vault through two copies.
// ---------------------------------------------------------------------------

use std::fs;
use std::path::{Path, PathBuf};

use agendar_core::model::{
    DecisionCard, Harness, Mode, NodePath, PlanFeature, ProjectDeclared, SessionId, WriterId,
    WriterRole,
};
use agendar_core::runtime::steward::{SessionRegistration, register_session};
use agendar_core::service::Service;
use agendar_core::vault::writer_relative_path;

fn create_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("repo");
    repo
}

fn declare_sync_board(service: &Service, project: &ProjectSlug, session: &SessionId) {
    let patch: ProjectDeclared = serde_json::from_value(serde_json::json!({
        "title": "Sync demo",
        "brief": "Two hosts share a Vault.",
        "goal": "Converge after partial copies."
    }))
    .expect("project patch");
    service
        .declare_project(project, session, patch, None)
        .expect("declare project");
    let node = NodePath::parse("core/sync").expect("node");
    let patch: PlanFeature = serde_json::from_value(serde_json::json!({
        "title": "Sync the answer",
        "slice": "core",
        "demo_kind": "cli_transcript"
    }))
    .expect("feature patch");
    service
        .declare_feature(project, session, node.clone(), patch, None)
        .expect("declare feature");
    service
        .progress_started(
            project,
            session,
            &node,
            agendar_core::model::EtaRange {
                min_s: agendar_core::model::Seconds::new(600).expect("min"),
                max_s: agendar_core::model::Seconds::new(1_800).expect("max"),
            },
            None,
            None,
        )
        .expect("start work");
    let card: DecisionCard = serde_json::from_value(support::card()).expect("card");
    service
        .ask(project, session, &node, card, Mode::Ask, None)
        .expect("ask");
}

fn collect_relative(root: &Path, current: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_relative(root, &path, out);
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.to_path_buf());
        }
    }
}

fn copy_file(source_root: &Path, target_root: &Path, relative: &Path) {
    let source = source_root.join(relative);
    if !source.is_file() {
        return;
    }
    let target = target_root.join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).expect("target dir");
    }
    fs::copy(&source, &target).expect("copy file");
}

/// Deterministic shuffle so a failure is reproducible without a random seed.
fn shuffle_paths(mut values: Vec<PathBuf>, seed: u64) -> Vec<PathBuf> {
    let mut state = seed;
    for index in (1..values.len()).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let pick = (state >> 33) as usize % (index + 1);
        values.swap(index, pick);
    }
    values
}

#[test]
fn two_vault_copies_converge_after_random_order_partial_and_duplicate_copies() {
    let temp = tempfile::tempdir().expect("temporary root");
    let root = temp.path();
    let vault_a = root.join("vault-a");
    let vault_b = root.join("vault-b");
    let data_a = root.join("data-a");
    let data_b = root.join("data-b");
    let repo = create_repo(root);
    let project = ProjectSlug::parse("sync-demo").expect("project");

    let (service_a, created) = Service::init(&vault_a, &data_a).expect("init host A");
    assert!(created);
    let pid = std::process::id();
    let session_a = register_session(
        &service_a,
        SessionRegistration {
            project: project.clone(),
            harness: Harness::Generic,
            harness_session_id: "host-a".to_owned(),
            pid,
            process_start: agendar_core::runtime::sessions::process_identity(pid)
                .expect("process identity"),
            repo_root: repo.clone(),
        },
    )
    .expect("register host A session");
    declare_sync_board(&service_a, &project, &session_a);

    // The second copy starts from the same create-once manifest and event bytes.
    fs::create_dir_all(vault_b.join("projects")).expect("host B projects");
    fs::copy(vault_a.join("vault.json"), vault_b.join("vault.json")).expect("manifest copy");
    let (service_b, created) = Service::init(&vault_b, &data_b).expect("init host B");
    assert!(!created);

    let agent_writer = WriterId::new(
        service_a.host().clone(),
        WriterRole::Agent(session_a.clone()),
    );
    let agent_relative = writer_relative_path(&project, &agent_writer, 1).expect("relative path");
    let source = vault_a.join(&agent_relative);
    let bytes = fs::read(&source).expect("host A agent stream");
    let last_line = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|end| {
            let start = bytes[..end]
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |previous| previous + 1);
            (start, end + 1)
        })
        .expect("at least one complete line");
    let cut = last_line.0 + (last_line.1 - last_line.0) / 2;
    let target = vault_b.join(&agent_relative);
    fs::create_dir_all(target.parent().expect("parent")).expect("host B dir");
    fs::write(&target, &bytes[..cut]).expect("half-line copy");
    let partial = service_b
        .export_state(SystemClock.now())
        .expect("state after a half-line copy");
    assert_eq!(
        partial
            .projects
            .get(&project)
            .map_or(0, |project| project.decisions.len()),
        0,
        "an incomplete final line never becomes an event"
    );
    let mut complete = fs::OpenOptions::new()
        .append(true)
        .open(&target)
        .expect("reopen host B stream");
    use std::io::Write as _;
    complete.write_all(&bytes[cut..]).expect("finish the line");
    drop(complete);
    let completed = service_b
        .export_state(SystemClock.now())
        .expect("state after the rest arrives");
    let project_b = completed.projects.get(&project).expect("project on host B");
    assert_eq!(project_b.decisions.len(), 1, "the ask is now complete");
    let ask_id = project_b.decisions.keys().next().expect("ask id").clone();

    // The answer is authored on host B, in host B's own stream.
    let answer = service_b
        .answer_decision(
            &project,
            &ask_id,
            &agendar_core::model::OptionId::parse("a").expect("option"),
            None,
            None,
            None,
        )
        .expect("answer on host B");

    // The reverse direction is partial too: host A sees half of host B's
    // answer line, then the rest, without a duplicate event.
    let operator_writer = WriterId::new(service_b.host().clone(), WriterRole::Operator);
    let answer_relative = writer_relative_path(&project, &operator_writer, 1).expect("answer path");
    let answer_bytes = fs::read(vault_b.join(&answer_relative)).expect("answer stream");
    let answer_target = vault_a.join(&answer_relative);
    fs::create_dir_all(answer_target.parent().expect("parent")).expect("answer dir");
    let halfway = answer_bytes.len() / 2;
    fs::write(&answer_target, &answer_bytes[..halfway]).expect("half answer copy");
    let partial_answer = service_a
        .export_state(SystemClock.now())
        .expect("state after half the answer");
    assert!(
        partial_answer
            .projects
            .get(&project)
            .is_none_or(|project| !project.receipts.contains_key(&answer)),
        "a cut answer line never becomes a receipt"
    );
    let mut rest = fs::OpenOptions::new()
        .append(true)
        .open(&answer_target)
        .expect("reopen answer stream");
    rest.write_all(&answer_bytes[halfway..])
        .expect("finish the answer");
    drop(rest);
    let whole_answer = service_a
        .export_state(SystemClock.now())
        .expect("state after the answer");
    assert!(
        whole_answer
            .projects
            .get(&project)
            .expect("project")
            .receipts
            .contains_key(&answer)
    );

    // Copy every file in a deterministic random order, with duplicates. The
    // union of both copies is already complete on each side after this pass.
    let mut paths = Vec::new();
    collect_relative(&vault_a, &vault_a.join("projects"), &mut paths);
    collect_relative(&vault_b, &vault_b.join("projects"), &mut paths);
    paths.sort();
    paths.dedup();
    let shuffled = shuffle_paths(paths, 0x5eed);
    for (index, relative) in shuffled.iter().enumerate() {
        copy_file(&vault_a, &vault_b, relative);
        copy_file(&vault_b, &vault_a, relative);
        if index % 3 == 0 {
            // Duplicate cloud copies are harmless and collapse by event ID.
            copy_file(&vault_a, &vault_b, relative);
        }
    }

    let as_of = SystemClock.now();
    let state_a = service_a.export_state(as_of).expect("state A");
    let state_b = service_b.export_state(as_of).expect("state B");
    assert_eq!(
        canonical(&state_a),
        canonical(&state_b),
        "both copies reach the same canonical state"
    );
    let project_a = state_a.projects.get(&project).expect("project A");
    assert_eq!(project_a.receipts.len(), 1);
    assert!(
        project_a.receipts.contains_key(&answer),
        "the answer written on host B is the effective receipt on both hosts"
    );
    assert_eq!(project_a.sessions.len(), 1);
}

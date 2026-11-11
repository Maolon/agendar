//! The usage listing: what each project holds and what a cleanup could free.
//!
//! One world built from explicit events: an archived project (deliveries, a
//! transcript also published as a report, an icon) and a live one (a delivery
//! superseded 60 days ago, one superseded 20 days ago, the current one, a
//! decision image, a file no event cites, a file that is not on this
//! computer and one that has not synced yet).

mod support;

use std::collections::BTreeMap;

use agendar_core::model::{Event, HostId, ProjectSlug, UtcTime};
use agendar_core::reduce::evidence::blob_referenced;
use agendar_core::reduce::usage::{
    BlobCategory, BlobFile, BlobReason, BlobStatus, BlobUsage, BlobUse, ProjectFiles, ProjectUsage,
    SUPERSEDED_AFTER_S, StorageUsage, Tally, storage_usage, visit_event_blobs,
};
use agendar_core::reduce::{ReduceContext, reduce};
use agendar_core::vault::listing::list_files;
use agendar_core::vault::{SystemClock, UlidIdSource, Vault};
use serde_json::{Value, json};

const OLD: &str = "old";
const LIVE: &str = "live";
const HOST: &str = support::HOST;
const SESSION: &str = support::SESSION;
const DAY: i64 = 24 * 60 * 60;
const NOW_DAY: i64 = 60;

fn at(s: i64) -> UtcTime {
    UtcTime::parse("2026-01-01T00:00:00.000Z")
        .expect("timestamp")
        .add_seconds(s)
}

fn slug(value: &str) -> ProjectSlug {
    ProjectSlug::parse(value).expect("slug")
}

fn sha(tag: u8) -> String {
    format!("{tag:02x}").repeat(32)
}

fn blob(tag: u8, ext: &str, bytes: u64) -> Value {
    json!({"host": HOST, "sha256": sha(tag), "ext": ext, "bytes": bytes})
}

fn file_name(tag: u8, ext: &str) -> String {
    format!("{}.{ext}", sha(tag))
}

fn agent() -> Value {
    json!({"session": SESSION, "harness": "generic"})
}

struct World {
    events: Vec<Event>,
    next_id: u64,
    seq: BTreeMap<String, u64>,
}

impl World {
    fn push(
        &mut self,
        project: &str,
        operator: bool,
        day: i64,
        kind: &str,
        refs: Value,
        body: Value,
    ) -> String {
        let id = format!("{:026}", self.next_id);
        self.next_id += 1;
        let writer = if operator {
            format!("{HOST}/operator")
        } else {
            format!("{HOST}/s-{SESSION}")
        };
        let seq = self.seq.entry(format!("{project}/{writer}")).or_insert(0);
        *seq += 1;
        let value = json!({
            "v": 1, "id": id, "seq": *seq, "writer": writer, "ts": at(day * DAY),
            "project": project, "refs": refs, "kind": kind, "body": body,
        });
        self.events
            .push(support::parse_value(&value).expect("test event decodes"));
        id
    }

    fn start(&mut self, project: &str, node: &str) -> String {
        self.push(
            project,
            false,
            0,
            "plan.feature",
            json!({"node": node}),
            json!({"title": node, "slice": "s", "demo_kind": "screenshot", "agent": agent()}),
        );
        self.push(
            project,
            false,
            0,
            "work.progress",
            json!({"node": node}),
            json!({"agent": agent(), "status": "started",
                   "eta": {"min_s": 600, "max_s": 1200}, "evidence": []}),
        )
    }

    fn deliver(
        &mut self,
        project: &str,
        day: i64,
        attempt: &str,
        supersedes: Option<&str>,
        demos: Value,
    ) -> String {
        let mut refs = json!({"node": "x", "attempt": attempt});
        if let Some(previous) = supersedes {
            refs["supersedes"] = json!(previous);
        }
        self.push(
            project,
            false,
            day,
            "delivery",
            refs,
            json!({
                "agent": agent(), "name": "Demo", "demos": demos, "criteria": [],
                "disclosure": {"proven": ["It runs"], "not_proven_yet": [], "faked_for_now": []},
            }),
        )
    }
}

fn screenshot(tag: u8, bytes: u64) -> Value {
    json!({"kind": "screenshot", "body": {"blob": blob(tag, "png", bytes), "alt": "The screen"}})
}

// Blob tags. Archived project:
const ICON: u8 = 0x01; // 10 bytes
const SHOT: u8 = 0x02; // 100
const VIDEO: u8 = 0x03; // 1000
const POSTER: u8 = 0x04; // 50
const TRANSCRIPT: u8 = 0x05; // 20, also a published report
// Live project:
const FIRST: u8 = 0x11; // 300, delivery superseded 60 days ago; not on this computer
const SECOND: u8 = 0x12; // 400, delivery superseded 20 days ago
const THIRD: u8 = 0x13; // 500, the current delivery; not synced yet
const SHARED: u8 = 0x14; // 70, in the first and the current delivery
const OPTION: u8 = 0x15; // 30, a decision option image
const CHECKED: u8 = 0x16; // 21, evidence on a progress event
const STRAY: u8 = 0x17; // 999, no event cites it

fn world() -> World {
    let mut world = World {
        events: Vec::new(),
        next_id: 1000,
        seq: BTreeMap::new(),
    };
    world.push(
        OLD,
        true,
        0,
        "project.declared",
        json!({}),
        json!({"title": "Old", "brief": "b", "goal": "g", "icon": blob(ICON, "png", 10)}),
    );
    let attempt = world.start(OLD, "x");
    world.deliver(
        OLD,
        1,
        &attempt,
        None,
        json!([
            screenshot(SHOT, 100),
            {"kind": "recording", "body": {"blob": blob(VIDEO, "mp4", 1000), "poster": blob(POSTER, "png", 50)}},
            {"kind": "cli_transcript", "body": {"blob": blob(TRANSCRIPT, "txt", 20)}},
        ]),
    );
    world.push(
        OLD,
        false,
        2,
        "report.published",
        json!({}),
        json!({"agent": agent(), "title": "Run", "summary": "The run.", "report": blob(TRANSCRIPT, "txt", 20)}),
    );
    world.push(
        OLD,
        true,
        3,
        "project.declared",
        json!({}),
        json!({"archived": true}),
    );

    world.push(
        LIVE,
        true,
        0,
        "project.declared",
        json!({}),
        json!({"title": "Live", "brief": "b", "goal": "g"}),
    );
    let attempt = world.start(LIVE, "x");
    let first = world.deliver(
        LIVE,
        0,
        &attempt,
        None,
        json!([screenshot(FIRST, 300), screenshot(SHARED, 70)]),
    );
    let second = world.deliver(
        LIVE,
        40,
        &attempt,
        Some(&first),
        json!([screenshot(SECOND, 400)]),
    );
    world.deliver(
        LIVE,
        55,
        &attempt,
        Some(&second),
        json!([screenshot(THIRD, 500), screenshot(SHARED, 70)]),
    );
    let mut card = support::card();
    card["options"][0]["image"] = blob(OPTION, "png", 30);
    world.push(
        LIVE,
        false,
        5,
        "decision.ask",
        json!({"node": "x"}),
        json!({"agent": agent(), "card": card, "policy": support::policy_eval()}),
    );
    world.push(
        LIVE,
        false,
        6,
        "work.progress",
        json!({"node": "x", "attempt": attempt}),
        json!({"agent": agent(), "status": "verified", "evidence": [{
            "criterion": "done", "status": "passed", "claim": "agent_says",
            "evidence": blob(CHECKED, "txt", 21),
        }]}),
    );
    world
}

fn present(tag: u8, ext: &str, bytes: u64, local: bool) -> BlobFile {
    BlobFile {
        host: HostId::parse(HOST).expect("host"),
        name: file_name(tag, ext),
        bytes,
        local,
    }
}

/// The files as a host would list them.
fn files() -> BTreeMap<ProjectSlug, ProjectFiles> {
    BTreeMap::from([
        (
            slug(OLD),
            ProjectFiles {
                event_bytes: 2000,
                event_local_bytes: 2000,
                blobs: vec![
                    present(ICON, "png", 10, true),
                    present(SHOT, "png", 100, true),
                    present(VIDEO, "mp4", 1000, true),
                    present(POSTER, "png", 50, true),
                    present(TRANSCRIPT, "txt", 20, true),
                ],
            },
        ),
        (
            slug(LIVE),
            ProjectFiles {
                event_bytes: 3000,
                event_local_bytes: 3000,
                blobs: vec![
                    // Evicted: the size is known, the content is elsewhere.
                    present(FIRST, "png", 300, false),
                    present(SECOND, "png", 400, true),
                    // THIRD has not synced: no file at all.
                    present(SHARED, "png", 70, true),
                    present(OPTION, "png", 30, true),
                    present(CHECKED, "txt", 21, true),
                    present(STRAY, "png", 999, true),
                ],
            },
        ),
    ])
}

fn measure(world: &World, only: Option<&ProjectSlug>) -> StorageUsage {
    let now = at(NOW_DAY * DAY);
    let state = reduce(&world.events, &ReduceContext { as_of: now });
    storage_usage(&world.events, &state, &files(), now, only)
}

fn project<'a>(usage: &'a StorageUsage, name: &str) -> &'a ProjectUsage {
    usage
        .projects
        .iter()
        .find(|project| project.project.as_str() == name)
        .expect("project listed")
}

fn find(project: &ProjectUsage, tag: u8) -> &BlobUsage {
    project
        .blobs
        .iter()
        .find(|blob| blob.file.starts_with(&sha(tag)))
        .expect("blob listed")
}

fn tally(count: u32, bytes: u64, local_bytes: u64) -> Tally {
    Tally {
        count,
        bytes,
        local_bytes,
    }
}

#[test]
fn an_archived_project_frees_its_delivery_attachments_and_keeps_the_rest() {
    let usage = measure(&world(), None);
    let old = project(&usage, OLD);
    assert!(old.archived);
    assert_eq!(old.title.as_deref(), Some("Old"));
    assert_eq!(old.numbers.event_bytes, 2000);
    assert_eq!(old.numbers.attachments, tally(5, 1180, 1180));
    assert_eq!(old.numbers.total_bytes, 3180);
    // The screenshot, the recording and its poster.
    assert_eq!(old.numbers.reclaimable, tally(3, 1150, 1150));
    assert_eq!(old.numbers.superseded, Tally::default());
    assert_eq!(old.numbers.unreferenced, Tally::default());
    for tag in [SHOT, VIDEO, POSTER] {
        let blob = find(old, tag);
        assert_eq!(blob.status, BlobStatus::Reclaimable);
        assert_eq!(blob.reason, BlobReason::ArchivedProjectDelivery);
        assert_eq!(blob.category, BlobCategory::Delivery);
    }
    assert_eq!(
        find(old, VIDEO).path,
        format!("projects/old/{HOST}/blobs/{}", file_name(VIDEO, "mp4"))
    );

    // The transcript is a demo and a published report: the report keeps it,
    // and it counts once.
    let transcript = find(old, TRANSCRIPT);
    assert_eq!(transcript.status, BlobStatus::Kept);
    assert_eq!(transcript.reason, BlobReason::Report);
    assert_eq!(transcript.category, BlobCategory::Report);
    assert_eq!(transcript.bytes, 20);
    let uses: Vec<BlobUse> = transcript
        .referrers
        .iter()
        .map(|referrer| referrer.usage)
        .collect();
    assert_eq!(uses, [BlobUse::DeliveryDemo, BlobUse::Report]);

    let icon = find(old, ICON);
    assert_eq!(
        (icon.status, icon.reason),
        (BlobStatus::Kept, BlobReason::Icon)
    );

    assert_eq!(
        old.numbers.categories,
        BTreeMap::from([
            (BlobCategory::Delivery, tally(3, 1150, 1150)),
            (BlobCategory::Report, tally(1, 20, 20)),
            (BlobCategory::Icon, tally(1, 10, 10)),
        ])
    );
}

#[test]
fn a_live_project_frees_nothing_and_shows_old_superseded_work_beside_it() {
    let usage = measure(&world(), None);
    let live = project(&usage, LIVE);
    assert!(!live.archived);
    assert_eq!(live.numbers.reclaimable, Tally::default());

    // Superseded 60 days ago, and no other referrer: the second line. Its
    // file is not on this computer, so it frees nothing here.
    let first = find(live, FIRST);
    assert_eq!(first.status, BlobStatus::Superseded);
    assert_eq!(first.reason, BlobReason::SupersededDelivery);
    assert_eq!((first.bytes, first.local_bytes), (300, 0));
    assert_eq!(live.numbers.superseded, tally(1, 300, 0));

    // Superseded, but only 20 days old.
    let second = find(live, SECOND);
    assert_eq!(
        (second.status, second.reason),
        (BlobStatus::Kept, BlobReason::SupersededRecently)
    );

    // The current delivery; its file has not synced yet.
    let third = find(live, THIRD);
    assert_eq!(
        (third.status, third.reason),
        (BlobStatus::Kept, BlobReason::CurrentDelivery)
    );
    assert_eq!((third.bytes, third.local_bytes), (500, 0));

    // In the old superseded delivery and in the current one: kept, once.
    let shared = find(live, SHARED);
    assert_eq!(
        (shared.status, shared.reason),
        (BlobStatus::Kept, BlobReason::CurrentDelivery)
    );
    assert_eq!(shared.referrers.len(), 2);

    let option = find(live, OPTION);
    assert_eq!(
        (option.status, option.reason),
        (BlobStatus::Kept, BlobReason::DecisionImage)
    );
    let checked = find(live, CHECKED);
    assert_eq!(
        (checked.status, checked.reason),
        (BlobStatus::Kept, BlobReason::Evidence)
    );

    // The file no event cites is listed on its own line.
    let stray = find(live, STRAY);
    assert_eq!(
        (stray.status, stray.reason),
        (BlobStatus::Unreferenced, BlobReason::Unreferenced)
    );
    assert!(stray.referrers.is_empty());
    assert_eq!(live.numbers.unreferenced, tally(1, 999, 999));

    // 300 + 400 + 500 + 70 + 30 + 21 referenced, 999 not.
    assert_eq!(
        live.numbers.attachments,
        tally(7, 2320, 400 + 70 + 30 + 21 + 999)
    );
    assert_eq!(
        live.numbers.categories,
        BTreeMap::from([
            (BlobCategory::Delivery, tally(4, 1270, 470)),
            (BlobCategory::Evidence, tally(1, 21, 21)),
            (BlobCategory::DecisionImage, tally(1, 30, 30)),
            (BlobCategory::Unreferenced, tally(1, 999, 999)),
        ])
    );
    assert_eq!(live.numbers.total_bytes, 3000 + 2320);
    assert_eq!(live.numbers.total_local_bytes, 3000 + 1520);
}

#[test]
fn the_thirty_days_are_event_time_and_strict() {
    let world = world();
    let superseded = |day: i64, extra_s: i64| {
        let now = at(day * DAY + extra_s);
        let state = reduce(&world.events, &ReduceContext { as_of: now });
        let usage = storage_usage(&world.events, &state, &files(), now, None);
        project(&usage, LIVE).numbers.superseded.count
    };
    assert_eq!(SUPERSEDED_AFTER_S, 30 * DAY);
    // The second delivery was made on day 40 and superseded on day 55.
    assert_eq!(superseded(60, 0), 1);
    assert_eq!(
        superseded(70, 0),
        1,
        "exactly 30 days old is not older than 30 days"
    );
    assert_eq!(superseded(70, 1), 2);
}

#[test]
fn the_totals_add_up_and_one_project_can_be_asked_for() {
    let usage = measure(&world(), None);
    assert_eq!(usage.superseded_after_days, 30);
    // Largest first.
    let order: Vec<&str> = usage
        .projects
        .iter()
        .map(|project| project.project.as_str())
        .collect();
    assert_eq!(order, [LIVE, OLD]);
    assert_eq!(usage.total.total_bytes, 3180 + 5320);
    assert_eq!(usage.total.reclaimable, tally(3, 1150, 1150));
    assert_eq!(usage.total.superseded, tally(1, 300, 0));
    assert_eq!(usage.total.unreferenced, tally(1, 999, 999));
    for project in &usage.projects {
        let numbers = &project.numbers;
        let parts = numbers
            .categories
            .values()
            .fold(Tally::default(), |sum, part| Tally {
                count: sum.count + part.count,
                bytes: sum.bytes + part.bytes,
                local_bytes: sum.local_bytes + part.local_bytes,
            });
        assert_eq!(parts, numbers.attachments, "{}", project.project);
        assert_eq!(
            usize::try_from(numbers.attachments.count).expect("count"),
            project.blobs.len()
        );
    }

    let only = measure(&world(), Some(&slug(OLD)));
    assert_eq!(only.projects.len(), 1);
    assert_eq!(only.projects[0], *project(&usage, OLD));
    assert_eq!(only.total, only.projects[0].numbers);
}

#[test]
fn the_media_check_and_the_listing_read_the_same_references() {
    let world = world();
    let usage = measure(&world, None);
    for project in &usage.projects {
        for listed in &project.blobs {
            let mut cited = Vec::new();
            for event in world
                .events
                .iter()
                .filter(|event| event.project == project.project)
            {
                visit_event_blobs(event, &mut |_, blob| {
                    if format!("{}.{}", blob.sha256, blob.ext.as_str()) == listed.file {
                        cited.push(blob.clone());
                    }
                });
            }
            assert_eq!(cited.is_empty(), listed.status == BlobStatus::Unreferenced);
            for blob in &cited {
                assert!(blob_referenced(&world.events, &project.project, blob, true));
                // Reports are served on their own path only.
                let only_reports = listed
                    .referrers
                    .iter()
                    .all(|referrer| referrer.usage.is_report());
                assert_eq!(
                    blob_referenced(&world.events, &project.project, blob, false),
                    !only_reports
                );
            }
        }
    }
    // A blob of one project is never referenced in the other.
    let mut shot = None;
    for event in &world.events {
        visit_event_blobs(event, &mut |_, blob| {
            if blob.sha256.as_str() == sha(SHOT) {
                shot = Some(blob.clone());
            }
        });
    }
    assert!(!blob_referenced(
        &world.events,
        &slug(LIVE),
        &shot.expect("shot"),
        true
    ));
}

#[test]
fn listing_the_vault_reads_names_and_sizes_only() {
    let root = tempfile::tempdir().expect("temporary root");
    let vault =
        Vault::init(&root.path().join("vault"), &SystemClock, &mut UlidIdSource).expect("vault");
    let host = vault.root().join("projects").join(LIVE).join(HOST);
    let blobs = host.join("blobs");
    std::fs::create_dir_all(&blobs).expect("blobs");
    std::fs::write(host.join("operator.jsonl"), vec![b'x'; 40]).expect("segment");
    std::fs::write(host.join("operator.2.jsonl"), vec![b'x'; 2]).expect("segment");
    std::fs::write(blobs.join(file_name(SECOND, "png")), vec![0_u8; 400]).expect("blob");
    // An iCloud stub stands for a blob that is not on this computer.
    std::fs::write(
        blobs.join(format!(".{}.icloud", file_name(FIRST, "png"))),
        b"stub",
    )
    .expect("stub");
    std::fs::write(blobs.join(".DS_Store"), b"finder").expect("hidden");
    // A folder that is not a project or a host is not measured.
    std::fs::create_dir_all(vault.root().join("projects").join("Not A Slug")).expect("stray dir");
    std::fs::create_dir_all(vault.root().join("projects").join(LIVE).join("notes"))
        .expect("stray dir");

    let listing = list_files(&vault).expect("listing");
    assert_eq!(listing.len(), 1);
    let live = &listing[&slug(LIVE)];
    assert_eq!((live.event_bytes, live.event_local_bytes), (42, 42));
    assert_eq!(
        live.blobs,
        vec![
            present(FIRST, "png", 0, false),
            present(SECOND, "png", 400, true)
        ]
    );
}

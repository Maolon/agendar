#![forbid(unsafe_code)]

//! Delivery review from a mobile client: read one delivery with every kind of demo,
//! accept it, send it back, and the refusals. The Mac is a core `Service` on
//! the same Vault; the agent writes raw lines and demo files, as its host
//! would.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use agendar_core::guard::{current_receipt, delivery_revision};
use agendar_core::model::{EventId, NodePath, ProjectSlug, ReviewStatus, ReviewVerdict, Sha256};
use agendar_core::service::Service;
use agendar_core::vault::{Clock, SystemClock};
use agendar_ffi::{
    CommentPlace, Demo, DemoText, DiffLineKind, FfiError, NeedKind, NewComment, RefusalCode,
    ReviewRound, ReviewState, RoundComment, SendBackDetail, Session, TEXT_MAX_LINES, Verdict,
};
use serde_json::{Value, json};

const PROJECT: &str = "mobile";
const MAC: &str = "h-00000000000000000000000001";
const SESSION: &str = "00000000000000000000000101";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -10,2 +10,2 @@ fn a()\n keep\n-old\n+new\ndiff --git a/src/b.rs b/src/b.rs\n--- a/src/b.rs\n+++ b/src/b.rs\n@@ -1,0 +1,1 @@\n+added\n";

struct World {
    dir: tempfile::TempDir,
    mac: Service,
    mobile: Session,
    next_id: Cell<u64>,
    seqs: std::cell::RefCell<BTreeMap<String, u64>>,
    /// The attempt `with_delivery` delivered; a redelivery continues it.
    attempt: std::cell::RefCell<String>,
}

impl World {
    /// One project with two started steps, `shots` and `links`.
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path().join("vault");
        let (mac, _) = Service::init(&vault, &dir.path().join("mac")).expect("init");
        let mobile = Session::open(
            vault.display().to_string(),
            dir.path().join("mobile").display().to_string(),
        )
        .expect("open");
        let world = Self {
            dir,
            mac,
            mobile,
            next_id: Cell::new(0),
            seqs: std::cell::RefCell::new(BTreeMap::new()),
            attempt: std::cell::RefCell::default(),
        };
        world.line(
            "operator",
            "project.declared",
            json!({}),
            json!({"brief": "Fixture", "goal": "Review from a mobile client", "title": "Mobile"}),
        );
        for (node, title, depends_on) in [
            ("shots", "Screens", json!([])),
            ("links", "Links", json!([])),
            ("after", "After screens", json!(["shots"])),
        ] {
            world.line(
                "operator",
                "plan.feature",
                json!({"node": node}),
                json!({"demo_kind": "screenshot", "depends_on": depends_on, "slice": "s0", "title": title}),
            );
        }
        world
    }

    fn vault(&self) -> PathBuf {
        self.dir.path().join("vault")
    }

    fn slug() -> ProjectSlug {
        ProjectSlug::parse(PROJECT).expect("slug")
    }

    /// Append one raw line as a writer of the Mac; returns the event id.
    fn line(&self, writer: &str, kind: &str, refs: Value, body: Value) -> String {
        self.next_id.set(self.next_id.get() + 1);
        let number = self.next_id.get();
        let id = format!("{number:026}");
        let seq = {
            let mut seqs = self.seqs.borrow_mut();
            let seq = seqs.entry(writer.to_owned()).or_insert(0);
            *seq += 1;
            *seq
        };
        let line = json!({
            "v": 1, "id": id, "seq": seq, "writer": format!("{MAC}/{writer}"),
            "ts": SystemClock.now().add_seconds(i64::try_from(number).expect("id") - 600).canonical(),
            "project": PROJECT, "refs": refs, "kind": kind, "body": body,
        });
        let host_dir = self.vault().join("projects").join(PROJECT).join(MAC);
        std::fs::create_dir_all(&host_dir).expect("host dir");
        let file = host_dir.join(format!("{writer}.jsonl"));
        let mut text = std::fs::read_to_string(&file).unwrap_or_default();
        text.push_str(&serde_json::to_string(&line).expect("json"));
        text.push('\n');
        std::fs::write(&file, text).expect("write line");
        id
    }

    fn agent_line(&self, kind: &str, refs: Value, mut body: Value) -> String {
        body["agent"] = json!({"harness": "claude_code", "session": SESSION});
        self.line(&format!("s-{SESSION}"), kind, refs, body)
    }

    fn start(&self, node: &str) -> String {
        self.agent_line(
            "work.progress",
            json!({"node": node}),
            json!({"eta": {"max_s": 900, "min_s": 300}, "evidence": [], "status": "started"}),
        )
    }

    /// A demo file of the Mac in the Vault, and its reference.
    fn blob(&self, ext: &str, bytes: &[u8]) -> Value {
        let sha = Sha256::from_bytes(bytes);
        let dir = self
            .vault()
            .join("projects")
            .join(PROJECT)
            .join(MAC)
            .join("blobs");
        std::fs::create_dir_all(&dir).expect("blobs dir");
        std::fs::write(dir.join(format!("{}.{ext}", sha.as_str())), bytes).expect("blob");
        json!({"host": MAC, "sha256": sha.as_str(), "ext": ext, "bytes": bytes.len()})
    }

    fn deliver(&self, node: &str, attempt: &str, name: &str, demos: Value) -> String {
        self.redeliver(node, attempt, name, demos, None, json!([]))
    }

    fn redeliver(
        &self,
        node: &str,
        attempt: &str,
        name: &str,
        demos: Value,
        supersedes: Option<&str>,
        replies: Value,
    ) -> String {
        let mut refs = json!({"node": node, "attempt": attempt});
        let mut body = json!({
            "name": name, "demos": demos, "criteria": [],
            "disclosure": {
                "proven": ["The screen renders."],
                "not_proven_yet": ["Dark mode."],
                "faked_for_now": ["The avatar is a placeholder."],
            },
        });
        if let Some(earlier) = supersedes {
            refs["supersedes"] = json!(earlier);
            body["replies"] = replies;
        }
        self.agent_line("delivery", refs, body)
    }

    /// The three demos the desktop renders: a screenshot, a transcript, a diff.
    fn shown_demos(&self) -> Value {
        json!([
            {"kind": "screenshot", "body": {"blob": self.blob("png", b"png bytes"), "alt": "The settings screen"}},
            {"kind": "cli_transcript", "body": {"blob": self.blob("txt", b"$ cargo test\r\nok\n")}},
            {"kind": "diff", "body": {
                "blob": self.blob("txt", DIFF.as_bytes()), "commit": COMMIT,
                "files_total": 3, "hunks_total": 5, "added_total": 9, "removed_total": 4,
                "shown": [
                    {"path": "src/a.rs", "hunk": 1, "why": "The fix itself."},
                    {"path": "src/b.rs", "hunk": 2, "why": "The new call."},
                    {"path": "src/c.rs", "hunk": 1, "why": "Listed but not cut."},
                ],
                "omitted": [{"path": "Cargo.lock", "added": 7, "removed": 2}],
            }},
        ])
    }

    /// A delivery of `shots` with the three shown demos, read by the mobile client.
    fn with_delivery() -> (Self, String) {
        let world = Self::new();
        let attempt = world.start("shots");
        let delivery = world.deliver("shots", &attempt, "Settings screen", world.shown_demos());
        *world.attempt.borrow_mut() = attempt;
        world.mobile.refresh(2_000).expect("refresh");
        (world, delivery)
    }

    /// The delivery's status and revision, as the Mac reads them now.
    fn mac_reads(&self, delivery: &str) -> (ReviewStatus, String) {
        let fresh = self.mac.refresh(SystemClock.now(), false).expect("refresh");
        let project = &fresh.state.projects[&Self::slug()];
        let state = &project.deliveries[&EventId::parse(delivery).expect("id")];
        let revision = delivery_revision(&Self::slug(), state, current_receipt(project, state));
        (state.review, revision.as_str().to_owned())
    }

    /// Every line the mobile client wrote, oldest first.
    fn mobile_lines(&self) -> Vec<Value> {
        let host = self.mobile.health().expect("health").host;
        let file = self
            .vault()
            .join("projects")
            .join(PROJECT)
            .join(host)
            .join("operator.jsonl");
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("json"))
            .collect()
    }
}

fn refused<T: std::fmt::Debug>(result: Result<T, FfiError>, expected: RefusalCode) {
    match result {
        Err(FfiError::Refused { code, .. }) => assert_eq!(code, expected),
        other => panic!("expected {expected:?}, got {other:?}"),
    }
}

fn send_back(reason: &str) -> Verdict {
    Verdict::SendBack {
        reason: Some(reason.to_owned()),
        comments: Vec::new(),
    }
}

/// A send-back that says everything in its comments.
fn commented(comments: Vec<NewComment>) -> Verdict {
    Verdict::SendBack {
        reason: None,
        comments,
    }
}

fn on(place: CommentPlace, text: &str) -> NewComment {
    NewComment {
        place,
        text: text.to_owned(),
    }
}

/// A line of the diff of `shown_demos` (demo 2); the quote is what a caller
/// must not be believed about.
fn diff_line(file: &str, old_side: bool, line: u32) -> CommentPlace {
    CommentPlace::DiffLine {
        demo: 2,
        file: file.to_owned(),
        old_side,
        line,
        quote: "forged".to_owned(),
    }
}

fn transcript_line(demo: u8, line: u32) -> CommentPlace {
    CommentPlace::TranscriptLine {
        demo,
        line,
        quote: String::new(),
    }
}

fn region(demo: u8, x: u16, y: u16, w: u16, h: u16) -> CommentPlace {
    CommentPlace::Region { demo, x, y, w, h }
}

/// A refused send-back with `detail` and the comment at fault as fields. The
/// message is unchanged: it starts with `starts` and ends with the core's
/// detail code.
fn invalid<T: std::fmt::Debug>(
    result: Result<T, FfiError>,
    expected: SendBackDetail,
    at: Option<u32>,
    starts: &str,
) {
    match result {
        Err(FfiError::SendBackRefused {
            detail,
            comment,
            message,
        }) => {
            assert_eq!(detail, expected, "{message}");
            assert_eq!(comment, at, "{message}");
            assert!(
                message.ends_with(&format!("[{}]", expected.as_str())),
                "{message}"
            );
            assert!(message.starts_with(starts), "{message}");
        }
        other => panic!("expected {expected:?}, got {other:?}"),
    }
}

/// Move a demo file of the Mac aside, as a sync app that has not downloaded
/// it leaves it.
fn evict(world: &World, ext: &str, bytes: &[u8]) {
    let sha = Sha256::from_bytes(bytes);
    let blobs = world
        .vault()
        .join(format!("projects/{PROJECT}/{MAC}/blobs"));
    std::fs::rename(
        blobs.join(format!("{}.{ext}", sha.as_str())),
        blobs.join(format!(".{}.{ext}.icloud", sha.as_str())),
    )
    .expect("evict");
}

#[test]
fn a_delivery_reads_with_its_screenshot_transcript_and_diff() {
    let (world, delivery_id) = World::with_delivery();
    let (_, revision) = world.mac_reads(&delivery_id);

    // The review need points at the delivery.
    let now = world.mobile.now(None).expect("now");
    let need = now
        .needs
        .iter()
        .find(|need| need.kind == NeedKind::Review)
        .expect("a review need");
    assert_eq!(need.source, delivery_id);
    let page = world.mobile.project(PROJECT.to_owned()).expect("page");
    assert_eq!(page.deliveries.len(), 1);
    assert_eq!(page.deliveries[0].delivery_id, delivery_id);
    assert_eq!(page.deliveries[0].review, ReviewState::Pending);
    assert_eq!(
        page.deliveries[0].need_id.as_deref(),
        Some(need.id.as_str())
    );
    assert_eq!(page.deliveries[0].demos, 3);

    let delivery = world
        .mobile
        .delivery(need.source.clone())
        .expect("delivery");
    assert_eq!(delivery.delivery_id, delivery_id);
    assert_eq!(delivery.name, "Settings screen");
    assert_eq!(
        (delivery.project.as_str(), delivery.project_title.as_str()),
        (PROJECT, "Mobile")
    );
    assert_eq!(
        (delivery.node.as_str(), delivery.step_title.as_str()),
        ("shots", "Screens")
    );
    assert_eq!(delivery.revision, revision, "the desktop's revision");
    assert_eq!(delivery.review, ReviewState::Pending);
    assert!(!delivery.superseded);
    assert_eq!(delivery.need_id.as_deref(), Some(need.id.as_str()));
    assert_eq!(delivery.since.as_deref(), Some(need.since.as_str()));
    assert_eq!(delivery.asked, None, "no decision asked for it");
    assert_eq!(delivery.unblocks, ["After screens"]);
    assert_eq!(delivery.disclosure.proven, ["The screen renders."]);
    assert_eq!(delivery.disclosure.not_proven_yet, ["Dark mode."]);
    assert_eq!(
        delivery.disclosure.faked_for_now,
        ["The avatar is a placeholder."]
    );
    assert!(delivery.criteria.is_empty() && delivery.answers.is_none());

    let [screenshot, transcript, diff] = delivery.demos.as_slice() else {
        panic!("three demos in order: {:?}", delivery.demos);
    };
    // A screenshot is its alt text and where its bytes are.
    let Demo::Screenshot { file, alt } = screenshot else {
        panic!("{screenshot:?}");
    };
    assert_eq!(alt, "The settings screen");
    let sha = Sha256::from_bytes(b"png bytes");
    assert_eq!(
        file.path,
        format!("projects/{PROJECT}/{MAC}/blobs/{}.png", sha.as_str())
    );
    assert_eq!((file.sha256.as_str(), file.bytes), (sha.as_str(), 9));
    assert_eq!(file.ext, "png");
    assert_eq!(
        std::fs::read(world.vault().join(&file.path)).expect("the file is there"),
        b"png bytes"
    );

    let Demo::CliTranscript { transcript } = transcript else {
        panic!("{transcript:?}");
    };
    assert_eq!(transcript.state, DemoText::Loaded);
    assert_eq!(transcript.text, "$ cargo test\nok");
    assert_eq!((transcript.lines_total, transcript.truncated), (2, false));
    assert!(transcript.file.path.ends_with(".txt"));

    let Demo::Diff { diff } = diff else {
        panic!("{diff:?}");
    };
    assert_eq!(diff.state, DemoText::Loaded);
    assert_eq!(diff.commit, COMMIT);
    assert_eq!(
        (
            diff.files_total,
            diff.hunks_total,
            diff.added_total,
            diff.removed_total
        ),
        (3, 5, 9, 4)
    );
    assert_eq!(diff.omitted.len(), 1);
    assert_eq!(
        (
            diff.omitted[0].path.as_str(),
            diff.omitted[0].added,
            diff.omitted[0].removed
        ),
        ("Cargo.lock", 7, 2)
    );
    // Each hunk the agent picked, in its order, with its reason.
    let hunks: Vec<_> = diff
        .hunks
        .iter()
        .map(|hunk| (hunk.path.as_str(), hunk.hunk, hunk.why.as_str()))
        .collect();
    assert_eq!(
        hunks,
        [
            ("src/a.rs", 1, "The fix itself."),
            ("src/b.rs", 2, "The new call."),
            ("src/c.rs", 1, "Listed but not cut."),
        ]
    );
    assert_eq!(
        diff.hunks[0].header.as_deref(),
        Some("@@ -10,2 +10,2 @@ fn a()")
    );
    let lines: Vec<_> = diff.hunks[0]
        .lines
        .iter()
        .map(|line| (line.kind, line.text.as_str(), line.old_line, line.new_line))
        .collect();
    assert_eq!(
        lines,
        [
            (DiffLineKind::Context, "keep", Some(10), Some(10)),
            (DiffLineKind::Del, "old", Some(11), None),
            (DiffLineKind::Add, "new", None, Some(11)),
        ]
    );
    assert_eq!(diff.hunks[1].lines_total, 1);
    assert_eq!(diff.hunks[1].lines[0].text, "added");
    // The excerpt holds two hunks: the third listed one has no lines.
    assert_eq!(
        (diff.hunks[2].header.as_ref(), diff.hunks[2].lines_total),
        (None, 0)
    );
}

#[test]
fn a_preview_url_and_a_recording_read_as_what_they_are() {
    let world = World::new();
    let attempt = world.start("links");
    let demos = json!([
        {"kind": "preview_url", "body": {"url": "http://localhost:5173/settings"}},
        {"kind": "recording", "body": {"blob": world.blob("mp4", b"video"), "poster": world.blob("png", b"poster")}},
    ]);
    let delivery_id = world.deliver("links", &attempt, "Links", demos);
    world.mobile.refresh(2_000).expect("refresh");

    let delivery = world.mobile.delivery(delivery_id).expect("delivery");
    let [Demo::PreviewUrl { url }, Demo::Recording { file, poster }] = delivery.demos.as_slice()
    else {
        panic!("{:?}", delivery.demos);
    };
    assert_eq!(url, "http://localhost:5173/settings");
    assert_eq!((file.ext.as_str(), file.bytes), ("mp4", 5));
    assert_eq!((poster.ext.as_str(), poster.bytes), ("png", 6));
    assert!(poster.path.ends_with(&format!("{}.png", poster.sha256)));
}

#[test]
fn text_that_is_not_here_or_too_long_says_so() {
    let world = World::new();
    let attempt = world.start("shots");
    let long: String = (0..TEXT_MAX_LINES + 25)
        .map(|line| format!("line {line}\n"))
        .collect();
    let demos = json!([
        {"kind": "cli_transcript", "body": {"blob": world.blob("txt", long.as_bytes())}},
        {"kind": "cli_transcript", "body": {"blob": world.blob("txt", b"evicted\n")}},
        {"kind": "cli_transcript", "body": {"blob": world.blob("txt", &[0xff, 0xfe, 0x00])}},
    ]);
    let delivery_id = world.deliver("shots", &attempt, "Logs", demos);
    world.mobile.refresh(2_000).expect("refresh");
    // The sync app has not downloaded the second file.
    let evicted = Sha256::from_bytes(b"evicted\n");
    let blobs = world
        .vault()
        .join(format!("projects/{PROJECT}/{MAC}/blobs"));
    std::fs::rename(
        blobs.join(format!("{}.txt", evicted.as_str())),
        blobs.join(format!(".{}.txt.icloud", evicted.as_str())),
    )
    .expect("evict");

    let delivery = world.mobile.delivery(delivery_id).expect("delivery");
    let transcripts: Vec<_> = delivery
        .demos
        .iter()
        .map(|demo| match demo {
            Demo::CliTranscript { transcript } => transcript,
            other => panic!("{other:?}"),
        })
        .collect();
    let first = transcripts[0];
    assert_eq!(first.state, DemoText::Loaded);
    assert!(first.truncated);
    assert_eq!(first.lines_total as usize, TEXT_MAX_LINES + 25);
    assert_eq!(first.text.lines().count(), TEXT_MAX_LINES);
    assert!(first.text.starts_with("line 0\nline 1\n"));
    // Still named, so a mobile client can ask the sync app for it.
    assert_eq!(transcripts[1].state, DemoText::NotOnDevice);
    assert_eq!(transcripts[1].text, "");
    assert_eq!(transcripts[1].file.sha256, evicted.as_str());
    assert_eq!(transcripts[2].state, DemoText::Unreadable);
}

#[test]
fn accept_writes_one_review_and_a_second_verdict_is_refused() {
    let (world, delivery_id) = World::with_delivery();
    let read = world
        .mobile
        .delivery(delivery_id.clone())
        .expect("delivery");
    assert!(world.mobile_lines().is_empty());

    // A revision that is not the one on screen writes nothing.
    refused(
        world
            .mobile
            .review(delivery_id.clone(), "0".repeat(64), Verdict::Accept),
        RefusalCode::StaleRevision,
    );
    assert!(world.mobile_lines().is_empty());

    let receipt = world
        .mobile
        .review(delivery_id.clone(), read.revision.clone(), Verdict::Accept)
        .expect("accept");
    assert_eq!(receipt.delivery_id, delivery_id);
    assert_eq!(receipt.project, PROJECT);
    assert_eq!(receipt.review, ReviewState::Accepted);
    assert_eq!(receipt.next_need_id, None);

    // One human event by the mobile client's operator, shaped like the desktop's.
    let lines = world.mobile_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["id"], json!(receipt.review_id));
    assert_eq!(line["kind"], "review.result");
    assert_eq!(line["body"], json!({"verdict": "accept"}));
    assert_eq!(
        line["refs"],
        json!({"node": "shots", "delivery": delivery_id})
    );
    assert_eq!(line["ts"], json!(receipt.written_at));
    let host = world.mobile.health().expect("health").host;
    assert_eq!(
        receipt.writer_file,
        format!("projects/{PROJECT}/{host}/operator.jsonl")
    );

    // The Mac reads the verdict; the need is gone on the mobile client.
    let (status, revision) = world.mac_reads(&delivery_id);
    assert_eq!(status, ReviewStatus::Accepted);
    assert_ne!(revision, read.revision, "a verdict changes the revision");
    let after = world
        .mobile
        .delivery(delivery_id.clone())
        .expect("delivery");
    assert_eq!(after.review, ReviewState::Accepted);
    assert_eq!((after.need_id, after.revision), (None, revision));
    let now = world.mobile.now(None).expect("now");
    assert!(now.needs.iter().all(|need| need.source != delivery_id));

    // A second verdict on the revision that was on screen is refused the way
    // the desktop refuses it: the delivery changed. Nothing more is written.
    for verdict in [Verdict::Accept, send_back("On second thought, no.")] {
        refused(
            world
                .mobile
                .review(delivery_id.clone(), read.revision.clone(), verdict),
            RefusalCode::StaleRevision,
        );
    }
    assert_eq!(world.mobile_lines().len(), 1);
}

#[test]
fn send_back_carries_the_owners_words_and_the_redelivery_shows_the_round() {
    let (world, delivery_id) = World::with_delivery();
    let read = world
        .mobile
        .delivery(delivery_id.clone())
        .expect("delivery");

    // Sending back needs words.
    match world
        .mobile
        .review(delivery_id.clone(), read.revision.clone(), send_back("  "))
    {
        Err(FfiError::SendBackRefused {
            detail, comment, ..
        }) => {
            assert_eq!(detail, SendBackDetail::SendBackRequiresReason);
            assert_eq!(comment, None);
        }
        other => panic!("{other:?}"),
    }
    assert!(world.mobile_lines().is_empty());

    let receipt = world
        .mobile
        .review(
            delivery_id.clone(),
            read.revision.clone(),
            send_back("  The title is cut off on a small screen.  "),
        )
        .expect("send back");
    assert_eq!(receipt.review, ReviewState::SentBack);
    let lines = world.mobile_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0]["body"],
        json!({"verdict": "send_back", "reason": "The title is cut off on a small screen."})
    );
    assert_eq!(world.mac_reads(&delivery_id).0, ReviewStatus::SentBack);
    refused(
        world
            .mobile
            .review(delivery_id.clone(), read.revision, Verdict::Accept),
        RefusalCode::StaleRevision,
    );

    // The agent delivers again and replies to the reason.
    let attempt = world.attempt.borrow().clone();
    let again = world.redeliver(
        "shots",
        &attempt,
        "Settings screen, again",
        world.shown_demos(),
        Some(&delivery_id),
        json!([{"text": "The title wraps now."}]),
    );
    world.mobile.refresh(2_000).expect("refresh");

    let old = world.mobile.delivery(delivery_id.clone()).expect("old");
    assert!(old.superseded);
    refused(
        world
            .mobile
            .review(delivery_id.clone(), old.revision, Verdict::Accept),
        RefusalCode::StaleRevision,
    );
    let page = world.mobile.project(PROJECT.to_owned()).expect("page");
    let listed: Vec<&str> = page
        .deliveries
        .iter()
        .map(|row| row.delivery_id.as_str())
        .collect();
    assert_eq!(
        listed,
        [again.as_str()],
        "the replaced delivery is not listed"
    );

    let new = world.mobile.delivery(again.clone()).expect("new");
    assert_eq!(new.review, ReviewState::Pending);
    let round = new.answers.expect("the round it answers");
    assert_eq!(round.delivery_id, delivery_id);
    assert_eq!(round.name, "Settings screen");
    assert_eq!(round.sent_back_at, receipt.written_at);
    assert_eq!(
        round.reason.as_deref(),
        Some("The title is cut off on a small screen.")
    );
    assert_eq!(round.reply.as_deref(), Some("The title wraps now."));
    assert!(round.comments.is_empty());
    world
        .mobile
        .review(again, new.revision, Verdict::Accept)
        .expect("accept the redelivery");
}

#[test]
fn the_desktops_line_comments_come_back_with_the_agents_replies() {
    let (world, delivery_id) = World::with_delivery();
    // The Mac sends back with comments pinned to a transcript line, a diff
    // line and an area of the screenshot.
    let comments = serde_json::from_value(json!([
        {"anchor": {"kind": "transcript_line", "demo": 1, "line": 2, "quote": "ok"}, "text": "Which tests?"},
        {"anchor": {"kind": "diff_line", "demo": 2, "file": "src/a.rs", "side": "new", "line": 11, "quote": "new"}, "text": "Name it."},
        {"anchor": {"kind": "region", "demo": 0, "x": 100, "y": 200, "w": 300, "h": 50}, "text": "Too tight."},
    ]))
    .expect("comments");
    world
        .mac
        .review_delivery(
            &World::slug(),
            &NodePath::parse("shots").expect("node"),
            &EventId::parse(&delivery_id).expect("id"),
            ReviewVerdict::SendBack,
            None,
            None,
            comments,
            vec![EventId::parse(&delivery_id).expect("id")],
        )
        .expect("the Mac sends back");
    let attempt = world.attempt.borrow().clone();
    let again = world.redeliver(
        "shots",
        &attempt,
        "Settings screen, again",
        world.shown_demos(),
        Some(&delivery_id),
        json!([{"comment": 2, "text": "Renamed."}, {"text": "All three looked at."}]),
    );
    world.mobile.refresh(2_000).expect("refresh");

    let round = world
        .mobile
        .delivery(again)
        .expect("delivery")
        .answers
        .expect("round");
    assert_eq!(round.reason, None);
    assert_eq!(round.reply.as_deref(), Some("All three looked at."));
    let rows: Vec<_> = round
        .comments
        .iter()
        .map(|row| (row.n, row.text.as_str(), row.reply.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [
            (1, "Which tests?", None),
            (2, "Name it.", Some("Renamed.")),
            (3, "Too tight.", None),
        ]
    );
    assert_eq!(
        round.comments[0].place,
        CommentPlace::TranscriptLine {
            demo: 1,
            line: 2,
            quote: "ok".to_owned()
        }
    );
    assert_eq!(
        round.comments[1].place,
        CommentPlace::DiffLine {
            demo: 2,
            file: "src/a.rs".to_owned(),
            old_side: false,
            line: 11,
            quote: "new".to_owned()
        }
    );
    assert_eq!(
        round.comments[2].place,
        CommentPlace::Region {
            demo: 0,
            x: 100,
            y: 200,
            w: 300,
            h: 50
        }
    );
}

#[test]
fn the_mac_reviews_first_and_an_unknown_delivery_is_not_synced() {
    let (world, delivery_id) = World::with_delivery();
    let read = world
        .mobile
        .delivery(delivery_id.clone())
        .expect("delivery");
    world
        .mac
        .review_delivery(
            &World::slug(),
            &NodePath::parse("shots").expect("node"),
            &EventId::parse(&delivery_id).expect("id"),
            ReviewVerdict::Accept,
            None,
            None,
            Vec::new(),
            Vec::new(),
        )
        .expect("the Mac accepts");
    // The write refreshes first, so the mobile client sees the Mac's verdict.
    refused(
        world
            .mobile
            .review(delivery_id, read.revision.clone(), send_back("No.")),
        RefusalCode::StaleRevision,
    );
    assert!(world.mobile_lines().is_empty());

    let unknown = "00000000000000000000009999".to_owned();
    refused(
        world
            .mobile
            .review(unknown.clone(), read.revision, Verdict::Accept),
        RefusalCode::NotSynced,
    );
    match world.mobile.delivery(unknown) {
        Err(FfiError::Core { code, detail, .. }) => {
            assert_eq!(code, "E_NOT_FOUND");
            assert_eq!(detail.as_deref(), Some("not_found"));
        }
        other => panic!("{other:?}"),
    }
}

/// The two ids of a verdict are checked before anything is read: each is
/// `E_VALIDATION` with its own detail and the argument at fault. Everything
/// else a send-back can get wrong is `SendBackRefused`.
#[test]
fn a_verdict_with_a_bad_id_or_revision_names_which() {
    let (world, delivery) = World::with_delivery();
    let (_, revision) = world.mac_reads(&delivery);
    for verdict in [|| Verdict::Accept, || send_back("Use the darker blue.")] {
        for (id, revision, want_detail, want_field) in [
            (
                "not an id",
                revision.as_str(),
                "invalid_event_id",
                "/delivery_id",
            ),
            (
                delivery.as_str(),
                "not a revision",
                "invalid_revision",
                "/expected_revision",
            ),
        ] {
            match world
                .mobile
                .review(id.to_owned(), revision.to_owned(), verdict())
            {
                Err(FfiError::Core {
                    code,
                    detail,
                    field,
                    retryable,
                    message,
                }) => {
                    assert_eq!(code, "E_VALIDATION", "{message}");
                    assert_eq!(detail.as_deref(), Some(want_detail), "{message}");
                    assert_eq!(field.as_deref(), Some(want_field), "{message}");
                    assert!(!retryable && !message.ends_with(']'), "{message}");
                }
                other => panic!("expected {want_detail}, got {other:?}"),
            }
        }
    }
    assert!(world.mobile_lines().is_empty());
}

#[test]
fn a_send_back_carries_comments_and_each_line_is_quoted_from_its_file() {
    let (world, delivery_id) = World::with_delivery();
    let read = world
        .mobile
        .delivery(delivery_id.clone())
        .expect("delivery");

    // No reason: the comments say it. One of each kind, both sides of a diff.
    let receipt = world
        .mobile
        .review(
            delivery_id.clone(),
            read.revision.clone(),
            Verdict::SendBack {
                reason: Some("   ".to_owned()),
                comments: vec![
                    on(diff_line("src/a.rs", false, 11), "Name it."),
                    on(diff_line("src/a.rs", true, 11), "Why did this go?"),
                    on(diff_line("src/b.rs", false, 1), "Added where?"),
                    on(transcript_line(1, 1), "Which tests?"),
                    on(region(0, 100, 200, 300, 50), "Too tight."),
                ],
            },
        )
        .expect("send back with comments");
    assert_eq!(receipt.review, ReviewState::SentBack);
    let lines = world.mobile_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"], "review.result");
    // The quotes are the files' lines (the carriage return dropped), never
    // the caller's; a region has none; a blank reason is no reason.
    assert_eq!(
        lines[0]["body"],
        json!({"verdict": "send_back", "comments": [
            {"anchor": {"kind": "diff_line", "demo": 2, "file": "src/a.rs", "side": "new", "line": 11, "quote": "new"}, "text": "Name it."},
            {"anchor": {"kind": "diff_line", "demo": 2, "file": "src/a.rs", "side": "old", "line": 11, "quote": "old"}, "text": "Why did this go?"},
            {"anchor": {"kind": "diff_line", "demo": 2, "file": "src/b.rs", "side": "new", "line": 1, "quote": "added"}, "text": "Added where?"},
            {"anchor": {"kind": "transcript_line", "demo": 1, "line": 1, "quote": "$ cargo test"}, "text": "Which tests?"},
            {"anchor": {"kind": "region", "demo": 0, "x": 100, "y": 200, "w": 300, "h": 50}, "text": "Too tight."},
        ]})
    );
    assert_eq!(world.mac_reads(&delivery_id).0, ReviewStatus::SentBack);

    // The agent answers them, and the mobile client reads its own comments back.
    let attempt = world.attempt.borrow().clone();
    let again = world.redeliver(
        "shots",
        &attempt,
        "Settings screen, again",
        world.shown_demos(),
        Some(&delivery_id),
        json!([{"comment": 5, "text": "Loosened."}]),
    );
    world.mobile.refresh(2_000).expect("refresh");
    let round = world
        .mobile
        .delivery(again)
        .expect("delivery")
        .answers
        .expect("round");
    assert_eq!(round.reason, None);
    assert_eq!(round.comments.len(), 5);
    assert_eq!(
        round.comments[0].place,
        CommentPlace::DiffLine {
            demo: 2,
            file: "src/a.rs".to_owned(),
            old_side: false,
            line: 11,
            quote: "new".to_owned(),
        }
    );
    assert_eq!(round.comments[4].place, region(0, 100, 200, 300, 50));
    assert_eq!(round.comments[4].reply.as_deref(), Some("Loosened."));
}

#[test]
fn a_reason_and_comments_go_together_in_one_review() {
    let (world, delivery_id) = World::with_delivery();
    let read = world
        .mobile
        .delivery(delivery_id.clone())
        .expect("delivery");
    world
        .mobile
        .review(
            delivery_id,
            read.revision,
            Verdict::SendBack {
                reason: Some(" Two things. ".to_owned()),
                comments: vec![on(transcript_line(1, 2), "Which?")],
            },
        )
        .expect("send back");
    assert_eq!(
        world.mobile_lines()[0]["body"],
        json!({"verdict": "send_back", "reason": "Two things.", "comments": [
            {"anchor": {"kind": "transcript_line", "demo": 1, "line": 2, "quote": "ok"}, "text": "Which?"},
        ]})
    );
}

/// The reason and the comments of one send-back.
type Input = (Option<String>, Vec<NewComment>);

/// Every refusal of a send-back's input says what is wrong and which comment
/// as fields, and writes nothing. `DemoNotOnDevice` has its own test below.
#[test]
fn a_send_back_that_does_not_fit_is_refused_with_fields_and_nothing_is_written() {
    use SendBackDetail as D;
    let (world, delivery_id) = World::with_delivery();
    let read = world
        .mobile
        .delivery(delivery_id.clone())
        .expect("delivery");
    let fine = || on(region(0, 0, 0, 10, 10), "Fine.");
    let file_line = |file: &str| on(diff_line(file, false, 11), "x");
    let words = |reason: &str| (Some(reason.to_owned()), Vec::new());
    let only = |comments: Vec<NewComment>| (None, comments);

    // (reason and comments, detail, comment at fault, start of the message)
    let table: Vec<(Input, D, Option<u32>, &str)> = vec![
        // The send-back as a whole: no words, too many comments, the reason.
        (
            only(Vec::new()),
            D::SendBackRequiresReason,
            None,
            "Say what",
        ),
        (words(" \n "), D::SendBackRequiresReason, None, "Say what"),
        (
            only((0..21).map(|_| fine()).collect()),
            D::TooManyComments,
            None,
            "A review carries at most 20 comments.",
        ),
        (words(&"x".repeat(700)), D::Length, None, "/reason is 700"),
        (words("a\0b"), D::ControlCharacter, None, "/reason"),
        (
            (Some("y".repeat(700)), vec![fine()]),
            D::Length,
            None,
            "/reason is 700",
        ),
        // One comment on its own: text, file, place.
        (
            only(vec![on(region(0, 0, 0, 10, 10), "  ")]),
            D::BlankString,
            Some(1),
            "Comment 1: ",
        ),
        (
            only(vec![fine(), on(region(0, 0, 0, 10, 10), &"x".repeat(400))]),
            D::Length,
            Some(2),
            "Comment 2: ",
        ),
        (
            only(vec![fine(), fine(), on(region(0, 0, 0, 10, 10), "a\nb")]),
            D::ControlCharacter,
            Some(3),
            "Comment 3: ",
        ),
        (
            only(vec![file_line(" ")]),
            D::BlankString,
            Some(1),
            "Comment 1: ",
        ),
        (
            only(vec![fine(), file_line(&"f".repeat(400))]),
            D::Length,
            Some(2),
            "Comment 2: ",
        ),
        (
            only(vec![on(region(0, 0, 0, 0, 10), "x")]),
            D::RegionOutOfRange,
            Some(1),
            "Comment 1: ",
        ),
        (
            only(vec![on(region(0, 900, 0, 101, 10), "x")]),
            D::RegionOutOfRange,
            Some(1),
            "Comment 1: ",
        ),
        (
            only(vec![on(region(3, 0, 0, 10, 10), "x")]),
            D::DemoOutOfRange,
            Some(1),
            "Comment 1: ",
        ),
        (
            only(vec![on(transcript_line(1, 0), "x")]),
            D::LineOutOfRange,
            Some(1),
            "Comment 1: ",
        ),
        // Against the delivery: the kind of the demo, and the lines it shows.
        (
            only(vec![on(region(1, 0, 0, 10, 10), "x")]),
            D::AnchorKindMismatch,
            Some(1),
            "Comment 1: ",
        ),
        (
            only(vec![fine(), on(transcript_line(0, 1), "x")]),
            D::AnchorKindMismatch,
            Some(2),
            "Comment 2: ",
        ),
        (
            only(vec![on(transcript_line(2, 1), "x")]),
            D::AnchorKindMismatch,
            Some(1),
            "Comment 1: ",
        ),
        (
            only(vec![fine(), on(transcript_line(1, 3), "x")]),
            D::LineNotInTranscript,
            Some(2),
            "Comment 2: ",
        ),
        (
            only(vec![on(diff_line("src/a.rs", false, 12), "x")]),
            D::LineNotInDiff,
            Some(1),
            "Comment 1: ",
        ),
        // `src/c.rs` is listed, but the excerpt has no hunk of it.
        (
            only(vec![
                fine(),
                fine(),
                on(diff_line("src/c.rs", false, 1), "x"),
            ]),
            D::LineNotInDiff,
            Some(3),
            "Comment 3: ",
        ),
    ];
    let mut seen = Vec::new();
    for ((reason, comments), detail, at, starts) in table {
        invalid(
            world.mobile.review(
                delivery_id.clone(),
                read.revision.clone(),
                Verdict::SendBack { reason, comments },
            ),
            detail,
            at,
            starts,
        );
        seen.push(detail);
    }
    assert!(world.mobile_lines().is_empty(), "nothing was written");
    assert_eq!(world.mac_reads(&delivery_id).0, ReviewStatus::Pending);

    // A delivery with fewer demos, one that takes no comment, one not text.
    let world = World::new();
    let attempt = world.start("links");
    let delivery_id = world.deliver(
        "links",
        &attempt,
        "Links",
        json!([
            {"kind": "preview_url", "body": {"url": "http://localhost:3000/"}},
            {"kind": "cli_transcript", "body": {"blob": world.blob("txt", &[0xff, 0xfe, 0x00])}},
        ]),
    );
    world.mobile.refresh(2_000).expect("refresh");
    let read = world
        .mobile
        .delivery(delivery_id.clone())
        .expect("delivery");
    for (comments, detail, at) in [
        (vec![on(region(2, 0, 0, 10, 10), "x")], D::NoSuchDemo, 1),
        (
            vec![on(region(0, 0, 0, 10, 10), "x")],
            D::AnchorKindMismatch,
            1,
        ),
        (
            vec![
                on(transcript_line(1, 1), "x"),
                on(region(2, 0, 0, 10, 10), "x"),
            ],
            D::DemoNotText,
            1,
        ),
    ] {
        invalid(
            world.mobile.review(
                delivery_id.clone(),
                read.revision.clone(),
                commented(comments),
            ),
            detail,
            Some(at),
            &format!("Comment {at}: "),
        );
        seen.push(detail);
    }
    assert!(world.mobile_lines().is_empty(), "nothing was written");

    // The table covers every detail but the one that needs a file moved away.
    for detail in SendBackDetail::ALL {
        assert_eq!(
            seen.contains(&detail),
            detail != D::DemoNotOnDevice,
            "{detail:?}"
        );
    }
}

#[test]
fn a_sent_back_delivery_reads_with_its_round_however_it_is_opened() {
    let (world, delivery_id) = World::with_delivery();
    let read = world
        .mobile
        .delivery(delivery_id.clone())
        .expect("delivery");
    assert_eq!(read.sent_back, None, "not reviewed yet");

    let receipt = world
        .mobile
        .review(
            delivery_id.clone(),
            read.revision,
            Verdict::SendBack {
                reason: Some(" Two things. ".to_owned()),
                comments: vec![
                    on(diff_line("src/a.rs", true, 11), "Why drop it?"),
                    on(region(0, 100, 200, 300, 50), "Too tight."),
                ],
            },
        )
        .expect("send back");

    // Opened by its own id, before any redelivery.
    let sent = world
        .mobile
        .delivery(delivery_id.clone())
        .expect("sent back");
    assert_eq!(sent.review, ReviewState::SentBack);
    assert!(!sent.superseded && sent.answers.is_none());
    let round = sent.sent_back.expect("the round it received");
    assert_eq!(
        round,
        ReviewRound {
            delivery_id: delivery_id.clone(),
            name: "Settings screen".to_owned(),
            sent_back_at: receipt.written_at,
            reason: Some("Two things.".to_owned()),
            reply: None,
            comments: vec![
                RoundComment {
                    n: 1,
                    place: CommentPlace::DiffLine {
                        demo: 2,
                        file: "src/a.rs".to_owned(),
                        old_side: true,
                        line: 11,
                        quote: "old".to_owned(),
                    },
                    text: "Why drop it?".to_owned(),
                    reply: None,
                },
                RoundComment {
                    n: 2,
                    place: region(0, 100, 200, 300, 50),
                    text: "Too tight.".to_owned(),
                    reply: None,
                },
            ],
        }
    );

    // The agent delivers again; the old delivery, opened by its id, shows
    // the same round with the replies, as the redelivery does.
    let attempt = world.attempt.borrow().clone();
    let again = world.redeliver(
        "shots",
        &attempt,
        "Settings screen, again",
        world.shown_demos(),
        Some(&delivery_id),
        json!([{"comment": 2, "text": "Loosened."}, {"text": "Both done."}]),
    );
    world.mobile.refresh(2_000).expect("refresh");
    let old = world.mobile.delivery(delivery_id).expect("old");
    assert!(old.superseded);
    let answered = old.sent_back.expect("still there");
    assert_eq!(answered.reply.as_deref(), Some("Both done."));
    assert_eq!(answered.comments[0].reply, None);
    assert_eq!(answered.comments[1].reply.as_deref(), Some("Loosened."));
    assert_eq!(
        ReviewRound {
            reply: None,
            comments: answered
                .comments
                .iter()
                .cloned()
                .map(|comment| RoundComment {
                    reply: None,
                    ..comment
                })
                .collect(),
            ..answered.clone()
        },
        round,
        "only the replies are new"
    );

    let new = world.mobile.delivery(again.clone()).expect("new");
    assert_eq!(new.answers, Some(answered));
    assert_eq!(new.sent_back, None, "the redelivery was not sent back");
    world
        .mobile
        .review(again.clone(), new.revision, Verdict::Accept)
        .expect("accept");
    let accepted = world.mobile.delivery(again).expect("accepted");
    assert_eq!(accepted.review, ReviewState::Accepted);
    assert_eq!(accepted.sent_back, None, "an accepted delivery has none");
}

#[test]
fn a_line_comment_waits_for_its_demo_file_and_an_area_comment_does_not() {
    let (world, delivery_id) = World::with_delivery();
    // The sync app has downloaded neither the transcript nor the image.
    evict(&world, "txt", b"$ cargo test\r\nok\n");
    evict(&world, "png", b"png bytes");
    let read = world
        .mobile
        .delivery(delivery_id.clone())
        .expect("delivery");
    let Demo::CliTranscript { transcript } = &read.demos[1] else {
        panic!("{:?}", read.demos[1]);
    };
    assert_eq!(transcript.state, DemoText::NotOnDevice);

    // The line cannot be quoted: refused whole, the area comment with it.
    let result = world.mobile.review(
        delivery_id.clone(),
        read.revision.clone(),
        commented(vec![
            on(region(0, 0, 0, 500, 500), "Too dark."),
            on(transcript_line(1, 2), "Which tests?"),
        ]),
    );
    match &result {
        Err(FfiError::SendBackRefused {
            detail,
            comment,
            message,
        }) => {
            assert_eq!(*detail, SendBackDetail::DemoNotOnDevice);
            assert_eq!(*comment, Some(2));
            assert_eq!(
                message,
                "Comment 2: the demo it points into is not on this device yet, so its line \
                 cannot be quoted. Nothing was written; try again when the file has downloaded."
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(world.mobile_lines().is_empty(), "nothing was written");

    // An area needs no quote, so it does not need the image either.
    world
        .mobile
        .review(
            delivery_id.clone(),
            read.revision,
            commented(vec![on(region(0, 0, 0, 500, 500), "Too dark.")]),
        )
        .expect("an area comment");
    assert_eq!(
        world.mobile_lines()[0]["body"]["comments"],
        json!([{"anchor": {"kind": "region", "demo": 0, "x": 0, "y": 0, "w": 500, "h": 500}, "text": "Too dark."}])
    );
    assert_eq!(world.mac_reads(&delivery_id).0, ReviewStatus::SentBack);
}

//! One command, one append to the writer file.
//!
//! A command that writes several events hands them to the writer as one
//! buffer of whole lines, so the segment grows once. The tests count the write
//! calls (not the lines), compare the bytes with what one append per event
//! writes, and cut a batch in the middle to show what a reader and the next
//! append make of it. The reader of the last test is another OS process: this
//! same test binary in reader mode (see `reader_process`).

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agendar_core::model::{
    Agent, DecisionCard, DeliveredOutcome, Event, EventId, EventPayload, Harness, HostId, Mode,
    NodePath, OptionId, PlanFeature, ProjectDeclared, ProjectSlug, Refs, ReviewVerdict, RuleId,
    RuleSet, SessionId, Sha256, SourcePath, UtcTime, WriterId, WriterRole, parse_event,
};
use agendar_core::runtime::LocalPaths;
use agendar_core::runtime::sessions::process_identity;
use agendar_core::runtime::steward::{SessionRegistration, register_session};
use agendar_core::service::Service;
use agendar_core::vault::{
    AppendResult, Clock, EventDraft, IdSource, MAX_BATCH_BYTES, MAX_LINE_BYTES, Vault, WriterKey,
    append_event_reserved, append_events_reserved, read_complete_lines, segment_write_calls,
    writer_journal_path, writer_relative_path,
};
use serde_json::json;

const HOST: &str = "h-00000000000000000000000001";
const SESSION: &str = "00000000000000000000000101";
const PROJECT: &str = "example";
const FIXED_TS: &str = "2026-01-01T00:00:00.000Z";

fn project() -> ProjectSlug {
    ProjectSlug::parse(PROJECT).expect("project")
}

fn session() -> SessionId {
    SESSION.parse().expect("session")
}

fn agent_writer() -> WriterId {
    WriterId::new(
        HostId::parse(HOST).expect("host"),
        WriterRole::Agent(session()),
    )
}

fn key() -> WriterKey {
    WriterKey {
        project: project(),
        writer: agent_writer(),
    }
}

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> UtcTime {
        UtcTime::parse(FIXED_TS).expect("timestamp")
    }
}

struct FixedIds(u64);

impl IdSource for FixedIds {
    fn next_event_id(&mut self) -> EventId {
        let value = self.0;
        self.0 += 1;
        EventId::parse(&format!("{value:026}")).expect("event id")
    }
}

struct Scratch {
    _temp: tempfile::TempDir,
    vault: Vault,
    local: LocalPaths,
    root: PathBuf,
}

fn scratch() -> Scratch {
    let temp = tempfile::tempdir().expect("temporary directory");
    let local = LocalPaths::new(temp.path().join("data")).expect("local paths");
    let vault =
        Vault::init(&temp.path().join("vault"), &FixedClock, &mut FixedIds(1)).expect("vault init");
    let root = vault.root().to_path_buf();
    Scratch {
        _temp: temp,
        vault,
        local,
        root,
    }
}

fn pulse(message: &str, cites: Option<&EventId>) -> EventDraft {
    EventDraft {
        refs: Refs {
            causes: cites.into_iter().cloned().collect(),
            ..Refs::default()
        },
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

/// An event line of about 11 KiB.
fn big(index: usize) -> EventDraft {
    let depends_on: Vec<String> = (0..64)
        .map(|n| {
            format!(
                "core/{}_{index}/{}_{n}/{}_{n}",
                "a".repeat(50),
                "b".repeat(50),
                "c".repeat(50)
            )
        })
        .collect();
    EventDraft {
        refs: Refs {
            node: Some(node("core/big")),
            ..Refs::default()
        },
        payload: EventPayload::PlanFeature(
            serde_json::from_value(json!({
                "agent": {"session": SESSION, "harness": "generic"},
                "title": "Big feature",
                "depends_on": depends_on,
                "slice": "core",
                "demo_kind": "cli_transcript"
            }))
            .expect("feature"),
        ),
    }
}

/// The three events of one imagined command: the second cites the first.
fn append_batch(scratch: &Scratch, ids: &mut FixedIds) -> agendar_core::Result<Vec<AppendResult>> {
    append_events_reserved(
        &scratch.vault,
        &scratch.local,
        &key(),
        pulse("first", None),
        None,
        &|first| {
            vec![
                (pulse("second", Some(first)), None),
                (pulse("third", None), None),
            ]
        },
        &FixedClock,
        ids,
    )
}

fn append_one(scratch: &Scratch, ids: &mut FixedIds, draft: EventDraft) -> AppendResult {
    append_event_reserved(
        &scratch.vault,
        &scratch.local,
        &key(),
        draft,
        None,
        &FixedClock,
        ids,
    )
    .expect("append")
}

fn segment(scratch: &Scratch, number: u32) -> PathBuf {
    scratch
        .root
        .join(writer_relative_path(&project(), &agent_writer(), number).expect("relative path"))
}

fn source() -> SourcePath {
    SourcePath::from_relative_path(
        &writer_relative_path(&project(), &agent_writer(), 1).expect("relative path"),
    )
    .expect("source")
}

fn events_of(bytes: &[u8]) -> Vec<Event> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .map(|line| parse_event(line, &source()).expect("a whole event line"))
        .collect()
}

/// The write calls `run` makes on this thread.
fn writes<T>(run: impl FnOnce() -> T) -> (T, u64) {
    let before = segment_write_calls();
    let value = run();
    (value, segment_write_calls() - before)
}

/// What one append per event writes: the bytes the batch must equal.
fn old_path_bytes() -> Vec<u8> {
    let scratch = scratch();
    let mut ids = FixedIds(100);
    let ((), calls) = writes(|| {
        let first = append_one(&scratch, &mut ids, pulse("first", None));
        append_one(&scratch, &mut ids, pulse("second", Some(&first.event.id)));
        append_one(&scratch, &mut ids, pulse("third", None));
    });
    assert_eq!(calls, 3, "one append per event is three write calls");
    fs::read(segment(&scratch, 1)).expect("segment")
}

#[test]
fn a_batch_is_one_write_call_and_the_bytes_of_one_append_per_event() {
    let expected = old_path_bytes();
    let scratch = scratch();
    let (results, calls) = writes(|| append_batch(&scratch, &mut FixedIds(100)).expect("batch"));
    assert_eq!(
        calls, 1,
        "the three events reach the file in one write call"
    );
    let bytes = fs::read(segment(&scratch, 1)).expect("segment");
    assert_eq!(bytes, expected, "the same bytes as one append per event");

    let events = events_of(&bytes);
    assert_eq!(
        results
            .iter()
            .map(|result| &result.event)
            .collect::<Vec<_>>(),
        events.iter().collect::<Vec<_>>(),
        "one result per draft, in order"
    );
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(events[1].refs.causes, vec![events[0].id.clone()]);
    let mut end = 0;
    for (result, line) in results
        .iter()
        .zip(bytes.split_inclusive(|byte| *byte == b'\n'))
    {
        end += line.len() as u64;
        assert_eq!(result.byte_end, end, "each event ends at its own line");
    }

    // The next append continues the stream in the same file.
    let (next, calls) = writes(|| append_one(&scratch, &mut FixedIds(200), pulse("after", None)));
    assert_eq!(calls, 1);
    assert_eq!(next.event.seq, 4);
    assert!(
        fs::read(segment(&scratch, 1))
            .expect("segment")
            .starts_with(&bytes)
    );
}

/// The state a process leaves when it dies inside the batch's write call: the
/// pending record and its bytes are durable, the segment holds `cut` bytes.
fn torn_batch(batch: &[u8], cut: usize) -> Scratch {
    let scratch = scratch();
    scratch
        .local
        .ensure_layout(&scratch.root, scratch.vault.id())
        .expect("layout");
    let target = segment(&scratch, 1);
    fs::create_dir_all(target.parent().expect("host directory")).expect("host directory");
    fs::write(&target, &batch[..cut]).expect("torn segment");
    let journal = writer_journal_path(&scratch.local, &scratch.vault, &key());
    fs::write(
        &journal,
        serde_json::to_vec(&json!({
            "v": 1,
            "project": PROJECT,
            "writer": agent_writer().to_string(),
            "next_seq": 1,
            "pending": {
                "path": writer_relative_path(&project(), &agent_writer(), 1).expect("path"),
                "offset": 0,
                "bytes": batch.len(),
                "digest": Sha256::from_bytes(batch).as_str(),
            },
        }))
        .expect("journal"),
    )
    .expect("journal");
    fs::write(journal.with_extension("pending"), batch).expect("pending bytes");
    scratch
}

/// A cut inside the second line: one whole line, then half a line.
fn middle_of_second_line(batch: &[u8]) -> (usize, usize) {
    let first = batch.iter().position(|byte| *byte == b'\n').expect("LF") + 1;
    (first, first + 40)
}

#[test]
fn a_reader_takes_the_whole_lines_of_a_torn_batch_and_stops_at_the_torn_one() {
    let batch = old_path_bytes();
    let (first, cut) = middle_of_second_line(&batch);
    let scratch = torn_batch(&batch, cut);

    let read = read_complete_lines(&segment(&scratch, 1), 0, MAX_BATCH_BYTES).expect("read");
    assert_eq!(read.lines.len(), 1, "only the whole line is an event");
    assert_eq!(read.lines[0].bytes, &batch[..first]);
    assert_eq!(events_of(&read.lines[0].bytes).len(), 1);
    assert!(read.trailing_partial, "the cut line is an unfinished tail");
    assert_eq!(
        read.next_offset, first as u64,
        "the offset never passes the unfinished line"
    );
}

#[test]
fn the_next_append_completes_a_torn_batch_without_truncating_it() {
    let batch = old_path_bytes();
    let (_, cut) = middle_of_second_line(&batch);
    let scratch = torn_batch(&batch, cut);

    let (next, calls) = writes(|| append_one(&scratch, &mut FixedIds(200), pulse("after", None)));
    assert_eq!(
        calls, 2,
        "one write completes the batch, one appends the new event"
    );
    assert_eq!(
        next.event.seq, 4,
        "the batch keeps its three sequence numbers"
    );
    let bytes = fs::read(segment(&scratch, 1)).expect("segment");
    assert_eq!(
        &bytes[..batch.len()],
        batch,
        "the batch is whole and in place"
    );
    let events = events_of(&bytes);
    assert_eq!(events.len(), 4);
    assert_eq!(events[3], next.event);
    assert!(
        !writer_journal_path(&scratch.local, &scratch.vault, &key())
            .with_extension("pending")
            .exists(),
        "the pending bytes are dropped once the batch is durable"
    );
}

#[test]
fn a_retry_of_a_torn_batch_returns_its_events_and_writes_nothing_twice() {
    let batch = old_path_bytes();
    let (_, cut) = middle_of_second_line(&batch);
    let scratch = torn_batch(&batch, cut);

    let (results, calls) = writes(|| append_batch(&scratch, &mut FixedIds(300)).expect("retry"));
    assert_eq!(calls, 1, "only the missing suffix is written");
    assert_eq!(fs::read(segment(&scratch, 1)).expect("segment"), batch);
    assert_eq!(
        results
            .into_iter()
            .map(|result| result.event)
            .collect::<Vec<_>>(),
        events_of(&batch),
        "the retry gets the original events"
    );
}

#[test]
fn a_torn_batch_without_its_pending_record_is_an_abandoned_tail() {
    let batch = old_path_bytes();
    let (first, cut) = middle_of_second_line(&batch);
    let scratch = scratch();
    let target = segment(&scratch, 1);
    fs::create_dir_all(target.parent().expect("host directory")).expect("host directory");
    fs::write(&target, &batch[..cut]).expect("torn segment");

    let next = append_one(&scratch, &mut FixedIds(200), pulse("after", None));
    assert_eq!(next.event.seq, 2, "the whole line of the batch counts");
    assert!(next.relative_path.ends_with(".2.jsonl"));
    assert_eq!(
        fs::read(&target).expect("segment"),
        &batch[..cut],
        "the torn tail is never truncated or completed by guessing"
    );
    let read = read_complete_lines(&target, 0, MAX_BATCH_BYTES).expect("read");
    assert_eq!(read.next_offset, first as u64);
    assert!(read.trailing_partial);
}

#[test]
fn a_reserved_event_already_written_is_returned_and_the_rest_is_one_write() {
    let scratch = scratch();
    let reserved = EventId::parse(&format!("{:026}", 500)).expect("id");
    let first = append_event_reserved(
        &scratch.vault,
        &scratch.local,
        &key(),
        pulse("first", None),
        Some(reserved.clone()),
        &FixedClock,
        &mut FixedIds(100),
    )
    .expect("first");
    let (results, calls) = writes(|| {
        append_events_reserved(
            &scratch.vault,
            &scratch.local,
            &key(),
            pulse("first", None),
            Some(reserved.clone()),
            &|first| vec![(pulse("second", Some(first)), None)],
            &FixedClock,
            &mut FixedIds(100),
        )
        .expect("retry with a follower")
    });
    assert_eq!(calls, 1);
    assert_eq!(
        results[0].event, first.event,
        "the original event, not a copy"
    );
    assert_eq!(results[1].event.seq, 2);
    assert_eq!(results[1].event.refs.causes, vec![reserved]);
    assert_eq!(
        events_of(&fs::read(segment(&scratch, 1)).expect("segment")).len(),
        2
    );
}

#[test]
fn a_pending_record_never_exceeds_one_line_so_a_larger_batch_takes_more_writes() {
    let scratch = scratch();
    let (results, calls) = writes(|| {
        append_events_reserved(
            &scratch.vault,
            &scratch.local,
            &key(),
            big(0),
            None,
            &|_| (1..12).map(|index| (big(index), None)).collect(),
            &FixedClock,
            &mut FixedIds(100),
        )
        .expect("large batch")
    });
    let bytes = fs::read(segment(&scratch, 1)).expect("segment");
    assert!(bytes.len() > MAX_LINE_BYTES);
    // Greedy: a write takes whole lines while they fit one line's limit.
    let (mut expected, mut filled) = (1_u64, 0_usize);
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if filled > 0 && filled + line.len() > MAX_LINE_BYTES {
            expected += 1;
            filled = 0;
        }
        filled += line.len();
    }
    assert!(expected > 1 && expected < 12);
    assert_eq!(calls, expected, "as few writes as fit the pending record");
    let events = events_of(&bytes);
    assert_eq!(events.len(), 12);
    for (index, (event, result)) in events.iter().zip(&results).enumerate() {
        assert_eq!(event.seq, index as u64 + 1);
        assert_eq!(event, &result.event, "one result per draft, in order");
    }
}

// --- the commands that write more than one event ---------------------------

struct Board {
    _root: tempfile::TempDir,
    service: Service,
    vault: PathBuf,
    session: SessionId,
}

fn node(name: &str) -> NodePath {
    NodePath::parse(name).expect("node")
}

fn card() -> DecisionCard {
    serde_json::from_value(support::card()).expect("decision card")
}

/// A project with the steps `x`, `y` and `z` and one registered agent.
fn board() -> Board {
    let root = tempfile::tempdir().expect("temporary root");
    let vault = root.path().join("vault");
    let repo = root.path().join("repo");
    fs::create_dir_all(&repo).expect("repo");
    let (service, _) = Service::init(&vault, &root.path().join("data")).expect("init");
    let pid = std::process::id();
    let session = register_session(
        &service,
        SessionRegistration {
            project: project(),
            harness: Harness::Generic,
            harness_session_id: "one-append".to_owned(),
            pid,
            process_start: process_identity(pid).expect("process identity"),
            repo_root: repo,
        },
    )
    .expect("register");
    let patch: ProjectDeclared = serde_json::from_value(json!({
        "title": "One append",
        "brief": "Several events, one write.",
        "goal": "Count the write calls."
    }))
    .expect("project patch");
    service
        .declare_project(&project(), &session, patch, None)
        .expect("declare project");
    for name in ["x", "y", "z"] {
        let feature: PlanFeature = serde_json::from_value(json!({
            "title": name,
            "slice": "core",
            "demo_kind": "cli_transcript"
        }))
        .expect("feature patch");
        service
            .declare_feature(&project(), &session, node(name), feature, None)
            .expect("declare feature");
    }
    Board {
        _root: root,
        service,
        vault,
        session,
    }
}

impl Board {
    /// The events of one writer file of this host, in file order.
    fn file(&self, name: &str) -> Vec<Event> {
        let relative = format!(
            "projects/{PROJECT}/{}/{name}.jsonl",
            self.service.host().as_str()
        );
        let source = SourcePath::from_relative_path(&relative).expect("source");
        fs::read(self.vault.join(&relative))
            .unwrap_or_default()
            .split_inclusive(|byte| *byte == b'\n')
            .map(|line| parse_event(line, &source).expect("event line"))
            .collect()
    }

    fn agent_file(&self) -> Vec<Event> {
        self.file(&format!("s-{}", self.session.as_str()))
    }
}

#[test]
fn an_assumed_ask_writes_the_ask_and_its_choice_in_one_append() {
    let board = board();
    let before = board.agent_file().len();
    let (result, calls) = writes(|| {
        board
            .service
            .ask(
                &project(),
                &board.session,
                &node("x"),
                card(),
                Mode::Assume,
                None,
            )
            .expect("assume-routed ask")
    });
    assert_eq!(calls, 1, "the ask and its choice are one write call");
    let events = board.agent_file();
    assert_eq!(events.len(), before + 2);
    let (ask, choice) = (&events[before], &events[before + 1]);
    assert!(matches!(ask.payload, EventPayload::DecisionAsk(_)));
    assert!(matches!(choice.payload, EventPayload::DecisionAssumed(_)));
    assert_eq!(ask.id, result.ask_id);
    assert_eq!(Some(&choice.id), result.receipt_id.as_ref());
    assert_eq!(choice.refs.ask.as_ref(), Some(&ask.id));
    assert_eq!(choice.seq, ask.seq + 1);
}

#[test]
fn an_assumed_ask_with_a_request_id_is_one_append_and_a_retry_writes_nothing() {
    let board = board();
    let before = board.agent_file().len();
    let ask = || {
        board
            .service
            .ask(
                &project(),
                &board.session,
                &node("x"),
                card(),
                Mode::Assume,
                Some("ask-1"),
            )
            .expect("assume-routed ask")
    };
    let (first, calls) = writes(ask);
    assert_eq!(calls, 1);
    let (again, calls) = writes(ask);
    assert_eq!(calls, 0, "the stored result answers the retry");
    assert_eq!(again.ask_id, first.ask_id);
    assert_eq!(again.receipt_id, first.receipt_id);
    assert_eq!(board.agent_file().len(), before + 2);
}

#[test]
fn a_plain_ask_is_one_event_and_one_append() {
    let board = board();
    let before = board.agent_file().len();
    let (result, calls) = writes(|| {
        board
            .service
            .ask(
                &project(),
                &board.session,
                &node("x"),
                card(),
                Mode::Ask,
                None,
            )
            .expect("ask")
    });
    assert_eq!(calls, 1);
    assert_eq!(result.receipt_id, None);
    assert_eq!(board.agent_file().len(), before + 1);
}

#[test]
fn a_send_back_that_saves_a_rule_writes_both_in_one_append() {
    let board = board();
    let delivery = EventId::parse(&format!("{:026}", 900)).expect("id");
    let ((review, rule), calls) = writes(|| {
        board
            .service
            .review_delivery(
                &project(),
                &node("x"),
                &delivery,
                ReviewVerdict::SendBack,
                Some("Not yet".to_owned()),
                Some(RuleSet {
                    rule_id: RuleId::parse("check-bytes").expect("rule"),
                    text: "Compare bytes before delivery".to_owned(),
                    enabled: true,
                }),
                Vec::new(),
                Vec::new(),
            )
            .expect("review")
    });
    assert_eq!(calls, 1, "the review and its rule are one write call");
    let events = board.file("operator");
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0].payload, EventPayload::ReviewResult(_)));
    assert!(matches!(events[1].payload, EventPayload::RuleSet(_)));
    assert_eq!(events[0].id, review);
    assert_eq!(Some(&events[1].id), rule.as_ref());
    assert_eq!(
        events[1].refs.causes,
        vec![review],
        "the rule cites the review"
    );
}

#[test]
fn dropping_several_steps_is_one_append() {
    let board = board();
    let nodes = [node("x"), node("y"), node("z")];
    let (dropped, calls) = writes(|| {
        board
            .service
            .drop_steps(&project(), &nodes, Some("Not needed"))
            .expect("drop")
    });
    assert_eq!(calls, 1, "three drops are one write call");
    let events = board.file("operator");
    assert_eq!(
        events.iter().map(|event| &event.id).collect::<Vec<_>>(),
        dropped.iter().collect::<Vec<_>>()
    );
    for (event, node) in events.iter().zip(&nodes) {
        assert!(matches!(event.payload, EventPayload::PlanDropped(_)));
        assert_eq!(event.refs.node.as_ref(), Some(node), "in the order given");
    }
}

#[test]
fn the_delivered_records_of_one_inbox_read_are_one_append() {
    let board = board();
    let option = OptionId::parse("a").expect("option");
    for name in ["x", "y"] {
        let ask = board
            .service
            .ask(
                &project(),
                &board.session,
                &node(name),
                card(),
                Mode::Ask,
                None,
            )
            .expect("ask")
            .ask_id;
        let (_, calls) = writes(|| {
            board
                .service
                .answer_decision(&project(), &ask, &option, None, None, None)
                .expect("answer")
        });
        assert_eq!(calls, 1, "an answer is one event: its receipt is derived");
    }
    let (packets, _, _) = board
        .service
        .inbox(&project(), &board.session, None, false)
        .expect("inbox");
    assert_eq!(packets.len(), 2, "two answers wait in the inbox");
    let before = board.file("steward").len();
    // A packet listed twice is still recorded once.
    let listed = [packets.clone(), vec![packets[0].clone()]].concat();
    let (recorded, calls) = writes(|| {
        board
            .service
            .mark_delivered(
                &project(),
                &board.session,
                &listed,
                DeliveredOutcome::InboxRead,
            )
            .expect("record the inbox read")
    });
    assert_eq!(calls, 1, "both delivered records are one write call");
    let events = board.file("steward");
    assert_eq!(events.len(), before + 2);
    for ((event, id), packet) in events[before..].iter().zip(&recorded).zip(&packets) {
        assert_eq!(&event.id, id);
        let EventPayload::Delivered(delivered) = &event.payload else {
            panic!("a delivered record");
        };
        assert_eq!(delivered.packet_id, packet.id, "in packet order");
    }

    // A second read of the same packets records nothing again.
    let (again, calls) = writes(|| {
        board
            .service
            .mark_delivered(
                &project(),
                &board.session,
                &packets,
                DeliveredOutcome::InboxRead,
            )
            .expect("repeat")
    });
    assert_eq!((again.len(), calls), (0, 0));
}

// --- a reader in another process --------------------------------------------

const READER_ENV: &str = "AGENDAR_ONE_APPEND_READER";

/// Reader mode: until the stop file appears, read the segment from the start
/// and require that everything taken as events is whole lines of consecutive
/// events, and that the offset never passes an unfinished line.
#[test]
fn reader_process() {
    let Ok(raw) = std::env::var(READER_ENV) else {
        return;
    };
    let (path, stop) = raw.split_once('\n').expect("segment and stop paths");
    let (path, stop) = (Path::new(path), Path::new(stop));
    let mut reads = 0_u64;
    let mut most = 0_usize;
    loop {
        let stopping = stop.exists();
        if path.exists() {
            let mut offset = 0_u64;
            let mut seen = 0_usize;
            loop {
                let read = read_complete_lines(path, offset, MAX_BATCH_BYTES).expect("read");
                for line in &read.lines {
                    assert_eq!(line.bytes.last(), Some(&b'\n'));
                    let event = parse_event(&line.bytes, &source()).expect("a whole event");
                    seen += 1;
                    assert_eq!(event.seq, seen as u64, "events arrive whole and in order");
                }
                if read.lines.is_empty() || read.next_offset >= read.observed_size {
                    break;
                }
                offset = read.next_offset;
            }
            assert!(seen >= most, "a reader never loses an event it has seen");
            most = seen;
            reads += 1;
        }
        if stopping {
            break;
        }
    }
    println!("reader_process: {reads} reads, {most} events");
}

#[test]
fn a_reader_in_another_process_only_ever_sees_whole_events_of_a_batch() {
    let scratch = scratch();
    let stop = scratch.root.join("stop-reading");
    let home = scratch.root.join("reader-home");
    fs::create_dir_all(&home).expect("reader home");
    let reader = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("reader_process")
        .arg("--nocapture")
        .env(
            READER_ENV,
            format!("{}\n{}", segment(&scratch, 1).display(), stop.display()),
        )
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("reader process");

    // Large lines make a batch many pages long, so a read can land inside it.
    let mut ids = FixedIds(100);
    let mut written = 0_usize;
    let deadline = Instant::now() + Duration::from_secs(20);
    while written < 60 && Instant::now() < deadline {
        let ((), calls) = writes(|| {
            append_events_reserved(
                &scratch.vault,
                &scratch.local,
                &key(),
                big(written),
                None,
                &|_| {
                    vec![
                        (big(written + 1), None),
                        (big(written + 2), None),
                        (big(written + 3), None),
                    ]
                },
                &FixedClock,
                &mut ids,
            )
            .expect("batch");
        });
        assert_eq!(calls, 1);
        written += 4;
    }
    fs::write(&stop, b"").expect("stop file");
    let output = reader.wait_with_output().expect("reader exit");
    assert!(
        output.status.success(),
        "the reader saw something that was not a whole event:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(
        said.contains(&format!("{written} events")),
        "the reader's last read holds every event: {said}"
    );
}

/// The commands that write one event were one append already: nothing to batch.
#[test]
fn a_start_a_delivery_and_a_report_are_one_event_and_one_append_each() {
    let board = board();
    let before = board.agent_file().len();
    let (started, calls) = writes(|| {
        board
            .service
            .progress_started(
                &project(),
                &board.session,
                &node("x"),
                agendar_core::model::EtaRange {
                    min_s: agendar_core::model::Seconds::new(600).expect("min"),
                    max_s: agendar_core::model::Seconds::new(1_800).expect("max"),
                },
                None,
                None,
            )
            .expect("start")
    });
    assert_eq!(calls, 1, "progress started");
    let attempt = EventId::parse(started["attempt"].as_str().expect("attempt")).expect("id");

    let body: agendar_core::model::Delivery = serde_json::from_value(json!({
        "agent": {"session": board.session.as_str(), "harness": "generic"},
        "name": "x shipped",
        "demos": [{"kind": "cli_transcript", "body": {"blob": support::blob()}}],
        "criteria": [],
        "disclosure": {
            "proven": ["x runs"],
            "not_proven_yet": ["Windows"],
            "faked_for_now": []
        }
    }))
    .expect("delivery body");
    let (_, calls) = writes(|| {
        board
            .service
            .deliver(
                &project(),
                &board.session,
                &node("x"),
                &attempt,
                None,
                None,
                body,
                None,
            )
            .expect("deliver")
    });
    assert_eq!(calls, 1, "deliver");

    let report = agendar_core::service::ReportDraft {
        title: "What x does".to_owned(),
        summary: "It runs.".to_owned(),
        report: serde_json::from_value(support::blob()).expect("blob"),
        kind: None,
        domain: None,
    };
    let (_, calls) = writes(|| {
        board
            .service
            .publish_report(&project(), &board.session, Some(&node("x")), report, None)
            .expect("report")
    });
    assert_eq!(calls, 1, "report");
    assert_eq!(board.agent_file().len(), before + 3);
}

// --- a retried command never writes its follower twice ----------------------

impl Board {
    fn writer_key(&self, role: WriterRole) -> WriterKey {
        WriterKey {
            project: project(),
            writer: WriterId::new(self.service.host().clone(), role),
        }
    }

    /// Put the last `lines` lines of a writer file back into its pending
    /// record: the state of a process that died after the write reached the
    /// segment and before the pending record was cleared.
    fn pending_not_cleared(&self, key: &WriterKey, lines: usize) {
        let relative = writer_relative_path(&key.project, &key.writer, 1).expect("path");
        let bytes = fs::read(self.vault.join(&relative)).expect("segment");
        let offset: usize = bytes
            .split_inclusive(|byte| *byte == b'\n')
            .rev()
            .skip(lines)
            .map(<[u8]>::len)
            .sum();
        let batch = &bytes[offset..];
        let path = writer_journal_path(self.service.local(), self.service.vault(), key);
        let mut journal: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("journal")).expect("journal");
        journal["pending"] = json!({
            "path": relative,
            "offset": offset,
            "bytes": batch.len(),
            "digest": Sha256::from_bytes(batch).as_str(),
        });
        fs::write(&path, serde_json::to_vec(&journal).expect("journal")).expect("journal");
        fs::write(path.with_extension("pending"), batch).expect("pending bytes");
    }

    /// Take a finished request back to reserved: the state of a process that
    /// died after its append and before it marked the operation committed.
    fn not_committed(&self, request: &str) {
        use agendar_core::runtime::sessions::{OperationEntry, OperationStatus, update_runtime};
        let key = format!("{}/{request}", self.session);
        update_runtime(self.service.local(), self.service.vault(), |runtime| {
            let Some(OperationEntry::Known(record)) = runtime.operations.get_mut(&key) else {
                panic!("the operation record");
            };
            assert_eq!(record.status, OperationStatus::Committed);
            record.status = OperationStatus::Reserved;
            record.result = None;
            Ok(())
        })
        .expect("runtime");
    }

    fn start(&self, name: &str) {
        self.service
            .progress_started(
                &project(),
                &self.session,
                &node(name),
                agendar_core::model::EtaRange {
                    min_s: agendar_core::model::Seconds::new(600).expect("min"),
                    max_s: agendar_core::model::Seconds::new(1_800).expect("max"),
                },
                None,
                None,
            )
            .expect("start");
    }
}

/// An assumed or auto ask on a step in progress, retried with its request id
/// after a crash. The retry reads the choice already in the file, so the
/// evaluation it rebuilds differs from the one written (one more speculative
/// feature, a longer demo gap): the original events must come back and nothing
/// may be written.
fn a_retried_ask_writes_nothing(mode: Mode, pending_cleared: bool) {
    let board = board();
    board.start("x");
    let ask = || {
        board
            .service
            .ask(
                &project(),
                &board.session,
                &node("x"),
                card(),
                mode,
                Some("ask-1"),
            )
            .expect("ask")
    };
    let first = ask();
    assert_eq!(first.effective_mode, mode);
    let written = board.agent_file();
    let (asked, chosen) = (&written[written.len() - 2], &written[written.len() - 1]);
    assert_eq!(
        (&asked.id, Some(&chosen.id)),
        (&first.ask_id, first.receipt_id.as_ref())
    );

    board.not_committed("ask-1");
    if !pending_cleared {
        board.pending_not_cleared(
            &board.writer_key(WriterRole::Agent(board.session.clone())),
            2,
        );
    }
    // The demo clock of the step in progress counts whole seconds: an auto
    // choice is not speculative, so only the gap makes its retry differ.
    std::thread::sleep(Duration::from_millis(1_100));
    let (again, calls) = writes(ask);
    assert_eq!(calls, 0, "the retry writes nothing");
    assert_eq!(
        board.agent_file(),
        written,
        "one ask and one choice in the file"
    );
    assert_eq!(again.ask_id, first.ask_id, "the original ask");
    assert_eq!(again.receipt_id, first.receipt_id, "the original choice");
    assert_ne!(
        again.policy.budget, first.policy.budget,
        "the retry's own evaluation counted the choice already written"
    );
}

#[test]
fn a_retried_assumed_ask_whose_pending_record_was_not_cleared_writes_nothing() {
    a_retried_ask_writes_nothing(Mode::Assume, false);
}

#[test]
fn a_retried_assumed_ask_that_was_not_marked_committed_writes_nothing() {
    a_retried_ask_writes_nothing(Mode::Assume, true);
}

#[test]
fn a_retried_auto_ask_whose_pending_record_was_not_cleared_writes_nothing() {
    a_retried_ask_writes_nothing(Mode::Auto, false);
}

#[test]
fn a_retried_auto_ask_that_was_not_marked_committed_writes_nothing() {
    a_retried_ask_writes_nothing(Mode::Auto, true);
}

/// A review has no request id, so only the pending record can tell a retry
/// from a new review: while it is not cleared, the same review and rule come
/// back as the original events.
#[test]
fn a_retried_send_back_with_a_rule_whose_pending_record_was_not_cleared_writes_nothing() {
    let board = board();
    let delivery = EventId::parse(&format!("{:026}", 900)).expect("id");
    let review = || {
        board
            .service
            .review_delivery(
                &project(),
                &node("x"),
                &delivery,
                ReviewVerdict::SendBack,
                Some("Not yet".to_owned()),
                Some(RuleSet {
                    rule_id: RuleId::parse("check-bytes").expect("rule"),
                    text: "Compare bytes before delivery".to_owned(),
                    enabled: true,
                }),
                Vec::new(),
                Vec::new(),
            )
            .expect("review")
    };
    let first = review();
    let written = board.file("operator");
    board.pending_not_cleared(&board.writer_key(WriterRole::Operator), 2);
    let (again, calls) = writes(review);
    assert_eq!(calls, 0);
    assert_eq!(again, first, "the original review and rule");
    assert_eq!(board.file("operator"), written);
}

/// The record of a one-event command keeps the shape every version reads; only
/// a command with followers names them, each with its reserved ID.
#[test]
fn an_operation_record_names_its_followers_only_when_it_has_any() {
    use agendar_core::runtime::sessions::{OperationEntry, update_runtime};
    let board = board();
    for (mode, request) in [(Mode::Ask, "plain"), (Mode::Assume, "assumed")] {
        board
            .service
            .ask(
                &project(),
                &board.session,
                &node("x"),
                card(),
                mode,
                Some(request),
            )
            .expect("ask");
    }
    let (records, _) = update_runtime(board.service.local(), board.service.vault(), |runtime| {
        Ok(["plain", "assumed"].map(|request| {
            let key = format!("{}/{request}", board.session);
            let Some(OperationEntry::Known(record)) = runtime.operations.get(&key) else {
                panic!("the operation record");
            };
            (**record).clone()
        }))
    })
    .expect("runtime");
    let [plain, assumed] = records;
    let stored = serde_json::to_value(&plain).expect("record");
    assert!(
        stored.get("following").is_none(),
        "one event: the old shape"
    );

    let events = board.agent_file();
    let choice = events.last().expect("the choice");
    assert_eq!(assumed.following.len(), 1);
    assert_eq!(assumed.following[0].reserved_event, choice.id);
    assert_eq!(assumed.following[0].draft.payload, choice.payload);
    assert_eq!(assumed.following[0].draft.refs.ask, assumed.reserved_event);
}

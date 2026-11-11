use std::collections::BTreeMap;
use std::sync::Mutex;

use super::*;

fn wall(seconds: i64) -> UtcTime {
    UtcTime::parse("2026-11-10T14:02:00.000Z")
        .expect("time")
        .add_seconds(seconds)
}

fn file(name: &str, len: u64, written: UtcTime) -> OwnFile {
    OwnFile {
        path: PathBuf::from(name),
        len,
        modified: SystemTime::UNIX_EPOCH + Duration::from_millis(written.millis().unsigned_abs()),
        written,
    }
}

#[derive(Default)]
struct FakeReader {
    flags: Mutex<BTreeMap<PathBuf, UploadFlag>>,
    reads: Mutex<Vec<PathBuf>>,
}

impl FakeReader {
    fn set(&self, name: &str, flag: UploadFlag) {
        self.flags
            .lock()
            .expect("flags")
            .insert(PathBuf::from(name), flag);
    }

    fn reads(&self) -> usize {
        self.reads.lock().expect("reads").len()
    }
}

impl UploadReader for FakeReader {
    fn supported(&self) -> bool {
        true
    }

    fn read(&self, file: &Path) -> UploadFlag {
        self.reads.lock().expect("reads").push(file.to_path_buf());
        self.flags
            .lock()
            .expect("flags")
            .get(file)
            .copied()
            .unwrap_or(UploadFlag::Unknown)
    }
}

/// One due read through the reader, as the host does it.
fn read(tracker: &mut Tracker, reader: &FakeReader, now: Instant, wall: UtcTime) -> bool {
    let due = tracker.take_due(now);
    if due.is_empty() {
        return false;
    }
    let results: Vec<_> = due
        .into_iter()
        .map(|path| {
            let flag = reader.read(&path);
            (path, flag)
        })
        .collect();
    tracker.apply(&results, now, wall)
}

/// A tracker that has seen `a` unchanged, so the next size is a write.
fn seeded(start: Instant) -> Tracker {
    let mut tracker = Tracker::default();
    tracker.changed(vec![file("a", 1, wall(-3600))], start, wall(0));
    assert_eq!(tracker.next_due(), None);
    tracker
}

#[test]
fn the_schedule_doubles_to_thirty_seconds_and_stops_after_ten_minutes() {
    let start = Instant::now();
    let reader = FakeReader::default();
    reader.set("a", UploadFlag::Waiting);
    let mut tracker = seeded(start);
    tracker.changed(vec![file("a", 2, wall(0))], start, wall(0));

    let mut at = Vec::new();
    while let Some(due) = tracker.next_due() {
        let offset = due.duration_since(start);
        if offset > GIVE_UP {
            break;
        }
        at.push(offset.as_secs());
        read(
            &mut tracker,
            &reader,
            due,
            wall(i64::try_from(offset.as_secs()).expect("offset")),
        );
    }
    assert_eq!(&at[..7], &[0, 2, 6, 14, 30, 60, 90]);
    assert_eq!(at.last(), Some(&600));
    assert_eq!(at.len(), 24);
    assert!(at.windows(2).all(|pair| pair[1] - pair[0] <= 30));
    // Given up: the stuck line, and only one slow look from here on.
    assert_eq!(tracker.mark(), &UploadMark::Late { since: wall(0) });
    assert_eq!(
        tracker.next_due(),
        Some(start + GIVE_UP + STUCK_RECHECK),
        "after ten minutes the doubling re-reads stop"
    );
}

#[test]
fn a_write_shows_waiting_then_late_then_nothing_once_uploaded() {
    let start = Instant::now();
    let reader = FakeReader::default();
    reader.set("a", UploadFlag::Waiting);
    let mut tracker = seeded(start);
    tracker.changed(vec![file("a", 2, wall(0))], start, wall(0));
    // Nothing shows before the first read says so.
    assert_eq!(tracker.mark(), &UploadMark::Clear);

    // Not uploaded, but a normal upload takes seconds: quiet for 15 s.
    assert!(!read(&mut tracker, &reader, start, wall(0)));
    assert_eq!(tracker.mark(), &UploadMark::Clear);
    assert_eq!(tracker.next_remark(wall(0)), Some(Duration::from_secs(15)));
    assert!(!tracker.remark(wall(14)));
    assert!(tracker.remark(wall(15)), "time alone shows the line");
    assert_eq!(tracker.mark(), &UploadMark::Waiting { since: wall(0) });
    assert_eq!(
        tracker.next_remark(wall(15)),
        Some(Duration::from_secs(105))
    );
    assert_eq!(
        tracker.mark().view(),
        Some(json!({"state": "waiting", "since": "2026-11-10T14:02:00.000Z"}))
    );

    // Not due yet: no read.
    assert!(!read(
        &mut tracker,
        &reader,
        start + Duration::from_secs(1),
        wall(1)
    ));
    assert_eq!(reader.reads(), 1);

    let two_minutes = start + Duration::from_secs(120);
    assert!(read(&mut tracker, &reader, two_minutes, wall(120)));
    assert_eq!(tracker.mark(), &UploadMark::Late { since: wall(0) });

    reader.set("a", UploadFlag::Uploaded);
    let later = start + Duration::from_secs(150);
    assert!(read(&mut tracker, &reader, later, wall(150)));
    assert_eq!(tracker.mark(), &UploadMark::Clear);
    assert_eq!(tracker.mark().view(), None);
    // Everything is uploaded: no further work at all.
    assert_eq!(tracker.next_due(), None);
    let reads = reader.reads();
    assert!(!read(
        &mut tracker,
        &reader,
        start + Duration::from_secs(3600),
        wall(3600)
    ));
    assert_eq!(reader.reads(), reads);
}

#[test]
fn a_folder_outside_icloud_shows_nothing_and_is_read_once_per_write() {
    let start = Instant::now();
    let reader = FakeReader::default();
    reader.set("a", UploadFlag::Unknown);
    let mut tracker = seeded(start);
    tracker.changed(vec![file("a", 2, wall(0))], start, wall(0));
    assert!(!read(&mut tracker, &reader, start, wall(0)));
    assert_eq!(tracker.mark(), &UploadMark::Clear);
    assert_eq!(tracker.next_due(), None);
    assert_eq!(reader.reads(), 1);
    assert_eq!(NoReader.read(Path::new("a")), UploadFlag::Unknown);
    assert!(!NoReader.supported());
}

#[test]
fn an_unchanged_listing_and_other_files_cause_no_read() {
    let start = Instant::now();
    let reader = FakeReader::default();
    let mut tracker = seeded(start);
    // The change signal fired for another host's write: own files are as they were.
    tracker.changed(vec![file("a", 1, wall(-3600))], start, wall(0));
    assert_eq!(tracker.next_due(), None);
    assert!(!read(&mut tracker, &reader, start, wall(0)));
    assert_eq!(reader.reads(), 0);
}

#[test]
fn a_burst_of_writes_joins_the_schedule_instead_of_a_read_per_write() {
    let start = Instant::now();
    let reader = FakeReader::default();
    reader.set("a", UploadFlag::Waiting);
    let mut tracker = seeded(start);
    // One append every 2 s for a minute.
    for step in 0..30_u64 {
        let now = start + Duration::from_secs(step * 2);
        let seconds = i64::try_from(step * 2).expect("seconds");
        tracker.changed(vec![file("a", 2 + step, wall(seconds))], now, wall(seconds));
        read(&mut tracker, &reader, now, wall(seconds));
    }
    // 0, 2, 6, 14, 30 s: five reads, not thirty.
    assert_eq!(reader.reads(), 5);
    // Two quiet minutes are counted from the latest write: not late yet.
    assert_eq!(tracker.mark(), &UploadMark::Waiting { since: wall(0) });
}

#[test]
fn only_the_files_still_waiting_are_read_again() {
    let start = Instant::now();
    let reader = FakeReader::default();
    reader.set("a", UploadFlag::Uploaded);
    reader.set("b", UploadFlag::Waiting);
    let mut tracker = Tracker::default();
    tracker.changed(
        vec![file("a", 1, wall(-3600)), file("b", 1, wall(-3600))],
        start,
        wall(0),
    );
    tracker.changed(
        vec![file("a", 2, wall(0)), file("b", 2, wall(1))],
        start,
        wall(1),
    );
    read(&mut tracker, &reader, start, wall(1));
    read(&mut tracker, &reader, start + FIRST_DELAY, wall(3));
    read(&mut tracker, &reader, start + FIRST_DELAY * 3, wall(7));
    let reads = reader.reads.lock().expect("reads").clone();
    assert_eq!(
        reads,
        ["a", "b", "a", "b", "b"].map(PathBuf::from),
        "the uploaded file is not read again"
    );
    tracker.remark(wall(20));
    assert_eq!(tracker.mark(), &UploadMark::Waiting { since: wall(1) });
}

#[test]
fn a_file_written_again_and_again_turns_late_ten_minutes_after_its_first_write() {
    let start = Instant::now();
    let reader = FakeReader::default();
    reader.set("a", UploadFlag::Waiting);
    let mut tracker = seeded(start);
    // One append every 20 s: never two quiet minutes.
    for step in 0..=30_u64 {
        let now = start + Duration::from_secs(step * 20);
        let seconds = i64::try_from(step * 20).expect("seconds");
        tracker.changed(vec![file("a", 2 + step, wall(seconds))], now, wall(seconds));
        read(&mut tracker, &reader, now, wall(seconds));
        tracker.remark(wall(seconds));
        let expected = match seconds {
            0 => UploadMark::Clear,
            20..=580 => UploadMark::Waiting { since: wall(0) },
            _ => UploadMark::Late { since: wall(0) },
        };
        assert_eq!(tracker.mark(), &expected, "at {seconds} s");
    }
}

#[test]
fn an_upload_that_ends_within_fifteen_seconds_never_shows() {
    let start = Instant::now();
    let reader = FakeReader::default();
    reader.set("a", UploadFlag::Waiting);
    let mut tracker = seeded(start);
    tracker.changed(vec![file("a", 2, wall(0))], start, wall(0));
    let mut moved = false;
    for seconds in [0_u64, 2] {
        let wall = wall(i64::try_from(seconds).expect("seconds"));
        moved |= read(
            &mut tracker,
            &reader,
            start + Duration::from_secs(seconds),
            wall,
        );
    }
    reader.set("a", UploadFlag::Uploaded);
    moved |= read(
        &mut tracker,
        &reader,
        start + Duration::from_secs(6),
        wall(6),
    );
    assert!(!moved, "the line never changed");
    assert_eq!(tracker.next_due(), None);
    assert_eq!(tracker.next_remark(wall(6)), None);
    assert!(!tracker.remark(wall(60)));
}

/// Measured on iCloud Drive: straight after an append to an uploaded file
/// the flag still reads uploaded.
#[test]
fn uploaded_right_after_an_append_is_not_believed_until_the_first_delay() {
    let start = Instant::now();
    let reader = FakeReader::default();
    reader.set("a", UploadFlag::Uploaded);
    let mut tracker = seeded(start);
    tracker.changed(vec![file("a", 2, wall(0))], start, wall(0));
    assert!(!read(&mut tracker, &reader, start, wall(0)));
    // No line yet, and the file is asked again.
    assert_eq!(tracker.mark(), &UploadMark::Clear);
    assert_eq!(tracker.next_due(), Some(start + FIRST_DELAY));

    reader.set("a", UploadFlag::Waiting);
    read(&mut tracker, &reader, start + FIRST_DELAY, wall(2));

    // A second append while waiting: a stale "uploaded" does not hide the
    // line, which still counts from the first write.
    let again = start + Duration::from_secs(6);
    tracker.changed(vec![file("a", 3, wall(6))], again, wall(6));
    reader.set("a", UploadFlag::Uploaded);
    read(&mut tracker, &reader, again, wall(6));
    assert!(tracker.remark(wall(16)));
    assert_eq!(tracker.mark(), &UploadMark::Waiting { since: wall(0) });
    // Believed once the delay has passed.
    let settled = start + Duration::from_secs(20);
    assert!(read(&mut tracker, &reader, settled, wall(20)));
    assert_eq!(tracker.mark(), &UploadMark::Clear);
    assert_eq!(tracker.next_due(), None);
}

#[test]
fn launch_looks_only_at_files_written_in_the_last_ten_minutes() {
    let start = Instant::now();
    let reader = FakeReader::default();
    reader.set("new", UploadFlag::Waiting);
    reader.set("old", UploadFlag::Waiting);
    let mut tracker = Tracker::default();
    tracker.changed(
        vec![file("old", 1, wall(-3600)), file("new", 1, wall(-60))],
        start,
        wall(0),
    );
    read(&mut tracker, &reader, start, wall(0));
    assert_eq!(*reader.reads.lock().expect("reads"), [PathBuf::from("new")]);
    assert_eq!(tracker.mark(), &UploadMark::Waiting { since: wall(-60) });
}

#[test]
fn a_new_write_after_giving_up_starts_the_schedule_again() {
    let start = Instant::now();
    let reader = FakeReader::default();
    reader.set("a", UploadFlag::Waiting);
    let mut tracker = seeded(start);
    tracker.changed(vec![file("a", 2, wall(0))], start, wall(0));
    while let Some(due) = tracker.next_due() {
        if due.duration_since(start) > GIVE_UP {
            break;
        }
        read(&mut tracker, &reader, due, wall(0));
    }
    let now = start + Duration::from_secs(700);
    tracker.changed(vec![file("a", 3, wall(700))], now, wall(700));
    assert_eq!(tracker.next_due(), Some(now));
    read(&mut tracker, &reader, now, wall(700));
    // Still the first write that never uploaded: late, since then.
    assert_eq!(tracker.mark(), &UploadMark::Late { since: wall(0) });
    assert_eq!(tracker.next_due(), Some(now + FIRST_DELAY));
}

const HOST: &str = "h-00000000000000000000000004";
const OTHER: &str = "h-00000000000000000000000005";

/// A Vault folder with one writer file and one attachment for this host and
/// the same for another host.
fn folder() -> (tempfile::TempDir, HostId, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = HostId::parse(HOST).expect("host");
    let mine = dir.path().join("projects/demo").join(HOST);
    let other = dir.path().join("projects/demo").join(OTHER);
    for folder in [&mine, &other] {
        std::fs::create_dir_all(folder.join("blobs")).expect("folder");
        std::fs::write(folder.join("operator.jsonl"), b"{}\n").expect("operator");
        std::fs::write(folder.join("blobs/aa.png"), b"image").expect("blob");
    }
    (dir, host, mine, other)
}

#[test]
fn the_listing_is_this_hosts_writer_files_and_attachments_only() {
    let (dir, host, mine, _other) = folder();
    // Neither a writer file nor an attachment.
    std::fs::write(mine.join("notes.txt"), b"x").expect("notes");
    std::fs::write(mine.join("blobs/.aa.png.icloud"), b"x").expect("hidden");
    std::fs::create_dir_all(mine.join("blobs/nested")).expect("nested");

    let mut files = Disk.all(dir.path(), &host, wall(0));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let paths: Vec<_> = files.iter().map(|file| file.path.clone()).collect();
    assert_eq!(
        paths,
        [mine.join("blobs/aa.png"), mine.join("operator.jsonl")]
    );
    assert_eq!(files[0].len, 5);
    assert_eq!(files[1].len, 3);
    // Written just now on the host's clock.
    assert!((wall(0).millis() - files[1].written.millis()).abs() < 60_000);

    assert_eq!(Disk.project(dir.path(), "demo", &host, wall(0)).len(), 2);
    assert!(
        Disk.project(dir.path(), "absent", &host, wall(0))
            .is_empty()
    );
    assert_eq!(
        Disk.one(&mine.join("blobs/aa.png"), wall(0))
            .map(|file| file.len),
        Some(5)
    );
    assert_eq!(Disk.one(&mine.join("blobs/gone.png"), wall(0)), None);
    assert_eq!(Disk.one(&mine.join("blobs"), wall(0)), None);
}

#[test]
fn a_signal_concerns_only_the_own_files_it_names() {
    let root = Path::new("/vault");
    let host = HostId::parse(HOST).expect("host");
    let mine = root.join("projects/demo").join(HOST);
    let other = root.join("projects/demo").join(OTHER);
    let of = |paths: &[PathBuf]| concern(root, &host, paths);

    // Another host's writer file and attachment, and the folders above.
    let nothing = of(&[
        other.join("operator.jsonl"),
        other.join("blobs/aa.png"),
        other.join("blobs"),
        other.clone(),
        root.join("projects/demo"),
        root.join("projects"),
        root.to_path_buf(),
        root.join("vault.json"),
        mine.join("notes.txt"),
        mine.join("blobs/.aa.png.icloud"),
        mine.join("blobs/nested/aa.png"),
    ]);
    assert_eq!(nothing, Concern::default());
    assert!(nothing.is_empty());

    let own = of(&[
        mine.join("operator.jsonl"),
        mine.join("blobs/aa.png"),
        mine.join("operator.jsonl"),
    ]);
    assert_eq!(
        own.files.iter().cloned().collect::<Vec<_>>(),
        [mine.join("blobs/aa.png"), mine.join("operator.jsonl")]
    );
    assert!(own.projects.is_empty() && !own.unclear);

    // The own folder itself was named: that one project is listed.
    let folders = of(&[mine.clone(), mine.join("blobs")]);
    assert!(folders.files.is_empty());
    assert_eq!(
        folders.projects.iter().collect::<Vec<_>>(),
        [&"demo".to_owned()]
    );

    // Not under the root as this host spells it: the safety net's business.
    let outside = of(&[PathBuf::from("/elsewhere/projects/demo").join(HOST)]);
    assert!(outside.is_empty() && outside.unclear);
}

#[test]
fn a_named_file_joins_the_wait_and_leaves_the_rest_as_it_was() {
    let start = Instant::now();
    let reader = FakeReader::default();
    reader.set("a", UploadFlag::Waiting);
    reader.set("blob", UploadFlag::Waiting);
    let mut tracker = seeded(start);

    // An unchanged named file is no write.
    tracker.touched(vec![file("a", 1, wall(-3600))], &[], start);
    assert_eq!(tracker.next_due(), None);

    // A new attachment: it was never listed, so it is a write.
    tracker.touched(vec![file("blob", 9, wall(0))], &[], start);
    assert!(!read(&mut tracker, &reader, start, wall(0)));
    assert_eq!(
        *reader.reads.lock().expect("reads"),
        [PathBuf::from("blob")]
    );
    assert!(tracker.remark(wall(15)));
    assert_eq!(tracker.mark(), &UploadMark::Waiting { since: wall(0) });

    // The writer file is appended to: both wait, the line counts from the
    // older one, and two quiet minutes turn it late.
    let later = start + Duration::from_secs(20);
    tracker.touched(vec![file("a", 2, wall(20))], &[], later);
    read(&mut tracker, &reader, later, wall(20));
    assert_eq!(tracker.mark(), &UploadMark::Waiting { since: wall(0) });
    assert!(tracker.remark(wall(120)));
    assert_eq!(tracker.mark(), &UploadMark::Late { since: wall(0) });

    // The attachment is gone from the folder: only the writer file is left.
    tracker.touched(Vec::new(), &[PathBuf::from("blob")], later);
    assert!(tracker.remark(wall(121)));
    assert_eq!(tracker.mark(), &UploadMark::Waiting { since: wall(20) });

    // A later full listing with the same files finds nothing new.
    let reads = reader.reads();
    let listing = start + Duration::from_secs(21);
    tracker.changed(vec![file("a", 2, wall(20))], listing, wall(21));
    assert!(!read(&mut tracker, &reader, listing, wall(21)));
    assert_eq!(reader.reads(), reads);
}

/// Manual check, never part of a normal run: set `AGENDAR_UPLOAD_PROBE_DIR`
/// to a scratch folder inside the sync folder, spelled as a user's config
/// would spell it. Runs the real watcher on a Vault there, writes an own
/// writer file and an own attachment, and prints how the platform names them.
#[test]
#[ignore = "needs a scratch folder inside iCloud Drive"]
fn probe_how_the_watcher_spells_paths_in_a_sync_folder() {
    use agendar_core::runtime::VaultWatch;
    use agendar_core::vault::Vault;
    use agendar_core::vault::writer::{SystemClock, UlidIdSource};

    let Some(dir) = std::env::var_os("AGENDAR_UPLOAD_PROBE_DIR") else {
        return;
    };
    let configured = PathBuf::from(dir);
    let vault = Vault::init(&configured, &SystemClock, &mut UlidIdSource).expect("vault");
    let root = vault.root().to_path_buf();
    println!("configured root: {}", configured.display());
    println!("app's root:      {}", root.display());
    let host = HostId::parse(HOST).expect("host");
    let mine = root.join("projects/demo").join(HOST);
    std::fs::create_dir_all(mine.join("blobs")).expect("folder");
    let watch = VaultWatch::start(&vault).expect("watch");
    let writer = mine.join("operator.jsonl");
    let blob = mine.join("blobs/aa.png");
    std::fs::write(&writer, b"{}\n").expect("writer");
    std::fs::write(&blob, b"image").expect("blob");

    let mut named = BTreeSet::new();
    let mut unclear = false;
    let until = Instant::now() + Duration::from_secs(15);
    while Instant::now() < until {
        let (_, changes) = watch.wait_changes(Duration::from_secs(1));
        unclear |= changes.unclear;
        named.extend(changes.paths);
    }
    for path in &named {
        println!("named: {}", path.display());
    }
    let found = concern(&root, &host, &named);
    println!("unclear signal: {unclear}; concern: {found:?}");
    assert!(found.files.contains(&writer), "the writer file is matched");
    assert!(found.files.contains(&blob), "the attachment is matched");
    assert!(!found.unclear, "every named path is under the app's root");
}

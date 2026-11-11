//! The demos of one delivery as the desktop shows them: a screenshot by its
//! file, a transcript by its first lines, a diff by the hunks the agent
//! picked. Text is read from the verified demo file and bounded; a file that
//! is not on this device yet says so and still names itself, so a mobile client
//! can ask the sync app for it.
#![forbid(unsafe_code)]

use agendar_core::ErrorCode;
use agendar_core::model::{BlobRef, Demo as CoreDemo, DiffDemo as CoreDiff, ProjectSlug};
use agendar_core::vault::{Vault, blob_relative_path, read_blob};

use super::shared::count;
use super::{
    Attachment, Demo, DemoText, DiffDemo, DiffFile, DiffHunk, DiffLine, DiffLineKind, Transcript,
};

/// The largest demo text file a call reads into its rows.
pub const TEXT_MAX_BYTES: u64 = 1_048_576;
/// The most lines of one transcript, and of one diff hunk, a call carries.
pub const TEXT_MAX_LINES: usize = 400;
/// The most characters of one line a call carries.
pub const LINE_MAX_CHARS: usize = 2_000;

pub(super) fn demo(vault: &Vault, project: &ProjectSlug, demo: &CoreDemo) -> Demo {
    let file = |blob: &BlobRef| attachment(project, blob);
    match demo {
        CoreDemo::Screenshot(body) => Demo::Screenshot {
            file: file(&body.blob),
            alt: body.alt.clone(),
        },
        CoreDemo::CliTranscript(body) => {
            let (state, text) = text_of(vault, project, &body.blob);
            let lines = lines_of(&text);
            Demo::CliTranscript {
                transcript: Transcript {
                    file: file(&body.blob),
                    state,
                    text: lines
                        .iter()
                        .take(TEXT_MAX_LINES)
                        .map(|line| clip(line))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    lines_total: count(lines.len()),
                    truncated: lines.len() > TEXT_MAX_LINES
                        || lines
                            .iter()
                            .take(TEXT_MAX_LINES)
                            .any(|line| line.chars().count() > LINE_MAX_CHARS),
                },
            }
        }
        CoreDemo::PreviewUrl(body) => Demo::PreviewUrl {
            url: body.url.clone(),
        },
        CoreDemo::Recording(body) => Demo::Recording {
            file: file(&body.blob),
            poster: file(&body.poster),
        },
        CoreDemo::Diff(body) => Demo::Diff {
            diff: diff(vault, project, body),
        },
    }
}

fn attachment(project: &ProjectSlug, blob: &BlobRef) -> Attachment {
    Attachment {
        path: blob_relative_path(project, &blob.host, &blob.sha256, blob.ext),
        sha256: blob.sha256.as_str().to_owned(),
        bytes: blob.bytes,
        ext: blob.ext.as_str().to_owned(),
    }
}

/// The text of a demo file, read only when the whole file is here and its
/// bytes are the ones the delivery names (`read_blob` checks both).
fn text_of(vault: &Vault, project: &ProjectSlug, blob: &BlobRef) -> (DemoText, String) {
    if blob.bytes > TEXT_MAX_BYTES {
        return (DemoText::TooLarge, String::new());
    }
    match read_blob(vault, project, blob) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => (DemoText::Loaded, text),
            Err(_) => (DemoText::Unreadable, String::new()),
        },
        // Missing, or still downloading: pending media, never an error.
        Err(error) if error.code() == ErrorCode::NotFound => (DemoText::NotOnDevice, String::new()),
        Err(_) => (DemoText::Unreadable, String::new()),
    }
}

/// Lines as the desktop numbers them (1-based): a final newline starts no
/// line, and a carriage return before the newline is dropped.
fn lines_of(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

fn clip(line: &str) -> String {
    line.chars().take(LINE_MAX_CHARS).collect()
}

fn diff(vault: &Vault, project: &ProjectSlug, body: &CoreDiff) -> DiffDemo {
    let (state, text) = text_of(vault, project, &body.blob);
    let mut parsed = parse_hunks(&text).into_iter();
    DiffDemo {
        file: attachment(project, &body.blob),
        state,
        commit: body.commit.clone(),
        files_total: body.files_total,
        hunks_total: body.hunks_total,
        added_total: body.added_total,
        removed_total: body.removed_total,
        // Hunk `i` of the excerpt belongs to `shown[i]`, as in the desktop.
        hunks: body
            .shown
            .iter()
            .map(|shown| {
                let (header, lines) = parsed.next().unzip();
                let lines: Vec<DiffLine> = lines.unwrap_or_default();
                DiffHunk {
                    path: shown.path.clone(),
                    hunk: shown.hunk,
                    why: shown.why.clone(),
                    header,
                    lines_total: count(lines.len()),
                    lines: lines.into_iter().take(TEXT_MAX_LINES).collect(),
                }
            })
            .collect(),
        omitted: body
            .omitted
            .iter()
            .map(|file| DiffFile {
                path: file.path.clone(),
                added: file.added,
                removed: file.removed,
            })
            .collect(),
    }
}

/// Every hunk of a diff excerpt in order, with old and new line numbers from
/// its `@@` header (`parseHunks` in `app/src/state/diff.ts`; the numbers are
/// the ones a send-back comment anchors to). File header lines are dropped.
fn parse_hunks(text: &str) -> Vec<(String, Vec<DiffLine>)> {
    let mut hunks: Vec<(String, Vec<DiffLine>)> = Vec::new();
    let mut open = false;
    let (mut old, mut new) = (0_u32, 0_u32);
    for line in lines_of(text) {
        if line.starts_with("diff --git ") {
            open = false;
            continue;
        }
        if line.starts_with("@@ ") {
            (old, new) = hunk_start(line);
            hunks.push((line.to_owned(), Vec::new()));
            open = true;
            continue;
        }
        let Some((_, lines)) = hunks.last_mut().filter(|_| open) else {
            continue;
        };
        let mut chars = line.chars();
        let marker = chars.next();
        let body = clip(chars.as_str());
        let step = |counter: &mut u32| {
            *counter += 1;
            Some(*counter - 1)
        };
        lines.push(match marker {
            Some('+') => DiffLine {
                kind: DiffLineKind::Add,
                text: body,
                old_line: None,
                new_line: step(&mut new),
            },
            Some('-') => DiffLine {
                kind: DiffLineKind::Del,
                text: body,
                old_line: step(&mut old),
                new_line: None,
            },
            Some('\\') => DiffLine {
                kind: DiffLineKind::Note,
                text: clip(line),
                old_line: None,
                new_line: None,
            },
            _ => DiffLine {
                kind: DiffLineKind::Context,
                text: body,
                old_line: step(&mut old),
                new_line: step(&mut new),
            },
        });
    }
    hunks
}

/// `@@ -a,b +c,d @@` gives `(a, c)`; `(0, 0)` when the header does not parse.
fn hunk_start(header: &str) -> (u32, u32) {
    let mut parts = header.split(' ').skip(1);
    let number = |part: Option<&str>, sign: char| -> Option<u32> {
        part?.strip_prefix(sign)?.split(',').next()?.parse().ok()
    };
    let old = number(parts.next(), '-');
    let new = number(parts.next(), '+');
    old.zip(new).unwrap_or((0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXCERPT: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -10,3 +10,3 @@ fn x()\n ctx\n-old\n+new\n\\ No newline at end of file\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -40,2 +41,2 @@\r\n keep\r\n+more\n";

    #[test]
    fn hunks_are_numbered_from_their_headers_like_the_desktop() {
        let hunks = parse_hunks(EXCERPT);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].0, "@@ -10,3 +10,3 @@ fn x()");
        let rows: Vec<_> = hunks[0]
            .1
            .iter()
            .map(|line| (line.kind, line.text.as_str(), line.old_line, line.new_line))
            .collect();
        assert_eq!(
            rows,
            [
                (DiffLineKind::Context, "ctx", Some(10), Some(10)),
                (DiffLineKind::Del, "old", Some(11), None),
                (DiffLineKind::Add, "new", None, Some(11)),
                (
                    DiffLineKind::Note,
                    "\\ No newline at end of file",
                    None,
                    None
                ),
            ]
        );
        // File headers between hunks are dropped; a CR before the LF too.
        assert_eq!(hunks[1].0, "@@ -40,2 +41,2 @@");
        assert_eq!(hunks[1].1[1].text, "more");
        assert_eq!(hunks[1].1[1].new_line, Some(42));
    }

    #[test]
    fn transcript_lines_are_the_desktops() {
        assert_eq!(lines_of("a\r\nb\n"), ["a", "b"]);
        assert_eq!(lines_of("a\n\nb"), ["a", "", "b"]);
        assert!(lines_of("").is_empty());
        assert_eq!(clip(&"x".repeat(LINE_MAX_CHARS + 5)).len(), LINE_MAX_CHARS);
    }
}

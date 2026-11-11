//! Anchored send-back comments, checked against the delivery they point into.
//!
//! Every host that writes a review calls [`resolve_comments`] before the
//! write: it checks each comment's place against the delivery's demos and
//! replaces each line quote with the line's real text, read from the demo
//! file. A quote sent by a frontend is never trusted. Hosts map an
//! [`AnchorError`] to their own errors.

use crate::error::AgendarError;
use crate::model::{
    BlobRef, Demo, DiffSide, MAX_REVIEW_QUOTE, ProjectSlug, ReviewAnchor, ReviewComment,
};
use crate::vault::{Vault, read_blob};

/// Why a comment's place does not fit the delivery.
#[derive(Debug)]
pub enum AnchorDenial {
    /// The delivery has no demo at the comment's index.
    NoDemo,
    /// The comment's kind is not the demo's (a region on a diff, a line on a
    /// screenshot, any comment on a preview or a recording).
    KindMismatch,
    /// The line is not in the diff excerpt.
    LineNotInDiff,
    /// The line is not in the transcript.
    LineNotInTranscript,
    /// The demo file is not UTF-8 text.
    NotText,
    /// The demo file could not be read. `ErrorCode::NotFound` means it is not
    /// on this device (missing, or not whole yet).
    Blob(AgendarError),
}

/// One refused comment: which one, and why.
#[derive(Debug)]
pub struct AnchorError {
    /// 0-based index of the comment in the review.
    pub index: usize,
    pub denial: AnchorDenial,
}

impl AnchorError {
    /// JSON pointer of the offending field in the review request.
    pub fn field(&self) -> String {
        let anchor = format!("/comments/{}/anchor", self.index);
        match self.denial {
            AnchorDenial::NoDemo => format!("{anchor}/demo"),
            AnchorDenial::KindMismatch => format!("{anchor}/kind"),
            AnchorDenial::LineNotInDiff | AnchorDenial::LineNotInTranscript => {
                format!("{anchor}/line")
            }
            AnchorDenial::NotText | AnchorDenial::Blob(_) => anchor,
        }
    }

    /// Stable machine code of the denial.
    pub fn detail(&self) -> &str {
        match &self.denial {
            AnchorDenial::NoDemo => "no_such_demo",
            AnchorDenial::KindMismatch => "anchor_kind_mismatch",
            AnchorDenial::LineNotInDiff => "line_not_in_diff",
            AnchorDenial::LineNotInTranscript => "line_not_in_transcript",
            AnchorDenial::NotText => "demo_not_text",
            AnchorDenial::Blob(error) => error.detail().unwrap_or("blob_unreadable"),
        }
    }

    /// One sentence for the owner.
    pub fn message(&self) -> &str {
        match &self.denial {
            AnchorDenial::NoDemo => "This delivery has no demo at that index.",
            AnchorDenial::KindMismatch => "The comment's kind does not match that demo.",
            AnchorDenial::LineNotInDiff => "That line is not in the diff excerpt.",
            AnchorDenial::LineNotInTranscript => "That line is not in the transcript.",
            AnchorDenial::NotText => "The demo text is not valid UTF-8.",
            AnchorDenial::Blob(error) => error.message(),
        }
    }
}

/// Check every comment against the delivery's demos and replace each line
/// quote with the line's real text.
pub fn resolve_comments(
    vault: &Vault,
    project: &ProjectSlug,
    demos: &[Demo],
    comments: &mut [ReviewComment],
) -> Result<(), AnchorError> {
    for (index, comment) in comments.iter_mut().enumerate() {
        let denied = |denial| AnchorError { index, denial };
        let Some(demo) = demos.get(usize::from(comment.anchor.demo())) else {
            return Err(denied(AnchorDenial::NoDemo));
        };
        match (&mut comment.anchor, demo) {
            (
                ReviewAnchor::DiffLine {
                    file,
                    side,
                    line,
                    quote,
                    ..
                },
                Demo::Diff(body),
            ) => {
                let text = blob_text(vault, project, &body.blob).map_err(denied)?;
                let paths: Vec<&str> = body.shown.iter().map(|item| item.path.as_str()).collect();
                let Some(found) = excerpt_line(&text, &paths, file, *side, *line) else {
                    return Err(denied(AnchorDenial::LineNotInDiff));
                };
                *quote = clip_quote(&found);
            }
            (ReviewAnchor::TranscriptLine { line, quote, .. }, Demo::CliTranscript(body)) => {
                let text = blob_text(vault, project, &body.blob).map_err(denied)?;
                let Some(found) = transcript_line(&text, *line) else {
                    return Err(denied(AnchorDenial::LineNotInTranscript));
                };
                *quote = clip_quote(&found);
            }
            (ReviewAnchor::Region { .. }, Demo::Screenshot(_)) => {}
            _ => return Err(denied(AnchorDenial::KindMismatch)),
        }
    }
    Ok(())
}

fn blob_text(vault: &Vault, project: &ProjectSlug, blob: &BlobRef) -> Result<String, AnchorDenial> {
    let bytes = read_blob(vault, project, blob).map_err(AnchorDenial::Blob)?;
    String::from_utf8(bytes).map_err(|_| AnchorDenial::NotText)
}

/// Line `line` (1-based) of a transcript, split the way the app renders it.
pub fn transcript_line(text: &str, line: u32) -> Option<String> {
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
        .get(index)
        .map(|found| found.strip_suffix('\r').unwrap_or(found).to_owned())
}

/// The text of `line` on `side` of `file` in a diff excerpt. Hunk `i` of the
/// excerpt belongs to `paths[i]` (the demo's `shown` order), as in the app.
pub fn excerpt_line(
    text: &str,
    paths: &[&str],
    file: &str,
    side: DiffSide,
    line: u32,
) -> Option<String> {
    let mut hunks = 0usize;
    let mut active = false;
    let mut old = 0u32;
    let mut new = 0u32;
    // As the app does: a final newline does not start another line.
    let text = text.strip_suffix('\n').unwrap_or(text);
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.starts_with("diff --git ") {
            active = false;
            continue;
        }
        if raw.starts_with("@@ ") {
            hunks += 1;
            active = true;
            (old, new) = hunk_start(raw);
            continue;
        }
        if !active {
            continue;
        }
        let in_file = paths.get(hunks - 1).is_some_and(|path| *path == file);
        let mut chars = raw.chars();
        let marker = chars.next();
        let body = chars.as_str();
        let (old_no, new_no) = match marker {
            Some('+') => {
                new += 1;
                (None, Some(new - 1))
            }
            Some('-') => {
                old += 1;
                (Some(old - 1), None)
            }
            Some('\\') => (None, None),
            _ => {
                old += 1;
                new += 1;
                (Some(old - 1), Some(new - 1))
            }
        };
        let number = match side {
            DiffSide::Old => old_no,
            DiffSide::New => new_no,
        };
        if in_file && number == Some(line) {
            return Some(body.to_owned());
        }
    }
    None
}

/// `@@ -a,b +c,d @@` → `(a, c)`; `(0, 0)` when the header does not parse.
fn hunk_start(header: &str) -> (u32, u32) {
    let mut parts = header.split(' ').skip(1);
    let number = |part: Option<&str>, sign: char| -> Option<u32> {
        part?.strip_prefix(sign)?.split(',').next()?.parse().ok()
    };
    let old = number(parts.next(), '-');
    let new = number(parts.next(), '+');
    match (old, new) {
        (Some(old), Some(new)) => (old, new),
        _ => (0, 0),
    }
}

/// One line of at most `MAX_REVIEW_QUOTE` characters, ANSI escape sequences
/// and other control characters dropped, cut with "…" when longer.
pub fn clip_quote(line: &str) -> String {
    let mut clean = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // CSI: parameters, then one final byte in @..~.
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        if !c.is_control() || c == '\t' {
            clean.push(c);
        }
    }
    if clean.chars().count() <= MAX_REVIEW_QUOTE {
        return clean;
    }
    let mut cut: String = clean.chars().take(MAX_REVIEW_QUOTE - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXCERPT: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -10,3 +10,3 @@ fn x()\n ctx\n-old\n+new\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -40,2 +41,2 @@\n keep\n+more\n";

    #[test]
    fn excerpt_lines_follow_the_hunk_headers() {
        let paths = ["a.rs", "b.rs"];
        assert_eq!(
            excerpt_line(EXCERPT, &paths, "a.rs", DiffSide::New, 11).as_deref(),
            Some("new")
        );
        assert_eq!(
            excerpt_line(EXCERPT, &paths, "a.rs", DiffSide::Old, 11).as_deref(),
            Some("old")
        );
        assert_eq!(
            excerpt_line(EXCERPT, &paths, "b.rs", DiffSide::New, 42).as_deref(),
            Some("more")
        );
        assert_eq!(
            excerpt_line(EXCERPT, &paths, "a.rs", DiffSide::New, 42),
            None
        );
        assert_eq!(
            excerpt_line(EXCERPT, &paths, "b.rs", DiffSide::Old, 42),
            None
        );
    }

    #[test]
    fn transcript_lines_are_one_based_and_quotes_are_clipped() {
        assert_eq!(transcript_line("a\r\nb\n", 2).as_deref(), Some("b"));
        assert_eq!(transcript_line("a\nb\n", 3), None);
        assert_eq!(transcript_line("a", 0), None);
        assert_eq!(clip_quote("\u{1b}[32mok\u{1b}[0m"), "ok");
        let long = "x".repeat(300);
        let clipped = clip_quote(&long);
        assert_eq!(clipped.chars().count(), MAX_REVIEW_QUOTE);
        assert!(clipped.ends_with('…'));
    }
}

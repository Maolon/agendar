//! The records of delivery review, field for field as in `agendar.udl`.
#![forbid(unsafe_code)]

use super::ReviewState;

/// Whether the text of a transcript or a diff came with the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DemoText {
    Loaded,
    /// The file is not on this device, or not whole yet.
    NotOnDevice,
    /// Larger than the call carries.
    TooLarge,
    /// Not UTF-8 text.
    Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffLineKind {
    Add,
    Del,
    Context,
    Note,
}

/// Where a criterion's result comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provenance {
    Seen,
    AgentSays,
    NotProvenYet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckResult {
    Passed,
    Failed,
    NotProven,
}

/// The owner's verdict on a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Accept,
    /// With the owner's own words (1..=500 characters after trimming),
    /// comments pointing into the demos, or both; at least one of the two.
    SendBack {
        reason: Option<String>,
        comments: Vec<NewComment>,
    },
}

/// One comment of a send-back being written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewComment {
    pub place: CommentPlace,
    /// 1..=300 characters on one line.
    pub text: String,
}

/// Where a comment points. `quote` is the commented line's text on a comment
/// read back; on a new comment it is ignored and read from the demo file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentPlace {
    DiffLine {
        demo: u8,
        file: String,
        old_side: bool,
        line: u32,
        quote: String,
    },
    TranscriptLine {
        demo: u8,
        line: u32,
        quote: String,
    },
    /// Per-mille of the image (0..=1000).
    Region {
        demo: u8,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Demo {
    Screenshot {
        file: Attachment,
        alt: String,
    },
    CliTranscript {
        transcript: Transcript,
    },
    PreviewUrl {
        url: String,
    },
    Recording {
        file: Attachment,
        poster: Attachment,
    },
    Diff {
        diff: DiffDemo,
    },
}

/// One file of a delivery in the Vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Vault-relative.
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub ext: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub file: Attachment,
    pub state: DemoText,
    /// The first lines; empty unless `state` is `Loaded`.
    pub text: String,
    pub lines_total: u32,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

/// One hunk the agent picked, with its reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub path: String,
    pub hunk: u32,
    pub why: String,
    pub header: Option<String>,
    pub lines: Vec<DiffLine>,
    pub lines_total: u32,
}

/// A changed file none of whose hunks are shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffDemo {
    pub file: Attachment,
    pub state: DemoText,
    pub commit: String,
    pub files_total: u32,
    pub hunks_total: u32,
    pub added_total: u32,
    pub removed_total: u32,
    pub hunks: Vec<DiffHunk>,
    pub omitted: Vec<DiffFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryCriterion {
    pub id: String,
    pub text: String,
    pub provenance: Provenance,
    pub agent_result: CheckResult,
    pub seen_result: Option<CheckResult>,
    pub conflict: bool,
}

/// What the agent says about its own delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disclosure {
    pub proven: Vec<String>,
    pub not_proven_yet: Vec<String>,
    pub faked_for_now: Vec<String>,
}

/// One comment of an earlier send-back, with the agent's reply to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundComment {
    /// 1-based, in the order the owner placed them.
    pub n: u32,
    pub place: CommentPlace,
    pub text: String,
    pub reply: Option<String>,
}

/// One send-back: the one a redelivery answers (`Delivery.answers`), or the
/// one a delivery received (`Delivery.sent_back`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRound {
    pub delivery_id: String,
    pub name: String,
    pub sent_back_at: String,
    pub reason: Option<String>,
    /// The agent's reply to the reason, or its overall note.
    pub reply: Option<String>,
    pub comments: Vec<RoundComment>,
}

/// One delivery, waiting for the owner's verdict or already reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub delivery_id: String,
    pub project: String,
    pub project_title: String,
    pub node: String,
    pub step_title: String,
    pub name: String,
    /// The revision to pass back as `expected_revision`.
    pub revision: String,
    pub review: ReviewState,
    pub superseded: bool,
    pub need_id: Option<String>,
    pub since: Option<String>,
    pub asked: Option<String>,
    pub demos: Vec<Demo>,
    pub criteria: Vec<DeliveryCriterion>,
    pub disclosure: Disclosure,
    pub unblocks: Vec<String>,
    pub answers: Option<ReviewRound>,
    /// The send-back this delivery received.
    pub sent_back: Option<ReviewRound>,
}

/// What became of one verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewReceipt {
    pub review_id: String,
    pub delivery_id: String,
    pub project: String,
    pub review: ReviewState,
    pub written_at: String,
    pub writer_file: String,
    pub next_need_id: Option<String>,
}

//! Decision cards, delivery demos, criteria and evidence wire types.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::model::ids::{
    BlobRef, Confidence, CriterionId, Domain, EtaRange, EventId, OptionId, Reach, Reversibility,
    Seconds, Sha256,
};

fn exact_keys(object: &Map<String, Value>, allowed: &[&str]) -> std::result::Result<(), String> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("unknown field `{key}`"));
        }
    }
    for key in allowed {
        if !object.contains_key(*key) {
            return Err(format!("missing field `{key}`"));
        }
    }
    Ok(())
}

fn object<'a>(value: &'a Value, what: &str) -> std::result::Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{what} must be an object"))
}

fn from_value<T: serde::de::DeserializeOwned>(
    value: &Value,
    what: &str,
) -> std::result::Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|error| format!("invalid {what}: {error}"))
}

fn string<'a>(object: &'a Map<String, Value>, field: &str) -> std::result::Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{field}` must be a string"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckResult {
    Passed,
    Failed,
    NotProven,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Claim {
    AgentSays,
    Seen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Accept,
    SendBack,
}

/// Which side of a unified diff a commented line is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    Old,
    New,
}

/// Where in a delivery's demos a review comment points. Integers only: a
/// region is in per-mille of the image (0..=1000). `quote` is the exact text
/// of the commented line, captured by the app from the demo blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewAnchor {
    DiffLine {
        demo: u8,
        file: String,
        side: DiffSide,
        line: u32,
        quote: String,
    },
    TranscriptLine {
        demo: u8,
        /// 1-based.
        line: u32,
        quote: String,
    },
    Region {
        demo: u8,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
    },
}

impl ReviewAnchor {
    /// The index of the demo this anchor points into.
    pub fn demo(&self) -> u8 {
        match self {
            Self::DiffLine { demo, .. }
            | Self::TranscriptLine { demo, .. }
            | Self::Region { demo, .. } => *demo,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffLineAnchor {
    demo: u8,
    file: String,
    side: DiffSide,
    line: u32,
    quote: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TranscriptLineAnchor {
    demo: u8,
    line: u32,
    quote: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegionAnchor {
    demo: u8,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

impl<'de> Deserialize<'de> for ReviewAnchor {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let map = object(&value, "review anchor").map_err(serde::de::Error::custom)?;
        let kind = string(map, "kind").map_err(serde::de::Error::custom)?;
        let mut rest = map.clone();
        rest.remove("kind");
        let rest = Value::Object(rest);
        match kind {
            "diff_line" => {
                let body: DiffLineAnchor =
                    from_value(&rest, "diff line anchor").map_err(serde::de::Error::custom)?;
                Ok(Self::DiffLine {
                    demo: body.demo,
                    file: body.file,
                    side: body.side,
                    line: body.line,
                    quote: body.quote,
                })
            }
            "transcript_line" => {
                let body: TranscriptLineAnchor = from_value(&rest, "transcript line anchor")
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::TranscriptLine {
                    demo: body.demo,
                    line: body.line,
                    quote: body.quote,
                })
            }
            "region" => {
                let body: RegionAnchor =
                    from_value(&rest, "region anchor").map_err(serde::de::Error::custom)?;
                Ok(Self::Region {
                    demo: body.demo,
                    x: body.x,
                    y: body.y,
                    w: body.w,
                    h: body.h,
                })
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown review anchor kind `{other}`"
            ))),
        }
    }
}

/// One short human comment pinned to a place in a delivery's demos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewComment {
    pub anchor: ReviewAnchor,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Criterion {
    pub id: CriterionId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionResult {
    pub criterion: CriterionId,
    pub status: CheckResult,
    pub claim: Claim,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<BlobRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<EventId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disclosure {
    pub proven: Vec<String>,
    pub not_proven_yet: Vec<String>,
    pub faked_for_now: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionOption {
    pub id: OptionId,
    pub label: String,
    pub consequence: String,
    pub reversibility: Reversibility,
    pub external: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<EtaRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// A picture of this option (png, jpg or webp) for visual choices; the app
    /// shows the options' images side by side and at full size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<BlobRef>,
}

/// The reserved option id for an answer in the human's own words. No card may
/// declare it; only a human answer may choose it.
pub const OWN_OPTION: &str = "own";

/// The option id by which the owner lets the asking agent continue a step
/// another agent holds. It has this meaning only on a question asked by an
/// agent that is not the step's holder, after the holder's start; chosen by
/// the owner, it admits that agent's start (`reduce::plan::takeover_answers`).
pub const TAKEOVER_OPTION: &str = "takeover";

/// Longest own answer, in Unicode characters.
pub const OWN_ANSWER_MAX: usize = 600;

impl DecisionCard {
    /// The option a receipt shows for an answer in the human's own words.
    pub fn own_option(&self, text: &str) -> DecisionOption {
        DecisionOption {
            id: crate::model::ids::OptionId::parse(OWN_OPTION).expect("reserved option id parses"),
            label: "Your own answer".to_owned(),
            consequence: text.to_owned(),
            reversibility: self.reversibility,
            external: self.external,
            cost: None,
            eta: None,
            icon: Some("user".to_owned()),
            image: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recommendation {
    pub option: OptionId,
    pub confidence: Confidence,
    pub because: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub text: String,
    pub claim: Claim,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<EventId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CardEvidence {
    Bullets { items: Vec<String> },
    Table { rows: Vec<Vec<String>> },
}

impl<'de> Deserialize<'de> for CardEvidence {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let map = object(&value, "card evidence").map_err(serde::de::Error::custom)?;
        let kind = string(map, "kind").map_err(serde::de::Error::custom)?;
        match kind {
            "bullets" => {
                exact_keys(map, &["items", "kind"]).map_err(serde::de::Error::custom)?;
                let Some(items_value) = map.get("items") else {
                    return Err(serde::de::Error::custom("missing field `items`"));
                };
                let items = from_value(items_value, "card evidence items")
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::Bullets { items })
            }
            "table" => {
                exact_keys(map, &["kind", "rows"]).map_err(serde::de::Error::custom)?;
                let Some(rows_value) = map.get("rows") else {
                    return Err(serde::de::Error::custom("missing field `rows`"));
                };
                let rows = from_value(rows_value, "card evidence rows")
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::Table { rows })
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown card evidence kind `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OnTimeout {
    Hold,
    ProceedAsAssumption { after_s: Seconds },
}

impl<'de> Deserialize<'de> for OnTimeout {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let map = object(&value, "on_timeout").map_err(serde::de::Error::custom)?;
        let kind = string(map, "kind").map_err(serde::de::Error::custom)?;
        match kind {
            "hold" => {
                exact_keys(map, &["kind"]).map_err(serde::de::Error::custom)?;
                Ok(Self::Hold)
            }
            "proceed_as_assumption" => {
                exact_keys(map, &["after_s", "kind"]).map_err(serde::de::Error::custom)?;
                let Some(after_s_value) = map.get("after_s") else {
                    return Err(serde::de::Error::custom("missing field `after_s`"));
                };
                let after_s = from_value(after_s_value, "on_timeout after_s")
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::ProceedAsAssumption { after_s })
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown on_timeout kind `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionCard {
    pub why_now: String,
    pub question: String,
    pub reversibility: Reversibility,
    pub reach: Reach,
    pub external: bool,
    pub domain: Domain,
    pub options: Vec<DecisionOption>,
    pub recommended: Recommendation,
    #[serde(default)]
    pub already_checked: Vec<Fact>,
    pub done_when: Vec<Criterion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<CardEvidence>,
    pub on_timeout: OnTimeout,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<BlobRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoKind {
    Screenshot,
    CliTranscript,
    PreviewUrl,
    Recording,
    Diff,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoCheck {
    pub criterion: CriterionId,
    pub argv: Vec<String>,
    pub cwd: String,
    pub timeout_s: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotDemo {
    pub blob: BlobRef,
    pub alt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<DemoCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliTranscriptDemo {
    pub blob: BlobRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<DemoCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewUrlDemo {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingDemo {
    pub blob: BlobRef,
    pub poster: BlobRef,
}

/// One hunk the agent picked, in display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffShown {
    pub path: String,
    /// 1-based index of the hunk within that file's diff.
    pub hunk: u32,
    pub why: String,
}

/// A changed file none of whose hunks are shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffFile {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

/// The core of a commit's diff: the CLI cuts the picked hunks verbatim from
/// `git show`, so `blob` holds unified-diff text of only those hunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffDemo {
    pub blob: BlobRef,
    pub commit: String,
    pub files_total: u32,
    pub hunks_total: u32,
    pub added_total: u32,
    pub removed_total: u32,
    pub shown: Vec<DiffShown>,
    #[serde(default)]
    pub omitted: Vec<DiffFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum Demo {
    Screenshot(ScreenshotDemo),
    CliTranscript(CliTranscriptDemo),
    PreviewUrl(PreviewUrlDemo),
    Recording(RecordingDemo),
    Diff(DiffDemo),
}

impl Demo {
    /// Every Vault blob this demo references.
    pub fn blobs(&self) -> Vec<BlobRef> {
        match self {
            Self::Screenshot(value) => vec![value.blob.clone()],
            Self::CliTranscript(value) => vec![value.blob.clone()],
            Self::PreviewUrl(_) => Vec::new(),
            Self::Recording(value) => vec![value.blob.clone(), value.poster.clone()],
            Self::Diff(value) => vec![value.blob.clone()],
        }
    }
}

impl<'de> Deserialize<'de> for Demo {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let map = object(&value, "demo").map_err(serde::de::Error::custom)?;
        exact_keys(map, &["body", "kind"]).map_err(serde::de::Error::custom)?;
        let kind = string(map, "kind").map_err(serde::de::Error::custom)?;
        let Some(body) = map.get("body") else {
            return Err(serde::de::Error::custom("missing field `body`"));
        };
        match kind {
            "screenshot" => from_value(body, "screenshot demo")
                .map(Self::Screenshot)
                .map_err(serde::de::Error::custom),
            "cli_transcript" => from_value(body, "CLI transcript demo")
                .map(Self::CliTranscript)
                .map_err(serde::de::Error::custom),
            "preview_url" => from_value(body, "preview URL demo")
                .map(Self::PreviewUrl)
                .map_err(serde::de::Error::custom),
            "recording" => from_value(body, "recording demo")
                .map(Self::Recording)
                .map_err(serde::de::Error::custom),
            "diff" => from_value(body, "diff demo")
                .map(Self::Diff)
                .map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!(
                "unknown demo kind `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservationMethod {
    BlobDigest {
        sha256: Sha256,
    },
    Command {
        check_id: String,
        argv_digest: Sha256,
    },
}

impl<'de> Deserialize<'de> for ObservationMethod {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let map = object(&value, "observation method").map_err(serde::de::Error::custom)?;
        let kind = string(map, "kind").map_err(serde::de::Error::custom)?;
        match kind {
            "blob_digest" => {
                exact_keys(map, &["kind", "sha256"]).map_err(serde::de::Error::custom)?;
                let Some(sha256_value) = map.get("sha256") else {
                    return Err(serde::de::Error::custom("missing field `sha256`"));
                };
                let sha256 =
                    from_value(sha256_value, "blob digest").map_err(serde::de::Error::custom)?;
                Ok(Self::BlobDigest { sha256 })
            }
            "command" => {
                exact_keys(map, &["argv_digest", "check_id", "kind"])
                    .map_err(serde::de::Error::custom)?;
                let check_id = string(map, "check_id")
                    .map_err(serde::de::Error::custom)?
                    .to_owned();
                let argv_digest = from_value(
                    map.get("argv_digest")
                        .ok_or_else(|| "missing field `argv_digest`".to_owned())
                        .map_err(serde::de::Error::custom)?,
                    "command argv digest",
                )
                .map_err(serde::de::Error::custom)?;
                Ok(Self::Command {
                    check_id,
                    argv_digest,
                })
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown observation method `{other}`"
            ))),
        }
    }
}

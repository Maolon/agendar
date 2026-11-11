//! Validated identifiers, scalar wire primitives and shared enums.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{AgendarError, Result};

pub const MAX_BLOB_BYTES: u64 = 52_428_800;
pub const MAX_SECONDS: u32 = 2_592_000;

macro_rules! string_id {
    ($name:ident, $what:literal, $check:path) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Result<Self> {
                $check(value)
                    .map_err(|detail| AgendarError::validation(format!("{}: {detail}", $what)))?;
                Ok(Self(value.to_owned()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = AgendarError;

            fn from_str(value: &str) -> Result<Self> {
                Self::parse(value)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

fn canonical_ulid(value: &str, uppercase: bool, what: &str) -> std::result::Result<(), String> {
    if value.len() != 26 {
        return Err(format!("{what} must be 26 Crockford characters"));
    }
    let ulid = value
        .parse::<ulid::Ulid>()
        .map_err(|_| format!("{what} is not a valid ULID"))?;
    let canonical = if uppercase {
        ulid.to_string()
    } else {
        ulid.to_string().to_ascii_lowercase()
    };
    if canonical != value {
        return Err(format!("{what} is not canonical"));
    }
    Ok(())
}

fn check_event_id(value: &str) -> std::result::Result<(), String> {
    canonical_ulid(value, true, "EventId")
}

fn check_session_id(value: &str) -> std::result::Result<(), String> {
    canonical_ulid(value, true, "SessionId")
}

fn check_vault_id(value: &str) -> std::result::Result<(), String> {
    canonical_ulid(value, true, "VaultId")
}

fn check_host_id(value: &str) -> std::result::Result<(), String> {
    let Some(ulid) = value.strip_prefix("h-") else {
        return Err("HostId must start with `h-`".to_owned());
    };
    if value.len() != 28 {
        return Err("HostId must be `h-` plus 26 Crockford characters".to_owned());
    }
    canonical_ulid(ulid, false, "HostId")
}

fn check_sha256(value: &str) -> std::result::Result<(), String> {
    if value.len() != 64 {
        return Err("Sha256 must be 64 lowercase hex characters".to_owned());
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("Sha256 must be lowercase hexadecimal".to_owned());
    }
    Ok(())
}

fn check_project_slug(value: &str) -> std::result::Result<(), String> {
    if value.is_empty() || value.len() > 64 {
        return Err("ProjectSlug must be 1..=64 characters".to_owned());
    }
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return Err("ProjectSlug must not be blank".to_owned());
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err("ProjectSlug must start with a-z or 0-9".to_owned());
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("ProjectSlug allows only a-z, 0-9 and `-`".to_owned());
    }
    Ok(())
}

fn check_node_path(value: &str) -> std::result::Result<(), String> {
    if value.is_empty() || value.len() > 255 {
        return Err("NodePath must be 1..=255 bytes".to_owned());
    }
    let segments: Vec<&str> = value.split('/').collect();
    if segments.is_empty() || segments.len() > 16 {
        return Err("NodePath must have 1..=16 slash-separated segments".to_owned());
    }
    for segment in segments {
        if segment.is_empty() || segment.len() > 64 {
            return Err("NodePath segments must be 1..=64 characters".to_owned());
        }
        if segment == "." || segment == ".." {
            return Err("NodePath segments must not be `.` or `..`".to_owned());
        }
        let mut bytes = segment.bytes();
        let Some(first) = bytes.next() else {
            return Err("NodePath segments must not be blank".to_owned());
        };
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return Err("NodePath segments must start with a-z or 0-9".to_owned());
        }
        if !segment.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        }) {
            return Err("NodePath segments allow only a-z, 0-9, `_` and `-`".to_owned());
        }
    }
    Ok(())
}

fn check_member_id(value: &str, what: &str) -> std::result::Result<(), String> {
    if value.is_empty() || value.len() > 32 {
        return Err(format!("{what} must be 1..=32 characters"));
    }
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return Err(format!("{what} must not be blank"));
    };
    if !first.is_ascii_lowercase() {
        return Err(format!("{what} must start with a-z"));
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
    }) {
        return Err(format!("{what} allows only a-z, 0-9, `_` and `-`"));
    }
    Ok(())
}

fn check_option_id(value: &str) -> std::result::Result<(), String> {
    check_member_id(value, "OptionId")
}

fn check_criterion_id(value: &str) -> std::result::Result<(), String> {
    check_member_id(value, "CriterionId")
}

fn check_rule_id(value: &str) -> std::result::Result<(), String> {
    check_member_id(value, "RuleId")
}

string_id!(EventId, "EventId", check_event_id);
string_id!(SessionId, "SessionId", check_session_id);
string_id!(VaultId, "VaultId", check_vault_id);
string_id!(HostId, "HostId", check_host_id);
string_id!(ProjectSlug, "ProjectSlug", check_project_slug);
string_id!(NodePath, "NodePath", check_node_path);
string_id!(Sha256, "Sha256", check_sha256);
string_id!(OptionId, "OptionId", check_option_id);
string_id!(CriterionId, "CriterionId", check_criterion_id);
string_id!(RuleId, "RuleId", check_rule_id);

impl Sha256 {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        use sha2::Digest as _;

        let digest = sha2::Sha256::digest(bytes);
        Self(hex::encode(digest))
    }

    /// Encode an already finalized SHA-256 digest (no second hashing pass).
    pub fn from_digest(digest: &[u8; 32]) -> Self {
        Self(hex::encode(digest))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    ClaudeCode,
    Codex,
    Generic,
    Pi,
    Herdr,
    #[serde(rename = "opencode")]
    OpenCode,
    /// Cursor's agent CLI.
    Cursor,
    /// DeepSeek Harness.
    Dsh,
    /// Google Antigravity CLI.
    Agy,
    /// Gemini CLI.
    Gemini,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reversibility {
    Easy,
    Costly,
    Irreversible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    Local,
    Wide,
}

/// Declaration order is the canonical wire-string order (`arch`, `code`, `ux`),
/// so derived `Ord` and every `BTreeMap<Domain, _>` follow the wire order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Arch,
    Code,
    Ux,
}

/// What a report asks of its reader: a proposal is read before choosing (its
/// questions link it), a finding is read to know (an experiment, an audit, an
/// investigation). Finished work is a delivery, not a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    Proposal,
    Finding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Auto,
    Assume,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Med,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobExt {
    Txt,
    Md,
    Html,
    Png,
    Jpg,
    Webp,
    Mp4,
    Webm,
    /// Only as a project icon, served under a sandbox CSP.
    Svg,
}

impl BlobExt {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Txt => "txt",
            Self::Md => "md",
            Self::Html => "html",
            Self::Png => "png",
            Self::Jpg => "jpg",
            Self::Webp => "webp",
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
            Self::Svg => "svg",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobRef {
    pub host: HostId,
    pub sha256: Sha256,
    pub ext: BlobExt,
    pub bytes: u64,
}

impl BlobRef {
    pub fn validate(&self, field: &str) -> Result<()> {
        if self.bytes == 0 || self.bytes > MAX_BLOB_BYTES {
            return Err(AgendarError::validation_at(
                format!("{field}/bytes"),
                "blob_size",
                format!(
                    "{field}/bytes must be 1..={MAX_BLOB_BYTES}; got {}.",
                    self.bytes
                ),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seconds(u32);

impl Seconds {
    pub fn new(value: u32) -> Result<Self> {
        if value == 0 || value > MAX_SECONDS {
            return Err(AgendarError::validation(format!(
                "Seconds must be 1..={MAX_SECONDS}; got {value}."
            )));
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Serialize for Seconds {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(self.0)
    }
}

impl<'de> Deserialize<'de> for Seconds {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EtaRange {
    pub min_s: Seconds,
    pub max_s: Seconds,
}

impl EtaRange {
    pub fn validate(&self, field: &str) -> Result<()> {
        if self.min_s.get() > self.max_s.get() {
            return Err(AgendarError::validation_at(
                field,
                "eta_order",
                format!(
                    "{field}/min_s must be <= {field}/max_s; got {}..{}.",
                    self.min_s.get(),
                    self.max_s.get()
                ),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcTime(DateTime<Utc>);

impl UtcTime {
    pub fn parse(text: &str) -> Result<Self> {
        let parsed = DateTime::parse_from_rfc3339(text).map_err(|_| {
            AgendarError::validation(
                "UtcTime must be a canonical UTC RFC3339 timestamp with milliseconds and `Z`.",
            )
        })?;
        if parsed.offset().local_minus_utc() != 0 {
            return Err(AgendarError::validation(
                "UtcTime must use the `Z` UTC offset.",
            ));
        }
        let value = parsed.with_timezone(&Utc);
        let canonical = value.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        if canonical != text {
            return Err(AgendarError::validation(
                "UtcTime must be a canonical millisecond UTC timestamp.",
            ));
        }
        Self::from_datetime(value)
    }

    pub fn from_datetime(value: DateTime<Utc>) -> Result<Self> {
        let year = value.year();
        if !(1970..=9999).contains(&year) {
            return Err(AgendarError::validation(
                "UtcTime year must be 1970..=9999.",
            ));
        }
        if !value.timestamp_subsec_nanos().is_multiple_of(1_000_000) {
            return Err(AgendarError::validation(
                "UtcTime must have millisecond precision.",
            ));
        }
        Ok(Self(value))
    }

    pub const fn as_datetime(&self) -> DateTime<Utc> {
        self.0
    }

    pub fn canonical(&self) -> String {
        self.0.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
    }

    /// Milliseconds since the Unix epoch (never floats).
    pub fn millis(&self) -> i64 {
        self.0.timestamp_millis()
    }

    /// Saturating millisecond addition, clamped to the supported year range.
    pub fn add_millis(&self, millis: i64) -> Self {
        let shifted = self.0 + chrono::TimeDelta::milliseconds(millis);
        let minimum = chrono::DateTime::parse_from_rfc3339("1970-01-01T00:00:00.000Z")
            .expect("fixed timestamp")
            .with_timezone(&Utc);
        let maximum = chrono::DateTime::parse_from_rfc3339("9999-12-31T23:59:59.999Z")
            .expect("fixed timestamp")
            .with_timezone(&Utc);
        Self(shifted.clamp(minimum, maximum))
    }

    /// Saturating second addition.
    pub fn add_seconds(&self, seconds: i64) -> Self {
        self.add_millis(seconds.saturating_mul(1000))
    }
}

impl fmt::Display for UtcTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.canonical())
    }
}

impl Serialize for UtcTime {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.canonical())
    }
}

impl<'de> Deserialize<'de> for UtcTime {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WriterRole {
    Operator,
    Steward,
    Agent(SessionId),
}

impl WriterRole {
    fn suffix(&self) -> String {
        match self {
            Self::Operator => "operator".to_owned(),
            Self::Steward => "steward".to_owned(),
            Self::Agent(session) => format!("s-{session}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WriterId {
    host: HostId,
    role: WriterRole,
}

impl WriterId {
    pub fn new(host: HostId, role: WriterRole) -> Self {
        Self { host, role }
    }

    pub fn parse(value: &str) -> Result<Self> {
        let mut parts = value.split('/');
        let (Some(host), Some(role), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(AgendarError::validation(
                "WriterId must be `<HostId>/operator`, `<HostId>/steward` or `<HostId>/s-<SessionId>`.",
            ));
        };
        if host.is_empty() || role.is_empty() {
            return Err(AgendarError::validation(
                "WriterId host and role must not be blank.",
            ));
        }
        let host = HostId::parse(host)?;
        let role = if role == "operator" {
            WriterRole::Operator
        } else if role == "steward" {
            WriterRole::Steward
        } else if let Some(session) = role.strip_prefix("s-") {
            WriterRole::Agent(SessionId::parse(session)?)
        } else {
            return Err(AgendarError::validation(
                "WriterId role must be `operator`, `steward` or `s-<SessionId>`.",
            ));
        };
        Ok(Self { host, role })
    }

    pub const fn host(&self) -> &HostId {
        &self.host
    }

    pub const fn role(&self) -> &WriterRole {
        &self.role
    }

    pub fn session(&self) -> Option<&SessionId> {
        match &self.role {
            WriterRole::Agent(session) => Some(session),
            WriterRole::Operator | WriterRole::Steward => None,
        }
    }

    pub const fn is_operator(&self) -> bool {
        matches!(&self.role, WriterRole::Operator)
    }

    pub const fn is_steward(&self) -> bool {
        matches!(&self.role, WriterRole::Steward)
    }

    pub fn is_agent(&self) -> bool {
        matches!(&self.role, WriterRole::Agent(_))
    }
}

impl fmt::Display for WriterId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.host, self.role.suffix())
    }
}

impl FromStr for WriterId {
    type Err = AgendarError;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

impl PartialOrd for WriterId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WriterId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.to_string().cmp(&other.to_string())
    }
}

impl Serialize for WriterId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for WriterId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

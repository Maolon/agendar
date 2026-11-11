//! Update detection: is there a newer public release than this build?
//!
//! One conditional GET to the GitHub releases API (`/releases/latest`, which
//! already skips drafts and pre-releases). The answer and its ETag are cached
//! in the local data directory (`update-check.json`), never in the Vault, so a
//! later check sends `If-None-Match` and an unchanged release costs a 304.
//!
//! Only a `release` build (built by the release workflow with
//! `AGENDAR_CHANNEL=release`) is ever told an update is available. Every other
//! build is `dev`: it may be ahead of the latest public release while its
//! version string still matches an older one, so it only gets neutral info.
//!
//! Nothing here installs anything. Reading the cache never touches the network;
//! only [`check`] does, through a [`Fetcher`] that tests replace.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::UtcTime;
use crate::runtime::local_state::{ensure_dir, read_json_file, write_json_atomic};

/// The public releases endpoint.
pub const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/Maolon/agendar/releases/latest";
/// Release pages a "What's new" link may open; anything else falls back here.
pub const RELEASES_PAGE: &str = "https://github.com/Maolon/agendar/releases";
/// Overrides the endpoint (tests serve a fixture on a loopback port).
pub const URL_ENV: &str = "AGENDAR_UPDATE_URL";
/// The cache file in the local data directory.
pub const CACHE_FILE: &str = "update-check.json";
/// How often the app checks on its own.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Stored release notes are cut to this many bytes (at a character boundary).
pub const MAX_NOTES_BYTES: usize = 16 * 1024;
/// A release response larger than this is refused.
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_SHORT_TEXT: usize = 200;
const CACHE_VERSION: u8 = 1;

/// The version this build reports.
pub const fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Who built this binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// Built and published by the release workflow.
    Release,
    /// Anything else: a local build, `cargo build`, a CI check.
    Dev,
}

impl Channel {
    /// The channel compiled into this binary (`AGENDAR_CHANNEL` at build time).
    pub fn of_build() -> Self {
        Self::parse(option_env!("AGENDAR_CHANNEL"))
    }

    /// Only the exact value `release` makes a release build.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("release") => Self::Release,
            _ => Self::Dev,
        }
    }
}

/// Parse a release tag or version (`v0.2.0`, `0.2.0-rc.1`).
pub fn parse_version(text: &str) -> Option<semver::Version> {
    let trimmed = text.trim();
    let bare = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    semver::Version::parse(bare).ok()
}

/// SemVer precedence of two versions (build metadata ignored). `None` when
/// either does not parse.
pub fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    Some(parse_version(left)?.cmp_precedence(&parse_version(right)?))
}

/// Whether this build should be offered `latest`: a release build only, and
/// only for a strictly newer version. A dev build never is.
pub fn is_available(channel: Channel, current: &str, latest: &str) -> bool {
    channel == Channel::Release && compare_versions(latest, current) == Some(Ordering::Greater)
}

/// The latest public release, as cached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    /// The tag without its `v`.
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The release page on github.com.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// Parse a `/releases/latest` response. A draft, a pre-release or a tag that
/// is not a version is `Ok(None)`: there is nothing to offer.
pub fn parse_release(body: &[u8]) -> Result<Option<Release>> {
    let raw: GithubRelease = serde_json::from_slice(body).map_err(|error| {
        AgendarError::new(
            ErrorCode::Io,
            format!("The release response is not readable: {error}."),
        )
        .with_detail("update_response")
        .with_retryable(true)
    })?;
    if raw.draft || raw.prerelease {
        return Ok(None);
    }
    let Some(version) = parse_version(&raw.tag_name) else {
        return Ok(None);
    };
    if !version.pre.is_empty() {
        return Ok(None);
    }
    let url = raw
        .html_url
        .filter(|url| is_release_page(url))
        .unwrap_or_else(|| RELEASES_PAGE.to_owned());
    Ok(Some(Release {
        tag: cut(raw.tag_name.trim(), MAX_SHORT_TEXT),
        version: version.to_string(),
        name: raw
            .name
            .map(|name| cut(name.trim(), MAX_SHORT_TEXT))
            .filter(|name| !name.is_empty()),
        url,
        published_at: raw.published_at.map(|time| cut(&time, 64)),
        notes: raw
            .body
            .map(|notes| cut(notes.trim(), MAX_NOTES_BYTES))
            .filter(|notes| !notes.is_empty()),
    }))
}

/// Whether `url` is a page under this project's releases on github.com, the
/// only kind of link the app opens for an update.
pub fn is_release_page(url: &str) -> bool {
    url.strip_prefix(RELEASES_PAGE)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        && url.len() <= 300
        // Plain path characters only: the link is handed to the system opener
        // (`cmd /C start` on Windows), where `&`, `^` or quotes would mean more.
        && url
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'.' | b'-' | b'_' | b'+' | b'~'))
}

fn cut(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// What was last learned, kept in `<data-dir>/update-check.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCache {
    pub v: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<UtcTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<Release>,
}

impl Default for UpdateCache {
    fn default() -> Self {
        Self {
            v: CACHE_VERSION,
            checked_at: None,
            etag: None,
            latest: None,
        }
    }
}

pub fn cache_path(data_root: &Path) -> PathBuf {
    data_root.join(CACHE_FILE)
}

/// Read the cache. Missing, unreadable or from another version: empty. Never
/// touches the network.
pub fn load_cache(data_root: &Path) -> UpdateCache {
    match read_json_file::<UpdateCache>(&cache_path(data_root)) {
        Ok(Some(cache)) if cache.v == CACHE_VERSION => cache,
        _ => UpdateCache::default(),
    }
}

/// Replace the cache atomically.
pub fn store_cache(data_root: &Path, cache: &UpdateCache) -> Result<()> {
    ensure_dir(data_root)?;
    write_json_atomic(&cache_path(data_root), cache)
}

/// One HTTP answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// 304: the cached release is still the latest.
    NotModified,
    /// 200 with its body and ETag.
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
    },
    /// 404: the repository has no published release yet.
    NoRelease,
}

/// The network side of a check, so tests never reach it.
pub trait Fetcher {
    fn fetch(&self, url: &str, etag: Option<&str>) -> Result<Fetched>;
}

/// The real fetcher: HTTPS with rustls and the bundled web PKI roots.
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn fetch(&self, url: &str, etag: Option<&str>) -> Result<Fetched> {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .user_agent(format!("agendar/{}", current_version()))
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let mut request = agent
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        let mut response = request.call().map_err(network_error)?;
        match response.status().as_u16() {
            304 => Ok(Fetched::NotModified),
            404 => Ok(Fetched::NoRelease),
            200 => {
                let etag = response
                    .headers()
                    .get("etag")
                    .and_then(|value| value.to_str().ok())
                    .map(|value| cut(value, MAX_SHORT_TEXT));
                let bytes = response
                    .body_mut()
                    .with_config()
                    .limit(MAX_RESPONSE_BYTES)
                    .read_to_vec()
                    .map_err(network_error)?;
                Ok(Fetched::Body { bytes, etag })
            }
            status => Err(AgendarError::new(
                ErrorCode::Io,
                format!("The release server answered HTTP {status}."),
            )
            .with_detail("update_http_status")
            .with_hint(if status == 403 || status == 429 {
                "GitHub limits unauthenticated checks to 60 an hour; try again later."
            } else {
                "Try again later."
            })
            .with_retryable(true)),
        }
    }
}

fn network_error(error: ureq::Error) -> AgendarError {
    AgendarError::new(
        ErrorCode::Io,
        format!("Cannot reach the release server: {error}."),
    )
    .with_detail("update_network")
    .with_hint("Check the network connection and try again.")
    .with_retryable(true)
}

/// The endpoint to ask: `AGENDAR_UPDATE_URL` when set (HTTPS, or plain HTTP to
/// a loopback address only), else the public releases API.
pub fn endpoint() -> Result<String> {
    match std::env::var(URL_ENV) {
        Ok(value) if !value.trim().is_empty() => validate_endpoint(value.trim()),
        _ => Ok(LATEST_RELEASE_URL.to_owned()),
    }
}

fn validate_endpoint(value: &str) -> Result<String> {
    let parsed = url::Url::parse(value).ok();
    let allowed = parsed.as_ref().is_some_and(|url| match url.scheme() {
        "https" => true,
        "http" => matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
        _ => false,
    });
    if allowed {
        Ok(value.to_owned())
    } else {
        Err(
            AgendarError::new(ErrorCode::Usage, format!("Invalid {URL_ENV}."))
                .with_field(format!("/env/{URL_ENV}"))
                .with_detail("update_url")
                .with_hint("Use an https:// URL, or http:// to a loopback address."),
        )
    }
}

/// Ask once and update the cache. A 304 keeps the cached release; a failure
/// leaves the cache untouched and returns the error.
pub fn check(
    data_root: &Path,
    fetcher: &dyn Fetcher,
    url: &str,
    now: UtcTime,
) -> Result<UpdateCache> {
    let mut cache = load_cache(data_root);
    // An ETag without the release it describes cannot answer a 304.
    let etag = cache.latest.as_ref().and(cache.etag.as_deref());
    match fetcher.fetch(url, etag)? {
        Fetched::NotModified if cache.latest.is_some() => {}
        Fetched::NotModified | Fetched::NoRelease => {
            cache.latest = None;
            cache.etag = None;
        }
        Fetched::Body { bytes, etag } => {
            cache.latest = parse_release(&bytes)?;
            cache.etag = etag;
        }
    }
    cache.checked_at = Some(now);
    store_cache(data_root, &cache)?;
    Ok(cache)
}

/// What a build shows about updates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateStatus {
    pub current: String,
    pub channel: Channel,
    /// The latest public release version, when known.
    pub latest: Option<String>,
    /// A release build and a strictly newer release.
    pub available: bool,
    pub url: Option<String>,
    pub published_at: Option<String>,
    pub checked_at: Option<UtcTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// The status of `current` on `channel` from a cache; `with_notes` adds the
/// release notes.
pub fn status(
    cache: &UpdateCache,
    channel: Channel,
    current: &str,
    with_notes: bool,
) -> UpdateStatus {
    let latest = cache.latest.as_ref();
    UpdateStatus {
        current: current.to_owned(),
        channel,
        latest: latest.map(|release| release.version.clone()),
        available: latest.is_some_and(|release| is_available(channel, current, &release.version)),
        url: latest.map(|release| release.url.clone()),
        published_at: latest.and_then(|release| release.published_at.clone()),
        checked_at: cache.checked_at,
        name: latest.and_then(|release| release.name.clone()),
        notes: if with_notes {
            latest.and_then(|release| release.notes.clone())
        } else {
            None
        },
    }
}

/// This build's status from the cache alone (no network): what `status`
/// commands and snapshots show.
pub fn cached_status(data_root: &Path, with_notes: bool) -> UpdateStatus {
    status(
        &load_cache(data_root),
        Channel::of_build(),
        current_version(),
        with_notes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const FIXTURE: &str = include_str!("../tests/fixtures/github-release-latest.json");

    fn now() -> UtcTime {
        UtcTime::parse("2026-11-09T12:00:00.000Z").expect("time")
    }

    struct Scripted {
        answers: RefCell<Vec<Result<Fetched>>>,
        seen: RefCell<Vec<Option<String>>>,
    }

    impl Scripted {
        fn new(answers: Vec<Result<Fetched>>) -> Self {
            Self {
                answers: RefCell::new(answers),
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl Fetcher for Scripted {
        fn fetch(&self, _url: &str, etag: Option<&str>) -> Result<Fetched> {
            self.seen.borrow_mut().push(etag.map(str::to_owned));
            self.answers.borrow_mut().remove(0)
        }
    }

    #[test]
    fn versions_compare_by_semver_precedence() {
        assert_eq!(compare_versions("v0.2.0", "0.1.0"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.1.0", "v0.1.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("V0.1.0", "0.1.1"), Some(Ordering::Less));
        assert_eq!(compare_versions("0.10.0", "0.9.9"), Some(Ordering::Greater));
        assert_eq!(
            compare_versions("1.0.0-rc.1", "1.0.0"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("1.0.0-rc.2", "1.0.0-rc.10"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("1.0.0+build.5", "1.0.0"),
            Some(Ordering::Equal)
        );
        assert_eq!(compare_versions("latest", "0.1.0"), None);
    }

    #[test]
    fn a_dev_build_is_never_offered_an_update() {
        for latest in ["0.2.0", "1.0.0", "0.1.0", "0.0.9"] {
            assert!(!is_available(Channel::Dev, "0.1.0", latest), "{latest}");
        }
        assert!(is_available(Channel::Release, "0.1.0", "0.2.0"));
        assert!(is_available(Channel::Release, "0.1.0", "v0.1.1"));
        assert!(!is_available(Channel::Release, "0.1.0", "0.1.0"), "equal");
        assert!(!is_available(Channel::Release, "0.2.0", "0.1.0"), "older");
        assert!(!is_available(Channel::Release, "0.2.0", "0.2.0-rc.1"));
        assert!(!is_available(Channel::Release, "0.1.0", "garbage"));
    }

    #[test]
    fn only_the_exact_release_value_selects_the_release_channel() {
        assert_eq!(Channel::parse(Some("release")), Channel::Release);
        assert_eq!(Channel::parse(Some("Release")), Channel::Dev);
        assert_eq!(Channel::parse(Some("dev")), Channel::Dev);
        assert_eq!(Channel::parse(None), Channel::Dev);
    }

    #[test]
    fn github_release_json_parses_and_caps_its_notes() {
        let release = parse_release(FIXTURE.as_bytes())
            .expect("parse")
            .expect("release");
        assert_eq!(release.tag, "v0.2.0");
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.name.as_deref(), Some("v0.2.0"));
        assert_eq!(
            release.url,
            "https://github.com/Maolon/agendar/releases/tag/v0.2.0"
        );
        assert_eq!(
            release.published_at.as_deref(),
            Some("2026-11-01T10:00:00Z")
        );
        assert_eq!(
            release.notes.as_deref(),
            Some("## Features\n\n* update detection")
        );

        let mut long: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture");
        long["body"] = serde_json::json!("é".repeat(MAX_NOTES_BYTES));
        long["html_url"] = serde_json::json!("https://evil.example/releases");
        let release = parse_release(long.to_string().as_bytes())
            .expect("parse")
            .expect("release");
        let notes = release.notes.expect("notes");
        assert!(notes.len() <= MAX_NOTES_BYTES);
        assert!(notes.chars().all(|c| c == 'é'));
        assert_eq!(release.url, RELEASES_PAGE, "foreign links are not kept");

        let mut pre: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture");
        pre["prerelease"] = serde_json::json!(true);
        assert_eq!(
            parse_release(pre.to_string().as_bytes()).expect("parse"),
            None
        );
        pre["prerelease"] = serde_json::json!(false);
        pre["tag_name"] = serde_json::json!("v0.3.0-beta.1");
        assert_eq!(
            parse_release(pre.to_string().as_bytes()).expect("parse"),
            None
        );
        assert!(parse_release(b"<html>").is_err());
    }

    #[test]
    fn release_links_stay_on_the_project_release_pages() {
        assert!(is_release_page(
            "https://github.com/Maolon/agendar/releases/tag/v0.2.0"
        ));
        assert!(is_release_page(RELEASES_PAGE));
        assert!(!is_release_page(
            "https://github.com/Maolon/agendar-evil/releases"
        ));
        assert!(!is_release_page(
            "https://github.com/Maolon/agendar/releases/x y"
        ));
        assert!(!is_release_page(
            "http://github.com/Maolon/agendar/releases"
        ));
        assert!(!is_release_page(
            "https://github.com/Maolon/agendar/releases/tag/v1&calc"
        ));
    }

    #[test]
    fn the_cache_round_trips_and_a_304_keeps_the_release() {
        let root = tempfile::tempdir().expect("temp");
        let data = root.path().join("data");
        assert_eq!(
            load_cache(&data),
            UpdateCache::default(),
            "missing is empty"
        );

        let fetcher = Scripted::new(vec![
            Ok(Fetched::Body {
                bytes: FIXTURE.as_bytes().to_vec(),
                etag: Some("W/\"abc\"".to_owned()),
            }),
            Ok(Fetched::NotModified),
            Err(AgendarError::new(ErrorCode::Io, "offline")),
        ]);
        let first = check(&data, &fetcher, LATEST_RELEASE_URL, now()).expect("first");
        assert_eq!(first.etag.as_deref(), Some("W/\"abc\""));
        assert_eq!(load_cache(&data), first, "round trip");

        let later = now().add_seconds(3600);
        let second = check(&data, &fetcher, LATEST_RELEASE_URL, later).expect("second");
        assert_eq!(second.latest, first.latest, "304 keeps the release");
        assert_eq!(second.etag, first.etag);
        assert_eq!(second.checked_at, Some(later));

        assert!(check(&data, &fetcher, LATEST_RELEASE_URL, later.add_seconds(1)).is_err());
        assert_eq!(load_cache(&data), second, "a failure leaves the cache");
        assert_eq!(
            *fetcher.seen.borrow(),
            vec![
                None,
                Some("W/\"abc\"".to_owned()),
                Some("W/\"abc\"".to_owned())
            ],
            "the cached ETag is sent back"
        );
    }

    #[test]
    fn status_reads_the_cache_without_the_network() {
        let root = tempfile::tempdir().expect("temp");
        let fetcher = Scripted::new(vec![Ok(Fetched::Body {
            bytes: FIXTURE.as_bytes().to_vec(),
            etag: None,
        })]);
        check(root.path(), &fetcher, LATEST_RELEASE_URL, now()).expect("seed");
        // Reading takes no fetcher at all; `agendar status` is proven offline
        // end to end in the CLI tests.
        let cache = load_cache(root.path());
        let release = status(&cache, Channel::Release, "0.1.0", false);
        assert!(release.available);
        assert_eq!(release.latest.as_deref(), Some("0.2.0"));
        assert_eq!(release.notes, None);
        let dev = status(&cache, Channel::Dev, "0.1.0", true);
        assert!(!dev.available, "dev never");
        assert!(dev.notes.is_some());
        let local = cached_status(root.path(), false);
        assert_eq!(local.current, current_version());
        assert_eq!(local.checked_at, Some(now()));
    }

    #[test]
    fn a_missing_release_clears_the_cache() {
        let root = tempfile::tempdir().expect("temp");
        let fetcher = Scripted::new(vec![
            Ok(Fetched::Body {
                bytes: FIXTURE.as_bytes().to_vec(),
                etag: Some("e".to_owned()),
            }),
            Ok(Fetched::NoRelease),
        ]);
        check(root.path(), &fetcher, LATEST_RELEASE_URL, now()).expect("seed");
        let cleared = check(root.path(), &fetcher, LATEST_RELEASE_URL, now()).expect("404");
        assert_eq!(cleared.latest, None);
        assert_eq!(cleared.etag, None);
    }

    #[test]
    fn only_https_or_loopback_endpoints_are_accepted() {
        assert!(validate_endpoint("https://example.com/latest").is_ok());
        assert!(validate_endpoint("http://127.0.0.1:4000/latest").is_ok());
        assert!(validate_endpoint("http://localhost:4000/latest").is_ok());
        assert!(validate_endpoint("http://example.com/latest").is_err());
        assert!(validate_endpoint("file:///etc/passwd").is_err());
        assert!(validate_endpoint("not a url").is_err());
    }
}

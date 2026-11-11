//! Local, non-Vault state primitives: cross-process locks, atomic journals and
//! the selected local configuration.
//!
//! Everything written here lives under the selected local data root and never
//! inside the Vault. Files are replaced by a sibling temp file plus rename so a
//! reader observes either the previous or the next complete journal, never a
//! partially written one.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::ProjectSlug;

/// How long a contending process waits for a local lock before `E_BUSY`,
/// unless it chose otherwise with [`set_lock_wait`].
pub const LOCK_WAIT: Duration = Duration::from_secs(5);

static LOCK_WAIT_MS: AtomicU64 = AtomicU64::new(LOCK_WAIT.as_millis() as u64);

/// Choose how long this process waits for a contended local lock before
/// `E_BUSY`. A caller's own deadline still wins when it is earlier.
///
/// Every lock here is held for well under a second, but several processes
/// (the app, MCP servers, waiting agents) refresh together after each Vault
/// change, and on a loaded machine the queue can pass five seconds. An agent
/// command that gives up then loses its work: an ack it must repeat, an answer
/// it never sees. The CLI therefore waits longer than the app.
pub fn set_lock_wait(wait: Duration) {
    let millis = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
    LOCK_WAIT_MS.store(millis, Ordering::Relaxed);
}

/// The wait [`acquire_lock`] uses in this process.
pub fn lock_wait() -> Duration {
    Duration::from_millis(LOCK_WAIT_MS.load(Ordering::Relaxed))
}

/// Bound for every local JSON journal read so a damaged file cannot allocate
/// without limit.
pub const MAX_LOCAL_JSON_BYTES: u64 = 1_048_576;

/// Acquire an exclusive advisory file lock, retrying only for contention until
/// [`lock_wait`] has passed.
pub fn acquire_lock(path: &Path) -> Result<File> {
    acquire_lock_until(path, None)
}

/// Acquire an exclusive advisory file lock, never waiting past `deadline`.
///
/// The caller-supplied deadline bounds the ordinary [`lock_wait`] so a
/// long-running operation with an end-to-end budget (the Claude Code Stop hook)
/// cannot spend the budget waiting for another process. An elapsed deadline is
/// `E_BUSY` immediately, never a lock violation.
pub fn acquire_lock_until(path: &Path, until: Option<Instant>) -> Result<File> {
    let file = open_lock_file(path)?;
    let mut deadline = Instant::now() + lock_wait();
    if let Some(until) = until {
        deadline = deadline.min(until);
    }
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(error) if is_lock_contention(&error) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(busy_error());
                }
                std::thread::sleep(Duration::from_millis(5).min(deadline - now));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Whether a refused non-blocking lock attempt means "another process holds the
/// lock" rather than an unrelated I/O failure.
///
/// [`std::io::ErrorKind::WouldBlock`] is the portable signal but not the only
/// one. On Windows `fs2` reports contention as `ERROR_LOCK_VIOLATION` (33) from
/// `LockFileEx`, and Rust does not map that OS code to `WouldBlock`, so a
/// kind-only test would turn ordinary Windows contention into an immediate
/// nonretryable `E_IO` instead of the bounded wait. The platform's own contention code comes from `fs2` rather than a
/// local table, so this comparison stays correct per platform; every other
/// failure is still propagated immediately.
fn is_lock_contention(error: &std::io::Error) -> bool {
    is_contention_error(error, fs2::lock_contended_error().raw_os_error())
}

fn is_contention_error(error: &std::io::Error, contended_code: Option<i32>) -> bool {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    match contended_code {
        Some(code) => error.raw_os_error() == Some(code),
        None => false,
    }
}

/// Open the lock file, creating it once, without following a leaf alias.
///
/// A lock must never be a symbolic link or a reparse point: either redirects the
/// path to another object, so following one would lock a file outside the
/// selected local data root, possibly inside the Vault. Creation uses
/// `create_new`, which refuses any pre-existing leaf (including a dangling
/// symlink) instead of following it. The
/// reopen path does not follow the leaf either (O_NOFOLLOW on Unix,
/// `FILE_FLAG_OPEN_REPARSE_POINT` on Windows) and validates the *identity of the
/// leaf it opened* — device/inode on Unix, volume serial/file index through the
/// safe `same_file` handle on Windows — against the leaf it checked, so an
/// exchanged leaf is refused rather than locked.
fn open_lock_file(path: &Path) -> Result<File> {
    match OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let checked = leaf_identity(path)?;
            let file = open_existing_leaf(path)?;
            validate_lock_leaf(checked, &file)?;
            Ok(file)
        }
        Err(error) => Err(error.into()),
    }
}

/// Identity of one lock leaf: the object the path names, not the path itself.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct LockIdentity {
    /// Device number.
    serial: u64,
    /// Inode number on that device.
    index: u64,
}

/// Identity of one lock leaf on Windows.
///
/// Windows exposes neither the volume serial number nor the file index of an
/// open handle through safe std, so the identity is a `same_file` handle: it
/// compares exactly those two values for the handle it was built from, without
/// this crate needing an `unsafe` FFI boundary.
#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
struct LockIdentity {
    handle: same_file::Handle,
}

/// Identity of one lock leaf on platforms without a file identity. Nothing is
/// ever constructed here: every constructor refuses the platform first.
#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct LockIdentity;

/// Refuse a leaf that is not the object validated before the open.
fn validate_lock_leaf(checked: LockIdentity, file: &File) -> Result<()> {
    if checked != handle_identity(file)? {
        return Err(alias_error());
    }
    Ok(())
}

fn alias_error() -> AgendarError {
    AgendarError::new(
        ErrorCode::Io,
        "Cannot lock local state: the lock file changed while it was opened.",
    )
    .with_detail("local_state_alias")
    .with_hint("Remove the swapped lock file and retry; locks live outside the Vault.")
}

fn symlink_error() -> AgendarError {
    AgendarError::validation_at(
        "/local-state",
        "local_state_symlink",
        "Local locks must not be symbolic links.",
    )
}

fn not_file_error() -> AgendarError {
    AgendarError::validation_at(
        "/local-state",
        "local_state_not_file",
        "Local locks must be regular files.",
    )
}

/// Reject a leaf alias and report the identity of the leaf itself, without
/// following it.
#[cfg(unix)]
fn leaf_identity(path: &Path) -> Result<LockIdentity> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(symlink_error());
    }
    if !metadata.is_file() {
        return Err(not_file_error());
    }
    Ok(identity_of(&metadata))
}

/// Open an existing lock leaf without following a symbolic-link leaf.
#[cfg(unix)]
fn open_existing_leaf(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => Ok(file),
        // The leaf became a symbolic link between the check and the open.
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => Err(symlink_error()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn handle_identity(file: &File) -> Result<LockIdentity> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(not_file_error());
    }
    Ok(identity_of(&metadata))
}

/// Device and inode of the object itself: a genuine file identity.
#[cfg(unix)]
fn identity_of(metadata: &fs::Metadata) -> LockIdentity {
    use std::os::unix::fs::MetadataExt as _;
    LockIdentity {
        serial: metadata.dev(),
        index: metadata.ino(),
    }
}

/// Reject a leaf alias and report the identity of the leaf itself.
///
/// The leaf is opened with `FILE_FLAG_OPEN_REPARSE_POINT` so a reparse point is
/// never followed, then the opened handle is inspected with std's safe Windows
/// metadata extensions. A reparse-point handle is an alias, never a lock, and
/// the handle's own volume serial/file index identity must match the leaf that
/// was checked.
#[cfg(windows)]
fn leaf_identity(path: &Path) -> Result<LockIdentity> {
    let metadata = fs::symlink_metadata(path)?;
    if is_reparse_point(&metadata) {
        return Err(symlink_error());
    }
    if !metadata.is_file() {
        return Err(not_file_error());
    }
    let file = open_existing_leaf(path)?;
    handle_identity(&file)
}

#[cfg(windows)]
fn open_existing_leaf(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    /// `FILE_FLAG_OPEN_REPARSE_POINT`: open the reparse point itself, never its
    /// target.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?)
}

#[cfg(windows)]
fn handle_identity(file: &File) -> Result<LockIdentity> {
    let metadata = file.metadata()?;
    if is_reparse_point(&metadata) {
        return Err(symlink_error());
    }
    if !metadata.is_file() {
        return Err(not_file_error());
    }
    // Genuine object identity without an `unsafe` boundary: `same_file` reports
    // the volume serial number and file index behind this handle, and its
    // equality is exactly that pair — the Windows counterpart of device/inode.
    let handle = same_file::Handle::from_file(file.try_clone()?)?;
    Ok(LockIdentity { handle })
}

/// `FILE_ATTRIBUTE_REPARSE_POINT`: the leaf is a symlink, junction or another
/// reparse point rather than a plain file.
#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Platforms without an available file identity: a lock leaf cannot be
/// validated here, so locking is refused rather than silently trusting a path.
#[cfg(not(any(unix, windows)))]
fn leaf_identity(_path: &Path) -> Result<LockIdentity> {
    Err(unsupported_lock_error())
}

#[cfg(not(any(unix, windows)))]
fn open_existing_leaf(path: &Path) -> Result<File> {
    Ok(OpenOptions::new().read(true).write(true).open(path)?)
}

#[cfg(not(any(unix, windows)))]
fn handle_identity(_file: &File) -> Result<LockIdentity> {
    Err(unsupported_lock_error())
}

#[cfg(not(any(unix, windows)))]
fn unsupported_lock_error() -> AgendarError {
    AgendarError::new(
        ErrorCode::Unsupported,
        "Local locks need a file identity this platform does not expose.",
    )
    .with_detail("local_state_unsupported")
    .with_hint("Run Agendar on macOS, Linux or Windows.")
}

fn busy_error() -> AgendarError {
    AgendarError::new(ErrorCode::Busy, "Writer/index is busy.")
        .with_detail("lock_timeout")
        .with_hint("Retry shortly; another Agendar process holds the local lock.")
        .with_retryable(true)
}

/// Atomically replace `path` with the compact JSON serialization of `value`
/// plus one LF.
pub fn write_json_atomic<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize + ?Sized,
{
    let mut bytes = serde_json::to_vec(value).map_err(|error| {
        AgendarError::new(
            ErrorCode::Io,
            format!("Cannot serialize local state: {error}."),
        )
        .with_detail("local_state_serialize")
    })?;
    bytes.push(b'\n');
    write_bytes_atomic(path, &bytes)
}

/// Atomically replace `path` with `bytes`.
pub fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        AgendarError::new(
            ErrorCode::Io,
            "Cannot write local state without a parent directory.",
        )
        .with_detail("local_state_path")
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            AgendarError::new(ErrorCode::Io, "Local state path has no usable file name.")
                .with_detail("local_state_path")
        })?;
    let temporary = parent.join(format!(".{file_name}.tmp-{}", Ulid::generate()));

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    // Drop the handle before rename on platforms that refuse renaming over an
    // open file, and before removing a failed temporary.
    drop(file);
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    sync_dir(parent)
}

/// Read a local JSON journal. A missing file is `Ok(None)`; a symlink, an
/// oversized file or malformed JSON is a typed local-state error.
pub fn read_json_file<T>(path: &Path) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    let Some(value) = read_json_value(path)? else {
        return Ok(None);
    };
    let typed = serde_json::from_value(value).map_err(|error| {
        AgendarError::validation_at(
            "/local-state",
            "local_state_malformed",
            format!("Local state has an unexpected shape: {error}."),
        )
    })?;
    Ok(Some(typed))
}

/// Read a local JSON journal as an untyped value so callers can inspect the
/// version before committing to a typed shape.
pub fn read_json_value(path: &Path) -> Result<Option<serde_json::Value>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(AgendarError::validation_at(
            "/local-state",
            "local_state_symlink",
            "Local state must not be a symbolic link.",
        ));
    }
    if !metadata.is_file() {
        return Err(AgendarError::validation_at(
            "/local-state",
            "local_state_not_file",
            "Local state must be a regular file.",
        ));
    }
    if metadata.len() > MAX_LOCAL_JSON_BYTES {
        return Err(AgendarError::validation_at(
            "/local-state",
            "local_state_too_large",
            "Local state exceeds the bounded journal size.",
        ));
    }
    let bytes = fs::read(path)?;
    let value = serde_json::from_slice(&bytes).map_err(|error| {
        AgendarError::validation_at(
            "/local-state",
            "local_state_malformed",
            format!("Local state is malformed JSON: {error}."),
        )
    })?;
    Ok(Some(value))
}

/// Fsync a directory entry where the platform supports opening directories.
///
/// Only a filesystem that reports the operation itself as unsupported
/// (`ENOTSUP`) is skipped. An access failure is a failure: a durability check
/// that could not be performed must never be reported as durable, and callers
/// keep their journals and pending bytes until it succeeds.
#[cfg(unix)]
pub fn sync_dir(path: &Path) -> Result<()> {
    let directory = File::open(path)?;
    match directory.sync_all() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Directory fsync is not exposed by the standard library on this platform;
/// callers still fsync every file they write.
#[cfg(not(unix))]
pub fn sync_dir(_path: &Path) -> Result<()> {
    Ok(())
}

/// Create a directory and every missing parent, returning whether anything was
/// created. Components are created one at a time and each new entry's parent is
/// fsynced where the platform supports it, so a crash cannot leave a child
/// directory entry without its parent entry.
///
/// The deepest already-visible component is re-synced with its own parent on
/// every call. A component may have been left behind by an interrupted or
/// concurrent creation whose parent sync never completed, and syncing only its
/// children would treat that unpublished entry as durable on retry.
pub fn ensure_dir(path: &Path) -> Result<bool> {
    let mut missing: Vec<&Path> = Vec::new();
    let mut existing = path;
    loop {
        match fs::symlink_metadata(existing) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(AgendarError::validation_at(
                        "/local-state",
                        "local_state_not_directory",
                        "Local state must be created inside ordinary directories.",
                    ));
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(existing);
                match existing.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => existing = parent,
                    _ => {
                        return Err(AgendarError::validation_at(
                            "/local-state",
                            "local_state_path",
                            "Local state path has no existing ancestor directory.",
                        ));
                    }
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    if let Some(parent) = existing
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        sync_dir(parent)?;
    }
    for directory in missing.iter().rev() {
        match fs::create_dir(directory) {
            Ok(()) => {}
            // Another process may create the same component first; its entry is
            // published by the parent sync below, exactly like our own.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(parent) = directory.parent() {
            sync_dir(parent)?;
        }
    }
    Ok(!missing.is_empty())
}

/// Version of the local configuration journal.
pub const CONFIG_VERSION: u8 = 1;

/// Selected local configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub v: u8,
    pub revision: u64,
    /// Selected Vault path as the user supplied it (local only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault: Option<String>,
    /// Selected project slug.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectSlug>,
    /// Canonical repo root -> project mapping used by hook context resolution.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repos: BTreeMap<String, ProjectSlug>,
    /// Optional local path of the Codex CLI used for queue delivery. Never
    /// synchronized and never guessed from another host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_bin: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            v: CONFIG_VERSION,
            revision: 0,
            vault: None,
            project: None,
            repos: BTreeMap::new(),
            codex_bin: None,
        }
    }
}

impl Config {
    /// Load the configuration without creating it. A missing file is the default
    /// empty configuration; an unknown version freezes local mutation.
    pub fn load(data_root: &Path) -> Result<Option<Self>> {
        let Some(value) = read_json_value(&data_root.join("config.json"))? else {
            return Ok(None);
        };
        let version = value
            .get("v")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                AgendarError::validation_at(
                    "/config",
                    "config_missing_version",
                    "config.json has no version field.",
                )
            })?;
        if version != u64::from(CONFIG_VERSION) {
            return Err(AgendarError::new(
                ErrorCode::Unsupported,
                format!("Local config version {version} is not supported."),
            )
            .with_detail("config_version")
            .with_hint("Use the Agendar version that created this config file."));
        }
        let config: Config = serde_json::from_value(value).map_err(|error| {
            AgendarError::validation_at(
                "/config",
                "config_malformed",
                format!("config.json has an unexpected shape: {error}."),
            )
        })?;
        Ok(Some(config))
    }

    /// Load or return the default configuration.
    pub fn load_or_default(data_root: &Path) -> Result<Self> {
        Ok(Self::load(data_root)?.unwrap_or_default())
    }

    /// Atomically update the configuration under the config lock. An update
    /// that changes nothing writes nothing: every agent start selects its
    /// project and links its repo again, and bumping the revision for that
    /// would make the app's revision-guarded Settings edits fail as stale.
    pub fn update<T>(data_root: &Path, action: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
        let lock = acquire_lock(&data_root.join("config.lock"))?;
        let stored = Self::load(data_root)?;
        let mut config = stored.clone().unwrap_or_default();
        let value = action(&mut config)?;
        config.v = CONFIG_VERSION;
        if stored.as_ref() == Some(&config) {
            drop(lock);
            return Ok(value);
        }
        config.revision = config.revision.saturating_add(1);
        write_json_atomic(&data_root.join("config.json"), &config)?;
        drop(lock);
        Ok(value)
    }

    /// Record the selected Vault and, when supplied, the selected project.
    pub fn select(
        data_root: &Path,
        vault: Option<&Path>,
        project: Option<&ProjectSlug>,
    ) -> Result<()> {
        Self::update(data_root, |config| {
            if let Some(vault) = vault {
                config.vault = Some(vault.to_string_lossy().into_owned());
            }
            if let Some(project) = project {
                config.project = Some(project.clone());
            }
            Ok(())
        })
    }

    /// Map one canonical repo root to a project for hook context resolution.
    pub fn map_repo(data_root: &Path, repo: &Path, project: &ProjectSlug) -> Result<()> {
        let canonical = repo.canonicalize().map_err(|_error| {
            AgendarError::new(ErrorCode::Context, "The harness repository was not found.")
                .with_detail("missing_repo")
                .with_hint("Pass an existing local repository path.")
                .with_retryable(false)
        })?;
        if !canonical.is_dir() {
            return Err(AgendarError::validation_at(
                "/repo",
                "not_a_directory",
                "The harness repository must be a directory.",
            ));
        }
        let key = canonical.to_string_lossy().into_owned();
        Self::update(data_root, |config| {
            config.repos.insert(key, project.clone());
            Ok(())
        })
    }

    /// Remove one repository link by its stored key (the canonical path the
    /// link was recorded under). Returns whether a link was removed; the
    /// repository itself is never touched.
    pub fn unmap_repo(data_root: &Path, key: &str) -> Result<bool> {
        Self::update(data_root, |config| Ok(config.repos.remove(key).is_some()))
    }

    /// Find the longest registered repo root that contains `cwd`.
    pub fn project_for_cwd(&self, cwd: &Path) -> Option<ProjectSlug> {
        let canonical = cwd.canonicalize().ok()?;
        self.repos
            .iter()
            .filter_map(|(root, project)| {
                let root = PathBuf::from(root);
                canonical
                    .starts_with(&root)
                    .then_some((root.components().count(), project.clone()))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, project)| project)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().expect("temporary directory")
    }

    #[test]
    fn the_longest_linked_repo_prefix_wins_and_siblings_do_not_match() {
        let root = root();
        let parent = root.path().join("repo");
        let child = parent.join("nested");
        let sibling = root.path().join("repo-other");
        fs::create_dir_all(&child).expect("nested repo");
        fs::create_dir_all(&sibling).expect("sibling repo");
        let parent_project = ProjectSlug::parse("parent").expect("slug");
        let child_project = ProjectSlug::parse("child").expect("slug");
        Config::map_repo(root.path(), &parent, &parent_project).expect("parent link");
        Config::map_repo(root.path(), &child, &child_project).expect("child link");

        let config = Config::load(root.path()).expect("config").expect("present");
        assert_eq!(
            config.project_for_cwd(&child).as_ref(),
            Some(&child_project),
            "the nested link is the longest prefix"
        );
        assert_eq!(
            config.project_for_cwd(&parent).as_ref(),
            Some(&parent_project)
        );
        assert_eq!(
            config.project_for_cwd(&sibling),
            None,
            "a sibling with a shared string prefix is not inside the link"
        );
    }

    #[test]
    fn an_update_that_changes_nothing_keeps_the_revision() {
        let root = root();
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).expect("repo");
        let vault = root.path().join("vault");
        let project = ProjectSlug::parse("demo").expect("slug");
        Config::select(root.path(), Some(&vault), Some(&project)).expect("select");
        Config::map_repo(root.path(), &repo, &project).expect("link");
        let before = Config::load(root.path()).expect("config").expect("present");

        // What every agent start does again: select and link the same values.
        Config::select(root.path(), Some(&vault), Some(&project)).expect("select again");
        Config::map_repo(root.path(), &repo, &project).expect("link again");
        let after = Config::load(root.path()).expect("config").expect("present");
        assert_eq!(
            after.revision, before.revision,
            "no-op updates write nothing"
        );

        let other = ProjectSlug::parse("other").expect("slug");
        Config::select(root.path(), None, Some(&other)).expect("select another");
        let changed = Config::load(root.path()).expect("config").expect("present");
        assert_eq!(
            changed.revision,
            before.revision + 1,
            "a real change still bumps it"
        );
    }

    #[test]
    fn lock_contention_is_recognised_by_kind_or_platform_code() {
        // The portable signal and the platform's own contention error.
        assert!(is_lock_contention(&std::io::Error::from(
            std::io::ErrorKind::WouldBlock
        )));
        assert!(is_lock_contention(&fs2::lock_contended_error()));

        // An unrelated failure is never retried, so contention handling cannot
        // swallow real I/O errors.
        assert!(!is_lock_contention(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
        assert!(!is_lock_contention(&std::io::Error::from_raw_os_error(2)));

        // The Windows shape: contention arrives as a raw OS code whose kind is
        // not `WouldBlock` (Rust leaves `ERROR_LOCK_VIOLATION` = 33
        // uncategorized), so the classifier must fall back to the platform's
        // contention code instead of trusting the kind.
        let uncategorized = std::io::Error::from_raw_os_error(33);
        // 33 is `ERROR_LOCK_VIOLATION` on Windows and `EDOM` on Unix; std maps
        // neither to `WouldBlock`, so the raw-code comparison is what has to
        // recognise it.
        assert_ne!(uncategorized.kind(), std::io::ErrorKind::WouldBlock);
        assert!(is_contention_error(&uncategorized, Some(33)));

        // A different failure is never retried, whether it arrives as a kind or
        // as another raw OS code.
        assert!(!is_contention_error(
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            Some(33)
        ));
        assert!(!is_contention_error(
            &std::io::Error::from_raw_os_error(2),
            Some(33)
        ));
        assert!(!is_contention_error(&uncategorized, None));
    }

    #[test]
    fn reopening_an_unchanged_lock_leaf_is_accepted() {
        let root = root();
        let path = root.path().join("writer.lock");
        let first = open_lock_file(&path).expect("create lock");
        drop(first);

        let checked = leaf_identity(&path).expect("checked leaf");
        let reopened = open_existing_leaf(&path).expect("reopen leaf");
        validate_lock_leaf(checked, &reopened).expect("the same leaf is the same lock");
        drop(reopened);

        open_lock_file(&path).expect("reopen through the lock primitive");
    }

    #[test]
    fn a_leaf_exchanged_for_another_file_is_refused() {
        let root = root();
        let path = root.path().join("writer.lock");
        fs::write(&path, b"").expect("lock leaf");
        // Hold the original object open so its identity cannot be reused by the
        // replacement file.
        let held = File::open(&path).expect("hold the original leaf");
        let checked = leaf_identity(&path).expect("checked leaf");

        // The exchange happens between the identity check and the open: the
        // leaf is replaced by a real file that already lives in the Vault.
        let replacement = root.path().join("operator.jsonl");
        fs::write(&replacement, b"{\"v\":1}\n").expect("event file");
        fs::rename(&replacement, &path).expect("exchange the leaf");

        let reopened = open_existing_leaf(&path).expect("the replacement is a regular file");
        let error = validate_lock_leaf(checked, &reopened)
            .expect_err("the exchanged leaf is not the checked lock");
        assert_eq!(error.code(), ErrorCode::Io);
        assert_eq!(error.detail(), Some("local_state_alias"));
        drop(reopened);
        drop(held);
    }

    #[cfg(unix)]
    #[test]
    fn a_leaf_exchanged_for_a_symlink_is_refused_without_following_it() {
        use std::os::unix::fs::symlink;

        let root = root();
        let path = root.path().join("writer.lock");
        fs::write(&path, b"").expect("lock leaf");
        let checked = leaf_identity(&path).expect("checked leaf");

        let target = root.path().join("operator.jsonl");
        fs::write(&target, b"{\"v\":1}\n").expect("event file");
        fs::remove_file(&path).expect("remove the leaf");
        symlink(&target, &path).expect("exchange the leaf for a symlink");

        // Both the no-follow open and the identity check refuse the alias, and
        // nothing is locked through it.
        let error = open_existing_leaf(&path).expect_err("the symlink leaf is refused");
        assert_eq!(error.code(), ErrorCode::Validation);
        assert_eq!(error.detail(), Some("local_state_symlink"));
        let error = leaf_identity(&path).expect_err("a symlink leaf is never a lock");
        assert_eq!(error.detail(), Some("local_state_symlink"));
        let error = open_lock_file(&path).expect_err("the public lock primitive refuses it");
        assert_eq!(error.code(), ErrorCode::Validation);
        let _ = checked;
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_opened_for_syncing_is_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let root = root();
        let directory = root.path().join("unreadable");
        fs::create_dir(&directory).expect("directory");
        let mut permissions = fs::metadata(&directory).expect("metadata").permissions();
        permissions.set_mode(0o333);
        fs::set_permissions(&directory, permissions).expect("withhold read access");

        let result = sync_dir(&directory);

        let mut permissions = fs::metadata(&directory).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&directory, permissions).expect("restore access");

        let error = result.expect_err("a durability check that cannot run is not a pass");
        assert_eq!(error.code(), ErrorCode::Io);
        sync_dir(&directory).expect("the restored directory syncs");
    }

    #[cfg(unix)]
    #[test]
    fn a_retry_re_syncs_an_already_visible_component_before_accepting_descendants() {
        use std::os::unix::fs::PermissionsExt;

        let root = root();
        let parent = root.path().join("projects");
        fs::create_dir(&parent).expect("parent");
        let mut permissions = fs::metadata(&parent).expect("metadata").permissions();
        permissions.set_mode(0o333);
        fs::set_permissions(&parent, permissions).expect("withhold read access");

        // The interrupted creation: `visible/child` cannot publish `visible`.
        let child = parent.join("visible").join("child");
        let error = ensure_dir(&child).expect_err("the visible entry cannot be synced");
        assert_eq!(error.code(), ErrorCode::Io);
        assert!(parent.join("visible").is_dir());

        // The retry stops at the already-visible component, so it must refresh
        // that component's own entry instead of assuming it is durable.
        let error = ensure_dir(&child).expect_err("the retry re-syncs the visible entry");
        assert_eq!(error.code(), ErrorCode::Io);

        let mut permissions = fs::metadata(&parent).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&parent, permissions).expect("restore access");

        assert!(ensure_dir(&child).expect("created after restoring access"));
        assert!(child.is_dir());
        assert!(!ensure_dir(&child).expect("already exists"));
    }
}

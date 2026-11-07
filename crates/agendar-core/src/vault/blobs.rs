//! Verified, create-only content-addressed blobs.
//!
//! Publication is serialized per `(vault, project, host, sha256, ext)` with an
//! outside-Vault lock and one durable publication journal. Only the lock owner
//! may finish a partial target; contenders wait briefly, verify the completed
//! bytes and reuse them. A partial target without a journal is corruption and
//! is never overwritten or truncated.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::{BlobExt, BlobRef, HostId, MAX_BLOB_BYTES, ProjectSlug, Sha256, canonical_json};
use crate::runtime::LocalPaths;
use crate::runtime::local_state::{
    acquire_lock, ensure_dir, read_json_value, sync_dir, write_json_atomic,
};
use crate::runtime::paths::LocalLayout;
use crate::vault::paths::{
    Vault, blob_relative_path, contained_path, ensure_directory, ensure_host_directory,
};

const JOURNAL_VERSION: u8 = 1;
const COPY_CHUNK: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicationKey {
    project: ProjectSlug,
    host: HostId,
    sha256: Sha256,
    ext: BlobExt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationJournal {
    v: u8,
    project: ProjectSlug,
    host: HostId,
    sha256: Sha256,
    ext: BlobExt,
    bytes: u64,
    /// Bare staging file name inside the local `tmp` directory. Never a path:
    /// the resolved location is validated under exactly `layout.tmp` before any
    /// read or cleanup, including completed-journal recovery.
    staged_name: String,
    target_path: String,
    published: u64,
    complete: bool,
}

/// Path of the shared publication lock for one blob target.
pub fn blob_lock_path(
    local: &LocalPaths,
    vault: &Vault,
    project: &ProjectSlug,
    host: &HostId,
    sha256: &Sha256,
    ext: BlobExt,
) -> PathBuf {
    let key = PublicationKey {
        project: project.clone(),
        host: host.clone(),
        sha256: sha256.clone(),
        ext,
    };
    local
        .locks_dir(vault.id())
        .join(format!("blob-{}.lock", key_hash(vault, &key)))
}

/// Path of the durable publication journal for one blob target.
pub fn blob_journal_path(
    local: &LocalPaths,
    vault: &Vault,
    project: &ProjectSlug,
    host: &HostId,
    sha256: &Sha256,
    ext: BlobExt,
) -> PathBuf {
    let key = PublicationKey {
        project: project.clone(),
        host: host.clone(),
        sha256: sha256.clone(),
        ext,
    };
    local
        .pending_dir(vault.id())
        .join(format!("blob-{}.json", key_hash(vault, &key)))
}

fn key_hash(vault: &Vault, key: &PublicationKey) -> Sha256 {
    let value = serde_json::json!([
        "blob",
        vault.id().as_str(),
        key.project.as_str(),
        key.host.as_str(),
        key.sha256.as_str(),
        key.ext.as_str()
    ]);
    Sha256::from_bytes(canonical_json(&value).as_bytes())
}

/// Stream `input` into a staged local file and publish it as a create-only
/// Vault blob. Existing identical bytes are reused; existing different bytes
/// are never overwritten.
pub fn put_blob(
    vault: &Vault,
    local: &LocalPaths,
    project: &ProjectSlug,
    host: &HostId,
    ext: BlobExt,
    input: &mut dyn Read,
) -> Result<BlobRef> {
    let layout = local.ensure_layout(vault.root(), vault.id())?;
    let staged = stage_blob(&layout.tmp, input)?;
    let key = PublicationKey {
        project: project.clone(),
        host: host.clone(),
        sha256: staged.sha256.clone(),
        ext,
    };
    let journal_path = blob_journal_path(local, vault, project, host, &staged.sha256, ext);
    let lock_path = blob_lock_path(local, vault, project, host, &staged.sha256, ext);
    let blob = BlobRef {
        host: host.clone(),
        sha256: staged.sha256.clone(),
        ext,
        bytes: staged.bytes,
    };

    let result = (|| -> Result<BlobRef> {
        let _lock = acquire_lock(&lock_path)?;
        if let Some(value) = read_json_value(&journal_path)? {
            let journal = parse_journal(value, &key)?;
            recover_publication(vault, &layout, &journal_path, &journal)?;
        }
        ensure_host_directory(vault.root(), project, host)?;
        ensure_directory(
            vault.root(),
            &format!("{}/blobs", crate::vault::host_relative_path(project, host)),
        )?;
        let relative_target = blob_relative_path(project, host, &staged.sha256, ext);
        let target = contained_path(vault.root(), &relative_target)?;
        match fs::symlink_metadata(&target) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(blob_corrupt("blob target is not a regular file"));
                }
                if metadata.len() != staged.bytes {
                    return Err(blob_corrupt("existing blob has a different length"));
                }
                if sha256_file(&target)? != staged.sha256 {
                    return Err(blob_corrupt("existing blob has a different digest"));
                }
                Ok(blob)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                publish_new(
                    &layout,
                    &journal_path,
                    &key,
                    &staged,
                    &relative_target,
                    &target,
                )?;
                Ok(blob)
            }
            Err(error) => Err(error.into()),
        }
    })();

    // Keep the staged file only while a journal references it.
    let staged_is_referenced = read_json_value(&journal_path)
        .ok()
        .flatten()
        .and_then(|value| {
            value
                .get("staged_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|referenced| referenced == staged.name);
    if !staged_is_referenced {
        // A leftover staged file is harmless local garbage; never turn a
        // successful publication into an error because cleanup failed.
        let _ = remove_if_present(&staged.path);
    }
    result
}

/// Read and verify a blob. Missing or partially hydrated media is pending
/// media, never Seen; only a publication or recovery path that establishes a
/// conflicting local target calls the bytes corrupt.
pub fn read_blob(vault: &Vault, project: &ProjectSlug, blob: &BlobRef) -> Result<Vec<u8>> {
    blob.validate("/blob")?;
    let relative = blob_relative_path(project, &blob.host, &blob.sha256, blob.ext);
    let path = contained_path(vault.root(), &relative)
        .map_err(|_| blob_corrupt("blob path is not contained in the Vault"))?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(media_pending("blob_missing"));
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(blob_corrupt("blob is not a regular file"));
    }
    if metadata.len() != blob.bytes {
        // A lock-free reader cannot distinguish a remote partial hydration from
        // a damaged local file; it reports pending media and lets the
        // publication paths decide corruption.
        return Err(media_pending("blob_pending_length"));
    }
    let bytes = fs::read(&path)?;
    if Sha256::from_bytes(&bytes) != blob.sha256 {
        return Err(media_pending("blob_pending_digest"));
    }
    Ok(bytes)
}

/// The Vault file of `blob`, after its length and digest are verified (a
/// half-synced or altered file is never handed to another program).
pub fn verified_blob_path(vault: &Vault, project: &ProjectSlug, blob: &BlobRef) -> Result<PathBuf> {
    read_blob(vault, project, blob)?;
    let relative = blob_relative_path(project, &blob.host, &blob.sha256, blob.ext);
    contained_path(vault.root(), &relative)
        .map_err(|_| blob_corrupt("blob path is not contained in the Vault"))
}

/// One staged publication input: a local temporary file whose name is the only
/// reference the durable journal stores.
struct StagedBlob {
    path: PathBuf,
    name: String,
    sha256: Sha256,
    bytes: u64,
}

fn stage_blob(tmp_dir: &Path, input: &mut dyn Read) -> Result<StagedBlob> {
    ensure_dir(tmp_dir)?;
    let mut temporary = tempfile::Builder::new()
        .prefix("blob-")
        .suffix(".stage")
        .tempfile_in(tmp_dir)?;
    let mut hasher = sha2::Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; COPY_CHUNK];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > MAX_BLOB_BYTES {
            return Err(AgendarError::validation_at(
                "/bytes",
                "blob_too_large",
                format!("A blob must be 1..={MAX_BLOB_BYTES} bytes."),
            ));
        }
        hasher.update(&buffer[..read]);
        temporary.write_all(&buffer[..read])?;
    }
    if total == 0 {
        return Err(AgendarError::validation_at(
            "/bytes",
            "blob_empty",
            "An empty blob cannot be published.",
        ));
    }
    temporary.as_file_mut().sync_all()?;
    let digest = Sha256::parse(&hex::encode(hasher.finalize()))?;
    let (_file, path) = temporary
        .keep()
        .map_err(|error| AgendarError::from(error.error))?;
    // The staging file name must be a durable `tmp` entry before any journal
    // can reference it, otherwise a crash loses the bytes a recovery needs.
    sync_dir(tmp_dir)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| blob_corrupt("staged blob has no usable file name"))?
        .to_owned();
    Ok(StagedBlob {
        path,
        name,
        sha256: digest,
        bytes: total,
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_new(
    layout: &LocalLayout,
    journal_path: &Path,
    key: &PublicationKey,
    staged: &StagedBlob,
    relative_target: &str,
    target: &Path,
) -> Result<()> {
    let journal = PublicationJournal {
        v: JOURNAL_VERSION,
        project: key.project.clone(),
        host: key.host.clone(),
        sha256: key.sha256.clone(),
        ext: key.ext,
        bytes: staged.bytes,
        staged_name: staged.name.clone(),
        target_path: relative_target.to_owned(),
        published: 0,
        complete: false,
    };
    if journal.bytes == 0 || journal.bytes > MAX_BLOB_BYTES {
        return Err(blob_corrupt("staged blob has an invalid length"));
    }
    write_json_atomic(journal_path, &journal)?;

    let mut journal = journal;
    let mut file = match OpenOptions::new().write(true).create_new(true).open(target) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(blob_corrupt(
                "blob target appeared while publication owned the lock",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    copy_staged(
        &staged.path,
        &mut file,
        0,
        journal.bytes,
        &mut journal,
        journal_path,
    )?;
    file.sync_all()?;
    drop(file);
    sync_target_entry(target)?;
    verify_target(target, key)?;
    complete_publication(layout, journal_path, &journal, &staged.path)?;
    Ok(())
}

fn recover_publication(
    vault: &Vault,
    layout: &LocalLayout,
    journal_path: &Path,
    journal: &PublicationJournal,
) -> Result<()> {
    let key = PublicationKey {
        project: journal.project.clone(),
        host: journal.host.clone(),
        sha256: journal.sha256.clone(),
        ext: journal.ext,
    };
    let expected_target = blob_relative_path(&key.project, &key.host, &key.sha256, key.ext);
    if journal.target_path != expected_target {
        return Err(blob_corrupt(
            "publication journal target does not match its key",
        ));
    }
    if journal.bytes == 0 || journal.bytes > MAX_BLOB_BYTES {
        return Err(blob_corrupt("publication journal has an invalid length"));
    }
    // Validate the staging reference before any read or cleanup, including the
    // completed-journal path: a damaged local journal must never authorize
    // deleting event truth or anything outside the staging directory.
    let staged_path = staged_file_path(layout, &journal.staged_name)?;
    let target = contained_path(vault.root(), &journal.target_path)?;
    if let Some(parent) = journal
        .target_path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
    {
        ensure_directory(vault.root(), parent)?;
    }
    if journal.complete {
        verify_target(&target, &key)?;
        sync_published_target(&target)?;
        complete_publication(layout, journal_path, journal, &staged_path)?;
        return Ok(());
    }

    let (target_exists, target_size) = match fs::symlink_metadata(&target) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(blob_corrupt("blob target is not a regular file"));
            }
            (true, metadata.len())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (false, 0),
        Err(error) => return Err(error.into()),
    };
    if target_size > journal.bytes {
        return Err(blob_corrupt("blob target is longer than its journal"));
    }
    let mut journal = journal.clone();
    if target_size == journal.bytes {
        // A full-length target is verified directly, but the bytes may only be
        // in the page cache: flush the target and its entry before the recovery
        // material is retired.
        verify_target(&target, &key)?;
        sync_published_target(&target)?;
        complete_publication(layout, journal_path, &journal, &staged_path)?;
        return Ok(());
    }

    let staged_path = verified_staged_file(layout, &journal.staged_name)?;
    let staged_metadata = match fs::metadata(&staged_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(blob_corrupt("publication journal lost its staged bytes"));
        }
        Err(error) => return Err(error.into()),
    };
    if staged_metadata.len() != journal.bytes || sha256_file(&staged_path)? != key.sha256 {
        return Err(blob_corrupt(
            "staged bytes do not match the publication journal",
        ));
    }
    if target_size > 0 {
        verify_prefix(&target, &staged_path, target_size)?;
    }
    let mut file = if target_exists {
        OpenOptions::new().append(true).open(&target)?
    } else {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?
    };
    copy_staged(
        &staged_path,
        &mut file,
        target_size,
        journal.bytes,
        &mut journal,
        journal_path,
    )?;
    file.sync_all()?;
    drop(file);
    sync_target_entry(&target)?;
    verify_target(&target, &key)?;
    complete_publication(layout, journal_path, &journal, &staged_path)?;
    Ok(())
}

/// Resolve a journal's staging reference inside exactly `layout.tmp`.
///
/// The journal stores a bare file name; separators, `.`/`..`, absolute paths
/// and control bytes are rejected before anything is read or removed.
fn staged_file_path(layout: &LocalLayout, name: &str) -> Result<PathBuf> {
    let invalid = || {
        AgendarError::validation_at(
            "/local-state",
            "blob_stage_name",
            "A publication journal staging name is not a bare local file name.",
        )
        .with_hint("Inspect the local publication journal; never delete Vault bytes to repair it.")
    };
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.chars().any(char::is_control)
        || Path::new(name).file_name().and_then(|leaf| leaf.to_str()) != Some(name)
        || Path::new(name).components().count() != 1
    {
        return Err(invalid());
    }
    Ok(layout.tmp.join(name))
}

/// Resolve and verify a staging file that must be readable before publication.
fn verified_staged_file(layout: &LocalLayout, name: &str) -> Result<PathBuf> {
    let path = staged_file_path(layout, name)?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| blob_corrupt("publication journal lost its staged bytes"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(blob_corrupt(
            "staged publication bytes are not a regular local file",
        ));
    }
    let canonical_tmp = layout.tmp.canonicalize()?;
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(&canonical_tmp) || canonical.parent() != Some(canonical_tmp.as_path())
    {
        return Err(blob_corrupt(
            "staged publication bytes resolve outside the staging directory",
        ));
    }
    Ok(canonical)
}

fn copy_staged(
    staged_path: &Path,
    target: &mut File,
    start: u64,
    total: u64,
    journal: &mut PublicationJournal,
    journal_path: &Path,
) -> Result<()> {
    let mut staged = File::open(staged_path)?;
    staged.seek(SeekFrom::Start(start))?;
    let mut remaining = total.saturating_sub(start);
    let mut written = start;
    let mut buffer = vec![0_u8; COPY_CHUNK];
    while remaining > 0 {
        let take = usize::try_from(remaining.min(COPY_CHUNK as u64)).unwrap_or(COPY_CHUNK);
        staged.read_exact(&mut buffer[..take])?;
        target.write_all(&buffer[..take])?;
        written = written.saturating_add(take as u64);
        remaining -= take as u64;
        journal.published = written;
        write_json_atomic(journal_path, journal)?;
    }
    Ok(())
}

fn complete_publication(
    layout: &LocalLayout,
    journal_path: &Path,
    journal: &PublicationJournal,
    staged_path: &Path,
) -> Result<()> {
    let mut completed = journal.clone();
    completed.complete = true;
    completed.published = journal.bytes;
    write_json_atomic(journal_path, &completed)?;
    remove_if_present(staged_path)?;
    remove_if_present(journal_path)?;
    sync_dir(&layout.pending)?;
    sync_dir(&layout.tmp)?;
    Ok(())
}

/// Flush the directory entry that names a finished blob target.
fn sync_target_entry(target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

/// Flush a verified target before its publication journal is retired: readable
/// bytes in the page cache are not a completed publication.
fn sync_published_target(target: &Path) -> Result<()> {
    let file = OpenOptions::new().append(true).open(target)?;
    file.sync_all()?;
    drop(file);
    sync_target_entry(target)
}

fn verify_target(target: &Path, key: &PublicationKey) -> Result<()> {
    let metadata = fs::symlink_metadata(target)
        .map_err(|_| blob_corrupt("blob target is missing or unreadable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(blob_corrupt("blob target is not a regular file"));
    }
    if metadata.len() == 0 || metadata.len() > MAX_BLOB_BYTES {
        return Err(blob_corrupt("blob target has an invalid length"));
    }
    if sha256_file(target)? != key.sha256 {
        return Err(blob_corrupt("blob target digest does not match"));
    }
    Ok(())
}

fn verify_prefix(target: &Path, staged_path: &Path, length: u64) -> Result<()> {
    let mut target_file = File::open(target)?;
    let mut staged_file = File::open(staged_path)?;
    let mut remaining = length;
    let mut target_buffer = vec![0_u8; COPY_CHUNK];
    let mut staged_buffer = vec![0_u8; COPY_CHUNK];
    while remaining > 0 {
        let take = usize::try_from(remaining.min(COPY_CHUNK as u64)).unwrap_or(COPY_CHUNK);
        target_file.read_exact(&mut target_buffer[..take])?;
        staged_file.read_exact(&mut staged_buffer[..take])?;
        if target_buffer[..take] != staged_buffer[..take] {
            return Err(blob_corrupt(
                "partial blob target conflicts with the durable staged bytes",
            ));
        }
        remaining -= take as u64;
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<Sha256> {
    let mut file = File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0_u8; COPY_CHUNK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Sha256::parse(&hex::encode(hasher.finalize()))
}

fn parse_journal(value: serde_json::Value, key: &PublicationKey) -> Result<PublicationJournal> {
    let version = value
        .get("v")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| blob_corrupt("publication journal has no version"))?;
    if version != u64::from(JOURNAL_VERSION) {
        return Err(AgendarError::new(
            ErrorCode::Unsupported,
            format!("Publication journal version {version} is not supported."),
        )
        .with_detail("blob_journal_version")
        .with_hint("Use the Agendar version that created this local journal."));
    }
    let journal: PublicationJournal = serde_json::from_value(value)
        .map_err(|_| blob_corrupt("publication journal is malformed"))?;
    if journal.project != key.project
        || journal.host != key.host
        || journal.sha256 != key.sha256
        || journal.ext != key.ext
    {
        return Err(blob_corrupt("publication journal does not match its key"));
    }
    Ok(journal)
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn media_pending(detail: &str) -> AgendarError {
    AgendarError::new(ErrorCode::NotFound, "Blob media is not available yet.")
        .with_detail(detail)
        .with_hint("The blob may still be syncing; retry after the media completes.")
        .with_retryable(true)
}

fn blob_corrupt(detail: &str) -> AgendarError {
    AgendarError::new(ErrorCode::BlobCorrupt, "Blob is incomplete or corrupt.")
        .with_detail(detail)
        .with_hint("Inspect the blob; never overwrite or truncate Vault bytes.")
}

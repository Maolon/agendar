//! Contained Vault paths for writer segments and content-addressed blobs.
//!
//! This module owns the Vault root and its create-once manifest: only a
//! user-selected canonical root is trusted, every path below it is joined
//! component by component with symlink/escape rejection, and every directory
//! this code creates is durably published entry by entry.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::{
    BlobExt, HostId, ProjectSlug, Sha256, SourcePath, UtcTime, VaultId, WriterId, WriterRole,
};

use crate::runtime::local_state::{ensure_dir, sync_dir};
use crate::vault::writer::{Clock, IdSource};

const MANIFEST_FILE: &str = "vault.json";
const MANIFEST_VERSION: u8 = 1;
const MAX_MANIFEST_BYTES: u64 = 4_096;

/// Create-once Vault manifest written by `init`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultManifest {
    pub v: u8,
    pub id: VaultId,
    pub created_at: UtcTime,
}

/// A verified, opened Vault rooted at one canonical directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vault {
    root: PathBuf,
    manifest: VaultManifest,
    created: bool,
}

impl Vault {
    /// Initialize a Vault or open the existing one. The manifest is created
    /// with `create_new`; initialization never changes an existing id or
    /// timestamp.
    pub fn init(root: &Path, clock: &dyn Clock, ids: &mut dyn IdSource) -> Result<Self> {
        ensure_dir(root)?;
        let canonical = root.canonicalize()?;
        if !canonical.is_dir() {
            return Err(AgendarError::validation_at(
                "/vault",
                "not_a_directory",
                "The Vault root must be a directory.",
            ));
        }
        let manifest_path = canonical.join(MANIFEST_FILE);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest_path)
        {
            Ok(mut file) => {
                let id = VaultId::parse(ids.next_event_id().as_str())?;
                let manifest = VaultManifest {
                    v: MANIFEST_VERSION,
                    id,
                    created_at: clock.now(),
                };
                let mut bytes = serde_json::to_vec(&manifest).map_err(|error| {
                    AgendarError::new(
                        ErrorCode::Io,
                        format!("Cannot serialize the Vault manifest: {error}."),
                    )
                })?;
                bytes.push(b'\n');
                if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
                    drop(file);
                    // The manifest was created by this call and never held a
                    // usable id; remove the partial file so a retry can init.
                    let _ = fs::remove_file(&manifest_path);
                    return Err(error.into());
                }
                drop(file);
                ensure_dir(&canonical.join("projects"))?;
                sync_dir(&canonical)?;
                Ok(Self {
                    root: canonical,
                    manifest,
                    created: true,
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::open(&canonical)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Open a Vault from its canonical root and validate the create-once
    /// manifest.
    pub fn open(root: &Path) -> Result<Self> {
        let canonical = root
            .canonicalize()
            .map_err(|error| vault_not_found(error, "vault_root_missing"))?;
        let manifest_path = canonical.join(MANIFEST_FILE);
        let metadata = fs::symlink_metadata(&manifest_path)
            .map_err(|error| vault_not_found(error, "vault_missing"))?;
        if metadata.file_type().is_symlink() {
            return Err(AgendarError::validation_at(
                "/vault.json",
                "vault_symlink",
                "The Vault manifest must not be a symbolic link.",
            ));
        }
        if !metadata.is_file() {
            return Err(AgendarError::validation_at(
                "/vault.json",
                "vault_manifest_not_file",
                "The Vault manifest must be a regular file.",
            ));
        }
        if metadata.len() > MAX_MANIFEST_BYTES {
            return Err(AgendarError::validation_at(
                "/vault.json",
                "vault_manifest_too_large",
                "The Vault manifest is larger than any supported version.",
            ));
        }
        let bytes = fs::read(&manifest_path)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            AgendarError::validation_at(
                "/vault.json",
                "vault_manifest_malformed",
                format!("The Vault manifest is not valid JSON: {error}."),
            )
        })?;
        let version = value
            .get("v")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                AgendarError::validation_at(
                    "/vault.json",
                    "vault_manifest_missing_version",
                    "The Vault manifest has no version field.",
                )
            })?;
        if version != u64::from(MANIFEST_VERSION) {
            return Err(AgendarError::new(
                ErrorCode::Unsupported,
                format!("Vault format version {version} is not supported."),
            )
            .with_detail("vault_manifest_version")
            .with_hint("Open this Vault with a client that supports its format version."));
        }
        let manifest: VaultManifest = serde_json::from_value(value).map_err(|error| {
            AgendarError::validation_at(
                "/vault.json",
                "vault_manifest_shape",
                format!("The Vault manifest has an unexpected shape: {error}."),
            )
        })?;
        Ok(Self {
            root: canonical,
            manifest,
            created: false,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn id(&self) -> &VaultId {
        &self.manifest.id
    }

    pub fn manifest(&self) -> &VaultManifest {
        &self.manifest
    }

    /// True only for the call that created the manifest.
    pub const fn created(&self) -> bool {
        self.created
    }

    /// Contained project directory. It is not created here; the caller creates
    /// it through the checked helpers when writing.
    pub fn project_dir(&self, project: &ProjectSlug) -> Result<PathBuf> {
        contained_path(&self.root, &format!("projects/{}", project.as_str()))
    }
}

/// True when `root` already holds a Vault manifest entry. A data directory in
/// such a place would put local config inside a Vault tree, so setup commands
/// reject it even when no Vault was selected. Any entry named `vault.json`
/// counts, including an unreadable or malformed one; only a missing entry means
/// "not a Vault".
pub fn contains_vault_manifest(root: &Path) -> bool {
    fs::symlink_metadata(root.join(MANIFEST_FILE)).is_ok()
}

/// `detail` says what is missing: the folder (`vault_root_missing`) or the
/// manifest in it (`vault_missing`).
fn vault_not_found(error: std::io::Error, detail: &str) -> AgendarError {
    if error.kind() == std::io::ErrorKind::NotFound {
        AgendarError::new(ErrorCode::NotFound, "Vault was not found.")
            .with_detail(detail)
            .with_hint(format!(
                "Run `{} init` or supply an existing Vault path.",
                crate::service::cli_token()
            ))
    } else {
        AgendarError::from(error)
    }
}

/// Exact writer base name for a role: `operator`, `steward` or `s-<SessionId>`.
fn writer_base(writer: &WriterId) -> String {
    match writer.role() {
        WriterRole::Operator => "operator".to_owned(),
        WriterRole::Steward => "steward".to_owned(),
        WriterRole::Agent(session) => format!("s-{session}"),
    }
}

/// `operator.jsonl` for segment 1, `operator.2.jsonl` and up for later segments.
pub fn writer_file_name(writer: &WriterId, segment: u32) -> Result<String> {
    if segment == 0 {
        return Err(AgendarError::validation_at(
            "/segment",
            "writer_segment",
            "Writer segment numbers start at 1.",
        ));
    }
    let base = writer_base(writer);
    Ok(if segment == 1 {
        format!("{base}.jsonl")
    } else {
        format!("{base}.{segment}.jsonl")
    })
}

/// Relative path of one writer segment, verified against the model grammar.
pub fn writer_relative_path(
    project: &ProjectSlug,
    writer: &WriterId,
    segment: u32,
) -> Result<String> {
    let file = writer_file_name(writer, segment)?;
    let relative = format!("projects/{}/{}/{}", project.as_str(), writer.host(), file);
    let parsed = SourcePath::from_relative_path(&relative)?;
    if parsed.project != *project || parsed.writer != *writer || parsed.segment != segment {
        return Err(AgendarError::validation_at(
            "/source",
            "source_path_mismatch",
            "The generated writer path does not match its project, writer and segment.",
        ));
    }
    Ok(relative)
}

/// Relative directory holding one host's writer streams and blobs.
pub fn host_relative_path(project: &ProjectSlug, host: &HostId) -> String {
    format!("projects/{}/{host}", project.as_str())
}

/// Relative content-addressed blob target.
pub fn blob_relative_path(
    project: &ProjectSlug,
    host: &HostId,
    sha256: &Sha256,
    ext: BlobExt,
) -> String {
    format!(
        "projects/{}/{host}/blobs/{sha256}.{}",
        project.as_str(),
        ext.as_str()
    )
}

/// Join `relative` below `root`, rejecting traversal, separators in
/// components, existing symlinks and pre-existing non-directory components.
pub fn contained_path(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.is_empty() {
        return Err(path_error("Vault path must not be empty."));
    }
    if relative.starts_with('/') || relative.contains('\\') || relative.contains('\0') {
        return Err(path_error(
            "Vault path must be a relative slash-separated path.",
        ));
    }
    if relative.chars().any(char::is_control) {
        return Err(path_error(
            "Vault path must not contain control characters.",
        ));
    }

    let mut current = root.to_path_buf();
    let mut missing = false;
    let components: Vec<&str> = relative.split('/').collect();
    for (index, component) in components.iter().enumerate() {
        if component.is_empty() || *component == "." || *component == ".." {
            return Err(path_error(
                "Vault path must not contain empty, `.` or `..` components.",
            ));
        }
        current.push(component);
        if missing {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(AgendarError::validation_at(
                        "/vault-path",
                        "vault_symlink",
                        "The Vault must not be traversed through symbolic links.",
                    ));
                }
                let is_last = index + 1 == components.len();
                if !is_last && !metadata.is_dir() {
                    return Err(AgendarError::validation_at(
                        "/vault-path",
                        "vault_path_not_directory",
                        "A Vault path component is not a directory.",
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing = true,
            Err(error) => return Err(error.into()),
        }
    }

    if !current.starts_with(root) {
        return Err(path_error(
            "The resolved Vault path escapes the Vault root.",
        ));
    }
    Ok(current)
}

/// Create a directory below the Vault, verifying containment before and after
/// creation and fsyncing every new parent entry where supported. Each component
/// is created separately so `<new-project>` and `<host>` both publish the entry
/// that names them.
pub fn ensure_directory(root: &Path, relative: &str) -> Result<PathBuf> {
    let directory = contained_path(root, relative)?;
    ensure_dir(&directory)?;
    let canonical = directory.canonicalize()?;
    if !canonical.starts_with(root) {
        return Err(AgendarError::validation_at(
            "/vault-path",
            "vault_escape",
            "A Vault directory resolves outside the Vault root.",
        ));
    }
    // Re-check the assembled path now that every component exists.
    let verified = contained_path(root, relative)?;
    if verified != directory {
        return Err(AgendarError::validation_at(
            "/vault-path",
            "vault_escape",
            "A Vault directory changed while it was being created.",
        ));
    }
    Ok(directory)
}

/// Create the host directory (`projects/<slug>/<host>`) for a writer or blob.
pub fn ensure_host_directory(root: &Path, project: &ProjectSlug, host: &HostId) -> Result<PathBuf> {
    ensure_directory(root, &host_relative_path(project, host))
}

/// Read the final byte of a file, if any. Used to detect an unfinished tail.
pub fn last_byte_is_lf(path: &Path) -> Result<Option<bool>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let size = file.metadata()?.len();
    if size == 0 {
        return Ok(Some(true));
    }
    file.seek(SeekFrom::Start(size - 1))?;
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte)?;
    Ok(Some(byte[0] == b'\n'))
}

fn path_error(message: &str) -> AgendarError {
    AgendarError::new(ErrorCode::Validation, message).with_detail("vault_path")
}

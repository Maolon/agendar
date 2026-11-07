//! Local data-root layout and Vault/data separation.
//!
//! The data root holds identity, config, preferences and the derived
//! per-Vault state (index, runtime journal, locks, pending blobs, temporary
//! staging). None of it may live inside the Vault; the checks here run before
//! any local directory or database file is created and again after creation, so
//! an alias or symlink cannot silently place derived state in the synced tree.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::VaultId;

use super::local_state::ensure_dir;

/// A path as people and agents read it: on Windows without the `\\?\`
/// verbatim prefix that `canonicalize` adds (`\\?\C:\x` becomes `C:\x`,
/// `\\?\UNC\host\share` becomes `\\host\share`). Other paths are unchanged.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return rest.to_owned();
    }
    text.into_owned()
}

/// Local data root plus every derived path used by the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPaths {
    data_root: PathBuf,
}

impl LocalPaths {
    /// Resolve a user-selected data root. Existing path components (including
    /// symlinks) are canonicalized; missing trailing components are appended
    /// lexically after checking that they are ordinary names.
    pub fn new(data_root: impl AsRef<Path>) -> Result<Self> {
        let resolved =
            resolve_directory(data_root.as_ref(), "/data-dir", "The local data directory")?;
        Ok(Self {
            data_root: resolved,
        })
    }

    /// Default local data root: the OS data directory followed by the app
    /// identifier The caller still decides whether
    /// this path is usable and never assumes it is selected.
    pub fn default_data_root() -> Result<PathBuf> {
        let base = directories::BaseDirs::new().ok_or_else(|| {
            AgendarError::new(
                ErrorCode::Context,
                "No OS application data directory is available.",
            )
            .with_hint("Set AGENDAR_DATA_DIR to a writable directory.")
        })?;
        Ok(base.data_dir().join("dev.agendar.app"))
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn identity_path(&self) -> PathBuf {
        self.data_root.join("identity.json")
    }

    pub fn identity_lock_path(&self) -> PathBuf {
        self.data_root.join("identity.lock")
    }

    pub fn config_path(&self) -> PathBuf {
        self.data_root.join("config.json")
    }

    pub fn preferences_path(&self) -> PathBuf {
        self.data_root.join("preferences.json")
    }

    pub fn vault_local_dir(&self, vault: &VaultId) -> PathBuf {
        self.data_root.join("vaults").join(vault.as_str())
    }

    pub fn locks_dir(&self, vault: &VaultId) -> PathBuf {
        self.vault_local_dir(vault).join("locks")
    }

    pub fn pending_dir(&self, vault: &VaultId) -> PathBuf {
        self.vault_local_dir(vault).join("pending")
    }

    pub fn tmp_dir(&self, vault: &VaultId) -> PathBuf {
        self.vault_local_dir(vault).join("tmp")
    }

    pub fn runtime_path(&self, vault: &VaultId) -> PathBuf {
        self.vault_local_dir(vault).join("runtime.json")
    }

    pub fn index_path(&self, vault: &VaultId) -> PathBuf {
        self.vault_local_dir(vault).join("index.sqlite")
    }

    /// Reject a Vault whose canonical root equals, contains or is contained by
    /// the canonical data root (including the `<data>/vaults` ancestor case).
    pub fn validate_against_vault_root(&self, vault_root: &Path) -> Result<()> {
        let vault = canonical_existing_directory(vault_root, "Vault")?;
        self.check_separation(&vault)
    }

    /// The same separation check for a selected Vault that does not exist yet.
    /// `project link` runs before `init` creates the Vault, so the path gets
    /// the data-root treatment: existing components are canonicalized and
    /// missing trailing components are appended lexically.
    pub fn validate_against_vault_selection(&self, vault_root: &Path) -> Result<()> {
        let vault = resolve_directory(vault_root, "/vault", "The Vault")?;
        self.check_separation(&vault)
    }

    /// Reject a data root that already holds a Vault manifest, even when the
    /// invocation selected no Vault: local config would land in the synced
    /// tree.
    pub fn validate_not_a_vault(&self) -> Result<()> {
        if crate::vault::contains_vault_manifest(&self.data_root) {
            return Err(separation_error(
                "The local data directory must not be a Vault.",
            ));
        }
        Ok(())
    }

    /// Equality and both nesting directions, applied to one canonical Vault.
    fn check_separation(&self, vault: &Path) -> Result<()> {
        if self.data_root == vault {
            return Err(separation_error(
                "The Vault and the local data directory must be different directories.",
            ));
        }
        if self.data_root.starts_with(vault) {
            return Err(separation_error(
                "The local data directory must not be inside the Vault.",
            ));
        }
        if vault.starts_with(&self.data_root) {
            return Err(separation_error(
                "The Vault must not be inside the local data directory.",
            ));
        }
        Ok(())
    }

    /// Validate separation, then create and re-verify the per-Vault local
    /// directories before any lock, journal or database file is written.
    pub fn ensure_layout(&self, vault_root: &Path, vault_id: &VaultId) -> Result<LocalLayout> {
        self.validate_against_vault_root(vault_root)?;
        let vault_path = canonical_existing_directory(vault_root, "Vault")?;
        // The data root may not exist yet. Create it once, then verify it still
        // resolves to the canonical directory recorded at construction.
        ensure_dir(&self.data_root)?;
        if self.data_root.canonicalize()? != self.data_root {
            return Err(AgendarError::validation_at(
                "/data-dir",
                "local_state_outside_data_root",
                "The local data directory changed while it was being created.",
            ));
        }
        let layout = LocalLayout {
            vault_local: self.vault_local_dir(vault_id),
            locks: self.locks_dir(vault_id),
            pending: self.pending_dir(vault_id),
            tmp: self.tmp_dir(vault_id),
        };
        for directory in [
            &layout.vault_local,
            &layout.locks,
            &layout.pending,
            &layout.tmp,
        ] {
            create_dir_checked(directory, &self.data_root, &vault_path)?;
        }
        Ok(layout)
    }
}

/// Local directories that must exist before a Vault mutation is admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalLayout {
    pub vault_local: PathBuf,
    pub locks: PathBuf,
    pub pending: PathBuf,
    pub tmp: PathBuf,
}

fn separation_error(message: &str) -> AgendarError {
    AgendarError::validation_at("/data-dir", "vault_data_separation", message)
        .with_hint("Choose disjoint Vault and data-directory paths, then retry.")
}

fn canonical_existing_directory(path: &Path, what: &str) -> Result<PathBuf> {
    let metadata = fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AgendarError::new(ErrorCode::NotFound, format!("{what} was not found."))
                .with_detail("missing_directory")
                .with_hint(format!(
                    "Run `{} init` or supply an existing path.",
                    crate::service::cli_token()
                ))
        } else {
            AgendarError::from(error)
        }
    })?;
    if !metadata.is_dir() {
        return Err(AgendarError::validation_at(
            "/data-dir",
            "not_a_directory",
            format!("{what} must be a directory."),
        ));
    }
    Ok(path.canonicalize()?)
}

/// Resolve a possibly-not-yet-created directory: canonicalize the nearest
/// existing ancestor, then append the remaining ordinary components. `field`
/// and `what` name the user-selected path in errors (data root or Vault).
fn resolve_directory(path: &Path, field: &str, what: &str) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(AgendarError::validation_at(
            field,
            "empty_path",
            format!("{what} must not be empty."),
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    let mut prefix = absolute.clone();
    let mut missing: Vec<OsString> = Vec::new();
    loop {
        match fs::symlink_metadata(&prefix) {
            Ok(_) => {
                let canonical = prefix.canonicalize()?;
                if !canonical.is_dir() {
                    return Err(AgendarError::validation_at(
                        field,
                        "not_a_directory",
                        format!("{what} must be a directory."),
                    ));
                }
                let mut resolved = canonical;
                for component in missing.iter().rev() {
                    let text = component.to_string_lossy();
                    if text.is_empty() || text == "." || text == ".." {
                        return Err(AgendarError::validation_at(
                            field,
                            "invalid_path",
                            format!("{what} must not contain `.` or `..` components."),
                        ));
                    }
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = prefix.file_name() else {
                    return Err(AgendarError::validation_at(
                        field,
                        "invalid_path",
                        format!("{what} has no existing ancestor."),
                    ));
                };
                missing.push(name.to_os_string());
                if !prefix.pop() {
                    return Err(AgendarError::validation_at(
                        field,
                        "invalid_path",
                        format!("{what} has no existing ancestor."),
                    ));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Create `directory`, rejecting aliases that resolve outside the data root or
/// into the Vault before and after creation.
fn create_dir_checked(directory: &Path, data_root: &Path, vault: &Path) -> Result<()> {
    let existing = nearest_existing(directory)?;
    let canonical_existing = existing.canonicalize()?;
    if !canonical_existing.is_dir() {
        return Err(AgendarError::validation_at(
            "/data-dir",
            "not_a_directory",
            "A local data path component is not a directory.",
        ));
    }
    if !canonical_existing.starts_with(data_root) {
        return Err(AgendarError::validation_at(
            "/data-dir",
            "local_state_outside_data_root",
            "Local state must stay inside the selected data directory.",
        ));
    }
    if canonical_existing == vault || canonical_existing.starts_with(vault) {
        return Err(separation_error(
            "The local data directory resolves inside the Vault.",
        ));
    }

    ensure_dir(directory)?;
    let canonical = directory.canonicalize()?;
    if !canonical.starts_with(data_root) {
        return Err(AgendarError::validation_at(
            "/data-dir",
            "local_state_outside_data_root",
            "Local state must stay inside the selected data directory.",
        ));
    }
    if canonical == vault || canonical.starts_with(vault) {
        return Err(separation_error(
            "The local data directory resolves inside the Vault.",
        ));
    }
    // `ensure_dir` fsyncs the parent entry of every component it creates and
    // re-syncs the deepest already-visible one, so each new
    // `<vaults>/<id>/<dir>` entry is durable without a second sync.
    Ok(())
}

fn nearest_existing(path: &Path) -> Result<PathBuf> {
    let mut current = path.to_path_buf();
    loop {
        match fs::symlink_metadata(&current) {
            Ok(_) => return Ok(current),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !current.pop() {
                    return Err(AgendarError::validation_at(
                        "/data-dir",
                        "invalid_path",
                        "The local data path has no existing ancestor.",
                    ));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod display_tests {
    use std::path::Path;

    use super::display_path;

    #[test]
    fn verbatim_windows_paths_read_as_plain_paths() {
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Users\a\vault")),
            r"C:\Users\a\vault"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\host\share\vault")),
            r"\\host\share\vault"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\GLOBALROOT\x")),
            r"\\?\GLOBALROOT\x"
        );
        assert_eq!(display_path(Path::new("/Users/a/vault")), "/Users/a/vault");
    }
}

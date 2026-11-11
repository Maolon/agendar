//! `Session::open`, and whose fault a failed open is.
//!
//! The open runs in three stages so the error can say which side failed
//! without anyone reading its sentence: the Vault folder and its manifest,
//! then this device's data directory and identity, then the service over
//! both. Every error the core raises with a detail keeps it (`vault_*` for
//! the Vault, `local_state_*`, `identity_*` and the path details for this
//! device). A plain file error has no detail in the core, so the stage it
//! came from names it.
#![forbid(unsafe_code)]

use std::path::Path;

use agendar_core::error::details;
use agendar_core::index::source_reads::{PlaceholderBits, content_not_local, placeholder_bits};
use agendar_core::runtime::{HostIdentity, LocalPaths};
use agendar_core::service::Service;
use agendar_core::vault::{Clock, SystemClock, Vault};
use agendar_core::{AgendarError, ErrorCode};

use super::{FfiError, Session};

const MANIFEST: &str = "vault.json";

impl Session {
    /// Open an existing Vault as one more host. The local host identity is
    /// created in `data_dir` when missing; a Vault is never created, so a
    /// wrongly picked folder is an error instead of a new empty Vault.
    ///
    /// The data directory is checked against the Vault before the identity is
    /// written, so a data directory inside the Vault is refused with nothing
    /// created there.
    pub fn open(vault_dir: String, data_dir: String) -> Result<Self, FfiError> {
        let vault_dir = Path::new(&vault_dir);
        let vault = Vault::open(vault_dir).map_err(|error| vault_fault(vault_dir, error))?;
        let local = LocalPaths::new(&data_dir).map_err(local_fault)?;
        local
            .validate_against_vault_root(vault.root())
            .map_err(local_fault)?;
        HostIdentity::load_or_create(local.data_root(), SystemClock.now()).map_err(local_fault)?;
        // The Vault was read a moment ago, so a plain file error from here on
        // is taken to be about the data directory.
        let service = Service::open(vault.root(), local.data_root()).map_err(local_fault)?;
        Ok(Self { service })
    }
}

/// A plain file error (`E_IO` without a detail), the only kind the core
/// cannot place.
fn unplaced(error: &AgendarError) -> bool {
    error.code() == ErrorCode::Io && error.detail().is_none()
}

/// An error from reading the Vault folder and its manifest.
fn vault_fault(vault_dir: &Path, error: AgendarError) -> FfiError {
    if !unplaced(&error) {
        return error.into();
    }
    let bits = std::fs::symlink_metadata(vault_dir.join(MANIFEST))
        .ok()
        .map(|metadata| placeholder_bits(&metadata));
    let (detail, field, retryable) = manifest_read_fault(bits);
    error
        .with_detail(detail)
        .with_field(field)
        .with_retryable(retryable)
        .into()
}

/// Why the manifest could not be read, from what its metadata says. A
/// placeholder of the sync app is the one case the core can tell: the content
/// is not on this device yet and a later open can succeed. Anything else (no
/// access to the folder, offline without a placeholder flag, a failing disk)
/// it cannot tell apart, so that is not marked retryable.
fn manifest_read_fault(bits: Option<PlaceholderBits>) -> (&'static str, &'static str, bool) {
    match bits {
        Some(bits) if content_not_local(bits) => {
            (details::VAULT_MANIFEST_NOT_ON_DEVICE, "/vault.json", true)
        }
        _ => (details::VAULT_UNREADABLE, "/vault", false),
    }
}

/// An error from this device's data directory.
fn local_fault(error: AgendarError) -> FfiError {
    if !unplaced(&error) {
        return error.into();
    }
    error
        .with_detail(details::LOCAL_DATA_UNREADABLE)
        .with_field("/data-dir")
        .into()
}

#[cfg(test)]
mod tests {
    use agendar_core::index::source_reads::{
        FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, PlaceholderBits, SF_DATALESS,
    };

    use super::*;

    fn plain() -> AgendarError {
        AgendarError::from(std::io::Error::other("no"))
    }

    #[test]
    fn a_placeholder_manifest_is_not_on_this_device_and_can_be_tried_again() {
        for bits in [
            PlaceholderBits {
                st_flags: SF_DATALESS,
                file_attributes: 0,
            },
            PlaceholderBits {
                st_flags: 0,
                file_attributes: FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
            },
        ] {
            assert_eq!(
                manifest_read_fault(Some(bits)),
                ("vault_manifest_not_on_device", "/vault.json", true)
            );
        }
        // The file is here in full, or its metadata cannot be read at all.
        for bits in [Some(PlaceholderBits::default()), None] {
            assert_eq!(
                manifest_read_fault(bits),
                ("vault_unreadable", "/vault", false)
            );
        }
    }

    #[test]
    fn only_a_plain_file_error_is_named_by_its_stage() {
        let FfiError::Core { detail, field, .. } = local_fault(plain()) else {
            panic!("a core error");
        };
        assert_eq!(detail.as_deref(), Some("local_data_unreadable"));
        assert_eq!(field.as_deref(), Some("/data-dir"));

        let named = AgendarError::validation_at("/identity", "identity_malformed", "Bad.");
        let missing = Path::new("no-such-folder");
        for crossed in [
            local_fault(named.clone()),
            vault_fault(missing, named.clone()),
        ] {
            assert_eq!(crossed, FfiError::from(named.clone()));
        }
        let FfiError::Core {
            detail, retryable, ..
        } = vault_fault(missing, plain())
        else {
            panic!("a core error");
        };
        assert_eq!(detail.as_deref(), Some("vault_unreadable"));
        assert!(!retryable);
    }
}

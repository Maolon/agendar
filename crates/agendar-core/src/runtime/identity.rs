//! Locally persisted host identity.
//!
//! The host ID is randomly generated on this machine, never derived from a
//! hostname, user name or network address, and lives in `<data>/identity.json`
//! outside every Vault.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::{HostId, UtcTime};

use super::local_state::{acquire_lock, ensure_dir, read_json_value, write_json_atomic};

const IDENTITY_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostIdentity {
    pub host: HostId,
    pub created_at: UtcTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityFile {
    v: u8,
    host: HostId,
    created_at: UtcTime,
}

impl HostIdentity {
    /// Load an existing identity without creating anything.
    pub fn load(data_root: &Path) -> Result<Option<Self>> {
        let path = data_root.join("identity.json");
        let Some(value) = read_json_value(&path)? else {
            return Ok(None);
        };
        let version = value
            .get("v")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                AgendarError::validation_at(
                    "/identity",
                    "identity_missing_version",
                    "identity.json has no version field.",
                )
            })?;
        if version != u64::from(IDENTITY_VERSION) {
            return Err(AgendarError::new(
                ErrorCode::Unsupported,
                format!("Identity version {version} is not supported."),
            )
            .with_detail("identity_version")
            .with_hint("Use a client version that understands this identity file."));
        }
        let file: IdentityFile = serde_json::from_value(value).map_err(|error| {
            AgendarError::validation_at(
                "/identity",
                "identity_malformed",
                format!("identity.json has an unexpected shape: {error}."),
            )
        })?;
        Ok(Some(Self {
            host: file.host,
            created_at: file.created_at,
        }))
    }

    /// Load the persisted host identity, creating it exactly once under the
    /// local identity lock. The caller validates Vault/data separation before
    /// invoking this function.
    pub fn load_or_create(data_root: &Path, now: UtcTime) -> Result<Self> {
        ensure_dir(data_root)?;
        let lock_path = data_root.join("identity.lock");
        let _lock = acquire_lock(&lock_path)?;
        if let Some(identity) = Self::load(data_root)? {
            return Ok(identity);
        }
        let host = HostId::parse(&format!(
            "h-{}",
            ulid::Ulid::generate().to_string().to_ascii_lowercase()
        ))?;
        let identity = Self {
            host: host.clone(),
            created_at: now,
        };
        let file = IdentityFile {
            v: IDENTITY_VERSION,
            host,
            created_at: now,
        };
        write_json_atomic(&data_root.join("identity.json"), &file)?;
        Ok(identity)
    }
}

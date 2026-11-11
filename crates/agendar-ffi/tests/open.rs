#![forbid(unsafe_code)]
#![cfg(unix)]

//! What a failed `Session::open` says, row by row: the code, the detail that
//! names the fault, the input or file it is about, and whether trying again
//! can help. The first table is the Vault folder and its manifest, the second
//! this device's own data directory. A mobile client tells the two apart by
//! the detail alone.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use agendar_core::error::details;
use agendar_core::runtime::local_state::{acquire_lock, set_lock_wait};
use agendar_core::service::Service;
use agendar_ffi::{FfiError, Session};

const MANIFEST: &str =
    r#"{"v":1,"id":"00000000000000000000000001","created_at":"2026-01-01T00:00:00.000Z"}"#;

/// What one row expects: code, detail, field, retryable.
type Expected = (&'static str, &'static str, Option<&'static str>, bool);
/// Arranges a folder to pick, and returns it.
type Picked = Box<dyn Fn(&Place) -> PathBuf>;
/// Arranges a data directory beside the Vault given, and returns it.
type DataDir = Box<dyn Fn(&Place, &Path) -> PathBuf>;

struct Place {
    dir: tempfile::TempDir,
}

impl Place {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    /// A folder to pick, not a Vault yet.
    fn folder(&self) -> PathBuf {
        let folder = self.dir.path().join("picked");
        fs::create_dir_all(&folder).expect("folder");
        folder
    }

    /// A Vault another host made.
    fn vault(&self) -> PathBuf {
        let vault = self.dir.path().join("vault");
        Service::init(&vault, &self.dir.path().join("mac")).expect("init");
        vault
    }

    /// This device's data directory, made.
    fn data(&self) -> PathBuf {
        let data = self.dir.path().join("mobile");
        fs::create_dir_all(&data).expect("data");
        data
    }
}

fn open(vault: &Path, data: &Path) -> Result<Session, FfiError> {
    Session::open(vault.display().to_string(), data.display().to_string())
}

#[track_caller]
fn fails(name: &str, result: Result<Session, FfiError>, expected: Expected) {
    let (want_code, want_detail, want_field, want_retryable) = expected;
    match result.err() {
        Some(FfiError::Core {
            code,
            detail,
            field,
            retryable,
            message,
        }) => {
            assert_eq!(code, want_code, "{name}: {message}");
            assert_eq!(detail.as_deref(), Some(want_detail), "{name}: {message}");
            assert_eq!(field.as_deref(), want_field, "{name}: {message}");
            assert_eq!(retryable, want_retryable, "{name}: {message}");
            assert_eq!(details::listed(want_detail), Some(want_detail), "{name}");
            // The detail is a field now; the sentence does not repeat it.
            assert!(!message.is_empty() && !message.ends_with(']'), "{message}");
        }
        other => panic!("{name}: expected {want_code} ({want_detail}), got {other:?}"),
    }
}

/// A picked folder with this `vault.json`.
fn with_manifest(place: &Place, bytes: &[u8]) -> PathBuf {
    let folder = place.folder();
    fs::write(folder.join("vault.json"), bytes).expect("manifest");
    folder
}

#[test]
fn a_fault_of_the_vault_folder_or_its_manifest_names_itself() {
    let invalid = "E_VALIDATION";
    let at_manifest = Some("/vault.json");
    let rows: Vec<(&str, Picked, Expected)> = vec![
        (
            "no such folder",
            Box::new(|place| place.dir.path().join("gone")),
            ("E_NOT_FOUND", "vault_root_missing", None, false),
        ),
        (
            "no vault.json",
            Box::new(Place::folder),
            ("E_NOT_FOUND", "vault_missing", None, false),
        ),
        (
            "vault.json is a symlink",
            Box::new(|place| {
                let folder = place.folder();
                let real = place.dir.path().join("real.json");
                fs::write(&real, MANIFEST).expect("manifest");
                symlink(&real, folder.join("vault.json")).expect("symlink");
                folder
            }),
            (invalid, "vault_symlink", at_manifest, false),
        ),
        (
            "vault.json is a folder",
            Box::new(|place| {
                let folder = place.folder();
                fs::create_dir(folder.join("vault.json")).expect("dir");
                folder
            }),
            (invalid, "vault_manifest_not_file", at_manifest, false),
        ),
        (
            "too large",
            Box::new(|place| with_manifest(place, &[b' '; 4_097])),
            (invalid, "vault_manifest_too_large", at_manifest, false),
        ),
        (
            "not JSON",
            Box::new(|place| with_manifest(place, b"vault")),
            (invalid, "vault_manifest_malformed", at_manifest, false),
        ),
        (
            "empty",
            Box::new(|place| with_manifest(place, b"")),
            (invalid, "vault_manifest_malformed", at_manifest, false),
        ),
        (
            "no version",
            Box::new(|place| with_manifest(place, br#"{"id":"00000000000000000000000001"}"#)),
            (
                invalid,
                "vault_manifest_missing_version",
                at_manifest,
                false,
            ),
        ),
        (
            "a version this core does not read",
            Box::new(|place| {
                with_manifest(place, MANIFEST.replace("\"v\":1", "\"v\":2").as_bytes())
            }),
            ("E_UNSUPPORTED", "vault_manifest_version", None, false),
        ),
        (
            "a field missing",
            Box::new(|place| with_manifest(place, br#"{"v":1}"#)),
            (invalid, "vault_manifest_shape", at_manifest, false),
        ),
        (
            "a field too many",
            Box::new(|place| {
                with_manifest(place, MANIFEST.replace('}', ",\"extra\":true}").as_bytes())
            }),
            (invalid, "vault_manifest_shape", at_manifest, false),
        ),
        (
            "vault.json cannot be read",
            Box::new(|place| {
                let folder = with_manifest(place, MANIFEST.as_bytes());
                let manifest = folder.join("vault.json");
                fs::set_permissions(&manifest, fs::Permissions::from_mode(0o000)).expect("chmod");
                folder
            }),
            ("E_IO", "vault_unreadable", Some("/vault"), false),
        ),
        (
            "the picked path is a file",
            Box::new(|place| {
                let file = place.dir.path().join("a-file");
                fs::write(&file, MANIFEST).expect("file");
                file
            }),
            ("E_IO", "vault_unreadable", Some("/vault"), false),
        ),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for (name, arrange, expected) in rows {
        let place = Place::new();
        let picked = arrange(&place);
        let data = place.dir.path().join("mobile");
        fails(name, open(&picked, &data), expected);
        // Nothing of this device was made for a folder that is not a Vault.
        assert!(!data.exists(), "{name}");
        seen.insert(expected.1);
    }
    // One detail per fault, apart from the two pairs that are one fault.
    assert_eq!(seen.len(), 10);
}

#[test]
fn a_fault_of_this_devices_own_files_names_itself() {
    let invalid = "E_VALIDATION";
    let state = Some("/local-state");
    let identity = Some("/identity");
    let write_identity = |data: &Path, bytes: &[u8]| {
        fs::write(data.join("identity.json"), bytes).expect("identity");
    };
    let rows: Vec<(&str, DataDir, Expected)> = vec![
        (
            "no data directory given",
            Box::new(|_, _| PathBuf::new()),
            (invalid, "empty_path", Some("/data-dir"), false),
        ),
        (
            "the data directory is a file",
            Box::new(|place, _| {
                let file = place.dir.path().join("a-file");
                fs::write(&file, b"").expect("file");
                file
            }),
            (invalid, "not_a_directory", Some("/data-dir"), false),
        ),
        (
            "the data directory is the Vault",
            Box::new(|_, vault| vault.to_path_buf()),
            (invalid, "vault_data_separation", Some("/data-dir"), false),
        ),
        (
            "the data directory is inside the Vault",
            Box::new(|_, vault| vault.join("mobile")),
            (invalid, "vault_data_separation", Some("/data-dir"), false),
        ),
        (
            "the Vault is inside the data directory",
            Box::new(|place, _| place.dir.path().to_path_buf()),
            (invalid, "vault_data_separation", Some("/data-dir"), false),
        ),
        (
            "identity.json is a symlink",
            Box::new(|place, _| {
                let data = place.data();
                let real = place.dir.path().join("real.json");
                fs::write(&real, b"{}").expect("file");
                symlink(&real, data.join("identity.json")).expect("symlink");
                data
            }),
            (invalid, "local_state_symlink", state, false),
        ),
        (
            "identity.json is a folder",
            Box::new(|place, _| {
                let data = place.data();
                fs::create_dir(data.join("identity.json")).expect("dir");
                data
            }),
            (invalid, "local_state_not_file", state, false),
        ),
        (
            "identity.json is too large",
            Box::new(move |place, _| {
                let data = place.data();
                write_identity(&data, &vec![b' '; 1_048_577]);
                data
            }),
            (invalid, "local_state_too_large", state, false),
        ),
        (
            "identity.json is not JSON",
            Box::new(move |place, _| {
                let data = place.data();
                write_identity(&data, b"identity");
                data
            }),
            (invalid, "local_state_malformed", state, false),
        ),
        (
            "identity.json has no version",
            Box::new(move |place, _| {
                let data = place.data();
                write_identity(&data, br#"{"host":"h-00000000000000000000000009"}"#);
                data
            }),
            (invalid, "identity_missing_version", identity, false),
        ),
        (
            "identity.json is of a version this core does not read",
            Box::new(move |place, _| {
                let data = place.data();
                write_identity(&data, br#"{"v":2}"#);
                data
            }),
            ("E_UNSUPPORTED", "identity_version", None, false),
        ),
        (
            "identity.json has another shape",
            Box::new(move |place, _| {
                let data = place.data();
                write_identity(&data, br#"{"v":1}"#);
                data
            }),
            (invalid, "identity_malformed", identity, false),
        ),
        (
            "the data directory cannot be written",
            Box::new(|place, _| {
                let data = place.data();
                fs::set_permissions(&data, fs::Permissions::from_mode(0o555)).expect("chmod");
                data
            }),
            ("E_IO", "local_data_unreadable", Some("/data-dir"), false),
        ),
        (
            "a file sits where this Vault's local folder goes",
            Box::new(|place, _| {
                let data = place.data();
                fs::write(data.join("vaults"), b"").expect("file");
                data
            }),
            // From the third stage (the service): the core's sentence says
            // "Cannot access the Vault", the detail says whose file it is.
            ("E_IO", "local_data_unreadable", Some("/data-dir"), false),
        ),
    ];
    for (name, arrange, expected) in rows {
        let place = Place::new();
        let vault = place.vault();
        let before = listing(&vault);
        let data = arrange(&place, &vault);
        fails(name, open(&vault, &data), expected);
        // No detail of this table is one of the Vault's.
        assert!(!expected.1.starts_with("vault_manifest"), "{name}");
        // Whatever the data directory was, the Vault holds nothing new.
        assert_eq!(listing(&vault), before, "{name}");
    }
}

/// The identity lock held by another process is the one fault of an open
/// that passes by itself.
#[test]
fn a_busy_identity_lock_can_be_tried_again() {
    let place = Place::new();
    let vault = place.vault();
    let data = place.data();
    // Every other open in this file is alone in its own folder, so a short
    // wait changes nothing for them.
    set_lock_wait(Duration::from_millis(50));
    let held = acquire_lock(&data.join("identity.lock")).expect("lock");
    fails(
        "identity.lock is held",
        open(&vault, &data),
        ("E_BUSY", "lock_timeout", None, true),
    );
    drop(held);
    let session = open(&vault, &data).expect("the same open passes once the lock is free");
    assert!(!session.health().expect("health").host.is_empty());
}

/// Every entry below `root`, relative, sorted.
fn listing(root: &Path) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            entries.push(path.strip_prefix(root).expect("below root").to_path_buf());
        }
    }
    entries.sort();
    entries
}

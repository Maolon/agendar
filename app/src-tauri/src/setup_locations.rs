//! Read-only, allowlisted Setup location suggestions.
//!
//! Detection checks only known sync-provider roots and the user's ordinary
//! Documents/home folders. It never creates the suggested Agendar folder or
//! scans unrelated directories.

use std::path::{Path, PathBuf};

use directories::BaseDirs;
use serde::Serialize;

pub const DEFAULT_FOLDER_NAME: &str = "Agendar";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupLocationKind {
    IcloudDrive,
    Onedrive,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupLocation {
    pub id: String,
    pub kind: SetupLocationKind,
    pub label: String,
    pub path: String,
    pub folder_name: String,
}

fn location(id: String, kind: SetupLocationKind, label: &str, root: &Path) -> SetupLocation {
    SetupLocation {
        id,
        kind,
        label: label.to_owned(),
        path: root
            .join(DEFAULT_FOLDER_NAME)
            .to_string_lossy()
            .into_owned(),
        folder_name: DEFAULT_FOLDER_NAME.to_owned(),
    }
}

/// Detect suggestions from explicit candidates. All filesystem checks are
/// read-only directory probes; the suggested child is deliberately not created.
pub fn detect(
    home: &Path,
    onedrive_roots: Vec<(String, PathBuf)>,
    include_icloud: bool,
) -> Vec<SetupLocation> {
    let mut locations = Vec::new();
    let mut seen_roots = Vec::<PathBuf>::new();

    if include_icloud {
        let root = home
            .join("Library")
            .join("Mobile Documents")
            .join("com~apple~CloudDocs");
        if root.is_dir() {
            seen_roots.push(root.clone());
            locations.push(location(
                "icloud-drive".to_owned(),
                SetupLocationKind::IcloudDrive,
                "iCloud Drive",
                &root,
            ));
        }
    }

    for (label, root) in onedrive_roots {
        if !root.is_dir() || seen_roots.contains(&root) {
            continue;
        }
        seen_roots.push(root.clone());
        locations.push(location(
            format!("onedrive-{}", seen_roots.len()),
            SetupLocationKind::Onedrive,
            &label,
            &root,
        ));
    }

    let documents = home.join("Documents");
    let local_root = if documents.is_dir() {
        documents
    } else {
        home.to_path_buf()
    };
    if local_root.is_dir() && !seen_roots.contains(&local_root) {
        locations.push(location(
            "local".to_owned(),
            SetupLocationKind::Local,
            "This computer",
            &local_root,
        ));
    }

    locations
}

/// Detect current-user suggestions without accepting paths from JavaScript.
pub fn current() -> Option<Vec<SetupLocation>> {
    let base = BaseDirs::new()?;
    let onedrive = if cfg!(target_os = "windows") {
        let mut roots = Vec::new();
        for (variable, label) in [
            ("OneDrive", "OneDrive"),
            ("OneDriveConsumer", "OneDrive"),
            ("OneDriveCommercial", "OneDrive for Business"),
        ] {
            if let Some(value) = std::env::var_os(variable)
                && !value.is_empty()
            {
                roots.push((label.to_owned(), PathBuf::from(value)));
            }
        }
        roots
    } else {
        Vec::new()
    };
    Some(detect(base.home_dir(), onedrive, cfg!(target_os = "macos")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_icloud_with_default_name_without_creating_it() {
        let home = tempfile::tempdir().expect("temp home");
        let root = home
            .path()
            .join("Library")
            .join("Mobile Documents")
            .join("com~apple~CloudDocs");
        std::fs::create_dir_all(&root).expect("iCloud fixture");
        std::fs::create_dir(home.path().join("Documents")).expect("Documents fixture");

        let locations = detect(home.path(), Vec::new(), true);
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].kind, SetupLocationKind::IcloudDrive);
        assert_eq!(locations[0].label, "iCloud Drive");
        assert_eq!(locations[0].folder_name, "Agendar");
        assert_eq!(locations[0].path, root.join("Agendar").to_string_lossy());
        assert!(!root.join("Agendar").exists(), "detection must not write");
        assert_eq!(locations[1].kind, SetupLocationKind::Local);
    }

    #[test]
    fn offers_onedrive_and_deduplicates_shared_roots() {
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().join("OneDrive");
        std::fs::create_dir(&root).expect("OneDrive fixture");

        let locations = detect(
            home.path(),
            vec![
                ("OneDrive".to_owned(), root.clone()),
                ("OneDrive again".to_owned(), root.clone()),
            ],
            false,
        );
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].kind, SetupLocationKind::Onedrive);
        assert_eq!(locations[0].path, root.join("Agendar").to_string_lossy());
        assert!(!root.join("Agendar").exists(), "detection must not write");
    }

    #[test]
    fn falls_back_to_home_when_documents_is_missing() {
        let home = tempfile::tempdir().expect("temp home");
        let locations = detect(home.path(), Vec::new(), false);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].kind, SetupLocationKind::Local);
        assert_eq!(
            locations[0].path,
            home.path().join("Agendar").to_string_lossy()
        );
        assert!(!home.path().join("Agendar").exists());
    }
}

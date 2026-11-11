//! A read-only listing of the files under `projects/`, for the usage report.
//!
//! Only directory entries and file metadata are read: no file is opened, so a
//! file a sync app keeps online-only is never downloaded. Such a file is
//! listed with `local: false`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::error::Result;
use crate::index::source_reads::{content_not_local, placeholder_bits};
use crate::model::{HostId, ProjectSlug};
use crate::reduce::usage::{BlobFile, ProjectFiles};
use crate::vault::Vault;

/// List every project's writer files (as byte totals) and blob files.
pub fn list_files(vault: &Vault) -> Result<BTreeMap<ProjectSlug, ProjectFiles>> {
    let mut listing = BTreeMap::new();
    for (project_name, project_dir) in directories(&vault.root().join("projects"))? {
        let Ok(project) = ProjectSlug::parse(&project_name) else {
            continue;
        };
        let mut files = ProjectFiles::default();
        for (host_name, host_dir) in directories(&project_dir)? {
            let Ok(host) = HostId::parse(&host_name) else {
                continue;
            };
            for file in regular_files(&host_dir)? {
                files.event_bytes += file.bytes;
                if file.local {
                    files.event_local_bytes += file.bytes;
                }
            }
            for file in regular_files(&host_dir.join("blobs"))? {
                files.blobs.push(BlobFile {
                    host: host.clone(),
                    name: file.name,
                    bytes: file.bytes,
                    local: file.local,
                });
            }
        }
        files
            .blobs
            .sort_by(|a, b| (&a.host, &a.name).cmp(&(&b.host, &b.name)));
        // A file and its iCloud stub can both exist while a download finishes.
        files
            .blobs
            .dedup_by(|later, first| later.host == first.host && later.name == first.name);
        listing.insert(project, files);
    }
    Ok(listing)
}

struct Listed {
    name: String,
    bytes: u64,
    local: bool,
}

/// The real sub-directories of `directory` (no symlinks), by name. A missing
/// directory is empty.
fn directories(directory: &Path) -> Result<Vec<(String, std::path::PathBuf)>> {
    let mut found = Vec::new();
    for entry in entries(directory)? {
        let file_type = entry.file_type()?;
        if file_type.is_dir() && !file_type.is_symlink() {
            found.push((
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            ));
        }
    }
    found.sort();
    Ok(found)
}

/// The visible regular files of `directory`. An iCloud stub (`.name.icloud`)
/// stands for `name` with no bytes here; other hidden files are sync-app
/// bookkeeping and are left out.
fn regular_files(directory: &Path) -> Result<Vec<Listed>> {
    let mut found = Vec::new();
    for entry in entries(directory)? {
        let file_type = entry.file_type()?;
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(hidden) = name.strip_prefix('.') {
            if let Some(target) = hidden
                .strip_suffix(".icloud")
                .filter(|name| !name.is_empty())
            {
                found.push(Listed {
                    name: target.to_owned(),
                    bytes: 0,
                    local: false,
                });
            }
            continue;
        }
        let metadata = entry.metadata()?;
        found.push(Listed {
            name,
            bytes: metadata.len(),
            local: !content_not_local(placeholder_bits(&metadata)),
        });
    }
    // The real file sorts before its stub, so a dedup keeps the real one.
    found.sort_by(|a, b| (&a.name, !a.local).cmp(&(&b.name, !b.local)));
    Ok(found)
}

fn entries(directory: &Path) -> Result<Vec<fs::DirEntry>> {
    match fs::read_dir(directory) {
        Ok(entries) => Ok(entries.collect::<std::io::Result<Vec<_>>>()?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

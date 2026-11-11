//! `board --out <file>`: save a board page where the agent asked, so it can
//! grep it instead of reading it whole. Nothing is written without `--out`.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use agendar_core::error::{AgendarError, ErrorCode, Result};

fn out_error(detail: &str, message: impl Into<String>) -> AgendarError {
    AgendarError::validation_at("/out", detail, message)
}

/// Where `--out` lands: an absolute path whose folder exists, never inside
/// the Vault.
fn target(vault: &Path, out: &Path) -> Result<PathBuf> {
    let name = out
        .file_name()
        .ok_or_else(|| out_error("out_name", "--out needs a file name."))?;
    let parent = match out.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let parent = fs::canonicalize(&parent).map_err(|_| {
        out_error(
            "out_parent",
            format!("The folder of --out does not exist: {}.", parent.display()),
        )
    })?;
    if !parent.is_dir() {
        return Err(out_error(
            "out_parent",
            "The folder of --out is not a folder.",
        ));
    }
    let path = parent.join(name);
    let vault = fs::canonicalize(vault).unwrap_or_else(|_| vault.to_path_buf());
    if path.starts_with(&vault) {
        return Err(out_error(
            "out_in_vault",
            "--out must not point inside the Vault.",
        ));
    }
    if path.is_dir() {
        return Err(out_error("out_is_dir", "--out names a folder, not a file."));
    }
    Ok(path)
}

/// Write `data` (pretty JSON) to `out` atomically and return the envelope
/// data `{path, bytes, steps}`.
pub fn write_out(vault: &Path, out: &Path, data: &Value) -> Result<Value> {
    let path = target(vault, out)?;
    let mut bytes = serde_json::to_vec_pretty(data).map_err(|error| {
        AgendarError::new(
            ErrorCode::Io,
            format!("Cannot serialize the board: {error}."),
        )
    })?;
    bytes.push(b'\n');
    let parent = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = parent.join(format!(".{name}.{}.tmp", ulid::Ulid::generate()));
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| {
            file.write_all(&bytes)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temp, &path));
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(AgendarError::new(
            ErrorCode::Io,
            format!("Cannot write {}: {error}.", path.display()),
        ));
    }
    let steps = data["features"].as_array().map_or(0, Vec::len);
    Ok(json!({
        "path": path.to_string_lossy(),
        "bytes": bytes.len(),
        "steps": steps,
    }))
}

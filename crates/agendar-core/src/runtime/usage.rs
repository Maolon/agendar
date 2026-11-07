//! Token usage read from the agents' own local logs: Claude Code transcripts
//! (`~/.claude/projects/*/*.jsonl`) and Codex sessions
//! (`~/.codex/sessions/YYYY/MM/DD/*.jsonl`).
//!
//! Only numbers leave a log line: token counts, its timestamp and the working
//! directory it ran in. Message text is never kept. Lines are pre-filtered by
//! substring before they are parsed, files are read incrementally from where
//! the last scan stopped, and only the last [`WINDOW_MS`] are retained.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use serde_json::Value;

/// How far back usage is kept and summed.
pub const WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
/// Files untouched for longer than this are not read at all.
const STALE_FILE_MS: i64 = WINDOW_MS + 2 * 60 * 60 * 1000;
/// A single scan reads at most this much new data per file.
const MAX_READ_BYTES: u64 = 64 * 1024 * 1024;

/// Tokens used in the window, split the way providers bill them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
    /// New input (including cache writes).
    pub input: u64,
    /// Input served from the prompt cache.
    pub cached: u64,
    pub output: u64,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.cached)
            .saturating_add(self.output)
    }

    fn add(&mut self, other: &TokenUsage) {
        self.input = self.input.saturating_add(other.input);
        self.cached = self.cached.saturating_add(other.cached);
        self.output = self.output.saturating_add(other.output);
    }
}

#[derive(Debug, Clone)]
struct Entry {
    ts_ms: i64,
    cwd: Option<PathBuf>,
    usage: TokenUsage,
}

#[derive(Debug, Default)]
struct FileScan {
    offset: u64,
    /// Codex names its working directory once per session, not per line.
    cwd: Option<PathBuf>,
    entries: Vec<Entry>,
    /// Claude Code repeats a message's usage on each of its content lines.
    by_message: HashMap<String, usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Claude,
    Codex,
}

/// Incremental reader of local agent logs.
#[derive(Debug, Default)]
pub struct UsageScanner {
    files: HashMap<PathBuf, (Kind, FileScan)>,
}

impl UsageScanner {
    /// Read whatever the logs under `home` added since the last call.
    pub fn refresh(&mut self, home: &Path, now_ms: i64) {
        let mut seen = Vec::new();
        for (kind, path) in candidate_files(home, now_ms) {
            seen.push(path.clone());
            let (_, scan) = self
                .files
                .entry(path.clone())
                .or_insert_with(|| (kind, FileScan::default()));
            read_new_lines(&path, kind, scan);
            scan.entries
                .retain(|entry| now_ms - entry.ts_ms <= WINDOW_MS);
            scan.by_message
                .retain(|_, index| *index < scan.entries.len());
        }
        self.files.retain(|path, _| seen.contains(path));
    }

    /// Usage since `since_ms` of the logs whose working directory is inside
    /// one of `roots`, or contains one (an agent started one folder up).
    pub fn usage_for(&self, roots: &[PathBuf], since_ms: i64) -> Option<TokenUsage> {
        if roots.is_empty() {
            return None;
        }
        let mut total = TokenUsage::default();
        let mut any = false;
        for (_, scan) in self.files.values() {
            for entry in &scan.entries {
                if entry.ts_ms < since_ms {
                    continue;
                }
                let Some(cwd) = entry.cwd.as_ref().or(scan.cwd.as_ref()) else {
                    continue;
                };
                if roots
                    .iter()
                    .any(|root| cwd.starts_with(root) || root.starts_with(cwd))
                {
                    total.add(&entry.usage);
                    any = true;
                }
            }
        }
        any.then_some(total)
    }
}

fn modified_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

fn jsonl_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
                .collect()
        })
        .unwrap_or_default()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

/// Log files touched recently enough to hold usage inside the window.
fn candidate_files(home: &Path, now_ms: i64) -> Vec<(Kind, PathBuf)> {
    let fresh = |path: &PathBuf| modified_ms(path).is_some_and(|at| now_ms - at <= STALE_FILE_MS);
    let mut files = Vec::new();
    for project in subdirs(&crate::runtime::install::claude_dir(home).join("projects")) {
        files.extend(
            jsonl_in(&project)
                .into_iter()
                .filter(fresh)
                .map(|path| (Kind::Claude, path)),
        );
    }
    for year in subdirs(&crate::runtime::install::codex_dir(home).join("sessions")) {
        for month in subdirs(&year) {
            for day in subdirs(&month) {
                files.extend(
                    jsonl_in(&day)
                        .into_iter()
                        .filter(fresh)
                        .map(|path| (Kind::Codex, path)),
                );
            }
        }
    }
    files
}

/// Read complete lines added since `scan.offset`; a partial last line waits
/// for the next scan.
fn read_new_lines(path: &Path, kind: Kind, scan: &mut FileScan) {
    let Ok(mut file) = File::open(path) else {
        return;
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return;
    };
    if len < scan.offset {
        // Rewritten or truncated: start over.
        *scan = FileScan::default();
    }
    if len == scan.offset || file.seek(SeekFrom::Start(scan.offset)).is_err() {
        return;
    }
    let mut bytes = Vec::new();
    if file
        .take((len - scan.offset).min(MAX_READ_BYTES))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return;
    }
    let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return;
    };
    for line in bytes[..end].split(|byte| *byte == b'\n') {
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        match kind {
            Kind::Claude => claude_line(line, scan),
            Kind::Codex => codex_line(line, scan),
        }
    }
    scan.offset += u64::try_from(end + 1).unwrap_or(0);
}

fn timestamp_ms(value: &Value) -> Option<i64> {
    let text = value.get("timestamp")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.timestamp_millis())
}

fn count(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn claude_line(line: &str, scan: &mut FileScan) {
    if !line.contains("\"usage\"") || !line.contains("\"assistant\"") {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(usage) = value.pointer("/message/usage") else {
        return;
    };
    let Some(ts_ms) = timestamp_ms(&value) else {
        return;
    };
    let entry = Entry {
        ts_ms,
        cwd: value.get("cwd").and_then(Value::as_str).map(PathBuf::from),
        usage: TokenUsage {
            input: count(usage, "input_tokens")
                .saturating_add(count(usage, "cache_creation_input_tokens")),
            cached: count(usage, "cache_read_input_tokens"),
            output: count(usage, "output_tokens"),
        },
    };
    match value.pointer("/message/id").and_then(Value::as_str) {
        Some(id) => match scan.by_message.get(id) {
            Some(index) => scan.entries[*index] = entry,
            None => {
                scan.by_message.insert(id.to_owned(), scan.entries.len());
                scan.entries.push(entry);
            }
        },
        None => scan.entries.push(entry),
    }
}

fn codex_line(line: &str, scan: &mut FileScan) {
    let session = line.contains("\"session_meta\"") || line.contains("\"turn_context\"");
    if !session && !line.contains("\"token_count\"") {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if session {
        if let Some(cwd) = value.pointer("/payload/cwd").and_then(Value::as_str) {
            scan.cwd = Some(PathBuf::from(cwd));
        }
        return;
    }
    let Some(last) = value.pointer("/payload/info/last_token_usage") else {
        return;
    };
    let Some(ts_ms) = timestamp_ms(&value) else {
        return;
    };
    let cached = count(last, "cached_input_tokens");
    scan.entries.push(Entry {
        ts_ms,
        cwd: None,
        usage: TokenUsage {
            // OpenAI counts cached input inside input_tokens.
            input: count(last, "input_tokens").saturating_sub(cached),
            cached,
            output: count(last, "output_tokens"),
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-11-06T12:00:00Z";

    fn now_ms() -> i64 {
        chrono::DateTime::parse_from_rfc3339(NOW)
            .expect("now")
            .timestamp_millis()
    }

    #[test]
    fn claude_and_codex_logs_add_up_by_working_directory() {
        let home = tempfile::tempdir().expect("home");
        let claude = home.path().join(".claude/projects/-work-app");
        std::fs::create_dir_all(&claude).expect("claude dir");
        let usage = |id: &str, input: u64| {
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"2026-11-06T11:00:00Z\",\"cwd\":\"/work\",\
                 \"message\":{{\"id\":\"{id}\",\"content\":[{{\"text\":\"secret prose\"}}],\
                 \"usage\":{{\"input_tokens\":{input},\"cache_creation_input_tokens\":100,\
                 \"cache_read_input_tokens\":1000,\"output_tokens\":50}}}}}}\n"
            )
        };
        let old = "{\"type\":\"assistant\",\"timestamp\":\"2026-11-04T11:00:00Z\",\"cwd\":\"/work\",\
                   \"message\":{\"id\":\"old\",\"usage\":{\"input_tokens\":999999,\"output_tokens\":1}}}\n";
        std::fs::write(
            claude.join("s.jsonl"),
            format!(
                "{}{}{}{old}{{\"type\":\"user\",\"message\":{{\"content\":\"hi\"}}}}\n",
                usage("m1", 10),
                usage("m1", 10),
                usage("m2", 20)
            ),
        )
        .expect("write claude");
        let codex = home.path().join(".codex/sessions/2026/11/06");
        std::fs::create_dir_all(&codex).expect("codex dir");
        std::fs::write(
            codex.join("rollout.jsonl"),
            "{\"timestamp\":\"2026-11-06T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/work/app/sub\"}}\n\
             {\"timestamp\":\"2026-11-06T10:01:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\
             \"info\":{\"last_token_usage\":{\"input_tokens\":500,\"cached_input_tokens\":300,\"output_tokens\":40}}}}\n\
             {\"partial\":",
        )
        .expect("write codex");

        let mut scanner = UsageScanner::default();
        scanner.refresh(home.path(), now_ms());
        let roots = vec![PathBuf::from("/work/app")];
        let used = scanner
            .usage_for(&roots, now_ms() - WINDOW_MS)
            .expect("usage");
        // Claude: m1 once (110 new, 1000 cached, 50 out) + m2 (120, 1000, 50);
        // the two-day-old line is outside the window. Codex: 200 new, 300 cached, 40 out.
        assert_eq!(
            used,
            TokenUsage {
                input: 110 + 120 + 200,
                cached: 2000 + 300,
                output: 100 + 40
            }
        );
        assert!(
            scanner
                .usage_for(&[PathBuf::from("/elsewhere")], 0)
                .is_none()
        );

        // Appending a line is read incrementally; the partial line waited.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(codex.join("rollout.jsonl"))
            .expect("append");
        std::io::Write::write_all(&mut file, b"\"x\"}\n").expect("finish line");
        std::io::Write::write_all(
            &mut file,
            b"{\"timestamp\":\"2026-11-06T10:02:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5}}}}\n",
        )
        .expect("append line");
        scanner.refresh(home.path(), now_ms());
        let again = scanner
            .usage_for(&roots, now_ms() - WINDOW_MS)
            .expect("usage");
        assert_eq!(again.output, 100 + 40 + 5);
        assert_eq!(again.total(), again.input + again.cached + again.output);
    }
}

//! The `diff` demo input: the agent picks hunks of one commit; the CLI cuts
//! them verbatim from `git show`, so the excerpt cannot paraphrase code.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use agendar_core::error::{AgendarError, ErrorCode, Result};
use agendar_core::model::validate::{DIFF_OMITTED_MAX, DIFF_SHOWN_MAX};
use agendar_core::model::{DiffDemo, DiffFile, DiffShown};

/// Most excerpt lines one diff demo may carry.
pub const EXCERPT_MAX_LINES: usize = 600;

/// `{"commit", "repo"?, "hunks": [{"file", "hunk", "why"}]}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffInput {
    pub commit: String,
    #[serde(default)]
    pub repo: Option<String>,
    pub hunks: Vec<HunkPick>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HunkPick {
    pub file: String,
    pub hunk: u32,
    pub why: String,
}

/// One file section of `git show`: header lines and hunks, bytes as printed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub header: Vec<u8>,
    pub hunks: Vec<Vec<u8>>,
    pub added: u32,
    pub removed: u32,
}

/// The cut excerpt and everything the demo body needs except the blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    pub excerpt: Vec<u8>,
    pub commit: String,
    pub files_total: u32,
    pub hunks_total: u32,
    pub added_total: u32,
    pub removed_total: u32,
    pub shown: Vec<DiffShown>,
    pub omitted: Vec<DiffFile>,
}

impl Cut {
    pub fn into_demo(self, blob: agendar_core::model::BlobRef) -> DiffDemo {
        DiffDemo {
            blob,
            commit: self.commit,
            files_total: self.files_total,
            hunks_total: self.hunks_total,
            added_total: self.added_total,
            removed_total: self.removed_total,
            shown: self.shown,
            omitted: self.omitted,
        }
    }
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn strip_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// A `---`/`+++` path: drop the prefix and git's trailing tab.
fn side_path(rest: &[u8], prefix: &[u8]) -> Option<String> {
    let rest = strip_eol(rest);
    let rest = rest.strip_suffix(b"\t").unwrap_or(rest);
    if rest == b"/dev/null" {
        return None;
    }
    let rest = rest.strip_prefix(prefix).unwrap_or(rest);
    Some(String::from_utf8_lossy(rest).into_owned())
}

/// The path from `diff --git a/X b/Y` when nothing better is printed.
fn git_line_path(rest: &[u8]) -> String {
    let rest = String::from_utf8_lossy(strip_eol(rest)).into_owned();
    // Same name on both sides: split in the middle.
    let chars: Vec<char> = rest.chars().collect();
    if chars.len() % 2 == 1 {
        let half = chars.len() / 2;
        let left: String = chars[..half].iter().collect();
        let right: String = chars[half + 1..].iter().collect();
        if let (Some(left), Some(right)) = (left.strip_prefix("a/"), right.strip_prefix("b/"))
            && left == right
        {
            return right.to_owned();
        }
    }
    match rest.rfind(" b/") {
        Some(index) => rest[index + 3..].to_owned(),
        None => rest,
    }
}

/// Split `git show --format=` output into file sections and hunks.
pub fn parse(output: &[u8]) -> Vec<FileDiff> {
    struct Building {
        file: FileDiff,
        old: Option<String>,
        new: Option<String>,
        renamed: Option<String>,
        git_line: String,
        in_hunk: bool,
    }
    fn finish(building: Building, files: &mut Vec<FileDiff>) {
        let mut file = building.file;
        file.path = building
            .new
            .or(building.old)
            .or(building.renamed)
            .unwrap_or(building.git_line);
        files.push(file);
    }
    let mut files = Vec::new();
    let mut current: Option<Building> = None;
    for line in output.split_inclusive(|byte| *byte == b'\n') {
        if let Some(rest) = line.strip_prefix(b"diff --git ") {
            if let Some(done) = current.take() {
                finish(done, &mut files);
            }
            current = Some(Building {
                file: FileDiff {
                    header: line.to_vec(),
                    ..FileDiff::default()
                },
                old: None,
                new: None,
                renamed: None,
                git_line: git_line_path(rest),
                in_hunk: false,
            });
            continue;
        }
        let Some(building) = current.as_mut() else {
            continue;
        };
        if line.starts_with(b"@@ ") {
            building.in_hunk = true;
            building.file.hunks.push(line.to_vec());
            continue;
        }
        if building.in_hunk {
            if let Some(hunk) = building.file.hunks.last_mut() {
                hunk.extend_from_slice(line);
            }
            match line.first() {
                Some(b'+') => building.file.added += 1,
                Some(b'-') => building.file.removed += 1,
                _ => {}
            }
            continue;
        }
        building.file.header.extend_from_slice(line);
        if let Some(rest) = line.strip_prefix(b"--- ") {
            building.old = side_path(rest, b"a/");
        } else if let Some(rest) = line.strip_prefix(b"+++ ") {
            building.new = side_path(rest, b"b/");
        } else if let Some(rest) = line
            .strip_prefix(b"rename to ")
            .or_else(|| line.strip_prefix(b"copy to "))
        {
            building.renamed = Some(String::from_utf8_lossy(strip_eol(rest)).into_owned());
        }
    }
    if let Some(done) = current.take() {
        finish(done, &mut files);
    }
    files
}

/// Cut the picked hunks out of parsed file sections. `field` is the JSON
/// pointer of the demo body.
pub fn cut(files: &[FileDiff], commit: &str, picks: &[HunkPick], field: &str) -> Result<Cut> {
    if picks.is_empty() || picks.len() > DIFF_SHOWN_MAX {
        return Err(AgendarError::validation_at(
            format!("{field}/hunks"),
            "diff_hunk_count",
            format!("A diff demo picks 1..={DIFF_SHOWN_MAX} hunks."),
        ));
    }
    // (file index, hunk index, why) in pick order.
    let mut chosen: Vec<(usize, usize, &str)> = Vec::new();
    for (index, pick) in picks.iter().enumerate() {
        let at = format!("{field}/hunks/{index}");
        let why = pick.why.trim();
        if why.is_empty() || why.chars().count() > 160 {
            return Err(AgendarError::validation_at(
                format!("{at}/why"),
                "diff_why",
                "Each picked hunk needs a why of 1..=160 characters.",
            ));
        }
        let Some(file_index) = files.iter().position(|file| file.path == pick.file) else {
            return Err(AgendarError::validation_at(
                format!("{at}/file"),
                "diff_file_unknown",
                format!(
                    "`{}` is not changed in commit {}.",
                    pick.file,
                    short(commit)
                ),
            )
            .with_hint(
                "Use the path as `git show --stat` prints it, relative to the repository root.",
            ));
        };
        let total = files[file_index].hunks.len();
        if pick.hunk == 0 || pick.hunk as usize > total {
            return Err(AgendarError::validation_at(
                format!("{at}/hunk"),
                "diff_hunk_unknown",
                format!(
                    "`{}` has {total} hunk{} in commit {}; hunk {} does not exist.",
                    pick.file,
                    if total == 1 { "" } else { "s" },
                    short(commit),
                    pick.hunk
                ),
            ));
        }
        let hunk_index = pick.hunk as usize - 1;
        if chosen
            .iter()
            .any(|(file, hunk, _)| *file == file_index && *hunk == hunk_index)
        {
            return Err(AgendarError::validation_at(
                format!("{at}/hunk"),
                "diff_hunk_duplicate",
                "Each hunk can be picked once.",
            ));
        }
        chosen.push((file_index, hunk_index, why));
    }
    // Group by file in order of first pick; hunks keep pick order.
    let mut order: Vec<usize> = Vec::new();
    for (file, _, _) in &chosen {
        if !order.contains(file) {
            order.push(*file);
        }
    }
    let mut excerpt = Vec::new();
    let mut shown = Vec::new();
    for file_index in &order {
        let file = &files[*file_index];
        excerpt.extend_from_slice(&file.header);
        for (_, hunk, why) in chosen.iter().filter(|(file, _, _)| file == file_index) {
            excerpt.extend_from_slice(&file.hunks[*hunk]);
            shown.push(DiffShown {
                path: file.path.clone(),
                hunk: count(hunk + 1),
                why: (*why).to_owned(),
            });
        }
    }
    let lines = excerpt.split_inclusive(|byte| *byte == b'\n').count();
    if lines > EXCERPT_MAX_LINES {
        return Err(AgendarError::validation_at(
            format!("{field}/hunks"),
            "diff_excerpt_too_long",
            format!(
                "The picked hunks are {lines} lines; a diff demo holds at most {EXCERPT_MAX_LINES}. Pick fewer hunks."
            ),
        ));
    }
    if std::str::from_utf8(&excerpt).is_err() {
        return Err(AgendarError::validation_at(
            format!("{field}/hunks"),
            "diff_excerpt_not_text",
            "The picked hunks are not UTF-8 text.",
        ));
    }
    let omitted: Vec<DiffFile> = files
        .iter()
        .enumerate()
        .filter(|(index, _)| !order.contains(index))
        .take(DIFF_OMITTED_MAX)
        .map(|(_, file)| DiffFile {
            path: file.path.clone(),
            added: file.added,
            removed: file.removed,
        })
        .collect();
    Ok(Cut {
        excerpt,
        commit: commit.to_owned(),
        files_total: count(files.len()),
        hunks_total: count(files.iter().map(|file| file.hunks.len()).sum()),
        added_total: files
            .iter()
            .fold(0u32, |total, file| total.saturating_add(file.added)),
        removed_total: files
            .iter()
            .fold(0u32, |total, file| total.saturating_add(file.removed)),
        shown,
        omitted,
    })
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

fn git(repo: &Path, args: &[&str], field: &str) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("--no-pager")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .output()
        .map_err(|error| {
            AgendarError::new(
                ErrorCode::Unsupported,
                format!("Cannot run git for the diff demo: {error}."),
            )
            .with_detail("git_unavailable")
            .with_hint("Install git and make sure it is on PATH.")
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(AgendarError::validation_at(
            field,
            "diff_git_failed",
            format!(
                "git failed in {}: {}",
                repo.display(),
                if stderr.is_empty() {
                    "no output"
                } else {
                    stderr
                }
            ),
        ));
    }
    Ok(output.stdout)
}

/// Resolve the commit in `repo` (default: the current directory), refuse a
/// merge, read its diff and cut the picked hunks.
pub fn cut_commit(input: &DiffInput, field: &str) -> Result<Cut> {
    let repo = match input.repo.as_deref() {
        Some(repo) if repo.trim().is_empty() => {
            return Err(AgendarError::validation_at(
                format!("{field}/repo"),
                "diff_repo",
                "The repo path must not be blank.",
            ));
        }
        Some(repo) => PathBuf::from(repo),
        None => std::env::current_dir()?,
    };
    if !repo.is_dir() {
        return Err(AgendarError::validation_at(
            format!("{field}/repo"),
            "diff_repo",
            format!("{} is not a directory.", repo.display()),
        ));
    }
    let commit = input.commit.trim();
    if commit.is_empty() || commit.starts_with('-') {
        return Err(AgendarError::validation_at(
            format!("{field}/commit"),
            "diff_commit",
            "Name the commit by its sha.",
        ));
    }
    let spec = format!("{commit}^{{commit}}");
    let resolved = git(
        &repo,
        &["rev-parse", "--verify", "--end-of-options", &spec],
        &format!("{field}/commit"),
    )?;
    let sha = String::from_utf8_lossy(&resolved).trim().to_owned();
    let parents = git(
        &repo,
        &["rev-list", "--parents", "-n", "1", &sha],
        &format!("{field}/commit"),
    )?;
    if String::from_utf8_lossy(&parents).split_whitespace().count() > 2 {
        return Err(AgendarError::validation_at(
            format!("{field}/commit"),
            "diff_merge_commit",
            format!(
                "Commit {} is a merge; a diff demo cuts from a single-parent commit.",
                short(&sha)
            ),
        )
        .with_hint("Pick the commit that made the change, not the merge."));
    }
    let output = git(
        &repo,
        &[
            "show",
            "--format=",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--unified=3",
            "-M",
            &sha,
            "--",
        ],
        &format!("{field}/commit"),
    )?;
    cut(&parse(&output), &sha, &input.hunks, field)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,2 @@\n-one\n+uno\n two\n@@ -10,1 +10,2 @@\n ten\n+eleven\ndiff --git a/old name.txt b/new name.txt\nsimilarity index 100%\nrename from old name.txt\nrename to new name.txt\ndiff --git a/img.png b/img.png\nnew file mode 100644\nindex 0000000..1111111\nBinary files /dev/null and b/img.png differ\n";

    fn pick(file: &str, hunk: u32) -> HunkPick {
        HunkPick {
            file: file.to_owned(),
            hunk,
            why: "Because.".to_owned(),
        }
    }

    #[test]
    fn parses_files_hunks_renames_and_binaries() {
        let files = parse(SAMPLE.as_bytes());
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["src/a.rs", "new name.txt", "img.png"]);
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!((files[0].added, files[0].removed), (2, 1));
        assert!(files[1].hunks.is_empty() && files[2].hunks.is_empty());
    }

    #[test]
    fn cuts_header_and_the_picked_hunk_verbatim() {
        let files = parse(SAMPLE.as_bytes());
        let cut = cut(
            &files,
            &"a".repeat(40),
            &[pick("src/a.rs", 2)],
            "/demos/0/body",
        )
        .expect("cut");
        assert_eq!(
            String::from_utf8(cut.excerpt).expect("utf8"),
            "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -10,1 +10,2 @@\n ten\n+eleven\n"
        );
        assert_eq!((cut.files_total, cut.hunks_total), (3, 2));
        assert_eq!(cut.omitted.len(), 2);
    }

    #[test]
    fn refuses_unknown_and_duplicate_picks() {
        let files = parse(SAMPLE.as_bytes());
        let commit = "a".repeat(40);
        let error = cut(&files, &commit, &[pick("src/a.rs", 3)], "/d").expect_err("index");
        assert!(
            error.message().contains("has 2 hunks"),
            "{}",
            error.message()
        );
        cut(&files, &commit, &[pick("nope.rs", 1)], "/d").expect_err("file");
        cut(
            &files,
            &commit,
            &[pick("src/a.rs", 1), pick("src/a.rs", 1)],
            "/d",
        )
        .expect_err("duplicate");
    }
}

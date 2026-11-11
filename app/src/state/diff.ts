/**
 * Pure helpers for the `diff` demo: read its body defensively (it crosses the
 * IPC boundary) and split the excerpt into numbered hunks. Nothing is
 * reformatted: every line is shown as the CLI cut it from `git show`.
 */

import type { BlobRef, Demo, DiffDemoBody, DiffFile, DiffShown } from "../api/types";

export type DiffLineKind = "add" | "del" | "ctx" | "note";

export interface DiffLine {
  kind: DiffLineKind;
  text: string;
  old?: number;
  new?: number;
}

export interface DiffHunk {
  /** The `@@ … @@` line, verbatim. */
  header: string;
  lines: DiffLine[];
}

function isCount(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0;
}

function readBlob(value: unknown): BlobRef | undefined {
  if (!value || typeof value !== "object") return undefined;
  const record = value as Record<string, unknown>;
  if (typeof record.sha256 !== "string" || typeof record.ext !== "string") return undefined;
  return {
    host: String(record.host ?? ""),
    sha256: record.sha256,
    ext: record.ext,
    bytes: typeof record.bytes === "number" ? record.bytes : 0,
  };
}

function readShown(value: unknown): DiffShown | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  if (typeof record.path !== "string" || typeof record.why !== "string") return null;
  if (!isCount(record.hunk)) return null;
  return { path: record.path, hunk: record.hunk, why: record.why };
}

function readFile(value: unknown): DiffFile | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  if (typeof record.path !== "string" || !isCount(record.added) || !isCount(record.removed)) {
    return null;
  }
  return { path: record.path, added: record.added, removed: record.removed };
}

/** The diff demo body, or null when the shape is not the model's. */
export function diffBody(demo: Demo): DiffDemoBody | null {
  const body = demo.body as Record<string, unknown>;
  const blob = readBlob(body.blob);
  if (!blob || typeof body.commit !== "string") return null;
  const totals = [body.files_total, body.hunks_total, body.added_total, body.removed_total];
  if (!totals.every(isCount)) return null;
  const shown = Array.isArray(body.shown)
    ? body.shown.map(readShown).filter((item): item is DiffShown => item !== null)
    : [];
  const omitted = Array.isArray(body.omitted)
    ? body.omitted.map(readFile).filter((item): item is DiffFile => item !== null)
    : [];
  return {
    blob,
    commit: body.commit,
    files_total: body.files_total as number,
    hunks_total: body.hunks_total as number,
    added_total: body.added_total as number,
    removed_total: body.removed_total as number,
    shown,
    omitted,
  };
}

const HUNK_HEADER = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

/**
 * Every hunk of the excerpt in order, with old/new line numbers from its `@@`
 * header. File header lines (`diff --git`, `index`, `---`, `+++`) are dropped:
 * the view names the file itself.
 */
export function parseHunks(text: string): DiffHunk[] {
  const lines = text.split("\n");
  if (lines.length && lines[lines.length - 1] === "") lines.pop();
  const hunks: DiffHunk[] = [];
  let current: DiffHunk | null = null;
  let oldLine = 0;
  let newLine = 0;
  for (const raw of lines) {
    const line = raw.endsWith("\r") ? raw.slice(0, -1) : raw;
    if (line.startsWith("diff --git ")) {
      current = null;
      continue;
    }
    if (line.startsWith("@@ ")) {
      const match = HUNK_HEADER.exec(line);
      oldLine = match ? Number(match[1]) : 0;
      newLine = match ? Number(match[2]) : 0;
      current = { header: line, lines: [] };
      hunks.push(current);
      continue;
    }
    if (!current) continue;
    const marker = line[0];
    const body = line.slice(1);
    if (marker === "+") {
      current.lines.push({ kind: "add", text: body, new: newLine++ });
    } else if (marker === "-") {
      current.lines.push({ kind: "del", text: body, old: oldLine++ });
    } else if (marker === "\\") {
      current.lines.push({ kind: "note", text: line });
    } else {
      current.lines.push({ kind: "ctx", text: body, old: oldLine++, new: newLine++ });
    }
  }
  return hunks;
}

/** `crates/x/src/a.rs` → `["crates/x/src/", "a.rs"]`. */
export function splitPath(path: string): [string, string] {
  const slash = path.lastIndexOf("/");
  return slash < 0 ? ["", path] : [path.slice(0, slash + 1), path.slice(slash + 1)];
}

export function shortSha(commit: string): string {
  return commit.slice(0, 7);
}

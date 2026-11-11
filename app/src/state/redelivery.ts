/**
 * A redelivery against the delivery it replaces: pure comparisons for the
 * "Since your send-back" section. Demos match by kind and bytes (a diff also
 * by commit); disclosure lines match by exact string, per column.
 */

import { tx } from "../i18n";
import type {
  DeliveryState,
  Demo,
  Disclosure,
  ProjectState,
  ReviewAnchor,
  ReviewComment,
  SentBackNote,
} from "../api/types";
import { commentLabel, diffLineLabel } from "./comments";
import { demoAlt, demoUrl } from "./delivery";

/** The previous delivery and its send-back, when this one answers it. */
export interface Redelivery {
  previous: DeliveryState;
  note: SentBackNote;
}

export function redeliveryOf(project: ProjectState, delivery: DeliveryState): Redelivery | null {
  if (!delivery.supersedes) return null;
  const previous = project.deliveries[delivery.supersedes];
  const note = previous?.sent_back;
  return previous && note ? { previous, note } : null;
}

function sha(value: unknown): string {
  if (!value || typeof value !== "object") return "";
  const digest = (value as Record<string, unknown>).sha256;
  return typeof digest === "string" ? digest : "";
}

/** What makes two demos the same: kind, blob(s), and for a diff the commit. */
export function demoKey(demo: Demo): string {
  const body = demo.body ?? {};
  const parts: string[] = [demo.kind];
  if (demo.kind === "preview_url") parts.push(typeof body.url === "string" ? body.url : "");
  else parts.push(sha(body.blob));
  if (demo.kind === "recording") parts.push(sha(body.poster));
  if (demo.kind === "diff") parts.push(typeof body.commit === "string" ? body.commit : "");
  return parts.join("|");
}

export interface DemoDelta {
  /** Per current demo: unchanged from the previous delivery, or new. */
  marks: ("same" | "new")[];
  /** Previous demos with no match in this delivery. */
  dropped: number;
}

/** Match demos one to one, so a repeated demo counts once per copy. */
export function compareDemos(current: Demo[], previous: Demo[]): DemoDelta {
  const left = previous.map(demoKey);
  const marks = current.map((demo) => {
    const at = left.indexOf(demoKey(demo));
    if (at < 0) return "new" as const;
    left.splice(at, 1);
    return "same" as const;
  });
  return { marks, dropped: left.length };
}

export interface ColumnDelta {
  /** Indices of this column's lines not in the previous column. */
  added: number[];
  /** Previous lines no longer present, in their old order. */
  removed: string[];
}

export type DisclosureDelta = Record<keyof Disclosure, ColumnDelta>;

function compareColumn(current: string[], previous: string[]): ColumnDelta {
  const left = [...previous];
  const added: number[] = [];
  current.forEach((line, index) => {
    const at = left.indexOf(line);
    if (at < 0) added.push(index);
    else left.splice(at, 1);
  });
  return { added, removed: left };
}

export function compareDisclosure(current: Disclosure, previous: Disclosure): DisclosureDelta {
  const column = (key: keyof Disclosure) => compareColumn(current[key] ?? [], previous[key] ?? []);
  return {
    proven: column("proven"),
    not_proven_yet: column("not_proven_yet"),
    faked_for_now: column("faked_for_now"),
  };
}

/** One row of the section: the reason, a comment, or an overall note. */
export interface SinceRow {
  kind: "reason" | "comment" | "note";
  /** The comment's 1-based number. */
  n?: number;
  comment?: ReviewComment;
  /** What the human wrote; empty for an overall note. */
  text: string;
  reply?: string;
}

export function sinceRows(note: SentBackNote, replies: DeliveryState["replies"]): SinceRow[] {
  const list = replies ?? [];
  const overall = list.find((reply) => reply.comment === undefined)?.text;
  const rows: SinceRow[] = [];
  const reason = note.reason?.trim();
  if (reason) rows.push({ kind: "reason", text: reason, reply: overall });
  (note.comments ?? []).forEach((comment, index) => {
    const n = index + 1;
    rows.push({
      kind: "comment",
      n,
      comment,
      text: comment.text,
      reply: list.find((reply) => reply.comment === n)?.text,
    });
  });
  if (!reason && overall) rows.push({ kind: "note", text: "", reply: overall });
  return rows;
}

/** "validate.rs:840", "Transcript 2, line 12", "Screenshot 1, area". */
export function placeLabel(anchor: ReviewAnchor, demos: Demo[]): string {
  if (anchor.kind === "diff_line") return diffLineLabel(anchor.file, anchor.side, anchor.line);
  if (anchor.kind === "transcript_line") {
    const ordinal = demos
      .slice(0, anchor.demo + 1)
      .filter((demo) => demo.kind === "cli_transcript").length;
    return tx("rd.loc.tline", { n: Math.max(ordinal, 1), line: anchor.line });
  }
  return commentLabel(anchor, demos);
}

/** The quoted line of a comment, if its anchor carries one. */
export function anchorQuote(anchor: ReviewAnchor): string | undefined {
  // The stored line keeps its code indentation; one line shows it bare.
  return anchor.kind === "region" ? undefined : anchor.quote.trim() || undefined;
}

/** A one-line name for a demo: its alt text, a kind, or a link. */
export function demoLabel(demo: Demo): string {
  const body = demo.body ?? {};
  if (demo.kind === "screenshot") return demoAlt(demo);
  if (demo.kind === "cli_transcript") return tx("mv.transcript");
  if (demo.kind === "diff") {
    const commit = typeof body.commit === "string" ? body.commit.slice(0, 7) : "";
    return commit ? `${tx("mv.diff")} · ${commit}` : tx("mv.diff");
  }
  if (demo.kind === "preview_url") return demoUrl(demo) ?? demo.kind;
  return demo.kind;
}

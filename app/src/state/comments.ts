/**
 * Anchored send-back comments: pure helpers for the review page. A comment
 * points at a diff line, a transcript line or a rectangle on a screenshot;
 * the host checks it against the delivery and re-reads every quote.
 */

import { tx } from "../i18n";
import type { Demo, ReviewAnchor, ReviewComment } from "../api/types";

/** Most comments one send-back carries (the wire's limit). */
export const MAX_COMMENTS = 20;
/** Longest comment, in characters. */
export const MAX_COMMENT_TEXT = 300;
const MAX_QUOTE = 200;

/** A comment with its 1-based number on the page. */
export interface Pin {
  n: number;
  comment: ReviewComment;
}

/** What one demo needs to offer and show comments. */
export interface DemoComments {
  /** This demo's index in the delivery. */
  demo: number;
  /** New comments can be added: live, nothing pending, under the limit. */
  enabled: boolean;
  /** Existing comments can be edited or removed: live, nothing pending. */
  editable: boolean;
  pins: Pin[];
  onAdd: (anchor: ReviewAnchor, text: string) => void;
  /** Removes the page's comment at this 0-based index; later numbers move up. */
  onRemove: (index: number) => void;
  /** Replaces the text of the page's comment at this 0-based index. */
  onEdit: (index: number, text: string) => void;
  /**
   * A sent-back version: the comments are read in place, and `open` is the
   * number of the comment to open on (scrolled to, or zoomed to in the viewer).
   */
  past?: { open: number | null };
}

const nothing = () => {};

/** The comments of a send-back on one demo of the version they were made on. */
export function pastComments(comments: ReviewComment[], demo: number, open: number | null): DemoComments {
  return {
    demo,
    enabled: false,
    editable: false,
    pins: comments.map((comment, at) => ({ n: at + 1, comment })).filter(({ comment }) => comment.anchor.demo === demo),
    onAdd: nothing,
    onRemove: nothing,
    onEdit: nothing,
    past: { open },
  };
}

/** One line as a quote: at most 200 characters, cut with "…". */
export function clipQuote(line: string): string {
  const chars = Array.from(line);
  return chars.length <= MAX_QUOTE ? line : `${chars.slice(0, MAX_QUOTE - 1).join("")}…`;
}

/** Transcript lines exactly as the host numbers them (1-based). */
export function transcriptLines(text: string): string[] {
  const lines = text.split("\n");
  if (lines.length && lines[lines.length - 1] === "") lines.pop();
  return lines.map((line) => (line.endsWith("\r") ? line.slice(0, -1) : line));
}

/** Pins whose anchor is this diff line. */
export function diffPins(pins: Pin[], file: string, side: "old" | "new", line: number): Pin[] {
  return pins.filter(
    ({ comment: { anchor } }) =>
      anchor.kind === "diff_line" && anchor.file === file && anchor.side === side && anchor.line === line,
  );
}

/** Pins whose anchor is this transcript line. */
export function linePins(pins: Pin[], line: number): Pin[] {
  return pins.filter(
    ({ comment: { anchor } }) => anchor.kind === "transcript_line" && anchor.line === line,
  );
}

const clamp = (value: number, low: number, high: number) => Math.min(high, Math.max(low, value));

/**
 * The per-mille rectangle spanned by two points given as fractions of the
 * image (0..1), clamped into the image and at least 1‰ wide and high.
 */
export function regionFrom(
  a: { x: number; y: number },
  b: { x: number; y: number },
): { x: number; y: number; w: number; h: number } {
  const x0 = clamp(Math.round(Math.min(a.x, b.x) * 1000), 0, 999);
  const y0 = clamp(Math.round(Math.min(a.y, b.y) * 1000), 0, 999);
  const x1 = clamp(Math.round(Math.max(a.x, b.x) * 1000), x0 + 1, 1000);
  const y1 = clamp(Math.round(Math.max(a.y, b.y) * 1000), y0 + 1, 1000);
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

function baseName(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash < 0 ? path : path.slice(slash + 1);
}

/** "validate.rs:840"; a removed line is marked "(old)". */
export function diffLineLabel(file: string, side: "old" | "new", line: number): string {
  const place = `${baseName(file)}:${line}`;
  return side === "old" ? tx("cm.loc.old", { x: place }) : place;
}

/** Short location label: "validate.rs:840", "Transcript line 12", "Screenshot 1, area". */
export function commentLabel(anchor: ReviewAnchor, demos: Demo[]): string {
  if (anchor.kind === "diff_line") return diffLineLabel(anchor.file, anchor.side, anchor.line);
  if (anchor.kind === "transcript_line") return tx("cm.loc.line", { n: anchor.line });
  const ordinal = demos.slice(0, anchor.demo + 1).filter((demo) => demo.kind === "screenshot").length;
  return tx("cm.loc.area", { n: Math.max(ordinal, 1) });
}

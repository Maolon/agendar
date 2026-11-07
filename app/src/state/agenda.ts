/**
 * Agenda edit helpers (`update_project` / `update_feature`). The UI sends only the fields the human changed, with the
 * expected revision of exactly those fields, and never invents a revision for
 * a field the projection has not seen yet.
 */

import { tx } from "../i18n";
import type { Sha256, UtcTime } from "../api/types";

/** Same limits as the core (`model/validate.rs` max_len). */
export const GOAL_MAX = 220;
export const FEATURE_TITLE_MAX = 110;
export const PRIORITY_MAX = 3;

/** Revision guards for exactly the patched fields. */
export function expectedFields(
  revisions: Record<string, Sha256>,
  fields: string[],
): Record<string, Sha256> {
  const expected: Record<string, Sha256> = {};
  for (const field of fields) {
    const revision = revisions[field];
    if (revision) expected[field] = revision;
  }
  return expected;
}

export type DeadlinePatch = { op: "set"; value: UtcTime } | { op: "clear" };

export type FeatureEdit = {
  title?: string;
  deadline?: DeadlinePatch;
  priority?: number;
};

export type ProjectEdit = { goal?: string; archived?: boolean };

function trimmedLength(text: string): number {
  return [...text.trim()].length;
}

export function validateGoal(text: string): string | null {
  const length = trimmedLength(text);
  if (length === 0) return tx("ag.goal_blank");
  if (length > GOAL_MAX) return tx("ag.goal_long", { n: GOAL_MAX });
  return null;
}

export function validateFeatureTitle(text: string): string | null {
  const length = trimmedLength(text);
  if (length === 0) return tx("ag.title_blank");
  if (length > FEATURE_TITLE_MAX) {
    return tx("ag.title_long", { n: FEATURE_TITLE_MAX });
  }
  return null;
}

export function validatePriority(text: string): string | null {
  if (!/^\d+$/.test(text.trim())) return tx("ag.priority", { n: PRIORITY_MAX });
  const value = Number(text);
  if (!Number.isSafeInteger(value) || value < 0 || value > PRIORITY_MAX) {
    return tx("ag.priority", { n: PRIORITY_MAX });
  }
  return null;
}

/** Empty text means "clear the deadline"; anything else must be a readable time. */
export function parseDeadline(text: string): DeadlinePatch | string {
  const value = text.trim();
  if (!value) return { op: "clear" };
  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) return tx("ag.iso");
  return { op: "set", value: new Date(parsed).toISOString() };
}

/** `datetime-local` input value for an existing deadline, in local time. */
export function deadlineInputValue(deadline: UtcTime | undefined): string {
  if (!deadline) return "";
  const parsed = Date.parse(deadline);
  if (Number.isNaN(parsed)) return "";
  const date = new Date(parsed);
  const pad = (value: number): string => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}

/** Convert a `datetime-local` value to a UTC instant; local display only. */
export function fromDeadlineInput(value: string): string | null {
  if (!value.trim()) return null;
  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) return null;
  return new Date(parsed).toISOString();
}

/**
 * Build the `update_feature` patch. `deadline` is the tagged DeadlineUpdate;
 * the caller decides set/clear from the human's input.
 */
export function featurePatch(edit: FeatureEdit): Record<string, unknown> {
  const patch: Record<string, unknown> = {};
  if (edit.title !== undefined) patch.title = edit.title.trim();
  if (edit.deadline !== undefined) patch.deadline = edit.deadline;
  if (edit.priority !== undefined) patch.priority = edit.priority;
  return patch;
}

export function featureFields(edit: FeatureEdit): string[] {
  const fields: string[] = [];
  if (edit.title !== undefined) fields.push("title");
  if (edit.deadline !== undefined) fields.push("deadline");
  if (edit.priority !== undefined) fields.push("priority");
  return fields;
}

export function projectPatch(edit: ProjectEdit): Record<string, unknown> {
  const patch: Record<string, unknown> = {};
  if (edit.goal !== undefined) patch.goal = edit.goal.trim();
  if (edit.archived !== undefined) patch.archived = edit.archived;
  return patch;
}

export function projectFields(edit: ProjectEdit): string[] {
  return [
    ...(edit.goal !== undefined ? ["goal"] : []),
    ...(edit.archived !== undefined ? ["archived"] : []),
  ];
}

/** Human-readable deadline for the plan list; unreadable values stay visible. */
export function formatDeadline(deadline: UtcTime | undefined): string {
  if (!deadline) return "";
  const parsed = Date.parse(deadline);
  if (Number.isNaN(parsed)) return deadline;
  return new Intl.DateTimeFormat(undefined, {
    weekday: "short",
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(new Date(parsed));
}

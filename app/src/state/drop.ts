/**
 * The owner's drop (`drop_steps`): which steps can go, how a dropped step
 * reads, and the request id a retry repeats.
 */

import { isApiError } from "../api/client";
import { newRequestId } from "../api/format";
import type { DropStepsRequest, FeatureState } from "../api/types";
import { tx } from "../i18n";

/** Same limit as the core (`limits::NOTE`). */
export const DROP_NOTE_MAX = 240;

/** The core refuses a step that is finished or already dropped. */
export function canDrop(feature: Pick<FeatureState, "progress">): boolean {
  return feature.progress === "planned" || feature.progress === "doing";
}

/** "Dropped by you" with the owner's note, or the plain label for an agent's drop. */
export function droppedLabel(feature: Pick<FeatureState, "progress" | "owner_drop">): string {
  const drop = feature.owner_drop;
  if (!drop) return tx("progress.dropped");
  return drop.note ? `${tx("plan.dropped_by_you")} · ${drop.note}` : tx("plan.dropped_by_you");
}

/** The index of the refused step when the error names one (`/nodes/<index>`). */
export function refusedIndex(error: unknown): number | null {
  if (!isApiError(error) || !error.field) return null;
  const match = /^\/nodes\/(\d+)$/.exec(error.field);
  return match ? Number(match[1]) : null;
}

/**
 * Builds `drop_steps` requests. The same steps and note sent again carry the
 * same request id, so a retry replays the result; any change gets a new one.
 */
export function dropRequests(): (project: string, nodes: string[], note: string) => DropStepsRequest {
  let last: { key: string; id: string } | null = null;
  return (project, nodes, note) => {
    const trimmed = note.trim();
    const body = { project, nodes, ...(trimmed ? { note: trimmed } : {}) };
    const key = JSON.stringify(body);
    if (last?.key !== key) last = { key, id: newRequestId() };
    return { request_id: last.id, ...body };
  };
}

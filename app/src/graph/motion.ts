/**
 * Motion rules as pure decisions.
 *
 * The prototype's ambient loops, random ages and sync timers are deliberately
 * not ported: every animation is driven by one real `ChangedEvent` or one
 * `agendar://unblock` transition, and the gate below stops all of it for
 * reduced motion, the in-app preference, a hidden window or stale data.
 */

import type { ChangedEvent, FeatureState, UtcTime } from "../api/types";

/** One ripple: 1.2 s ease-out, removed at 1300 ms. */
export const BLIP_MS = 1200;
export const BLIP_REMOVE_MS = 1300;
/** One travelling dot: 1.1 s, opacity 0/1/1/0, removed 1300 ms after start. */
export const PACKET_MS = 1100;
export const PACKET_REMOVE_MS = 1300;
/** Outgoing edges start 90 ms apart. */
export const PACKET_STAGGER_MS = 90;
/** Sidebar segment flash: 0.9 s. */
export const SEGMENT_FLASH_MS = 900;
/** Unblock moment: packet at 350 ms, released blips at 1050 ms. */
export const UNBLOCK_PACKET_MS = 350;
export const UNBLOCK_BLIP_MS = 1050;
/** A resolved-answer moment stays available for this long. */
export const UNBLOCK_WINDOW_MS = 60_000;
/** Freshness budget for an otherwise live session. */
export const FRESH_MS = 15 * 60 * 1000;
/** Bounded pulse-id memory: never animate the same pulse twice. */
export const PULSE_MEMORY = 256;
/** A burst larger than this animates the first entries only. */
export const PULSE_BATCH_LIMIT = 32;

export interface MotionGate {
  /** `preferences.motion`. */
  preference: boolean;
  /** `prefers-reduced-motion: reduce`. */
  reduced: boolean;
  /** `document.visibilityState === "visible"`. */
  visible: boolean;
  /** The app is showing a fresh snapshot, not a stale banner. */
  fresh: boolean;
}

/** `motionAllowed = preferences.motion && !reduced && visible`, and never stale. */
export function motionAllowed(gate: MotionGate): boolean {
  return gate.preference && !gate.reduced && gate.visible && gate.fresh;
}

/**
 * Whether a feature may carry the ambient radar. Only live, firm, unpaused
 * doing work with an open gate qualifies; waiting, blocked, stopped, finished
 * and stale items never move.
 */
export function isMotionEligible(
  feature: FeatureState | undefined,
  anchor: UtcTime,
  lastActivity?: UtcTime,
): boolean {
  if (!feature) return false;
  if (feature.progress !== "doing") return false;
  if (feature.gate !== "open") return false;
  if (feature.basis === "tainted") return false;
  if (feature.paused) return false;
  if (feature.liveness !== "live") return false;
  if (!lastActivity) return true;
  const anchorMs = Date.parse(anchor);
  const activityMs = Date.parse(lastActivity);
  if (!Number.isFinite(anchorMs) || !Number.isFinite(activityMs)) return true;
  return anchorMs - activityMs <= FRESH_MS;
}

export interface MotionPulse {
  id: string;
  project: string;
  node: string;
  host: string;
  event_ts: UtcTime;
}

export interface PulseBatch {
  pulses: MotionPulse[];
  /** `local` blips immediately; `sync` shows the event's own timestamp. */
  batch: ChangedEvent["batch"];
  /** `as_of` of the batch, for age labels. */
  asOf: UtcTime;
}

/**
 * Pulses that may animate for one committed event: only entries whose node is
 * eligible, never seen before, and never for transport-only batches. The
 * caller passes the bounded seen-set; ids are recorded even when the entry
 * does not animate, so a later remount cannot replay it.
 */
export function animatablePulses(
  event: ChangedEvent,
  seen: Set<string>,
  eligible: (pulse: ChangedEvent["pulses"][number]) => boolean,
): PulseBatch {
  const batch = { pulses: [] as MotionPulse[], batch: event.batch, asOf: event.as_of };
  if (event.batch === "time" || event.batch === "rebuild") return batch;
  for (const pulse of event.pulses ?? []) {
    if (!pulse || typeof pulse.id !== "string") continue;
    if (seen.has(pulse.id)) continue;
    seen.add(pulse.id);
    if (batch.pulses.length >= PULSE_BATCH_LIMIT) continue;
    if (!eligible(pulse)) continue;
    batch.pulses.push({
      id: pulse.id,
      project: pulse.project,
      node: pulse.node,
      host: pulse.host,
      event_ts: pulse.event_ts,
    });
  }
  pruneSeen(seen);
  return batch;
}

/** Keep the dedupe memory bounded without an unbounded history. */
export function pruneSeen(seen: Set<string>, limit = PULSE_MEMORY): void {
  if (seen.size <= limit) return;
  let excess = seen.size - limit;
  for (const id of seen) {
    if (excess <= 0) break;
    seen.delete(id);
    excess -= 1;
  }
}

/** Start delay per outgoing edge: 0, 90, 180 … */
export function packetDelays(count: number): number[] {
  return Array.from({ length: Math.max(0, count) }, (_, index) => index * PACKET_STAGGER_MS);
}

export interface PlayableUnblock {
  answerId: string;
  project: string;
  from: string;
  released: string[];
}

/**
 * The one orchestrated moment: only the released-set nodes blip, and only
 * within the minute after a persisted answer. A stale or foreign transition
 * returns `null` instead of replaying history on remount.
 */
export function playableUnblock(
  play: { project: string; from: string; released: string[]; answer_id: string; at: number } | null,
  project: string,
  now: number,
): PlayableUnblock | null {
  if (!play) return null;
  if (play.project !== project) return null;
  if (now - play.at > UNBLOCK_WINDOW_MS) return null;
  if (!play.released.length) return null;
  return {
    answerId: play.answer_id,
    project: play.project,
    from: play.from,
    released: [...play.released],
  };
}

export interface PacketPlan {
  from: string;
  to: string;
  path: string;
  delay: number;
}

/**
 * Packets for a pulse: every outgoing edge of the pulsed node, staggered.
 * `paths` returns the drawn curve for one edge, or `undefined` when the graph
 * has no geometry for it.
 */
export function pulsePacketPlan(
  node: string,
  outgoing: readonly string[],
  pathFor: (to: string) => string | undefined,
): PacketPlan[] {
  const plans: PacketPlan[] = [];
  outgoing.forEach((to, index) => {
    const path = pathFor(to);
    if (!path) return;
    plans.push({ from: node, to, path, delay: index * PACKET_STAGGER_MS });
  });
  return plans;
}

/**
 * Packets for the unblock moment. `from` may name a node the refreshed graph no
 * longer publishes (a pending decision disappears once answered), so the plan
 * falls back to every edge that enters a released node.
 */
export function unblockPacketPlan(
  edges: readonly { from: string; to: string; path: string }[],
  from: string,
  released: readonly string[],
): PacketPlan[] {
  const targets = new Set(released);
  const outgoing = edges.filter((edge) => edge.from === from);
  // The pending decision node disappears once it is answered, so the graph may
  // no longer publish `from`; fall back to every edge entering a released node.
  const fromIsDrawable = outgoing.some((edge) => targets.has(edge.to));
  const chosen = fromIsDrawable
    ? outgoing.filter((edge) => targets.has(edge.to))
    : edges.filter((edge) => targets.has(edge.to));
  return chosen.map((edge, index) => ({
    from: edge.from,
    to: edge.to,
    path: edge.path,
    delay: index * PACKET_STAGGER_MS,
  }));
}

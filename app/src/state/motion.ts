/**
 * Motion wiring.
 *
 * `layout.ts`/`motion.ts` decide *what* may move; these hooks decide *when* the
 * environment allows moving at all and turn one committed change batch into
 * exactly one animation batch. Nothing here polls: the input is the store's
 * last `agendar://changed` event.
 */

import { useEffect, useRef, useState } from "react";
import type { ChangedEvent, Snapshot } from "../api/types";
import {
  SEGMENT_FLASH_MS,
  animatablePulses,
  motionAllowed as gateAllows,
  type MotionPulse,
} from "../graph/motion";
import { pulseEligible } from "./graph";

/** Tracks `prefers-reduced-motion` and window visibility without polling. */
export function useEnvironment(): { reduced: boolean; visible: boolean } {
  const [state, setState] = useState(() => readEnvironment());

  useEffect(() => {
    if (typeof window === "undefined") return;
    const update = () => setState(readEnvironment());
    const query =
      typeof window.matchMedia === "function"
        ? window.matchMedia("(prefers-reduced-motion: reduce)")
        : null;
    query?.addEventListener?.("change", update);
    document.addEventListener("visibilitychange", update);
    return () => {
      query?.removeEventListener?.("change", update);
      document.removeEventListener("visibilitychange", update);
    };
  }, []);

  return state;
}

function readEnvironment(): { reduced: boolean; visible: boolean } {
  if (typeof window === "undefined") return { reduced: false, visible: true };
  const reduced =
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const visible =
    typeof document === "undefined" || document.visibilityState !== "hidden";
  return { reduced, visible };
}

export function useMotionAllowed(preference: boolean, fresh: boolean): boolean {
  const environment = useEnvironment();
  return gateAllows({
    preference,
    reduced: environment.reduced,
    visible: environment.visible,
    fresh,
  });
}

export interface PulseFeed {
  /** Increments once per consumed batch; a graph animates on token change. */
  token: number;
  /** Eligible pulses of the latest batch, in arrival order. */
  pulses: MotionPulse[];
  /** `${project}/${node}` keys the sidebar flashes for one pulse. */
  flashes: ReadonlySet<string>;
  /**
   * Latest pulse time per `${project}/${node}`, including ineligible entries: a
   * sync batch shows the event's own timestamp, never “just now”.
   */
  ages: Readonly<Record<string, string>>;
}

const EMPTY_FEED: PulseFeed = { token: 0, pulses: [], flashes: new Set(), ages: {} };

/**
 * Turn the store's last change batch into motion, once.
 *
 * A batch is evaluated only after the projection reflects it, so a pulse on a
 * feature that just started is judged against its new axes, not the old ones.
 * While motion is off the ids are still consumed, so re-enabling it never
 * replays history.
 */
export function usePulseFeed(
  change: { event: ChangedEvent; seq: number } | null,
  snapshot: Snapshot | null,
  allowed: boolean,
): PulseFeed {
  const seen = useRef<Set<string>>(new Set());
  const [feed, setFeed] = useState<PulseFeed>(EMPTY_FEED);
  const [pending, setPending] = useState<ChangedEvent | null>(null);
  const lastSeq = useRef(0);

  useEffect(() => {
    if (!change || change.seq === lastSeq.current) return;
    lastSeq.current = change.seq;
    setPending(change.event);
  }, [change]);

  useEffect(() => {
    if (!pending) return;
    if (!snapshot || snapshot.generation < pending.generation) return;
    const batch = animatablePulses(pending, seen.current, (pulse) =>
      pulseEligible(snapshot, pulse),
    );
    const ages: Record<string, string> = {};
    for (const entry of pending.pulses ?? []) {
      if (!entry || typeof entry.id !== "string") continue;
      ages[`${entry.project}/${entry.node}`] = entry.event_ts;
    }
    setPending(null);
    if (!allowed) {
      // Consume the ids with no motion so a later enable cannot replay them.
      setFeed((current) => ({
        token: current.token + 1,
        pulses: [],
        flashes: new Set(),
        ages: { ...current.ages, ...ages },
      }));
      return;
    }
    const flashes = new Set(batch.pulses.map((pulse) => `${pulse.project}/${pulse.node}`));
    setFeed((current) => ({
      token: current.token + 1,
      pulses: batch.pulses,
      flashes,
      ages: { ...current.ages, ...ages },
    }));
  }, [allowed, pending, snapshot]);

  useEffect(() => {
    if (!feed.flashes.size) return;
    const timer = window.setTimeout(
      () => setFeed((current) => ({ ...current, flashes: new Set() })),
      SEGMENT_FLASH_MS,
    );
    return () => window.clearTimeout(timer);
  }, [feed.token, feed.flashes]);

  return feed;
}

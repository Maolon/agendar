import { describe, expect, it } from "vitest";
import type { ChangedEvent, FeatureState } from "../api/types";
import {
  FRESH_MS,
  PACKET_STAGGER_MS,
  PULSE_BATCH_LIMIT,
  PULSE_MEMORY,
  UNBLOCK_WINDOW_MS,
  animatablePulses,
  isMotionEligible,
  motionAllowed,
  packetDelays,
  playableUnblock,
  pruneSeen,
  pulsePacketPlan,
  unblockPacketPlan,
} from "../graph/motion";
import { makeProject, sha, ulid } from "./fixtures";

const ANCHOR = "2026-02-10T15:00:00.000Z";

function liveFeature(overrides: Partial<FeatureState> = {}): FeatureState {
  return {
    ...makeProject("alpha").features["core/one"]!,
    progress: "doing",
    gate: "open",
    basis: "firm",
    liveness: "live",
    paused: false,
    ...overrides,
  };
}

function changed(overrides: Partial<ChangedEvent> = {}): ChangedEvent {
  return {
    v: 1,
    epoch: ulid(900),
    generation: 8,
    event_set_digest: sha(0x99),
    as_of: ANCHOR,
    projects: ["alpha"],
    pulses: [
      { id: "pulse-1", project: "alpha", node: "core/one", host: "h-1", event_ts: ANCHOR },
      { id: "pulse-2", project: "alpha", node: "core/one", host: "h-1", event_ts: ANCHOR },
    ],
    batch: "local",
    ...overrides,
  };
}

describe("motion gate", () => {
  it("requires the preference, no reduced-motion request, a visible window and fresh data", () => {
    expect(
      motionAllowed({ preference: true, reduced: false, visible: true, fresh: true }),
    ).toBe(true);
    expect(
      motionAllowed({ preference: false, reduced: false, visible: true, fresh: true }),
    ).toBe(false);
    expect(
      motionAllowed({ preference: true, reduced: true, visible: true, fresh: true }),
    ).toBe(false);
    expect(
      motionAllowed({ preference: true, reduced: false, visible: false, fresh: true }),
    ).toBe(false);
    expect(
      motionAllowed({ preference: true, reduced: false, visible: true, fresh: false }),
    ).toBe(false);
  });
});

describe("motion eligibility", () => {
  it("allows a live, firm, unpaused doing feature", () => {
    expect(isMotionEligible(liveFeature(), ANCHOR, "2026-02-10T14:59:00.000Z")).toBe(true);
  });

  it("stops waiting, blocked, finished, stopped and tainted items", () => {
    expect(isMotionEligible(liveFeature({ progress: "planned" }), ANCHOR)).toBe(false);
    expect(isMotionEligible(liveFeature({ gate: "awaiting_upstream" }), ANCHOR)).toBe(false);
    expect(isMotionEligible(liveFeature({ progress: "done" }), ANCHOR)).toBe(false);
    expect(isMotionEligible(liveFeature({ progress: "verified" }), ANCHOR)).toBe(false);
    expect(isMotionEligible(liveFeature({ liveness: "lost" }), ANCHOR)).toBe(false);
    expect(isMotionEligible(liveFeature({ liveness: "quiet" }), ANCHOR)).toBe(false);
    expect(isMotionEligible(liveFeature({ basis: "tainted" }), ANCHOR)).toBe(false);
    expect(isMotionEligible(liveFeature({ paused: true }), ANCHOR)).toBe(false);
    expect(isMotionEligible(liveFeature({ phase: "in_review" }), ANCHOR)).toBe(false);
    expect(isMotionEligible(liveFeature({ phase: "sent_back" }), ANCHOR)).toBe(true);
    expect(isMotionEligible(undefined, ANCHOR)).toBe(false);
  });

  it("treats a live session with no event for over 15 minutes as stale", () => {
    const stale = new Date(Date.parse(ANCHOR) - FRESH_MS - 1).toISOString();
    const fresh = new Date(Date.parse(ANCHOR) - FRESH_MS + 1000).toISOString();
    expect(isMotionEligible(liveFeature(), ANCHOR, stale)).toBe(false);
    expect(isMotionEligible(liveFeature(), ANCHOR, fresh)).toBe(true);
  });

  it("keeps an assumed but firm-less feature still, and an assumed basis moving", () => {
    // `assumed` keeps the plan honest with a dashed outline; only a taint stops motion.
    expect(isMotionEligible(liveFeature({ basis: "assumed" }), ANCHOR)).toBe(true);
  });
});

describe("pulse batches", () => {
  it("animates one entry per new eligible pulse, never twice", () => {
    const seen = new Set<string>();
    const first = animatablePulses(changed(), seen, () => true);
    expect(first.pulses.map((pulse) => pulse.id)).toEqual(["pulse-1", "pulse-2"]);
    expect(first.batch).toBe("local");
    const second = animatablePulses(changed({ generation: 9 }), seen, () => true);
    expect(second.pulses).toEqual([]);
  });

  it("records ineligible pulses so a later remount cannot replay them", () => {
    const seen = new Set<string>();
    const batch = animatablePulses(changed(), seen, () => false);
    expect(batch.pulses).toEqual([]);
    expect(seen.has("pulse-1")).toBe(true);
    expect(animatablePulses(changed(), seen, () => true).pulses).toEqual([]);
  });

  it("never animates a time or rebuild batch", () => {
    const seen = new Set<string>();
    expect(animatablePulses(changed({ batch: "time" }), seen, () => true).pulses).toEqual([]);
    expect(animatablePulses(changed({ batch: "rebuild" }), seen, () => true).pulses).toEqual([]);
    // Those batches did not consume the ids either; only eligible motion does.
    expect(animatablePulses(changed({ batch: "sync" }), seen, () => true).pulses).toHaveLength(2);
  });

  it("keeps the remote event timestamp and the sync batch marker", () => {
    const seen = new Set<string>();
    const remote = "2026-02-10T14:20:00.000Z";
    const batch = animatablePulses(
      changed({
        batch: "sync",
        pulses: [{ id: "remote-1", project: "beta", node: "core/two", host: "h-2", event_ts: remote }],
      }),
      seen,
      () => true,
    );
    expect(batch.batch).toBe("sync");
    expect(batch.pulses[0]?.event_ts).toBe(remote);
  });

  it("caps a burst and bounds the dedupe memory", () => {
    const seen = new Set<string>();
    const pulses = Array.from({ length: PULSE_BATCH_LIMIT + 5 }, (_, index) => ({
      id: `p-${index}`,
      project: "alpha",
      node: "core/one",
      host: "h-1",
      event_ts: ANCHOR,
    }));
    const batch = animatablePulses(changed({ pulses }), seen, () => true);
    expect(batch.pulses).toHaveLength(PULSE_BATCH_LIMIT);
    expect(seen.size).toBeLessThanOrEqual(PULSE_MEMORY);
    for (let index = 0; index < PULSE_MEMORY + 40; index += 1) seen.add(`x-${index}`);
    pruneSeen(seen);
    expect(seen.size).toBe(PULSE_MEMORY);
  });

  it("staggers outgoing edges 90 ms apart", () => {
    expect(packetDelays(3)).toEqual([0, PACKET_STAGGER_MS, PACKET_STAGGER_MS * 2]);
    expect(packetDelays(0)).toEqual([]);
  });
});

describe("packet plans", () => {
  it("follows every outgoing edge of the pulsed node", () => {
    const plans = pulsePacketPlan("feature:a", ["feature:b", "feature:c"], (to) =>
      to === "feature:b" ? "M0 0" : undefined,
    );
    expect(plans).toEqual([
      { from: "feature:a", to: "feature:b", path: "M0 0", delay: 0 },
    ]);
  });

  it("plays the unblock moment from the answered node to the released set only", () => {
    const edges = [
      { from: "feature:ipc", to: "feature:wake", path: "M0 0" },
      { from: "feature:ipc", to: "feature:async", path: "M0 1" },
      { from: "feature:other", to: "feature:wake", path: "M0 2" },
    ];
    expect(unblockPacketPlan(edges, "feature:ipc", ["feature:wake"])).toEqual([
      { from: "feature:ipc", to: "feature:wake", path: "M0 0", delay: 0 },
    ]);
    // The pending decision node vanished from the refreshed graph: fall back to
    // the edges that actually enter a released feature.
    expect(unblockPacketPlan(edges, "decision:ask", ["feature:wake"])).toEqual([
      { from: "feature:ipc", to: "feature:wake", path: "M0 0", delay: 0 },
      { from: "feature:other", to: "feature:wake", path: "M0 2", delay: PACKET_STAGGER_MS },
    ]);
  });

  it("keeps the unblock moment inside its one-minute window", () => {
    const play = {
      project: "alpha",
      from: "feature:ipc",
      released: ["feature:wake"],
      answer_id: "answer-1",
      at: 1_000_000,
    };
    expect(playableUnblock(play, "alpha", 1_000_000 + UNBLOCK_WINDOW_MS)?.answerId).toBe("answer-1");
    expect(playableUnblock(play, "alpha", 1_000_000 + UNBLOCK_WINDOW_MS + 1)).toBeNull();
    expect(playableUnblock(play, "beta", 1_050_000)).toBeNull();
    expect(playableUnblock({ ...play, released: [] }, "alpha", 1_050_000)).toBeNull();
    expect(playableUnblock(null, "alpha", 1_050_000)).toBeNull();
  });
});

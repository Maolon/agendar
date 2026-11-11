import { describe, expect, it } from "vitest";
import type { FeatureState, GraphNode } from "../api/types";
import { isRadarCandidate, nodeDisplay } from "../graph/display";
import { makeProject } from "./fixtures";

const feature: FeatureState = makeProject("alpha").features["core/one"]!;

function node(overrides: Partial<GraphNode> = {}): GraphNode {
  return {
    id: "feature:core/one",
    kind: "feature",
    label: "CLI ask",
    feature: "core/one",
    state: "doing",
    assumed: false,
    ...overrides,
  };
}

describe("graph node display", () => {
  it("shows a stopped glyph before anything else", () => {
    const display = nodeDisplay(
      node(),
      { ...feature, progress: "doing", gate: "open", basis: "tainted", liveness: "lost" },
    );
    expect(display.dot).toBe("stopped");
    expect(display.sub).toBe("Stopped");
  });

  it("labels a tainted feature as rework and keeps its ETA", () => {
    const display = nodeDisplay(
      node(),
      {
        ...feature,
        progress: "verified",
        basis: "tainted",
        eta: {
          agent_range: { min_s: 60, max_s: 120 },
          anchor: "2026-02-10T14:00:00.000Z",
          paused_s: 0,
          agent_finish: {
            earliest: "2026-02-10T14:10:00.000Z",
            latest: "2026-02-10T14:20:00.000Z",
          },
          late_by_s: 300,
          paused: false,
        },
      },
    );
    expect(display.dot).toBe("tainted");
    expect(display.sub).toBe("Rework · 5m late");
  });

  it("keeps a verified node green and a pending decision amber", () => {
    const verified = nodeDisplay(node(), { ...feature, progress: "verified", gate: "open" });
    expect(verified.dot).toBe("verified");
    expect(verified.icon).toBe("check");
    const decision = nodeDisplay(
      {
        id: "decision:x",
        kind: "decision",
        label: "Wake method?",
        need_id: "need-ipc",
        state: "open",
        assumed: false,
      },
      undefined,
    );
    expect(decision.you).toBe(true);
    expect(decision.dot).toBe("you");
    expect(decision.sub).toBe("Your call");
    expect(decision.assumed).toBe(false);
  });

  it("renders a defaulted assumption as static, never as an open call", () => {
    const display = nodeDisplay(
      {
        id: "decision:x",
        kind: "decision",
        label: "Keep WebM as the capture format?",
        state: "defaulted",
        assumed: true,
      },
      undefined,
    );
    expect(display.you).toBe(false);
    expect(display.sub).toBe("Assumed · not confirmed");
    expect(display.assumed).toBe(true);
    expect(display.title).toContain("assumed");
  });

  it("treats answered and accepted nodes as history", () => {
    const answered = nodeDisplay(
      { id: "decision:x", kind: "decision", label: "Wake method?", state: "answered", assumed: false },
      undefined,
    );
    expect(answered.you).toBe(false);
    expect(answered.sub).toBe("Answered");
    const acked = nodeDisplay(
      { id: "decision:x", kind: "decision", label: "Wake method?", state: "acked", assumed: false },
      undefined,
    );
    expect(acked.sub).toBe("Acknowledged");
    expect(acked.dot).toBe("verified");
    const sentBack = nodeDisplay(
      { id: "review:x", kind: "review", label: "search-v2", state: "sent_back", assumed: false },
      undefined,
    );
    expect(sentBack.you).toBe(false);
    expect(sentBack.sub).toBe("Sent back");
    const accepted = nodeDisplay(
      { id: "review:x", kind: "review", label: "search-v2", state: "accepted", assumed: false },
      undefined,
    );
    expect(accepted.sub).toBe("Accepted");
  });

  it("keeps an amber cue for a defaulted assumption that still needs its confirm", () => {
    const display = nodeDisplay(
      {
        id: "decision:x",
        kind: "decision",
        label: "Keep WebM as the capture format?",
        need_id: "need-confirm",
        state: "defaulted",
        assumed: true,
      },
      undefined,
    );
    expect(display.you).toBe(true);
    expect(display.sub).toBe("Confirm it");
    expect(display.assumed).toBe(true);
  });

  it("marks an assumed basis so the node can render a dashed outline", () => {
    const display = nodeDisplay(node({ assumed: true }), { ...feature, basis: "assumed" });
    expect(display.assumed).toBe(true);
  });

  it("runs the radar only on live, firm, unpaused doing work with an open gate", () => {
    expect(isRadarCandidate({ ...feature, progress: "doing", gate: "open" })).toBe(true);
    expect(isRadarCandidate({ ...feature, progress: "doing", gate: "awaiting_upstream" })).toBe(
      false,
    );
    expect(isRadarCandidate({ ...feature, progress: "doing", basis: "tainted" })).toBe(false);
    expect(isRadarCandidate({ ...feature, progress: "doing", paused: true })).toBe(false);
    // Delivered and waiting for the owner's review: not moving; sent back: the agent's again.
    expect(isRadarCandidate({ ...feature, progress: "doing", gate: "open", phase: "in_review" })).toBe(false);
    expect(isRadarCandidate({ ...feature, progress: "doing", gate: "open", phase: "sent_back" })).toBe(true);
    expect(isRadarCandidate({ ...feature, progress: "done" })).toBe(false);
    expect(isRadarCandidate(undefined)).toBe(false);
  });
});

import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { ClaimState, DeliveryState, FeatureState, ProjectState, Snapshot } from "../api/types";
import { ProjectOverview } from "../components/ProjectOverview";
import { squareState } from "../components/PlanMap";
import { isRadarCandidate, nodeDisplay } from "../graph/display";
import { nowBoard, sentBackUnattended } from "../state/now";
import { idleAgents, reviewWaiting, whoLabel, workingAgents } from "../state/project";
import { makeDelivery, makeProject, makeSnapshot, ulid } from "./fixtures";

function feature(node: string, overrides: Partial<FeatureState> = {}): FeatureState {
  const base = makeProject("alpha").features["core/one"]!;
  return { ...base, node, title: node, depends_on: [], unblocks: [], gate: "open", ...overrides };
}

function delivery(node: string, id: number, overrides: Partial<DeliveryState> = {}): DeliveryState {
  return makeDelivery({ id: ulid(id), node, review: "pending", ...overrides });
}

function project(features: FeatureState[], deliveries: DeliveryState[]): ProjectState {
  return makeProject("alpha", {
    features: Object.fromEntries(features.map((item) => [item.node, item])),
    deliveries: Object.fromEntries(deliveries.map((item) => [item.id, item])),
  });
}

describe("honest review counts", () => {
  // Accepted (done, no review pending), agent-done with a review pending, in
  // review, verified, and a dropped attempt's pending delivery.
  const steps = [
    feature("accepted", { progress: "done" }),
    feature("awaiting", { progress: "done" }),
    feature("inreview", { progress: "doing", phase: "in_review" }),
    feature("verified", { progress: "verified" }),
    feature("dropped", { progress: "dropped" }),
    feature("working", { progress: "doing", liveness: "live" }),
  ];
  const deliveries = [
    delivery("accepted", 900, { review: "accepted" }),
    delivery("awaiting", 901),
    delivery("inreview", 902),
    delivery("dropped", 903),
    delivery("verified", 904, { review: "pending", superseded_by: ulid(905) }),
  ];

  it("counts only pending reviews as waiting and the rest as finished", () => {
    const value = project(steps, deliveries);
    expect([...reviewWaiting(value)].sort()).toEqual(["awaiting", "inreview"]);
    const { container } = render(<ProjectOverview project={value} />);
    const legend = container.querySelector(".olegend")!.textContent!;
    expect(legend).toContain("1 verified");
    expect(legend).toContain("1 finished");
    expect(legend).toContain("2 waiting for your review");
    expect(legend).toContain("2 of 5 steps finished");
    expect(legend).not.toContain("ready for review");
  });

  it("reads a done step with nothing pending as finished on the plan and the graph", () => {
    const value = project(steps, deliveries);
    const waiting = reviewWaiting(value);
    expect(squareState(value.features.accepted!, waiting)).toBe("done");
    expect(squareState(value.features.awaiting!, waiting)).toBe("review");
    expect(squareState(value.features.inreview!, waiting)).toBe("review");
    const node = { id: "feature:accepted", kind: "feature" as const, label: "a", feature: "accepted", state: "done", assumed: false };
    expect(nodeDisplay(node, value.features.accepted).sub).toBe("Done");
    expect(nodeDisplay({ ...node, feature: "inreview" }, value.features.inreview).sub).toBe("Ready for review");
    expect(whoLabel(value.features.inreview!, value).text).toBe("waiting on you");
  });
});

describe("home page placement", () => {
  function board(features: FeatureState[]): ReturnType<typeof nowBoard> {
    const snapshot: Snapshot = makeSnapshot({ withRescue: false });
    snapshot.state.projects = { alpha: project(features, []) };
    snapshot.needs = [];
    snapshot.brief.delivered = [];
    return nowBoard(snapshot);
  }

  it("keeps a step in review out of Moving now and Stalled", () => {
    const result = board([
      feature("live", { progress: "doing", phase: "in_review", liveness: "live" }),
      feature("quiet", { progress: "doing", phase: "in_review", liveness: "quiet" }),
    ]);
    expect(result.moving).toHaveLength(0);
    expect(result.stalled).toHaveLength(0);
  });

  it("lists one session with a step in review and a step in progress as moving on the second only", () => {
    const session = ulid(600);
    const steps = [
      feature("delivered", { progress: "doing", phase: "in_review", liveness: "live", session }),
      feature("other", { progress: "doing", liveness: "live", session }),
    ];
    const result = board(steps);
    expect(result.moving.map((item) => item.feature.node)).toEqual(["other"]);
    expect(result.stalled).toHaveLength(0);
    // The agent counts (tray, sidebar): working on the second step; with only
    // the step in review it is connected and idle, not working.
    const value = project(steps, []);
    value.summary.active_agents = 1;
    expect([workingAgents(value), idleAgents(value)]).toEqual([1, 0]);
    const waiting = project([steps[0]!], []);
    waiting.summary.active_agents = 1;
    expect([workingAgents(waiting), idleAgents(waiting)]).toEqual([0, 1]);
  });

  it("puts a sent-back step whose agent went quiet in Stalled as sent back", () => {
    const result = board([
      feature("back", { progress: "doing", phase: "sent_back", liveness: "quiet", session: ulid(500) }),
      feature("fixing", { progress: "doing", phase: "sent_back", liveness: "live" }),
    ]);
    expect(result.moving.map((item) => item.feature.node)).toEqual(["fixing"]);
    expect(result.stalled[0]!.steps.map((item) => [item.feature.node, item.agent])).toEqual([["back", "sent_back"]]);
  });

  it("stalls a sent-back step nobody is on whatever its claim says, by the core's rule", () => {
    const held: ClaimState = { state: "held", since: "2026-02-10T14:00:00.000Z", last_word: "2026-02-10T14:00:00.000Z" };
    const steps = [
      // Quiet inside its ETA: the ETA was for the delivery already answered.
      feature("quiet-held", { progress: "doing", phase: "sent_back", liveness: "quiet", claim: held }),
      // Its session is live elsewhere, the claim on this step has stalled.
      feature("live-stalled", { progress: "doing", phase: "sent_back", liveness: "live", claim: { ...held, state: "stalled" } }),
      feature("fixing", { progress: "doing", phase: "sent_back", liveness: "live", claim: held }),
      // Waiting on its own question: under that question, in neither list.
      feature("asking", { progress: "doing", phase: "sent_back", liveness: "quiet", claim: { ...held, waiting_on: ulid(700) } }),
    ];
    expect(steps.map(sentBackUnattended)).toEqual([true, true, false, false]);
    const result = board(steps);
    expect(result.moving.map((item) => item.feature.node)).toEqual(["fixing"]);
    expect(result.stalled[0]!.steps.map((item) => [item.feature.node, item.agent]).sort()).toEqual([
      ["live-stalled", "sent_back"],
      ["quiet-held", "sent_back"],
    ]);
  });

  it("does not list an accepted step as moving, and keeps the agent moving on its other step", () => {
    const session = ulid(601);
    const accepted = feature("accepted", { progress: "done", liveness: "live", session });
    const alone = board([accepted]);
    expect(alone.moving).toHaveLength(0);
    expect(alone.stalled).toHaveLength(0);
    expect(isRadarCandidate(accepted)).toBe(false);
    // Connected with nothing in progress: idle, not working.
    const value = project([accepted], []);
    value.summary.active_agents = 1;
    expect([workingAgents(value), idleAgents(value)]).toEqual([0, 1]);

    const other = feature("other", { progress: "doing", liveness: "live", session });
    const both = board([accepted, other]);
    expect(both.moving.map((item) => item.feature.node)).toEqual(["other"]);
    expect(both.stalled).toHaveLength(0);
  });
});

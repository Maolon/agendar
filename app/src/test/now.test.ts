import { describe, expect, it } from "vitest";
import type { ActivityItem, DeliveryState, FeatureState, Snapshot } from "../api/types";
import {
  boardEmpty,
  dueText,
  etaBar,
  lateText,
  nowBoard,
  riskChips,
  stalledCount,
  ulidTime,
} from "../state/now";
import { EVALUATED_AT, makeDelivery, makeProject, makeSnapshot, ulid } from "./fixtures";

function step(node: string, overrides: Partial<FeatureState> = {}): FeatureState {
  return {
    ...makeProject("x").features["core/one"]!,
    node,
    title: node,
    unblocks: [],
    gate: "open",
    ...overrides,
  };
}

/** A ULID whose time part is `iso`. */
function ulidAt(iso: string): string {
  const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
  let time = Date.parse(iso);
  let head = "";
  for (let index = 0; index < 10; index += 1) {
    head = alphabet[time % 32]! + head;
    time = Math.floor(time / 32);
  }
  return `${head}0000000000000001`;
}

function pulse(node: string, ts: string, message: string): ActivityItem {
  return {
    ts,
    actor: "Claude Code",
    actor_kind: "agent",
    icon: "radio",
    text: `Claude Code: ${message}`,
    msg: { key: "act.pulse", args: { actor: { t: "text", v: "Claude Code" }, message: { t: "text", v: message } } },
    node,
    count: 1,
  };
}

function finished(node: string, ts: string): ActivityItem {
  return {
    ts,
    actor: "Claude Code",
    actor_kind: "agent",
    icon: "check",
    text: `Claude Code finished ${node}`,
    msg: { key: "act.finished", args: { actor: { t: "text", v: "Claude Code" }, step: { t: "text", v: node } } },
    node,
    count: 1,
  };
}

/** One project, no needs, no brief rows: each test adds what it checks. */
function bare(features: Record<string, FeatureState>): Snapshot {
  const snapshot = makeSnapshot({ withRescue: false });
  const project = makeProject("alpha", { features });
  snapshot.state.projects = { alpha: project };
  snapshot.needs = [];
  snapshot.brief.delivered = [];
  return snapshot;
}

const ETA = {
  agent_range: { min_s: 3600, max_s: 7200 },
  anchor: "2026-02-10T14:00:00.000Z",
  paused_s: 0,
  agent_finish: { earliest: "2026-02-10T15:00:00.000Z", latest: "2026-02-10T16:00:00.000Z" },
  late_by_s: 0,
  paused: false,
};

describe("now board", () => {
  it("splits doing steps by liveness: live is moving, quiet and lost are stalled", () => {
    const board = nowBoard(
      bare({
        a: step("a", { progress: "doing", liveness: "live" }),
        b: step("b", { progress: "doing", liveness: "quiet" }),
        c: step("c", { progress: "doing", liveness: "lost" }),
        d: step("d", { progress: "verified", liveness: "lost" }),
        e: step("e", { progress: "planned", liveness: "lost" }),
      }),
    );
    expect(board.moving.map((item) => item.feature.node)).toEqual(["a"]);
    expect(board.stalled).toHaveLength(1);
    expect(board.stalled[0]!.steps.map((item) => [item.feature.node, item.agent]).sort()).toEqual([
      ["b", "none"],
      ["c", "lost"],
      ["e", "lost"],
    ]);
    expect(stalledCount(board)).toBe(3);
  });

  it("names a quiet agent when the step's session is known, and paused before quiet", () => {
    const snapshot = bare({
      q: step("q", { progress: "doing", liveness: "quiet", session: ulid(500) }),
      p: step("p", { progress: "doing", liveness: "quiet", paused: true }),
    });
    snapshot.state.projects.alpha!.sessions[ulid(500)] = {
      id: ulid(500),
      host: "h",
      harness: "codex",
      last_event: ulid(501),
      last_event_ts: EVALUATED_AT,
      liveness: "quiet",
      stalled_on: [],
    };
    const steps = nowBoard(snapshot).stalled[0]!.steps;
    expect(Object.fromEntries(steps.map((item) => [item.feature.node, item.agent]))).toEqual({
      q: "quiet",
      p: "paused",
    });
  });

  it("measures the ETA bar from the anchor to the window's latest finish", () => {
    const live = step("a", { progress: "doing", liveness: "live", eta: ETA });
    expect(etaBar(live, EVALUATED_AT)?.share).toBeCloseTo(0.5);
    const late = step("b", { progress: "doing", liveness: "live", eta: { ...ETA, late_by_s: 5400 } });
    expect(etaBar(late, EVALUATED_AT)).toMatchObject({ share: 1, lateS: 5400 });
    const held = step("c", { progress: "doing", liveness: "live", eta: { ...ETA, paused: true } });
    expect(etaBar(held, EVALUATED_AT)?.paused).toBe(true);
    expect(etaBar(step("d"), EVALUATED_AT)).toBeUndefined();
  });

  it("writes lateness coarsely and a due date as the shortest local label", () => {
    expect(lateText(2 * 86400 + 3600)).toBe("2d");
    expect(lateText(13 * 3600 + 120)).toBe("13h");
    expect(lateText(25 * 60)).toBe("25m");
    expect(lateText(5)).toBe("1m");
    // Same day: a clock time; this week: a weekday; later: a date.
    expect(dueText("2026-02-10T15:30:00.000Z", EVALUATED_AT)).toMatch(/\d{2}:\d{2}/);
    expect(dueText("2026-02-12T12:00:00.000Z", EVALUATED_AT)).toBe(
      new Intl.DateTimeFormat(undefined, { weekday: "short" }).format(new Date("2026-02-12T12:00:00.000Z")),
    );
  });

  it("marks a stalled step late by its ETA or a passed deadline, else due by its deadline", () => {
    const board = nowBoard(
      bare({
        eta: step("eta", { progress: "doing", liveness: "quiet", eta: { ...ETA, late_by_s: 7200 } }),
        passed: step("passed", { progress: "doing", liveness: "quiet", deadline: "2026-02-09T15:00:00.000Z" }),
        due: step("due", { progress: "doing", liveness: "quiet", deadline: "2026-02-12T15:00:00.000Z" }),
      }),
    );
    const steps = board.stalled[0]!.steps;
    // Most overdue first.
    expect(steps.map((item) => [item.feature.node, item.lateS, item.due])).toEqual([
      ["passed", 86400, undefined],
      ["eta", 7200, undefined],
      ["due", 0, "2026-02-12T15:00:00.000Z"],
    ]);
  });

  it("opens a stalled step's rescue need instead of the project", () => {
    const board = nowBoard(makeSnapshot());
    const gamma = board.stalled.find((group) => group.project.slug === "gamma");
    expect(gamma?.steps[0]?.needId).toBe("rescue-one");
  });

  it("groups stalled steps by project in sidebar order and keeps archived projects out", () => {
    const snapshot = makeSnapshot({ withRescue: false });
    for (const slug of ["alpha", "beta"] as const) {
      snapshot.state.projects[slug]!.features["core/one"]!.liveness = "quiet";
    }
    snapshot.state.projects.beta!.summary.archived = true;
    const board = nowBoard(snapshot);
    expect(board.stalled.map((group) => group.project.slug)).toEqual(["gamma", "alpha"]);
  });

  it("shows budget pressure as project chips", () => {
    const project = makeProject("alpha");
    project.summary.budget = {
      speculative_features: 9,
      pending_slices: 2,
      demo_gap_s: 172800,
      demo_gap_node: "core/one",
      blocked: true,
      reasons: [],
    };
    expect(riskChips(project)).toEqual([
      {
        level: "red",
        text: "no demo for 2d",
        why: "No demo for 2d (limit 4h): CLI ask is started, not delivered. Clears when the agent runs agendar deliver.",
      },
      { level: "amber", text: "9/10 on unconfirmed choices" },
      { level: "red", text: "2 slices await review" },
    ]);
    expect(riskChips(makeProject("beta"))).toEqual([]);
  });

  it("takes the latest pulse note of the current attempt only", () => {
    const snapshot = bare({ a: step("a", { progress: "doing", liveness: "live", eta: ETA }) });
    snapshot.state.projects.alpha!.activity = [
      pulse("a", "2026-02-10T14:50:00.000Z", "Writing the red tests"),
      pulse("a", "2026-02-10T14:20:00.000Z", "Older note"),
    ];
    expect(nowBoard(snapshot).moving[0]?.note).toBe("Writing the red tests");
    snapshot.state.projects.alpha!.activity = [pulse("a", "2026-02-10T13:00:00.000Z", "Last attempt")];
    expect(nowBoard(snapshot).moving[0]?.note).toBeUndefined();
  });

  it("lists done since the watermark: accepted deliveries and finished steps, once each", () => {
    const snapshot = bare({});
    const accepted: DeliveryState = makeDelivery({
      id: ulidAt("2026-02-10T14:30:00.000Z"),
      node: "core/one",
      name: "search-v2",
      review: "accepted",
      review_event: ulidAt("2026-02-10T14:40:00.000Z"),
    });
    const project = snapshot.state.projects.alpha!;
    project.deliveries = { [accepted.id]: accepted };
    project.features = {
      "core/two": step("core/two", { title: "Core two", progress: "done" }),
    };
    project.activity = [
      finished("core/one", "2026-02-10T14:41:00.000Z"),
      finished("core/two", "2026-02-10T13:00:00.000Z"),
      finished("core/old", "2026-02-10T11:00:00.000Z"),
    ];
    snapshot.brief.delivered = [
      {
        id: "delivered:one",
        text: "search-v2 accepted",
        target: { kind: "project", id: "alpha" },
        source_ids: [accepted.id, accepted.review_event!],
      },
    ];
    const done = nowBoard(snapshot).done;
    expect(done.map((item) => [item.kind, item.title, item.ts])).toEqual([
      ["accepted", "search-v2", "2026-02-10T14:40:00.000Z"],
      ["done", "Core two", "2026-02-10T13:00:00.000Z"],
    ]);
  });

  it("never claims a time for a delivery id that does not encode one in range", () => {
    expect(ulidTime(ulidAt("2026-02-10T14:40:00.000Z"))).toBe(Date.parse("2026-02-10T14:40:00.000Z"));
    const snapshot = bare({});
    const delivery = makeDelivery({ review: "accepted", review_event: ulid(700) });
    snapshot.state.projects.alpha!.deliveries = { [delivery.id]: delivery };
    snapshot.brief.delivered = [
      { id: "d", text: "x", target: { kind: "project", id: "alpha" }, source_ids: [delivery.id] },
    ];
    expect(nowBoard(snapshot).done[0]?.ts).toBeUndefined();
  });

  it("picks each project's first runnable planned step and counts the rest", () => {
    const board = nowBoard(
      bare({
        base: step("base", { progress: "verified" }),
        held: step("held", { progress: "planned", gate: "awaiting_decision" }),
        blocked: step("blocked", { progress: "planned", depends_on: ["held"] }),
        first: step("first", { progress: "planned", depends_on: ["base"] }),
        second: step("second", { progress: "planned" }),
        paused: step("paused", { progress: "planned", paused: true }),
      }),
    );
    expect(board.next).toHaveLength(1);
    expect(board.next[0]!.feature.node).toBe("second");
    expect(board.next[0]!.more).toBe(1);
  });

  it("is empty when nothing moves, stalls, waits, finished or is next", () => {
    const board = nowBoard(bare({ v: step("v", { progress: "verified" }) }));
    expect(boardEmpty(board)).toBe(true);
  });

  it("folds what a need holds up and its default into the need row", () => {
    const board = nowBoard(makeSnapshot());
    const row = board.needs.find((item) => item.need.id === "need-one");
    expect(row?.defaultAt).toBe("2026-02-10T18:22:00.000Z");
    expect(row?.defaultLabel).toBeTruthy();
    // core/two is not a step of alpha in this fixture: nothing is named.
    expect(row?.holds).toEqual([]);
  });
});

import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ClientContext } from "../api/store";
import type { ClaimState, Correction, FeatureState, ProjectState, Snapshot } from "../api/types";
import { Corrections } from "../components/Corrections";
import { squareState } from "../components/PlanMap";
import { workSplit } from "../components/ProjectOverview";
import { nodeDisplay } from "../graph/display";
import { statusWeight } from "../state/claim";
import { nowBoard } from "../state/now";
import { Brief } from "../views/Brief";
import { EVALUATED_AT, FakeClient, makeDelivery, makeNeed, makeProject, makeSnapshot, ulid } from "./fixtures";

const SESSION = ulid(500);
const ASK = ulid(700);

function claim(state: ClaimState["state"], overrides: Partial<ClaimState> = {}): ClaimState {
  return { state, since: "2026-02-10T12:00:00.000Z", last_word: "2026-02-10T12:00:00.000Z", ...overrides };
}

function feature(node: string, overrides: Partial<FeatureState> = {}): FeatureState {
  const base = makeProject("alpha").features["core/one"]!;
  return {
    ...base,
    node,
    title: node,
    depends_on: [],
    unblocks: [],
    gate: "open",
    progress: "doing",
    session: SESSION,
    ...overrides,
  };
}

function project(features: FeatureState[]): ProjectState {
  const value = makeProject("alpha", { features: Object.fromEntries(features.map((item) => [item.node, item])) });
  value.sessions[SESSION] = {
    id: SESSION,
    host: "h-other",
    harness: "claude_code",
    last_event: ulid(501),
    last_event_ts: EVALUATED_AT,
    liveness: "quiet",
    stalled_on: [],
  };
  value.summary.host_as_of = { "h-fixture": EVALUATED_AT, "h-other": "2026-02-08T15:00:00.000Z" };
  return value;
}

function snapshotOf(value: ProjectState): Snapshot {
  const snapshot = makeSnapshot({ withRescue: false });
  snapshot.state.projects = { alpha: value };
  snapshot.needs = [];
  snapshot.brief.delivered = [];
  return snapshot;
}

const SILENT: Correction = { rule: "silent", since: "2026-02-09T15:00:00.000Z", evidence: [ulid(1)] };

/** One step in every place of the home-page mapping. */
function mapped(): ProjectState {
  return project([
    feature("live", { liveness: "live", claim: claim("held", { last_word: "2026-02-10T14:58:00.000Z" }) }),
    feature("quiet", { liveness: "quiet", claim: claim("held", { last_word: "2026-02-10T14:20:00.000Z" }) }),
    feature("asking", { liveness: "quiet", claim: claim("held", { waiting_on: ASK }) }),
    feature("review", { liveness: "quiet", phase: "in_review" }),
    feature("stalled", { liveness: "quiet", claim: claim("stalled") }),
    feature("free", { liveness: "live", claim: claim("free", { since: "2026-02-09T15:00:00.000Z" }), correction: SILENT }),
    feature("back", {
      liveness: "quiet",
      phase: "sent_back",
      claim: claim("stalled"),
      correction: { rule: "sent_back", since: "2026-02-10T13:00:00.000Z", evidence: [ulid(2), ulid(3)] },
    }),
  ]);
}

describe("home page mapping from the claim", () => {
  it("puts each claim in its section", () => {
    const snapshot = snapshotOf(mapped());
    snapshot.needs = [makeNeed({ id: "decide-other", source: ASK, node: "elsewhere", unblocks: [] })];
    const board = nowBoard(snapshot);
    expect(board.moving.map((item) => item.feature.node).sort()).toEqual(["live", "quiet"]);
    const quiet = board.moving.find((item) => item.feature.node === "quiet")!;
    expect(quiet.lastWord).toBe("2026-02-10T14:20:00.000Z");
    expect(board.moving.find((item) => item.feature.node === "live")!.lastWord).toBeUndefined();
    const stalled = Object.fromEntries(board.stalled[0]!.steps.map((item) => [item.feature.node, item.agent]));
    expect(stalled).toEqual({ stalled: "silent", free: "abandoned", back: "sent_back" });
    // Held because its agent waits on its own question: under that question.
    expect(board.needs[0]!.waiting).toEqual(["asking"]);
  });

  it("says why, tersely, and marks a correction", () => {
    const snapshot = snapshotOf(mapped());
    snapshot.needs = [makeNeed({ id: "decide-other", source: ASK, node: "elsewhere", unblocks: [] })];
    render(
      <ClientContext.Provider value={new FakeClient(snapshot)}>
        <Brief snapshot={snapshot} onSelect={vi.fn()} />
      </ClientContext.Provider>,
    );
    const row = (node: string) => document.querySelector<HTMLElement>(`[data-feat="alpha/${node}"]`)!;
    expect(within(row("quiet")).getByText(/last word 40m ago/)).toBeInTheDocument();
    expect(within(row("quiet")).getByText(/host last heard 2d ago/)).toBeInTheDocument();
    expect(within(row("stalled")).getByText("no word for 3h")).toBeInTheDocument();
    expect(within(row("free")).getByText("abandoned 1d ago · open to any agent")).toBeInTheDocument();
    expect(row("free").classList.contains("abandoned")).toBe(true);
    expect(within(row("free")).getByRole("img", { name: "freed: no word for 24h" })).toBeInTheDocument();
    expect(within(row("back")).getByText("sent back, nobody on it")).toBeInTheDocument();
    expect(screen.getByText("waiting for your answer: asking")).toBeInTheDocument();
    expect(row("review")).toBeNull();
  });
});

describe("the same claim on the plan and the graph", () => {
  it("shows a freed step as stopped and a quiet step inside its ETA as at work", () => {
    const free = feature("free", { liveness: "live", claim: claim("free"), correction: SILENT });
    expect(squareState(free)).toBe("stopped");
    const display = nodeDisplay(
      { id: "feature:free", kind: "feature", label: "free", feature: "free" } as never,
      free,
    );
    expect(display.dot).toBe("stopped");
    expect(display.corrected).toBe("freed: no word for 24h");
    const quiet = feature("quiet", { liveness: "quiet", claim: claim("held") });
    expect(nodeDisplay({ id: "feature:quiet", kind: "feature", label: "quiet", feature: "quiet" } as never, quiet).dot).toBe(
      "doing",
    );
    expect(statusWeight(quiet)).toBe("muted");
    expect(statusWeight(free)).toBe("marked");
    expect(statusWeight(feature("live", { liveness: "live", claim: claim("held") }))).toBe("solid");
  });

  it("splits the work left by who stands behind it", () => {
    expect(
      workSplit({
        open_steps: 29,
        in_progress: 2,
        abandoned: 1,
        not_started: 26,
        agents: 1,
        work_ms: [1, 2],
        step_ms: [1, 2],
        basis_steps: 3,
      }),
    ).toBe(" · 2 in progress · 1 abandoned · 26 not started");
    expect(workSplit({ open_steps: 1, not_started: 1, agents: 0, work_ms: [1, 2], step_ms: [1, 2], basis_steps: 3 })).toBe(
      " · 1 not started",
    );
  });
});

describe("N corrected", () => {
  it("lists each correction with its reason and the owner's actions", async () => {
    const user = userEvent.setup();
    const accepted = feature("closed", {
      progress: "done",
      attempt: ulid(10),
      correction: { rule: "accepted", since: "2026-02-10T14:00:00.000Z", evidence: [ulid(12), ulid(11)] },
    });
    const free = feature("free", { attempt: ulid(20), claim: claim("free"), correction: SILENT });
    const value = project([accepted, free, feature("plain", { claim: claim("held") })]);
    value.deliveries[ulid(11)] = makeDelivery({ id: ulid(11), node: "closed", attempt: ulid(10), review: "accepted" });
    value.needs_you = [makeNeed({ id: "rescue:silent:x", kind: "rescue", node: "free", source: ulid(20) })];
    const onOpenNeed = vi.fn();
    render(
      <ClientContext.Provider value={new FakeClient(snapshotOf(value))}>
        <Corrections project={value} anchor={EVALUATED_AT} canMutate onOpenNeed={onOpenNeed} />
      </ClientContext.Provider>,
    );
    const list = screen.getByTestId("corrections");
    expect(within(list).getByText("2 corrected")).toBeInTheDocument();
    await user.click(within(list).getByText("2 corrected"));
    const closed = list.querySelector<HTMLElement>('[data-corrected="closed"]')!;
    expect(within(closed).getByText(/closed: you accepted/)).toBeInTheDocument();
    expect(within(closed).getByRole("button", { name: "Send back" })).toBeInTheDocument();
    expect(within(closed).queryByRole("button", { name: "Details" })).toBeNull();
    const freed = list.querySelector<HTMLElement>('[data-corrected="free"]')!;
    expect(within(freed).getByText(/freed: no word for 24h/)).toBeInTheDocument();
    await user.click(within(freed).getByRole("button", { name: "Details" }));
    expect(onOpenNeed).toHaveBeenCalledWith("rescue:silent:x");
    expect(list.querySelector('[data-corrected="plain"]')).toBeNull();
  });

  it("is not shown when nothing was corrected", () => {
    const value = project([feature("plain", { claim: claim("held") })]);
    render(<Corrections project={value} anchor={EVALUATED_AT} canMutate={false} />);
    expect(screen.queryByTestId("corrections")).toBeNull();
  });
});

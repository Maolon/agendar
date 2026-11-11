import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ClientContext } from "../api/store";
import type { FeatureState, NeedsYouItem, ProjectState, Snapshot } from "../api/types";
import { isInfoNeed } from "../state/claim";
import { nowBoard } from "../state/now";
import { activeNeeds, SnoozeContext, type SnoozeApi } from "../state/snooze";
import { Rescue } from "../views/Rescue";
import { EVALUATED_AT, FakeClient, makeNeed, makeProject, makeSnapshot, ulid } from "./fixtures";

const SESSION = ulid(500);

function freeStep(node: string): FeatureState {
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
    liveness: "quiet",
    claim: { state: "free", since: "2026-02-09T15:00:00.000Z", last_word: "2026-02-08T15:00:00.000Z" },
    correction: { rule: "silent", since: "2026-02-09T15:00:00.000Z", evidence: [ulid(1)] },
  };
}

function snapshotWith(project: ProjectState, needs: Snapshot["needs"]): Snapshot {
  const snapshot = makeSnapshot({ withRescue: false });
  snapshot.state.projects = { alpha: project };
  snapshot.needs = needs;
  snapshot.brief.delivered = [];
  return snapshot;
}

describe("a free step is open to any agent", () => {
  it("keeps its row out of Needs you but openable from the stalled step", () => {
    const project = makeProject("alpha", { features: { free: freeStep("free") } });
    const silent = makeNeed({ id: `rescue:silent:${ulid(1)}`, kind: "rescue", node: "free", unblocks: [] });
    const lost = makeNeed({ id: `rescue:lost:${ulid(2)}`, kind: "rescue", node: "other", unblocks: [] });
    const lostStep = makeNeed({ id: `rescue:lost-step:${ulid(2)}:x`, kind: "rescue", node: "x", unblocks: [] });
    const conflict = makeNeed({ id: `rescue:start-conflict:${ulid(3)}`, kind: "rescue", node: "y", unblocks: [] });
    const snapshot = snapshotWith(project, [silent, lost, lostStep, conflict]);
    expect([silent, lost, lostStep].every(isInfoNeed)).toBe(true);
    expect(isInfoNeed(conflict)).toBe(false);
    expect(activeNeeds(snapshot).map((need) => need.id)).toEqual([conflict.id]);
    const board = nowBoard(snapshot);
    expect(board.needs.map((row) => row.need.id)).toEqual([conflict.id]);
    const step = board.stalled[0]!.steps.find((item) => item.feature.node === "free")!;
    expect(step.agent).toBe("abandoned");
    expect(step.needId).toBe(silent.id);
  });
});

const SNOOZE: SnoozeApi = {
  until: () => null,
  offers: () => [],
  open: null,
  setOpen: () => undefined,
  snooze: async () => undefined,
  restore: async () => undefined,
  dismissed: () => false,
  dismiss: async () => undefined,
};

function renderRescue(project: ProjectState, need: NeedsYouItem) {
  return render(
    <ClientContext.Provider value={new FakeClient(snapshotWith(project, [need]))}>
      <SnoozeContext.Provider value={SNOOZE}>
        <Rescue
          project={project}
          need={need}
          anchor={EVALUATED_AT}
          canMutate
          onResolved={() => undefined}
          onRefresh={() => undefined}
        />
      </SnoozeContext.Provider>
    </ClientContext.Provider>,
  );
}

describe("takeover is pulled by the agent, never pushed by the owner", () => {
  it("asks nothing on a free step's page: no hand-off, no Dismiss, no Not now; Pause stays", () => {
    const project = makeProject("alpha", { features: { free: freeStep("free") } });
    for (const id of [`rescue:silent:${ulid(1)}`, `rescue:lost:${ulid(2)}`, `rescue:lost-step:${ulid(2)}:free`]) {
      const view = renderRescue(project, makeNeed({ id, kind: "rescue", node: "free", unblocks: [] }));
      expect(screen.getByText(/Nothing is asked of you\. Its own conversation takes this step back by itself/)).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Pause this task" })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /Hand to next session|Let another agent continue/ })).toBeNull();
      expect(screen.queryByRole("button", { name: "Dismiss" })).toBeNull();
      expect(screen.queryByRole("button", { name: "Not now" })).toBeNull();
      view.unmount();
    }
  });

  it("keeps the owner's choice only where the board asks who continues", () => {
    const project = makeProject("alpha", { features: { free: freeStep("free") } });
    const conflict = makeNeed({
      id: `rescue:start-conflict:${ulid(3)}`,
      kind: "rescue",
      node: "free",
      question: "Who should continue free?",
      unblocks: [],
    });
    renderRescue(project, conflict);
    expect(screen.getByRole("heading", { name: "Who should continue free?" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Let another agent continue" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Dismiss" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Hand to next session" })).toBeNull();
    expect(screen.queryByText(/Nothing is asked of you/)).toBeNull();
  });

  it("offers no takeover on a plan-repair row", () => {
    const project = makeProject("alpha", { features: { free: freeStep("free") } });
    const cycle = makeNeed({ id: `rescue:cycle:${ulid(4)}:a:free`, kind: "rescue", node: "free", unblocks: [] });
    renderRescue(project, cycle);
    expect(screen.queryByRole("button", { name: "Let another agent continue" })).toBeNull();
    expect(screen.getByRole("button", { name: "Pause this task" })).toBeInTheDocument();
  });
});

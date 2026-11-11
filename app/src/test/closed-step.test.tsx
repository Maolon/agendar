import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ClientContext } from "../api/store";
import type { Snapshot } from "../api/types";
import { isActiveNeed, isFoldedNeed } from "../state/claim";
import { nowBoard } from "../state/now";
import { activeNeeds, closedStepNeeds } from "../state/snooze";
import { Brief } from "../views/Brief";
import { FakeClient, makeNeed, makeProject, makeSnapshot, ulid } from "./fixtures";

function snapshotWith(needs: Snapshot["needs"]): Snapshot {
  const snapshot = makeSnapshot({ withRescue: false });
  snapshot.state.projects = { alpha: makeProject("alpha") };
  snapshot.needs = needs;
  snapshot.brief.delivered = [];
  return snapshot;
}

describe("questions on closed steps", () => {
  const active = makeNeed({ id: `decide:${ulid(1)}`, source: ulid(1), question: "Still needed?", unblocks: [] });
  // Inside the window: labelled, still active.
  const closing = makeNeed({
    id: `decide:${ulid(2)}`,
    source: ulid(2),
    question: "Closing soon?",
    unblocks: [],
    closed_step: { since: "2026-02-10T10:00:00.000Z" },
  });
  // A day after the step closed: folded out of Needs you.
  const folded = makeNeed({
    id: `decide:${ulid(3)}`,
    source: ulid(3),
    question: "Which cache key?",
    unblocks: [],
    closed_step: { since: "2026-02-08T10:00:00.000Z", folded: true },
  });

  it("fold out of Needs you a day after the step closed", () => {
    const snapshot = snapshotWith([active, closing, folded]);
    expect([active, closing].every(isActiveNeed)).toBe(true);
    expect(isFoldedNeed(folded)).toBe(true);
    expect(activeNeeds(snapshot).map((need) => need.id)).toEqual([active.id, closing.id]);
    expect(closedStepNeeds(snapshot).map((need) => need.id)).toEqual([folded.id]);
    const board = nowBoard(snapshot);
    expect(board.needs.map((row) => row.need.id)).toEqual([active.id, closing.id]);
    expect(board.closed.map((row) => row.need.id)).toEqual([folded.id]);
  });

  it("show as one folded group on the home page that opens to answer them", async () => {
    const user = userEvent.setup();
    const snapshot = snapshotWith([active, folded]);
    const onSelect = vi.fn();
    render(
      <ClientContext.Provider value={new FakeClient(snapshot)}>
        <Brief snapshot={snapshot} onSelect={onSelect} />
      </ClientContext.Provider>,
    );
    expect(screen.queryByText("Which cache key?")).toBeNull();
    await user.click(screen.getByRole("button", { name: "On closed steps (1)" }));
    await user.click(screen.getByText("Which cache key?"));
    expect(onSelect).toHaveBeenCalledWith({ kind: "need", id: folded.id });
    expect(screen.getByRole("button", { name: "Hide closed steps" })).toBeInTheDocument();
  });
});

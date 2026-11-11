import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ClientContext } from "../api/store";
import type { Snapshot } from "../api/types";
import { Brief } from "../views/Brief";
import { FakeClient, makeProject, makeSnapshot, ulid } from "./fixtures";

function renderBrief(snapshot: Snapshot, onSelect = vi.fn()) {
  const client = new FakeClient(snapshot);
  render(
    <ClientContext.Provider value={client}>
      <Brief snapshot={snapshot} onSelect={onSelect} />
    </ClientContext.Provider>,
  );
  return onSelect;
}

function section(id: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(`[data-dash="${id}"]`);
}

/** alpha moving with an ETA and a session, gamma stalled with its rescue. */
function boardSnapshot(): Snapshot {
  const snapshot = makeSnapshot();
  const alpha = snapshot.state.projects.alpha!;
  alpha.summary.title = "Alpha";
  alpha.features["core/one"] = {
    ...alpha.features["core/one"]!,
    title: "Any process reads lock-free when the index already matches the Vault",
    session: ulid(500),
    eta: {
      agent_range: { min_s: 3600, max_s: 7200 },
      anchor: "2026-02-10T14:00:00.000Z",
      paused_s: 0,
      agent_finish: { earliest: "2026-02-10T15:00:00.000Z", latest: "2026-02-10T16:00:00.000Z" },
      late_by_s: 0,
      paused: false,
    },
  };
  alpha.sessions[ulid(500)] = {
    id: ulid(500),
    host: "h",
    harness: "claude_code",
    last_event: ulid(501),
    last_event_ts: "2026-02-10T14:59:00.000Z",
    liveness: "live",
    stalled_on: [],
  };
  const beta = snapshot.state.projects.beta!;
  beta.features["core/one"] = {
    ...beta.features["core/one"]!,
    title: "Restore panes",
    eta: {
      agent_range: { min_s: 3600, max_s: 7200 },
      anchor: "2026-02-10T12:00:00.000Z",
      paused_s: 0,
      agent_finish: { earliest: "2026-02-10T13:00:00.000Z", latest: "2026-02-10T14:00:00.000Z" },
      late_by_s: 3600,
      paused: false,
    },
  };
  const gamma = snapshot.state.projects.gamma!;
  gamma.features["core/one"] = { ...gamma.features["core/one"]!, title: "Rule fixtures" };
  gamma.summary.budget = { ...gamma.summary.budget, demo_gap_s: 172800, blocked: true };
  gamma.features["core/late"] = {
    ...gamma.features["core/one"]!,
    node: "core/late",
    title: "Wire the PDF layout provider",
    progress: "doing",
    liveness: "quiet",
    eta: {
      agent_range: { min_s: 3600, max_s: 7200 },
      anchor: "2026-02-08T12:00:00.000Z",
      paused_s: 0,
      agent_finish: { earliest: "2026-02-08T13:00:00.000Z", latest: "2026-02-08T14:00:00.000Z" },
      late_by_s: 2 * 86400 + 3600,
      paused: false,
    },
  };
  return snapshot;
}

describe("home by state", () => {
  it("shows a moving card with title, agent, start, ETA bar and window", () => {
    renderBrief(boardSnapshot());
    const moving = section("moving")!;
    expect(within(moving).getByRole("heading", { name: /Moving now/ })).toBeInTheDocument();
    const card = moving.querySelector<HTMLElement>('[data-feat="alpha/core/one"]')!;
    expect(
      within(card).getByText("Any process reads lock-free when the index already matches the Vault"),
    ).toBeInTheDocument();
    expect(within(card).getByText("Claude Code")).toBeInTheDocument();
    expect(within(card).getByText("Alpha · started 1h ago")).toBeInTheDocument();
    expect(within(card).getByRole("img", { name: "50% of the expected time used" })).toBeInTheDocument();
    expect(within(card).getByText(/^done \d{2}:\d{2}–\d{2}:\d{2}$/)).toBeInTheDocument();
    // A step past its window is red with how late it is.
    const late = moving.querySelector<HTMLElement>('[data-feat="beta/core/one"]')!;
    expect(within(late).getByText("1h late")).toBeInTheDocument();
    expect(late.querySelector(".dash-bar.late")).not.toBeNull();
  });

  it("groups stalled steps by project with risk and lateness chips", () => {
    renderBrief(boardSnapshot());
    const stalled = section("stalled")!;
    const group = stalled.querySelector<HTMLElement>('[data-project="gamma"]')!;
    expect(within(group).getByText("gamma")).toBeInTheDocument();
    expect(within(group).getByText("no demo for 2d")).toBeInTheDocument();
    const late = group.querySelector<HTMLElement>('[data-feat="gamma/core/late"]')!;
    expect(within(late).getByText("2d late")).toBeInTheDocument();
    expect(within(late).getByText("no agent")).toBeInTheDocument();
    expect(within(late).getByTitle("Wire the PDF layout provider")).toBeInTheDocument();
    const lost = group.querySelector<HTMLElement>('[data-feat="gamma/core/one"]')!;
    expect(within(lost).getByText("agent lost")).toBeInTheDocument();
    // Most overdue first.
    expect([...group.querySelectorAll("[data-feat]")].map((row) => row.getAttribute("data-feat"))).toEqual([
      "gamma/core/late",
      "gamma/core/one",
    ]);
  });

  it("navigates: a moving card opens its project, a stalled step its rescue, a need its card", async () => {
    const user = userEvent.setup();
    const onSelect = renderBrief(boardSnapshot());
    await user.click(document.querySelector<HTMLElement>('[data-dash="moving"] [data-feat="alpha/core/one"]')!);
    expect(onSelect).toHaveBeenLastCalledWith({ kind: "project", id: "alpha" });
    await user.click(document.querySelector<HTMLElement>('[data-feat="gamma/core/one"]')!);
    expect(onSelect).toHaveBeenLastCalledWith({ kind: "need", id: "rescue-one" });
    await user.click(document.querySelector<HTMLElement>('[data-feat="gamma/core/late"]')!);
    expect(onSelect).toHaveBeenLastCalledWith({ kind: "project", id: "gamma" });
    await user.click(document.querySelector<HTMLElement>('[data-need="need-two"]')!);
    expect(onSelect).toHaveBeenLastCalledWith({ kind: "need", id: "need-two" });
  });

  it("folds the default into the need row and summarizes the board in one line", () => {
    renderBrief(boardSnapshot());
    const need = document.querySelector<HTMLElement>('[data-need="need-one"]')!;
    expect(within(need).getByText(/^goes with .+ at \d{2}:\d{2}$/)).toBeInTheDocument();
    expect(screen.getByTestId("dash-summary").textContent).toBe(
      "2 moving · 2 stalled (no agent on them) · 3 need you",
    );
    expect(screen.getByRole("button", { name: /Start with the first thing/ })).toBeInTheDocument();
  });

  it("shows only sections with content", () => {
    const snapshot = boardSnapshot();
    renderBrief(snapshot);
    expect(section("needs")).not.toBeNull();
    expect(section("moving")).not.toBeNull();
    expect(section("stalled")).not.toBeNull();
    expect(section("done")).toBeNull();
    expect(section("next")).toBeNull();
    expect(section("presence")).toBeNull();
    // The old fixed sections and the budget meter are gone.
    expect(screen.queryByText("COMING UP")).toBeNull();
    expect(screen.queryByText("No delivery yet")).toBeNull();
    expect(document.querySelector(".dots")).toBeNull();
  });

  it("lists up next per project and done since the last look", () => {
    const snapshot = makeSnapshot({ withRescue: false });
    snapshot.needs = [];
    const alpha = makeProject("alpha", {
      features: {
        "core/a": { ...makeProject("x").features["core/one"]!, node: "core/a", title: "First next", progress: "planned", gate: "open" },
        "core/b": { ...makeProject("x").features["core/one"]!, node: "core/b", title: "Second next", progress: "planned", gate: "open" },
        "core/c": { ...makeProject("x").features["core/one"]!, node: "core/c", title: "Shipped step", progress: "done" },
      },
      activity: [
        {
          ts: "2026-02-10T14:00:00.000Z",
          actor: "Claude Code",
          actor_kind: "agent",
          icon: "check",
          text: "Claude Code finished Shipped step",
          msg: { key: "act.finished", args: { actor: { t: "text", v: "Claude Code" }, step: { t: "text", v: "Shipped step" } } },
          node: "core/c",
          count: 1,
        },
      ],
    });
    snapshot.state.projects = { alpha };
    renderBrief(snapshot);
    const next = section("next")!;
    expect(within(next).getByText("First next")).toBeInTheDocument();
    expect(within(next).getByText("alpha · +1")).toBeInTheDocument();
    const done = section("done")!;
    expect(within(done).getByText("Shipped step")).toBeInTheDocument();
    expect(within(done).getByText("alpha · done 1h ago")).toBeInTheDocument();
    expect(section("moving")).toBeNull();
    expect(screen.getByTestId("dash-summary").textContent).toBe("nothing needs you · 1 done");
  });

  it("says All quiet once instead of empty sections", () => {
    const snapshot = makeSnapshot({ withRescue: false });
    snapshot.needs = [];
    snapshot.state.projects = { alpha: makeProject("alpha", { features: {} }) };
    renderBrief(snapshot);
    expect(screen.getByText("All quiet")).toBeInTheDocument();
    expect(document.querySelector("[data-dash]")).toBeNull();
  });
});

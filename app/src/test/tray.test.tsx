import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../api/client";
import { ClientContext } from "../api/store";
import { TrayPopover } from "../components/TrayPopover";
import type { AnswerDecisionData, NeedsYouItem } from "../api/types";
import { costlyCard, easyCard, FakeClient, makeSnapshot } from "./fixtures";

function Harness({
  client,
  snapshot,
  canMutate = true,
  onAnswered = vi.fn(),
  onOpen = vi.fn(),
  onRefresh = vi.fn(),
}: {
  client: FakeClient;
  snapshot: ReturnType<typeof makeSnapshot>;
  canMutate?: boolean;
  onAnswered?: (data: AnswerDecisionData) => void;
  onOpen?: (need: NeedsYouItem | null) => void;
  onRefresh?: () => void;
}) {
  return (
    <ClientContext.Provider value={client}>
      <TrayPopover
        snapshot={snapshot}
        canMutate={canMutate}
        onOpen={onOpen}
        onAnswered={onAnswered}
        onRefresh={onRefresh}
      />
    </ClientContext.Provider>
  );
}

describe("tray popover", () => {
  it("expands only the highest-ranked easy recommendation and lists the rest compactly", () => {
    const client = new FakeClient();
    const snapshot = makeSnapshot();
    const { container } = render(<Harness client={client} snapshot={snapshot} />);
    expect(container.querySelector(".pop-badge")?.textContent).toBe("3");
    expect(screen.getByText("How should answers reach idle agents?")).toBeInTheDocument();
    expect(screen.getByText("Approve recommended")).toBeInTheDocument();
    expect(screen.getByText("How long should pane history be kept?")).toBeInTheDocument();
    expect(screen.getByText("Agent stopped mid-task")).toBeInTheDocument();
    expect(screen.getAllByText("Approve recommended")).toHaveLength(1);
  });

  it("shows project progress above the questions, in local time", () => {
    const client = new FakeClient();
    const snapshot = makeSnapshot();
    const { container } = render(<Harness client={client} snapshot={snapshot} />);
    const rows = container.querySelectorAll(".tray-proj");
    expect(rows.length).toBeGreaterThan(0);
    // Progress comes before the Needs you list.
    const progress = container.querySelector(".tray-progress")!;
    const needs = container.querySelector('[aria-label="Needs you"]')!;
    expect(progress.compareDocumentPosition(needs) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // A one-line status in the header, local time in the footer, never raw UTC.
    expect(container.querySelector(".pop-status")?.textContent).toMatch(/working|idle|No agent connected/);
    expect(container.querySelector(".pop-foot")?.textContent).toMatch(/Updated \d\d:\d\d/);
    expect(container.textContent).not.toMatch(/\d\dZ\b/);
  });

  it("lists a project as in progress only while an agent is on a step", () => {
    const client = new FakeClient();
    const snapshot = makeSnapshot();
    // No questions: only work decides what is in progress.
    snapshot.needs = [];
    for (const project of Object.values(snapshot.state.projects)) {
      project.summary.needs_count = 0;
      project.summary.active_agents = 0;
      for (const feature of Object.values(project.features)) {
        feature.progress = feature.progress === "doing" ? "planned" : feature.progress;
      }
    }
    const alpha = snapshot.state.projects.alpha!;
    alpha.summary.active_agents = 1;
    const { container, rerender } = render(<Harness client={client} snapshot={snapshot} />);
    // Connected but nothing in progress: idle, not working; the row says the
    // open steps have nobody on them.
    expect(container.querySelector(".pop-status")?.textContent).toMatch(/^1 agent idle/);
    expect(container.querySelectorAll(".tray-proj")).toHaveLength(1);
    expect(container.querySelector(".tray-proj-line")?.textContent).toMatch(/^Idle · \d+ steps? open$/);

    const step = Object.values(alpha.features)[0]!;
    step.progress = "doing";
    step.liveness = "live";
    step.session = "01J00000000000000000000101";
    const next = structuredClone(snapshot);
    rerender(<Harness client={client} snapshot={next} />);
    expect(container.querySelector(".pop-status")?.textContent).toMatch(/^1 agent working/);
    expect(container.querySelectorAll(".tray-proj")).toHaveLength(1);
    expect(container.querySelector(".tray-proj-line")?.textContent).toContain(step.title);
  });

  it("never promotes a costly decision into one-key approve", () => {
    const client = new FakeClient();
    const snapshot = makeSnapshot({ firstCard: costlyCard() });
    render(<Harness client={client} snapshot={snapshot} />);
    expect(screen.queryByText("Approve recommended")).toBeNull();
    expect(screen.getByText(/No one-key approval here/)).toBeInTheDocument();
    expect(screen.getByText("How should answers reach idle agents?")).toBeInTheDocument();
    expect(screen.getByText("How long should pane history be kept?")).toBeInTheDocument();
  });

  it("approves through the same prepare and answer guard and keeps the popover open", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const snapshot = makeSnapshot();
    const onAnswered = vi.fn();
    render(<Harness client={client} snapshot={snapshot} onAnswered={onAnswered} />);
    await user.click(screen.getByText("Approve recommended"));
    await waitFor(() => expect(onAnswered).toHaveBeenCalledTimes(1));
    expect(client.order).toEqual(["prepare_answer", "answer_decision"]);
    const answer = client.calls.find((call) => call.command === "answer_decision")?.request as {
      echo_label: string;
    };
    expect(answer.echo_label).toBe("Use harness adapters");
  });

  it("moves the next easy question up after an approve, before the snapshot refreshes", async () => {
    const user = userEvent.setup();
    const snapshot = makeSnapshot({ secondCard: easyCard({ question: "How long should pane history be kept?" }) });
    const client = new FakeClient(snapshot);
    const onAnswered = vi.fn();
    render(<Harness client={client} snapshot={snapshot} onAnswered={onAnswered} />);
    expect(screen.getByText("How should answers reach idle agents?")).toBeInTheDocument();
    await user.click(screen.getByText("Approve recommended"));
    await waitFor(() => expect(onAnswered).toHaveBeenCalledTimes(1));
    expect(screen.queryByText("How should answers reach idle agents?")).toBeNull();
    expect(screen.getByText("How long should pane history be kept?").closest(".pop-ask")).not.toBeNull();
    await user.click(screen.getByText("Approve recommended"));
    await waitFor(() => expect(onAnswered).toHaveBeenCalledTimes(2));
    expect(client.order).toEqual(["prepare_answer", "answer_decision", "prepare_answer", "answer_decision"]);
  });

  it("refuses a one-key approval when the backend escalates the guard", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.guardOverride = "choose";
    const onAnswered = vi.fn();
    render(<Harness client={client} snapshot={makeSnapshot()} onAnswered={onAnswered} />);
    await user.click(screen.getByText("Approve recommended"));
    expect(await screen.findByRole("alert")).toHaveTextContent("no longer a one-key approval");
    expect(onAnswered).not.toHaveBeenCalled();
    expect(client.order).not.toContain("answer_decision");
  });

  it("sends a costly approval to the main window with an amber warning, on that exact question", async () => {
    const { App } = await import("../App");
    const { SnapshotStore } = await import("../api/store");
    const user = userEvent.setup();
    const snapshot = makeSnapshot();
    const client = new FakeClient(snapshot);
    client.guardOverride = "confirm";
    render(<App client={client} store={new SnapshotStore(client)} surface="popover" />);
    await user.click(await screen.findByText("Approve recommended"));
    const toast = await waitFor(() => {
      const found = document.querySelector<HTMLElement>(".toast.warn");
      expect(found).not.toBeNull();
      return found!;
    });
    expect(toast.textContent).toContain("can't be undone without rework");
    expect(client.order).not.toContain("answer_decision");
    await user.click(screen.getByRole("button", { name: "Open to confirm" }));
    const top = snapshot.needs[0]!;
    expect(client.calls.find((call) => call.command === "show_main")?.request).toEqual({
      project: top.project,
      item_id: top.id,
    });
  });

  it("opens the main window on the row that was clicked", async () => {
    const { App } = await import("../App");
    const { SnapshotStore } = await import("../api/store");
    const user = userEvent.setup();
    const snapshot = makeSnapshot();
    const client = new FakeClient(snapshot);
    render(<App client={client} store={new SnapshotStore(client)} surface="popover" />);
    const row = await screen.findByRole("button", { name: /Agent stopped mid-task/ });
    await user.click(row);
    expect(client.calls.find((call) => call.command === "show_main")?.request).toMatchObject({
      item_id: "rescue-one",
    });
  });

  it("disables approval while the state is stale", () => {
    const client = new FakeClient();
    render(<Harness client={client} snapshot={makeSnapshot()} canMutate={false} />);
    expect(screen.getByText("Approve recommended").closest("button")?.disabled).toBe(true);
  });

  it("suppresses snoozed items and lets Escape hide the popover", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const snapshot = makeSnapshot();
    snapshot.needs.find((need) => need.id === "need-one")!.snoozed_until = "2026-02-10T16:00:00.000Z";
    render(<Harness client={client} snapshot={snapshot} />);
    expect(screen.queryByText("How should answers reach idle agents?")).toBeNull();
    await user.keyboard("{Escape}");
    expect(client.order).toContain("hide_popover");
  });

  it("uses plain Enter only for the eligible easy recommendation", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const snapshot = makeSnapshot();
    const onAnswered = vi.fn();
    render(<Harness client={client} snapshot={snapshot} onAnswered={onAnswered} />);
    await user.keyboard("{Enter}");
    await waitFor(() => expect(onAnswered).toHaveBeenCalledTimes(1));
    expect(client.order).toContain("answer_decision");
  });

  it("retries the identical request after an uncertain answer outcome", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.answerError = new ApiError({
      code: "E_IO",
      message: "The commit may not have been recorded.",
      hint: "Retry the same answer.",
      retryable: true,
    });
    const onAnswered = vi.fn();
    render(<Harness client={client} snapshot={makeSnapshot()} onAnswered={onAnswered} />);
    await user.click(screen.getByText("Approve recommended"));
    expect(await screen.findByRole("alert")).toHaveTextContent("may not have been recorded");
    const answers = () =>
      client.calls.filter((call) => call.command === "answer_decision").map((call) => call.request);
    expect(answers()).toHaveLength(1);
    const first = answers()[0];

    client.answerError = null;
    await user.click(screen.getByText("Approve recommended"));
    await waitFor(() => expect(onAnswered).toHaveBeenCalledTimes(1));
    expect(answers()).toHaveLength(2);
    expect(answers()[1]).toEqual(first);
    expect(client.calls.filter((call) => call.command === "prepare_answer")).toHaveLength(1);
  });

  it("opens the main window from the footer", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const onOpen = vi.fn();
    render(<Harness client={client} snapshot={makeSnapshot()} onOpen={onOpen} />);
    await user.click(screen.getByRole("button", { name: "Open Agendar" }));
    expect(onOpen).toHaveBeenCalledWith(null);
  });

  it("puts the Update pill in the footer's left slot and opens Settings › About", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const snapshot = makeSnapshot();
    snapshot.update = {
      current: "0.1.0",
      channel: "release",
      latest: "0.2.0",
      available: true,
      url: "https://github.com/Maolon/agendar/releases/tag/v0.2.0",
      published_at: "2026-11-01T10:00:00Z",
      checked_at: "2026-11-09T11:00:00.000Z",
      enabled: true,
    };
    const { container } = render(<Harness client={client} snapshot={snapshot} />);
    const foot = container.querySelector(".pop-foot")!;
    expect(foot.textContent).not.toMatch(/Updated \d\d:\d\d/);
    await user.click(screen.getByRole("button", { name: "Update 0.2.0" }));
    await waitFor(() =>
      expect(client.calls.map((call) => call.command)).toEqual(
        expect.arrayContaining(["show_main", "hide_popover"]),
      ),
    );
    expect(client.calls.find((call) => call.command === "show_main")?.request).toEqual({ view: "about" });
  });

  it("keeps the Updated time for a local build or when nothing is newer", () => {
    const client = new FakeClient();
    for (const patch of [{ channel: "dev" as const }, { available: false }]) {
      const snapshot = makeSnapshot();
      snapshot.update = {
        current: "0.1.0",
        channel: "release",
        latest: "0.2.0",
        available: true,
        url: "https://github.com/Maolon/agendar/releases/tag/v0.2.0",
        published_at: "2026-11-01T10:00:00Z",
        checked_at: "2026-11-09T11:00:00.000Z",
        enabled: true,
        ...patch,
      };
      const { container, unmount } = render(<Harness client={client} snapshot={snapshot} />);
      expect(container.querySelector(".pop-update")).toBeNull();
      expect(container.querySelector(".pop-foot")?.textContent).toMatch(/Updated \d\d:\d\d/);
      unmount();
    }
  });
});

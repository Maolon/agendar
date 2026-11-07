import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { ApiError } from "../api/client";
import { SnapshotStore } from "../api/store";
import type { BriefSection, DecisionCard, Snapshot } from "../api/types";
import {
  FakeClient,
  ASK_ONE,
  EVALUATED_AT,
  easyCard,
  irreversibleCard,
  makeSnapshot,
  sha,
  type SnapshotOptions,
} from "./fixtures";

async function setup(options: SnapshotOptions & { card?: DecisionCard } = {}) {
  const snapshot = makeSnapshot(options);
  if (options.card) snapshot.state.projects.alpha!.decisions[snapshot.needs[0]!.source]!.card = options.card;
  const client = new FakeClient(snapshot);
  const store = new SnapshotStore(client);
  render(<App client={client} store={store} surface="main" />);
  await screen.findByRole("heading", { name: /Since you last looked/ });
  return { client, store };
}

describe("keyboard and action guards", () => {
  it("traverses visible needs and projects in order with ↑/↓ and clamps at the ends", async () => {
    const user = userEvent.setup();
    await setup();

    await user.keyboard("{ArrowUp}");
    expect(screen.getByRole("heading", { name: "How should answers reach idle agents?" })).toBeInTheDocument();

    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("heading", { name: "How long should pane history be kept?" })).toBeInTheDocument();

    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("heading", { name: "Agent stopped mid-task" })).toBeInTheDocument();

    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("heading", { name: "gamma" })).toBeInTheDocument();

    await user.keyboard("{ArrowDown}{ArrowDown}{ArrowDown}");
    expect(screen.getByRole("heading", { name: "beta" })).toBeInTheDocument();
  });

  it("returns to the brief on Escape", async () => {
    const user = userEvent.setup();
    await setup();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("heading", { name: "How should answers reach idle agents?" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(screen.getByRole("heading", { name: /Since you last looked/ })).toBeInTheDocument();
  });

  it("focuses search with ⌘/Ctrl-K and never navigates or submits while typing", async () => {
    const user = userEvent.setup();
    const { client } = await setup();
    await user.keyboard("{Control>}k{/Control}");
    const search = screen.getByRole("searchbox");
    expect(search).toHaveFocus();

    await user.keyboard("alpha{ArrowDown}{Enter}");
    expect(search).toHaveValue("alpha");
    expect(screen.getByRole("heading", { name: /Since you last looked/ })).toBeInTheDocument();
    expect(client.order).not.toContain("answer_decision");

    await user.keyboard("{Escape}");
    expect(search).not.toHaveFocus();
  });

  it("selects a costly option by number but plain Enter cannot submit it", async () => {
    const user = userEvent.setup();
    const { client } = await setup();
    await user.keyboard("{ArrowDown}{ArrowDown}2");

    const primary = document.querySelector<HTMLButtonElement>("[data-primary-action]");
    expect(primary?.textContent).toContain("Keep 24 hours");
    expect(primary?.disabled).toBe(false);

    await user.keyboard("{Enter}");
    expect(client.order).not.toContain("answer_decision");

    await user.keyboard("{Control>}{Enter}{/Control}");
    await waitFor(() => expect(client.order).toContain("answer_decision"));
  });

  it("plain Enter submits the easy recommendation and follows to the next need", async () => {
    const user = userEvent.setup();
    const { client } = await setup();
    await user.keyboard("{ArrowDown}{Enter}");
    await waitFor(() => expect(client.order).toContain("answer_decision"));
    await waitFor(() =>
      expect(
        screen.getByRole("heading", { name: "How long should pane history be kept?" }),
      ).toBeInTheDocument(),
    );
  });

  it("requires a number, then a confirm step, for an irreversible card", async () => {
    const user = userEvent.setup();
    const { client } = await setup({ card: irreversibleCard() });
    await user.keyboard("{ArrowDown}");

    const primary = () => document.querySelector<HTMLButtonElement>("[data-primary-action]");
    expect(primary()?.disabled).toBe(true);

    await user.keyboard("1");
    expect(primary()?.disabled).toBe(false);

    await user.keyboard("{Control>}{Enter}{/Control}");
    await waitFor(() => expect(client.order).toContain("prepare_answer"));
    expect(client.order).not.toContain("answer_decision");
    await screen.findByRole("button", { name: "Confirm: Deploy now" });

    await user.keyboard("{Control>}{Enter}{/Control}");
    await waitFor(() => expect(client.order).toContain("answer_decision"));
  });

  it("puts the second-confirm warning in an amber toast that lasts until the step ends", async () => {
    const user = userEvent.setup();
    await setup({ card: irreversibleCard() });
    await user.keyboard("{ArrowDown}");
    await user.keyboard("1");
    await user.keyboard("{Control>}{Enter}{/Control}");
    await screen.findByRole("button", { name: "Confirm: Deploy now" });

    const toast = await waitFor(() => {
      const found = document.querySelector<HTMLElement>(".toast.warn");
      expect(found).not.toBeNull();
      return found!;
    });
    expect(toast.getAttribute("role")).toBe("alert");
    expect(toast.textContent).toContain("can't be undone without rework");
    expect(toast.textContent).toContain("Deploy now");
    expect(document.querySelector(".dc-acts.confirming")).not.toBeNull();

    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(document.querySelector(".toast.warn")).toBeNull());
  });

  it("stops plain Enter when the backend escalates an easy card to choose", async () => {
    const user = userEvent.setup();
    const { client } = await setup();
    client.guardOverride = "choose";

    await user.keyboard("{ArrowDown}{Enter}");
    await waitFor(() => expect(client.order).toContain("prepare_answer"));
    expect(client.order).not.toContain("answer_decision");

    // Repeated plain Enter must stay inert once the backend guard is stricter.
    await user.keyboard("{Enter}{Enter}");
    expect(client.order).not.toContain("answer_decision");
    expect(client.calls.filter((call) => call.command === "prepare_answer")).toHaveLength(1);

    // A different selection is a new action; Cmd-Enter then commits explicitly.
    await user.keyboard("2");
    await user.keyboard("{Control>}{Enter}{/Control}");
    await waitFor(() => expect(client.order).toContain("answer_decision"));
    const requests = client.calls
      .filter((call) => call.command === "answer_decision")
      .map((call) => call.request as { request_id: string; echo_label: string; token: string });
    expect(requests.at(-1)?.echo_label).toBe("Use inbox on next start");
    // A deliberate new choice prepares again and commits the newer token.
    const prepares = client.calls
      .filter((call) => call.command === "prepare_answer")
      .map((call) => call.request as { option_id: string });
    expect(prepares.map((prepare) => prepare.option_id)).toEqual(["one", "two"]);
    expect(requests.at(-1)?.token).toBe("token-2");
  });

  it("keeps plain Enter inert when an already-selected option is escalated after preparation", async () => {
    const user = userEvent.setup();
    const { client } = await setup();
    client.guardOverride = "choose";

    // Explicitly select the recommendation, then press plain Enter. The card is
    // easy when the source is accepted; only `prepare_answer` reveals the stricter
    // effective guard, so Enter must be re-validated after preparation.
    await user.keyboard("{ArrowDown}1{Enter}");
    await waitFor(() => expect(client.order).toContain("prepare_answer"));
    expect(await screen.findByRole("alert")).toHaveTextContent(/deliberate submit/);
    expect(client.order).not.toContain("answer_decision");

    // A deliberate Cmd/Ctrl-Enter commits the same prepared transaction.
    await user.keyboard("{Control>}{Enter}{/Control}");
    await waitFor(() => expect(client.order).toContain("answer_decision"));
    expect(client.calls.filter((call) => call.command === "prepare_answer")).toHaveLength(1);
  });

  it("requires an explicit choice and an echoed confirmation for a backend confirm escalation", async () => {
    const user = userEvent.setup();
    const { client } = await setup();
    client.guardOverride = "confirm";

    await user.keyboard("{ArrowDown}{Enter}");
    await waitFor(() => expect(client.order).toContain("prepare_answer"));
    expect(client.order).not.toContain("answer_decision");
    await screen.findByRole("alert");

    // Plain Enter never commits the escalated card, not even twice.
    await user.keyboard("{Enter}{Enter}");
    expect(client.order).not.toContain("answer_decision");

    // Number selection, then Cmd-Enter arms the echoed confirmation only.
    await user.keyboard("1");
    await user.keyboard("{Control>}{Enter}{/Control}");
    await screen.findByRole("button", { name: "Confirm: Use harness adapters" });
    expect(client.order).not.toContain("answer_decision");

    // Plain Enter stays inert throughout the confirmation.
    await user.keyboard("{Enter}{Enter}");
    expect(client.order).not.toContain("answer_decision");

    await user.keyboard("{Control>}{Enter}{/Control}");
    await waitFor(() => expect(client.order).toContain("answer_decision"));
  });

  it("disarms the echoed confirmation when the decision revision changes", async () => {
    const user = userEvent.setup();
    const { client } = await setup();
    client.guardOverride = "confirm";

    await user.keyboard("{ArrowDown}{Enter}");
    await waitFor(() => expect(client.order).toContain("prepare_answer"));
    await user.keyboard("1");
    await user.keyboard("{Control>}{Enter}{/Control}");
    await screen.findByRole("button", { name: "Confirm: Use harness adapters" });

    const decision = client.snapshot.state.projects.alpha!.decisions[ASK_ONE]!;
    decision.revision = sha(0x99);
    client.snapshot.generation += 1;
    client.emit("agendar://changed", {
      v: 1,
      epoch: client.snapshot.epoch,
      generation: client.snapshot.generation,
      event_set_digest: sha(0x99),
      as_of: EVALUATED_AT,
      projects: ["alpha"],
      pulses: [],
      batch: "local",
    });
    await waitFor(() => expect(screen.queryByRole("button", { name: /Confirm:/ })).toBeNull());
    expect(client.order).not.toContain("answer_decision");

    // The retained stricter guard still keeps plain Enter inert.
    await user.keyboard("{Enter}{Enter}");
    expect(client.order).not.toContain("answer_decision");
  });

  it("keeps an edited done-when draft with its own ask when answering another", async () => {
    const user = userEvent.setup();
    const { client } = await setup();
    await user.keyboard("{ArrowDown}");
    expect(
      screen.getByRole("heading", { name: "How should answers reach idle agents?" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Edit done when" }));
    const input = screen.getByLabelText("Criterion 1");
    await user.clear(input);
    await user.type(input, "Criterion belonging only to ask A");
    await user.click(screen.getByRole("button", { name: "Save criteria" }));
    expect(screen.getByText("Criterion belonging only to ask A")).toBeInTheDocument();

    // Navigate straight to the other question: no draft may follow it.
    await user.keyboard("{ArrowDown}");
    expect(
      screen.getByRole("heading", { name: "How long should pane history be kept?" }),
    ).toBeInTheDocument();
    expect(screen.queryByText("Criterion belonging only to ask A")).toBeNull();

    await user.keyboard("2");
    await user.keyboard("{Control>}{Enter}{/Control}");
    await waitFor(() => expect(client.order).toContain("answer_decision"));
    const answer = client.calls.find((call) => call.command === "answer_decision")?.request as {
      done_when?: unknown;
    };
    expect(answer.done_when).toBeUndefined();

    // Answering follows to the next ranked need, never to the edited one.
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: "Agent stopped mid-task" })).toBeInTheDocument(),
    );
  });

  it("opens the first need with Ctrl/Cmd-Enter when no card is active", async () => {
    const user = userEvent.setup();
    await setup();
    await user.keyboard("{Control>}{Enter}{/Control}");
    expect(screen.getByRole("heading", { name: "How should answers reach idle agents?" })).toBeInTheDocument();
  });

  it("does not submit an easy card with plain Enter when confidence is low", async () => {
    const user = userEvent.setup();
    const card = easyCard({
      recommended: { option: "one", confidence: "low", because: "Gut feel." },
    });
    const { client } = await setup({ card });
    await user.keyboard("{ArrowDown}{Enter}");
    expect(client.order).not.toContain("answer_decision");
  });

  it("shows All clear only when all six brief sections are empty", async () => {
    const snapshot = emptyBrief(makeSnapshot());
    const client = new FakeClient(snapshot);
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    expect(await screen.findByRole("heading", { name: "All clear" })).toBeInTheDocument();
  });

  it("does not show All clear when only the needs list is empty", async () => {
    const snapshot = makeSnapshot();
    snapshot.needs = [];
    snapshot.brief = { ...snapshot.brief, delivered: [], needs_you: [] };
    const client = new FakeClient(snapshot);
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    expect(await screen.findByRole("heading", { name: /Since you last looked/ })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "All clear" })).toBeNull();
  });

  it("shows a recoverable error screen and reloads after retry", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.snapshotError = new ApiError({
      code: "E_IO",
      message: "Cannot reach the Agendar service.",
      hint: "Check that Agendar is running, then refresh.",
      retryable: true,
    });
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    expect(await screen.findByText("Cannot reach the Agendar service")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /Try again/ }));
    expect(await screen.findByRole("heading", { name: /Since you last looked/ })).toBeInTheDocument();
  });
});

function emptyBrief(snapshot: Snapshot): Snapshot {
  const next = structuredClone(snapshot);
  next.needs = [];
  for (const key of Object.keys(next.brief) as BriefSection[]) next.brief[key] = [];
  for (const project of Object.values(next.state.projects)) {
    project.needs_you = [];
    project.summary.needs_count = 0;
  }
  return next;
}

describe("progress notices", () => {
  it("open their project without a stale-item toast", async () => {
    const snapshot = makeSnapshot();
    const client = new FakeClient(snapshot);
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    await screen.findAllByText(snapshot.state.projects.alpha!.summary.title);
    client.emit("agendar://notification-focus", {
      navigation_epoch: snapshot.navigation_epoch,
      sequence: 5,
      vault_id: snapshot.state.projects.alpha!.summary.slug,
      project: "alpha",
      item_id: "project:alpha",
      status: "open",
    });
    expect(
      await screen.findByRole("heading", { level: 2, name: snapshot.state.projects.alpha!.summary.title }),
    ).toBeInTheDocument();
    expect(screen.queryByText(/no longer current/)).toBeNull();
  });
});

describe("listening state", () => {
  it("says on the question whether an agent hears the answer now", async () => {
    const user = userEvent.setup();
    const snapshot = makeSnapshot();
    snapshot.state.projects.alpha!.listening = { [ASK_ONE]: "connected" };
    const client = new FakeClient(snapshot);
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });
    await user.keyboard("{ArrowUp}");
    expect(await screen.findByText("sees your answer at its next check-in")).toBeInTheDocument();
  });
});

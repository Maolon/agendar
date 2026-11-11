import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { SnapshotStore } from "../api/store";
import type { FeatureState, Snapshot } from "../api/types";
import { FakeClient, makeDelivery, makeDeliverySnapshot, makeGraph, makeProject, makeSnapshot, textBlob } from "./fixtures";

function step(node: string, title: string, progress: FeatureState["progress"]): FeatureState {
  return { ...makeProject("x").features["core/one"]!, node, title, progress, gate: "open" };
}

/** alpha gains steps, an answered decision, a report and an accepted delivery. */
function richSnapshot(): Snapshot {
  const snapshot = makeSnapshot();
  const alpha = snapshot.state.projects.alpha!;
  alpha.features["docs/review"] = step("docs/review", "Review the docs", "doing");
  alpha.features["docs/old"] = step("docs/old", "Old review pass", "verified");
  const open = Object.values(alpha.decisions)[0]!;
  alpha.decisions.answered = {
    ...open,
    id: "answered",
    node: "docs/review",
    lifecycle: "answered",
    effective_receipt: "rcpt",
    card: { ...open.card, question: "Which review format?" },
  };
  alpha.receipts.rcpt = {
    id: "rcpt",
    ask: "answered",
    node: "docs/review",
    source: "human",
    chose: open.card.options[0]!,
    ruled_out: [],
    done_when: [],
    created_at: "2026-02-10T14:00:00.000Z",
  };
  alpha.reports = [
    {
      id: "rep1",
      node: "docs/review",
      title: "Review tools compared",
      summary: "Three ways to review copy",
      report: textBlob(),
      kind: "proposal",
      domain: "ux",
      ts: "2026-02-10T14:30:00.000Z",
      session: "s1",
      harness: "claude_code",
    },
  ];
  const delivery = makeDelivery({ id: "dlv1", node: "docs/review", name: "review-bundle", review: "accepted" });
  alpha.deliveries = { [delivery.id]: delivery };
  return snapshot;
}

async function setup(snapshot: Snapshot = richSnapshot()) {
  const client = new FakeClient(snapshot);
  client.graph = makeGraph({
    nodes: [
      { id: "feature:core/one", kind: "feature", label: "CLI ask", feature: "core/one", state: "doing", assumed: false },
      { id: "feature:docs/review", kind: "feature", label: "Review the docs", feature: "docs/review", state: "doing", assumed: false },
      { id: "decision:answered", kind: "decision", label: "Which review format?", feature: "docs/review", state: "answered", assumed: false },
      { id: "review:dlv1", kind: "review", label: "review-bundle", feature: "docs/review", state: "accepted", assumed: false },
    ],
    edges: [
      { from: "decision:answered", to: "feature:docs/review", source_event: "answered" },
      { from: "review:dlv1", to: "feature:docs/review", source_event: "dlv1" },
    ],
  });
  const store = new SnapshotStore(client);
  render(<App client={client} store={store} surface="main" />);
  await screen.findByRole("heading", { name: /Since you last looked/ });
  const user = userEvent.setup();
  const input = screen.getByRole("combobox", { name: "Search everything" });
  return { client, user, input };
}

async function search(user: ReturnType<typeof userEvent.setup>, input: HTMLElement, text: string) {
  await user.click(input);
  await user.clear(input);
  await user.type(input, text);
  return screen.findByRole("listbox", { name: "Search results" });
}

function groupNames(list: HTMLElement): string[] {
  return within(list)
    .getAllByRole("group")
    .map((group) => group.getAttribute("aria-labelledby"))
    .map((id) => document.getElementById(id!)?.firstChild?.textContent ?? "");
}

describe("search panel", () => {
  it("stays closed while the field is empty and lists every type with the match marked", async () => {
    const { user, input } = await setup();
    await user.click(input);
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(input).toHaveAttribute("aria-expanded", "false");

    const list = await search(user, input, "review");
    expect(input).toHaveAttribute("aria-expanded", "true");
    expect(groupNames(list)).toEqual(["Steps", "Decisions", "Reports", "Deliveries"]);
    const steps = within(list).getByRole("group", { name: /Steps/ });
    const options = within(steps).getAllByRole("option");
    // Open before finished.
    expect(options.map((option) => option.querySelector(".sr-title")?.textContent)).toEqual([
      "Review the docs",
      "Old review pass",
    ]);
    expect(options[0]!.querySelector("mark")?.textContent).toBe("Review");
    expect(options[0]).toHaveTextContent("alpha · docs/review · In progress");
    expect(within(list).getByRole("option", { name: /Which review format/ })).toHaveTextContent(
      /Chose Use harness adapters/,
    );
  });

  it("says No matches and does not filter the sidebar", async () => {
    const { user, input } = await setup();
    const sidebarRows = () => document.querySelectorAll(".side [data-row]").length;
    const before = sidebarRows();
    const list = await search(user, input, "zzz-nothing");
    expect(list).toHaveTextContent("No matches");
    expect(sidebarRows()).toBe(before);
  });

  it("moves with ↑/↓, reports the active option and opens it with Enter", async () => {
    const { user, input } = await setup();
    const list = await search(user, input, "review");
    const options = within(list).getAllByRole("option");
    expect(input).toHaveAttribute("aria-activedescendant", options[0]!.id);
    await user.keyboard("{ArrowDown}");
    expect(input).toHaveAttribute("aria-activedescendant", options[1]!.id);
    expect(options[1]).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{ArrowUp}{ArrowUp}");
    expect(input).toHaveAttribute("aria-activedescendant", options[0]!.id);
    await user.keyboard("{Enter}");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "alpha", level: 2 })).toBeInTheDocument();
    // The step is selected in the plan.
    await waitFor(() =>
      expect(document.querySelector('[data-graph-node="feature:docs/review"]')).toHaveClass("selected"),
    );
    expect(input).not.toHaveFocus();
  });

  it("shows a clear button only with text; it clears, closes the panel and keeps focus", async () => {
    const { user, input } = await setup();
    expect(screen.queryByRole("button", { name: "Clear search" })).not.toBeInTheDocument();
    await search(user, input, "review");
    await user.click(screen.getByRole("button", { name: "Clear search" }));
    expect(input).toHaveValue("");
    expect(input).toHaveFocus();
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Clear search" })).not.toBeInTheDocument();
  });

  it("closes on Escape and gives focus back", async () => {
    const { user, input } = await setup();
    await search(user, input, "review");
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(input).not.toHaveFocus();
    expect(screen.getByRole("heading", { name: /Since you last looked/ })).toBeInTheDocument();
  });

  it("⌘/Ctrl-K focuses the field from anywhere and selects its text", async () => {
    const { user, input } = await setup();
    await search(user, input, "review");
    await user.keyboard("{Escape}");
    await user.keyboard("{Control>}k{/Control}");
    expect(input).toHaveFocus();
    const field = input as HTMLInputElement;
    expect([field.selectionStart, field.selectionEnd]).toEqual([0, "review".length]);
  });

  it("opens a need in its card", async () => {
    const { user, input } = await setup();
    const list = await search(user, input, "idle agents");
    await user.click(within(list).getByRole("option", { name: /How should answers reach idle agents/ }));
    expect(screen.getByRole("heading", { name: "How should answers reach idle agents?" })).toBeInTheDocument();
  });

  it("opens a project page", async () => {
    const { user, input } = await setup();
    const list = await search(user, input, "gamma");
    await user.click(within(within(list).getByRole("group", { name: /Projects/ })).getByRole("option"));
    expect(await screen.findByRole("heading", { name: "gamma", level: 2 })).toBeInTheDocument();
  });

  it("opens an answered decision as its step in the project plan", async () => {
    const { user, input } = await setup();
    const list = await search(user, input, "review format");
    await user.click(within(list).getByRole("option", { name: /Which review format/ }));
    expect(await screen.findByRole("heading", { name: "alpha", level: 2 })).toBeInTheDocument();
    await waitFor(() =>
      expect(document.querySelector('[data-graph-node="feature:docs/review"]')).toHaveClass("selected"),
    );
  });

  it("opens a report in the in-app reader", async () => {
    const { user, input, client } = await setup();
    const list = await search(user, input, "tools compared");
    await user.click(within(list).getByRole("option", { name: /Review tools compared/ }));
    expect(await screen.findByRole("dialog", { name: /Review tools compared/ })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "alpha", level: 2 })).toBeInTheDocument();
    await waitFor(() => expect(client.order).toContain("read_report"));
  });

  it("opens a reviewed delivery on its project page, its step selected in the plan", async () => {
    const { user, input } = await setup();
    const list = await search(user, input, "bundle");
    await user.click(within(list).getByRole("option", { name: /review-bundle/ }));
    expect(await screen.findByRole("heading", { name: "alpha", level: 2 })).toBeInTheDocument();
    expect(document.querySelector('[data-delivery="dlv1"]')).toBeInTheDocument();
    await waitFor(() =>
      expect(document.querySelector('[data-graph-node="feature:docs/review"]')).toHaveClass("selected"),
    );
  });

  it("opens a delivery waiting for review in its review view", async () => {
    const { user, input } = await setup(makeDeliverySnapshot());
    const list = await search(user, input, "search-v2");
    await user.click(within(list).getByRole("option", { name: /search-v2 is ready/ }));
    expect(
      screen.getByRole("heading", { name: "search-v2 is ready. Does it do what you asked?" }),
    ).toBeInTheDocument();
  });

  it("caps a group at eight with an N more row that expands", async () => {
    const snapshot = richSnapshot();
    for (let i = 0; i < 11; i += 1) {
      snapshot.state.projects.beta!.features[`bulk/${i}`] = step(`bulk/${i}`, `Bulk step ${i}`, "planned");
    }
    const { user, input } = await setup(snapshot);
    const list = await search(user, input, "bulk");
    const group = within(list).getByRole("group", { name: /Steps/ });
    expect(within(group).getAllByRole("option")).toHaveLength(9);
    const more = within(group).getByRole("option", { name: "3 more" });
    await user.click(more);
    expect(within(within(list).getByRole("group", { name: /Steps/ })).getAllByRole("option")).toHaveLength(11);
  });
});

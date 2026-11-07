import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { SnapshotStore } from "../api/store";
import type { SnoozeNeedRequest } from "../api/types";
import { snoozeEnd } from "../state/snooze";
import { EVALUATED_AT, FakeClient, makeSnapshot } from "./fixtures";

async function setup() {
  const snapshot = makeSnapshot();
  const client = new FakeClient(snapshot);
  const store = new SnapshotStore(client);
  render(<App client={client} store={store} surface="main" />);
  await screen.findByRole("heading", { name: /Since you last looked/ });
  return { client, store };
}

const row = (id: string) => document.querySelector<HTMLElement>(`[data-row='need:${id}']`);
const snoozes = (client: FakeClient) =>
  client.calls.filter((call) => call.command === "snooze_need").map((call) => call.request as SnoozeNeedRequest);

describe("snooze", () => {
  it("hides a question for an hour: it leaves Needs you and the next one opens", async () => {
    const user = userEvent.setup();
    const { client } = await setup();
    await user.click(row("need-two")!);
    await screen.findByRole("heading", { name: "How long should pane history be kept?" });

    await user.click(screen.getByRole("button", { name: "Not now" }));
    await user.click(within(screen.getByRole("group", { name: "Not now" })).getByRole("button", { name: "1 hour" }));

    await waitFor(() => expect(row("need-two")).toBeNull());
    const [request] = snoozes(client);
    expect(request?.item_id).toBe("need-two");
    expect(await screen.findByText("Snoozed for an hour.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "1 snoozed" })).toBeInTheDocument();
    // The selection moved on instead of staying on the hidden item.
    expect(screen.queryByRole("heading", { name: "How long should pane history be kept?" })).toBeNull();
  });

  it("restores a snoozed item from N snoozed", async () => {
    const user = userEvent.setup();
    const { client } = await setup();
    await user.click(row("rescue-one")!);
    await user.click(screen.getByRole("button", { name: "Not now" }));
    await user.click(screen.getByRole("button", { name: "Tomorrow" }));
    await waitFor(() => expect(row("rescue-one")).toBeNull());

    await user.click(screen.getByRole("button", { name: "1 snoozed" }));
    await user.click(row("rescue-one")!);
    expect(await screen.findByText("Snoozed")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Restore" }));

    await waitFor(() => expect(screen.queryByRole("button", { name: /snoozed/ })).toBeNull());
    expect(row("rescue-one")).not.toBeNull();
    const restore = snoozes(client).at(-1)!;
    expect(Date.parse(restore.until)).toBeLessThanOrEqual(Date.parse(EVALUATED_AT));
    expect(await screen.findByText("Back in Needs you.")).toBeInTheDocument();
  });

  it("sets an item aside from the tray, where the hand-offs are", async () => {
    const user = userEvent.setup();
    const snapshot = makeSnapshot();
    const client = new FakeClient(snapshot);
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="popover" />);
    const rescue = await screen.findByRole("button", { name: /Agent stopped mid-task/ });
    const wrap = rescue.closest(".pop-row-wrap") as HTMLElement;
    await user.click(within(wrap).getByRole("button", { name: "Not now" }));
    await user.click(within(wrap).getByRole("button", { name: "Tomorrow" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: /Agent stopped mid-task/ })).toBeNull());
    expect(snoozes(client)[0]?.item_id).toBe("rescue-one");
  });

  it("dismisses a hand-off for good; its stopped step keeps a Hand off link", async () => {
    const user = userEvent.setup();
    const { client } = await setup();
    await user.click(row("rescue-one")!);
    await user.click(await screen.findByRole("button", { name: "Dismiss" }));
    await waitFor(() => expect(row("rescue-one")).toBeNull());
    expect(client.calls.find((call) => call.command === "dismiss_need")?.request).toMatchObject({
      item_id: "rescue-one",
    });
    expect(client.order).not.toContain("resolve_rescue");
    expect(screen.queryByRole("button", { name: /snoozed/ })).toBeNull();
    expect(await screen.findByText("Dismissed. The step stays stopped on its project page.")).toBeInTheDocument();

    await user.click(document.querySelector<HTMLElement>("[data-row='project:gamma']")!);
    const card = await waitFor(() => {
      const found = document.querySelector<HTMLElement>("[data-feat='core/one']");
      expect(found).not.toBeNull();
      return found!;
    });
    expect(document.querySelector("[data-need='rescue-one']")).toBeNull();
    expect(within(card).getByText("Hand off")).toBeInTheDocument();
    await user.click(card);
    expect(await screen.findByRole("heading", { name: "Agent stopped mid-task" })).toBeInTheDocument();
    expect(screen.getByText("Dismissed")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Hand to next session" })).toBeInTheDocument();
  });

  it("dismisses a hand-off from the tray with its own button", async () => {
    const user = userEvent.setup();
    const client = new FakeClient(makeSnapshot());
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="popover" />);
    const rescue = await screen.findByRole("button", { name: /Agent stopped mid-task/ });
    const wrap = rescue.closest(".pop-row-wrap") as HTMLElement;
    await user.click(within(wrap).getByRole("button", { name: "Dismiss" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: /Agent stopped mid-task/ })).toBeNull());
    expect(client.order).toContain("dismiss_need");
  });

  it("ends a snooze after an hour, tomorrow at nine, or just under a week", () => {
    const now = new Date(2026, 1, 10, 15, 30);
    expect(Date.parse(snoozeEnd("hour", now)) - now.getTime()).toBe(60 * 60 * 1000);
    const tomorrow = new Date(snoozeEnd("tomorrow", now));
    expect([tomorrow.getDate(), tomorrow.getHours(), tomorrow.getMinutes()]).toEqual([11, 9, 0]);
    expect(Date.parse(snoozeEnd("week", now)) - now.getTime()).toBeLessThan(7 * 24 * 60 * 60 * 1000);
  });
});

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { SnapshotStore } from "../api/store";
import type { SnoozeNeedRequest } from "../api/types";
import { formatDayClock } from "../api/format";
import { translator } from "../i18n";
import { activeNeeds, snoozedNeeds, snoozedUntil, snoozeEnd, snoozeOffers } from "../state/snooze";
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

  it("hides an item snoozed on another device: the state says so, nothing is local", async () => {
    const snapshot = makeSnapshot();
    const until = new Date(Date.parse(EVALUATED_AT) + 60 * 60 * 1000).toISOString();
    snapshot.needs.find((need) => need.id === "need-two")!.snoozed_until = until;
    expect(snoozedUntil(snapshot, "need-two")).toBe(until);
    expect(activeNeeds(snapshot).map((need) => need.id)).not.toContain("need-two");
    expect(snoozedNeeds(snapshot).map((need) => need.id)).toEqual(["need-two"]);

    const client = new FakeClient(snapshot);
    render(<App client={client} store={new SnapshotStore(client)} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });
    expect(row("need-two")).toBeNull();
    expect(screen.getByRole("button", { name: "1 snoozed" })).toBeInTheDocument();
    expect(snoozes(client)).toEqual([]);
  });

  it("ignores a snooze an earlier build kept in the local preferences", () => {
    const snapshot = makeSnapshot();
    const until = new Date(Date.parse(EVALUATED_AT) + 60 * 60 * 1000).toISOString();
    Object.assign(snapshot.preferences, { snoozed: { "need-two": until } });
    expect(snoozedUntil(snapshot, "need-two")).toBeNull();
    expect(activeNeeds(snapshot).map((need) => need.id)).toContain("need-two");
    expect(snoozedNeeds(snapshot)).toEqual([]);
  });

  it("shows a row again once its time has passed", () => {
    const snapshot = makeSnapshot();
    snapshot.needs.find((need) => need.id === "need-two")!.snoozed_until = EVALUATED_AT;
    expect(snoozedUntil(snapshot, "need-two")).toBeNull();
    expect(activeNeeds(snapshot).map((need) => need.id)).toContain("need-two");
  });

  it("offers only what the default allows, and shows the time a cut choice ends", () => {
    const now = new Date(2026, 1, 10, 15, 0, 0);
    const inMinutes = (minutes: number) => new Date(now.getTime() + minutes * 60 * 1000).toISOString();
    // No default: the three choices as they are.
    expect(snoozeOffers(undefined, now).map((offer) => [offer.choice, offer.cut])).toEqual([
      ["hour", false],
      ["tomorrow", false],
      ["week", false],
    ]);
    // The row returns in three hours: an hour fits, tomorrow ends then, a week is the same snooze.
    const limit = inMinutes(180);
    expect(snoozeOffers(limit, now)).toEqual([
      { choice: "hour", until: snoozeEnd("hour", now), cut: false },
      { choice: "tomorrow", until: limit, cut: true },
    ]);
    // In half an hour: one choice, at the limit.
    expect(snoozeOffers(inMinutes(30), now)).toEqual([{ choice: "hour", until: inMinutes(30), cut: true }]);
    // The limit is now or has passed: nothing the host would refuse is offered.
    expect(snoozeOffers(inMinutes(0), now)).toEqual([]);
    expect(snoozeOffers(inMinutes(-20), now)).toEqual([]);
    // A far default cuts nothing.
    expect(snoozeOffers(inMinutes(30 * 24 * 60), now).every((offer) => !offer.cut)).toBe(true);
  });

  it("snoozes a question with a default until just before it, and says when it returns", async () => {
    const user = userEvent.setup();
    const snapshot = makeSnapshot();
    const limit = new Date(Date.now() + 3 * 60 * 60 * 1000).toISOString();
    snapshot.needs.find((need) => need.id === "need-two")!.snooze_limit = limit;
    const client = new FakeClient(snapshot);
    render(<App client={client} store={new SnapshotStore(client)} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });
    await user.click(row("need-two")!);
    await user.click(await screen.findByRole("button", { name: "Not now" }));
    const menu = within(screen.getByRole("group", { name: "Not now" }));
    const at = formatDayClock(limit);
    expect(menu.getAllByRole("button").map((button) => button.textContent)).toEqual(["1 hour", `Until ${at}`]);
    await user.click(menu.getByRole("button", { name: `Until ${at}` }));

    await waitFor(() => expect(row("need-two")).toBeNull());
    expect(snoozes(client)[0]).toMatchObject({ item_id: "need-two", until: limit });
    expect(await screen.findByText(`Snoozed until ${at}, before the agent's default.`)).toBeInTheDocument();
  });

  it("offers no snooze once the default is under fifteen minutes away", async () => {
    const user = userEvent.setup();
    const snapshot = makeSnapshot();
    snapshot.needs.find((need) => need.id === "need-two")!.snooze_limit = new Date(Date.now() - 60 * 1000).toISOString();
    const client = new FakeClient(snapshot);
    render(<App client={client} store={new SnapshotStore(client)} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });
    await user.click(row("need-two")!);
    await screen.findByRole("heading", { name: "How long should pane history be kept?" });
    expect(screen.queryByRole("button", { name: "Not now" })).toBeNull();
  });

  it("keeps agent in Latin letters in the zh copy of a cut snooze", () => {
    expect(translator("zh-Hans")("snooze.done.until", { t: "16:40" })).toBe(
      "已延后到 16:40，赶在 agent 按默认执行之前。",
    );
    expect(translator("zh-Hant")("snooze.done.until", { t: "16:40" })).toContain("agent");
    expect(translator("zh-Hans")("snooze.until", { t: "16:40" })).toBe("到 16:40");
  });

  it("sets an item aside from the tray", async () => {
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

  it("dismisses a rescue row for good; its stopped step keeps a Details link", async () => {
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
    expect(within(card).getByText("Details")).toBeInTheDocument();
    await user.click(card);
    expect(await screen.findByRole("heading", { name: "Agent stopped mid-task" })).toBeInTheDocument();
    expect(screen.getByText("Dismissed")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Let another agent continue" })).toBeInTheDocument();
  });

  it("dismisses a rescue row from the tray with its own button", async () => {
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

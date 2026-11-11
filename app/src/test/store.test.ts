import { describe, expect, it, vi } from "vitest";
import { ApiError, createClient } from "../api/client";
import { SnapshotStore } from "../api/store";
import type { ChangedEvent, NotificationFocus } from "../api/types";
import { FakeClient, EVALUATED_AT, makeSnapshot, ulid } from "./fixtures";

function changed(
  client: FakeClient,
  generation: number,
  epoch: string = client.snapshot.epoch,
): ChangedEvent {
  return {
    v: 1,
    epoch,
    generation,
    event_set_digest: client.snapshot.state.event_set_digest,
    as_of: EVALUATED_AT,
    projects: [],
    pulses: [],
    batch: "local",
  };
}

function focus(epoch: string, sequence: number, itemId = "need-two"): NotificationFocus {
  return {
    navigation_epoch: epoch,
    sequence,
    vault_id: ulid(777),
    project: "beta",
    item_id: itemId,
    status: "open",
  };
}

async function started(client: FakeClient): Promise<SnapshotStore> {
  const store = new SnapshotStore(client);
  store.start();
  await vi.waitFor(() => expect(store.getState().phase).toBe("ready"));
  return store;
}

describe("SnapshotStore", () => {
  it("passes on the tray's request to open Settings › About once", async () => {
    const client = new FakeClient();
    const store = await started(client);
    client.emit("agendar://navigate", { view: "settings", tab: "about" });
    expect(store.getState().navigate).toEqual({ tab: "about", seq: 1 });
    store.clearNavigate();
    expect(store.getState().navigate).toBeNull();
    store.stop();
  });

  it("subscribes to every push event before fetching the first snapshot", async () => {
    const client = new FakeClient();
    const store = await started(client);
    expect(client.subscribeCountAtFirstFetch).toBe(7);
    expect(client.snapshotCalls).toBe(1);
    expect(store.getState().snapshot?.generation).toBe(7);
    store.stop();
    expect(client.unsubscribed).toHaveLength(7);
  });

  it("coalesces an event burst into one in-flight fetch plus one catch-up", async () => {
    const client = new FakeClient();
    client.holdSnapshot();
    const store = new SnapshotStore(client);
    store.start();
    await vi.waitFor(() => expect(client.snapshotCalls).toBe(1));

    client.snapshot.generation = 10;
    client.emit("agendar://changed", changed(client, 8));
    client.emit("agendar://changed", changed(client, 9));
    client.emit("agendar://changed", changed(client, 10));
    expect(client.snapshotCalls).toBe(1);

    client.releaseSnapshot();
    await vi.waitFor(() => expect(store.getState().phase).toBe("ready"));
    expect(client.snapshotCalls).toBe(2);
    expect(store.getState().snapshot?.generation).toBe(10);
    store.stop();
  });

  it("discards a response whose epoch was superseded while it was in flight", async () => {
    const client = new FakeClient();
    const store = await started(client);
    const nextEpoch = ulid(950);

    client.holdSnapshot();
    const refresh = store.refresh();
    await vi.waitFor(() => expect(client.snapshotCalls).toBe(2));
    client.snapshot.epoch = nextEpoch;
    client.snapshot.generation = 1;
    client.emit("agendar://changed", changed(client, 1, nextEpoch));
    client.releaseSnapshot();
    await refresh;
    await vi.waitFor(() => expect(store.getState().snapshot?.epoch).toBe(nextEpoch));
    expect(store.getState().snapshot?.generation).toBe(1);
    store.stop();
  });

  it("retains the last snapshot with a stale banner when a refresh fails", async () => {
    const client = new FakeClient();
    const store = await started(client);
    client.snapshotError = new ApiError({
      code: "E_IO",
      message: "Cannot read the Vault.",
      hint: "Check permissions and retry.",
      retryable: true,
    });
    await store.refresh();
    expect(store.getState().phase).toBe("stale");
    expect(store.getState().snapshot).not.toBeNull();
    expect(store.getState().error?.code).toBe("E_IO");

    await store.refresh();
    expect(store.getState().phase).toBe("ready");
    expect(store.getState().error).toBeNull();
    store.stop();
  });

  it("applies notification focus only when (epoch, sequence) is newer", async () => {
    const snapshot = makeSnapshot();
    snapshot.notification_focus = { ...focus(snapshot.navigation_epoch, 1), project: "alpha", item_id: "need-one" };
    const client = new FakeClient(snapshot);
    client.holdSnapshot();
    const store = new SnapshotStore(client);
    store.start();
    await vi.waitFor(() => expect(client.snapshotCalls).toBe(1));

    // Event sequence 2 arrives before snapshot sequence 1.
    client.emit("agendar://notification-focus", focus(snapshot.navigation_epoch, 2));
    client.releaseSnapshot();
    await vi.waitFor(() => expect(store.getState().phase).toBe("ready"));
    expect(store.getState().focus?.sequence).toBe(2);

    store.acknowledgeFocus(2);
    expect(store.getState().focus).toBeNull();

    // An older duplicate after the applied one is ignored.
    client.emit("agendar://notification-focus", focus(snapshot.navigation_epoch, 1));
    expect(store.getState().focus).toBeNull();

    // A new process navigation epoch resets the cursor and accepts sequence 1.
    const next = makeSnapshot();
    next.navigation_epoch = ulid(902);
    next.notification_focus = { ...focus(next.navigation_epoch, 1), project: "alpha", item_id: "need-one" };
    client.setSnapshot(next);
    await store.refresh();
    expect(store.getState().focus?.sequence).toBe(1);
    store.stop();
  });

  it("serves the plain-browser preview from the dev fixture transport", async () => {
    const { createFixtureTransport } = await import("../dev/fixtureTransport");
    const client = createClient(createFixtureTransport(), "fixture");
    expect(client.mode).toBe("fixture");
    const snapshot = await client.getSnapshot();
    const need = snapshot.needs.find((item) => item.kind === "decide");
    expect(need).toBeDefined();
    const decision = snapshot.state.projects[need!.project]?.decisions[need!.source];
    expect(decision).toBeDefined();
    const prepared = await client.prepareAnswer({
      ask_id: decision!.id,
      option_id: decision!.card.recommended.option,
      expected_decision_revision: decision!.revision,
    });
    expect(prepared.guard).toBe("easy");
    const answered = await client.answerDecision({
      request_id: "01J00000000000000000000001",
      token: prepared.token,
      echo_label: prepared.label,
    });
    expect(answered.awaiting_ack).toBe(true);
    const after = await client.getSnapshot();
    expect(after.needs.some((item) => item.id === need!.id)).toBe(false);
  });
});

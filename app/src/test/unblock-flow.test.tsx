import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { Transport } from "../api/client";
import { createClient } from "../api/client";
import { SnapshotStore } from "../api/store";
import type { UnblockEvent } from "../api/types";
import { createFixtureTransport } from "../dev/fixtureTransport";
import { App } from "../App";

/**
 * The answer → `agendar://unblock` → “See it unblock” loop against the dev-only
 * fixture transport. The ack phase is injected through the same event channel,
 * because a fixture must never fabricate an agent acknowledgement.
 */
function transportWithAck(): {
  transport: Transport;
  unblocks: UnblockEvent[];
  emitAck: () => Promise<void>;
} {
  const base = createFixtureTransport();
  const unblocks: UnblockEvent[] = [];
  let handler: ((event: UnblockEvent) => void) | null = null;
  const transport: Transport = {
    invoke: (command, request) => base.invoke(command, request),
    listen: async (event, listener) => {
      if (event === "agendar://unblock") {
        const unlisten = await base.listen(event, (payload) => {
          unblocks.push(payload as UnblockEvent);
          (listener as (payload: UnblockEvent) => void)(payload as UnblockEvent);
          handler = listener as (payload: UnblockEvent) => void;
        });
        return unlisten;
      }
      return base.listen(event, listener);
    },
  };
  return {
    transport,
    unblocks,
    emitAck: async () => {
      const answered = unblocks.find((event) => event.phase === "answered");
      if (!answered || !handler) throw new Error("no answered transition to acknowledge");
      await act(async () => {
        handler?.({ ...answered, phase: "acked" });
        await Promise.resolve();
      });
    },
  };
}

describe("answer → unblock flow", () => {
  it("plays the real transition, opens the graph and updates on the agent's ack", async () => {
    const { transport, unblocks, emitAck } = transportWithAck();
    const client = createClient(transport, "fixture");
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);

    await screen.findByRole("heading", { name: /Since you last looked/ });
    await userEvent.click(
      document.querySelector<HTMLElement>('[data-row="need:decide:core-ipc"]') as HTMLElement,
    );
    await screen.findByRole("heading", {
      name: "How should answers reach idle agents?",
    });

    await userEvent.click(document.querySelector<HTMLElement>("[data-primary-action]") as HTMLElement);
    // The persisted answer and its released set arrive as one real transition.
    await waitFor(() => expect(unblocks.length).toBeGreaterThan(0));
    expect(unblocks[0]?.phase).toBe("answered");
    expect(unblocks[0]?.project).toBe("agendar");
    expect(unblocks[0]?.released).toEqual(["core/wake"]);

    await userEvent.click(await screen.findByRole("button", { name: "See it unblock" }));
    await screen.findByText("Awaiting acknowledgement from the agent.");

    // The released feature is drawn in the graph the moment it starts.
    const node = await waitFor(() => {
      const found = document.querySelector<HTMLElement>('[data-graph-node="feature:core/wake"]');
      if (!found) throw new Error("released feature not drawn yet");
      return found;
    });
    expect(node.textContent).toContain("Wake idle agents");

    await emitAck();
    await screen.findByText("The agent acknowledged the receipt.");
    // A late ack never plays a second moment: no packet is drawn for it.
    expect(document.querySelectorAll("[data-packet]")).toHaveLength(0);
  });
});

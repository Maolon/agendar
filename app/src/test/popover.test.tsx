import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { SnapshotStore } from "../api/store";
import { FakeClient, makeSnapshot } from "./fixtures";

async function setup() {
  const client = new FakeClient(makeSnapshot());
  const store = new SnapshotStore(client);
  render(<App client={client} store={store} surface="popover" />);
  await screen.findByText("Approve recommended");
  return { client, store };
}

function press(init: KeyboardEventInit): void {
  const event = new KeyboardEvent("keydown", {
    key: "Enter",
    bubbles: true,
    cancelable: true,
    ...init,
  });
  // jsdom does not consistently apply every init-only field; set them explicitly.
  Object.defineProperty(event, "isComposing", { value: init.isComposing ?? false });
  Object.defineProperty(event, "repeat", { value: init.repeat ?? false });
  window.dispatchEvent(event);
}

function answers(client: FakeClient): unknown[] {
  return client.calls.filter((call) => call.command === "answer_decision").map((call) => call.request);
}

describe("app popover keyboard policy", () => {
  it("ignores an IME-composing Enter", async () => {
    const { client } = await setup();
    press({ isComposing: true });
    expect(client.order).not.toContain("prepare_answer");
    expect(client.order).not.toContain("answer_decision");
  });

  it("ignores key repeats", async () => {
    const { client } = await setup();
    press({ repeat: true });
    expect(client.order).not.toContain("prepare_answer");
    expect(client.order).not.toContain("answer_decision");
  });

  it("approves the eligible recommendation exactly once with plain Enter", async () => {
    const { client } = await setup();
    press({});
    await waitFor(() => expect(client.order).toContain("answer_decision"));
    expect(answers(client)).toHaveLength(1);
    expect(client.order.filter((name) => name !== "get_snapshot" && name !== "window_chrome")).toEqual([
      "prepare_answer",
      "answer_decision",
    ]);
    // A second Enter after the item was answered commits nothing new.
    press({});
    expect(answers(client)).toHaveLength(1);
  });
});

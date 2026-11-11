import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SnapshotStore } from "../api/store";
import type { Snapshot, UpdateStatus } from "../api/types";
import { Sidebar } from "../components/Sidebar";
import { FakeClient, makeSnapshot } from "./fixtures";

function status(patch: Partial<UpdateStatus> = {}): UpdateStatus {
  return {
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
}

function snapshotWith(update: UpdateStatus | undefined): Snapshot {
  const snapshot = makeSnapshot();
  snapshot.update = update;
  return snapshot;
}

function sidebar(snapshot: Snapshot, onSelect = vi.fn()) {
  return (
    <Sidebar snapshot={snapshot} selection={null} quietOpen={false} onSelect={onSelect} onToggleQuiet={vi.fn()} />
  );
}

function slot(container: HTMLElement): HTMLElement {
  const element = container.querySelector<HTMLElement>(".foot-tag");
  if (!element) throw new Error("no footer tag");
  return element;
}

function stubReducedMotion(reduced: boolean): void {
  window.matchMedia = ((query: string) => ({
    matches: reduced && query.includes("prefers-reduced-motion"),
    media: query,
    onchange: null,
    addListener: () => undefined,
    removeListener: () => undefined,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

afterEach(() => {
  stubReducedMotion(false);
});

describe("the footer status tag", () => {
  it("shows the sync label when nothing is going on", () => {
    const { container } = render(sidebar(snapshotWith(status({ available: false, latest: "0.1.0" }))));
    const tag = slot(container);
    expect(tag.dataset.state).toBe("sync");
    expect(tag).toHaveClass("foot-received");
    expect(tag.textContent).toMatch(/received|synced/i);
    expect(screen.queryByRole("button", { name: /Update/ })).toBeNull();
  });

  it("shows checking with a spinner, for release and dev builds alike", () => {
    for (const channel of ["release", "dev"] as const) {
      const { container, unmount } = render(sidebar(snapshotWith(status({ channel, checking: true }))));
      const tag = slot(container);
      expect(tag.dataset.state).toBe("checking");
      // Short on screen so the footer stays one line; full for the tooltip
      // and screen readers.
      const live = screen.getByRole("status", { name: "Checking for updates…" });
      expect(live).toHaveTextContent("Checking…");
      expect(live).toHaveAttribute("title", "Checking for updates…");
      expect(tag.querySelector(".foot-spin")).not.toBeNull();
      unmount();
    }
  });

  it("turns into the Update pill on a release build and opens About", async () => {
    const onSelect = vi.fn();
    const { container } = render(sidebar(snapshotWith(status()), onSelect));
    expect(slot(container).dataset.state).toBe("update");
    const pill = screen.getByRole("button", { name: "Update 0.2.0" });
    // The sync time stays reachable from the tooltip.
    expect(pill.getAttribute("title")).toMatch(/Agendar 0\.2\.0 is available · /);
    await userEvent.setup().click(pill);
    expect(onSelect).toHaveBeenCalledWith({ kind: "settings", id: "about" });
  });

  it("keeps every face's text truncatable inside the one-line footer", () => {
    for (const update of [status({ available: false, latest: "0.1.0" }), status({ checking: true }), status()]) {
      const { container, unmount } = render(sidebar(snapshotWith(update)));
      const row = container.querySelector(".foot-status")!;
      // One row: the tag sits directly in the status row next to the agents label.
      expect(slot(container).parentElement).toBe(row);
      expect(row.querySelector(".foot-agents")).not.toBeNull();
      // Its label is the ellipsis-truncating `.foot-text`.
      expect(slot(container).querySelector(".foot-layer.foot-in .foot-text, .foot-layer .foot-text")).not.toBeNull();
      unmount();
    }
  });

  it("never shows the Update pill on a dev build, an older release or with checks off", () => {
    for (const update of [
      status({ channel: "dev", available: false }),
      status({ channel: "dev", available: true }),
      status({ latest: "0.1.0", available: false }),
      status({ enabled: false }),
      undefined,
    ]) {
      const { container, unmount } = render(sidebar(snapshotWith(update)));
      expect(slot(container).dataset.state).toBe("sync");
      expect(screen.queryByRole("button", { name: /Update/ })).toBeNull();
      unmount();
    }
  });

  it("crossfades between faces when motion is allowed", () => {
    stubReducedMotion(false);
    const { container, rerender } = render(sidebar(snapshotWith(status({ available: false, latest: "0.1.0" }))));
    expect(slot(container)).toHaveClass("foot-morph");
    rerender(sidebar(snapshotWith(status({ checking: true }))));
    expect(slot(container).querySelector(".foot-layer.foot-in")).not.toBeNull();
    // The sync face fades out underneath, inert.
    const leaving = slot(container).querySelector(".foot-layer.foot-out");
    expect(leaving).not.toBeNull();
    expect(leaving).toHaveAttribute("aria-hidden", "true");
  });

  it("swaps at once with reduced motion: no transition classes, no fixed width", () => {
    stubReducedMotion(true);
    const { container, rerender } = render(sidebar(snapshotWith(status({ available: false, latest: "0.1.0" }))));
    rerender(sidebar(snapshotWith(status({ checking: true }))));
    rerender(sidebar(snapshotWith(status())));
    const tag = slot(container);
    expect(tag.dataset.state).toBe("update");
    expect(tag).not.toHaveClass("foot-morph");
    expect(tag.querySelector(".foot-in, .foot-out")).toBeNull();
    expect(tag.style.width).toBe("");
  });

  it("swaps at once with a static spinner when the Motion preference is off", () => {
    stubReducedMotion(false);
    const still = (update: UpdateStatus) => {
      const snapshot = snapshotWith(update);
      snapshot.preferences.motion = false;
      return sidebar(snapshot);
    };
    const { container, rerender } = render(still(status({ available: false, latest: "0.1.0" })));
    rerender(still(status({ checking: true })));
    const tag = slot(container);
    expect(tag.dataset.state).toBe("checking");
    expect(tag).toHaveClass("foot-still");
    expect(tag).not.toHaveClass("foot-morph");
    expect(tag.querySelector(".foot-in, .foot-out")).toBeNull();
    expect(tag.style.width).toBe("");
    expect(tag.querySelector(".foot-spin")).not.toBeNull();
  });

  it("follows the host's update events at once, without a new snapshot", async () => {
    const client = new FakeClient();
    client.snapshot.update = status({ channel: "release", available: false, latest: "0.1.0" });
    const store = new SnapshotStore(client);
    store.start();
    await vi.waitFor(() => expect(store.getState().phase).toBe("ready"));
    const fetches = client.snapshotCalls;
    const view = () => sidebar(store.getState().snapshot!);
    const { container, rerender } = render(view());
    expect(slot(container).dataset.state).toBe("sync");

    act(() => client.emit("agendar://update", { v: 1, phase: "started", update: status({ checking: true, latest: "0.1.0", available: false }) }));
    rerender(view());
    expect(slot(container).dataset.state).toBe("checking");

    act(() => client.emit("agendar://update", { v: 1, phase: "finished", update: status({ checking: false }) }));
    rerender(view());
    await waitFor(() => expect(screen.getAllByRole("button", { name: "Update 0.2.0" }).length).toBeGreaterThan(0));
    expect(client.snapshotCalls).toBe(fetches);
    store.stop();
  });
});

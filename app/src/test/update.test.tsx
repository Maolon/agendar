import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ClientContext } from "../api/store";
import type { SetPreferencesRequest, Snapshot, UpdateStatus } from "../api/types";
import { Settings } from "../views/Settings";
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

function renderAbout(snapshot: Snapshot, client = new FakeClient()) {
  client.setSnapshot(snapshot);
  const onRefresh = vi.fn();
  render(
    <ClientContext.Provider value={client}>
      <Settings snapshot={snapshot} onPreferences={vi.fn()} onRefresh={onRefresh} initialTab="about" />
    </ClientContext.Provider>,
  );
  return { client, onRefresh, panel: screen.getByRole("tabpanel") };
}

describe("update detection", () => {
  it("About shows the version, the release channel and the latest release", async () => {
    const { client, onRefresh, panel } = renderAbout(snapshotWith(status()));
    const view = within(panel);
    await waitFor(() => expect(view.getByText("0.1.0")).toBeInTheDocument());
    expect(view.getByText("Release")).toBeInTheDocument();
    expect(view.getByText("0.2.0 · 2026-11-01")).toBeInTheDocument();
    expect(view.getByText("Agendar 0.2.0 is available")).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(view.getByRole("button", { name: "What's new" }));
    expect(client.calls.some((call) => call.command === "open_release")).toBe(true);
    await user.click(view.getByRole("button", { name: "Check now" }));
    await waitFor(() => expect(client.calls.some((call) => call.command === "check_updates")).toBe(true));
    expect(onRefresh).toHaveBeenCalled();
  });

  it("About shows a local build neutrally, with no update prompt", () => {
    const { panel } = renderAbout(snapshotWith(status({ channel: "dev", available: false })));
    const view = within(panel);
    expect(view.getByText("Local build")).toBeInTheDocument();
    expect(view.getByText("0.2.0 · 2026-11-01")).toBeInTheDocument();
    expect(view.queryByText(/is available/)).toBeNull();
    expect(view.queryByText("Up to date")).toBeNull();
  });

  it("the switch turns checks off and Check now waits for it", async () => {
    const snapshot = snapshotWith(status());
    snapshot.preferences.update_check = false;
    const { client, panel } = renderAbout(snapshot);
    const view = within(panel);
    expect(view.getByRole("button", { name: "Check now" })).toBeDisabled();
    const toggle = view.getByRole("switch", { name: "Check for updates" });
    expect(toggle).toHaveAttribute("aria-checked", "false");
    await userEvent.setup().click(toggle);
    await waitFor(() =>
      expect(
        client.calls
          .filter((call) => call.command === "set_preferences")
          .map((call) => (call.request as SetPreferencesRequest).update_check),
      ).toEqual([true]),
    );
  });
});

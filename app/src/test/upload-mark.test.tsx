import { act, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { formatClockTime } from "../api/format";
import { SnapshotStore } from "../api/store";
import type { Snapshot, UploadStatus } from "../api/types";
import { footTagState } from "../components/FootStatus";
import { Sidebar } from "../components/Sidebar";
import { I18nProvider } from "../i18n";
import { FakeClient, makeSnapshot } from "./fixtures";

const SINCE = "2026-11-10T14:02:00.000Z";

function snapshotWith(upload: UploadStatus | undefined): Snapshot {
  const snapshot = makeSnapshot();
  if (upload) snapshot.local.upload = upload;
  else delete snapshot.local.upload;
  return snapshot;
}

function sidebar(snapshot: Snapshot, language = "en") {
  return (
    <I18nProvider preference={language}>
      <Sidebar snapshot={snapshot} selection={null} quietOpen={false} onSelect={vi.fn()} onToggleQuiet={vi.fn()} />
    </I18nProvider>
  );
}

function tag(container: HTMLElement): HTMLElement {
  const element = container.querySelector<HTMLElement>(".foot-tag");
  if (!element) throw new Error("no footer tag");
  return element;
}

describe("the footer's upload line", () => {
  it("shows nothing when no upload state is known (not iCloud, Windows, Linux)", () => {
    const { container } = render(sidebar(snapshotWith(undefined)));
    expect(tag(container).dataset.state).toBe("sync");
    expect(container.querySelector("[data-upload]")).toBeNull();
    expect(container.textContent).not.toMatch(/upload/i);
  });

  it("says Not uploaded yet while this Mac's writes wait", () => {
    const { container } = render(sidebar(snapshotWith({ state: "waiting", since: SINCE })));
    expect(tag(container).dataset.state).toBe("upload");
    const line = screen.getByRole("status", { name: "Not uploaded yet" });
    expect(line).toHaveTextContent("Not uploaded yet");
    expect(line.dataset.upload).toBe("waiting");
    // Stays readable on a narrow sidebar, as an icon with its tooltip.
    expect(tag(container)).not.toHaveClass("foot-received");
    expect(line).toHaveAttribute("title", "Not uploaded yet");
  });

  it("says since when after two minutes", () => {
    const time = formatClockTime(SINCE);
    render(sidebar(snapshotWith({ state: "late", since: SINCE })));
    const line = screen.getByRole("status", { name: `Still not uploaded since ${time}` });
    expect(line).toHaveTextContent(`Not uploaded since ${time}`);
    expect(line.dataset.upload).toBe("late");
  });

  it("localizes upload status in zh-Hans and zh-Hant", () => {
    const time = formatClockTime(SINCE);
    const hans = render(sidebar(snapshotWith({ state: "waiting", since: SINCE }), "zh-Hans"));
    expect(screen.getByRole("status", { name: "等待上传" })).toHaveTextContent("等待上传");
    hans.unmount();
    const late = render(sidebar(snapshotWith({ state: "late", since: SINCE }), "zh-Hans"));
    expect(screen.getByRole("status", { name: `${time} 起仍未上传` })).toHaveTextContent(`${time} 起未上传`);
    late.unmount();
    const hant = render(sidebar(snapshotWith({ state: "waiting", since: SINCE }), "zh-Hant"));
    expect(screen.getByRole("status", { name: "等待上傳" })).toBeInTheDocument();
    hant.unmount();
    render(<I18nProvider preference="en">{null}</I18nProvider>);
  });

  it("waiting replaces only the sync label; late wins over a check and an update", () => {
    const update = {
      current: "0.1.0",
      channel: "release" as const,
      latest: "0.2.0",
      available: true,
      url: null,
      published_at: null,
      checked_at: null,
      enabled: true,
    };
    const waiting = snapshotWith({ state: "waiting", since: SINCE });
    waiting.update = { ...update, available: false, latest: "0.1.0" };
    expect(footTagState(waiting)).toBe("upload");
    waiting.update = update;
    expect(footTagState(waiting)).toBe("update");
    render(sidebar(waiting));
    expect(screen.getByRole("button", { name: "Update 0.2.0" })).toBeInTheDocument();
    expect(screen.queryByRole("status", { name: "Not uploaded yet" })).toBeNull();
    waiting.update = { ...update, checking: true };
    expect(footTagState(waiting)).toBe("checking");

    const late = snapshotWith({ state: "late", since: SINCE });
    late.update = { ...update, checking: true };
    expect(footTagState(late)).toBe("upload");
    late.update = update;
    expect(footTagState(late)).toBe("upload");
  });

  it("follows the host's upload events without a new snapshot", async () => {
    const client = new FakeClient();
    const store = new SnapshotStore(client);
    store.start();
    await vi.waitFor(() => expect(store.getState().phase).toBe("ready"));
    const fetches = client.snapshotCalls;
    const view = () => sidebar(store.getState().snapshot!);
    const { container, rerender } = render(view());
    expect(tag(container).dataset.state).not.toBe("upload");

    act(() => client.emit("agendar://upload", { v: 1, upload: { state: "waiting", since: SINCE } }));
    rerender(view());
    expect(tag(container).dataset.state).toBe("upload");

    act(() => client.emit("agendar://upload", { v: 1, upload: null }));
    rerender(view());
    expect(tag(container).dataset.state).not.toBe("upload");
    expect(store.getState().snapshot!.local).not.toHaveProperty("upload");
    expect(client.snapshotCalls).toBe(fetches);
    store.stop();
  });
});

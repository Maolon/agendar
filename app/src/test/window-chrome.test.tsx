import { render, screen, waitFor, within } from "@testing-library/react";
import { createRef } from "react";
import { describe, expect, it, vi } from "vitest";
import { ClientContext } from "../api/store";
import type { WindowChrome } from "../api/types";
import { Shell } from "../components/Shell";
import { TrayPopover } from "../components/TrayPopover";
import { FakeClient, makeSnapshot } from "./fixtures";

function renderShell(client: FakeClient, chrome: WindowChrome) {
  return render(
    <ClientContext.Provider value={client}>
      <Shell
        snapshot={makeSnapshot()}
        phase="ready"
        error={null}
        filter=""
        selection={null}
        quietOpen={false}
        devFixture={false}
        motion={false}
        pulses={new Set()}
        searchRef={createRef<HTMLInputElement>()}
        onFilter={vi.fn()}
        onSelect={vi.fn()}
        onToggleQuiet={vi.fn()}
        onRefresh={vi.fn()}
        chrome={chrome}
      >
        <p>content</p>
      </Shell>
    </ClientContext.Provider>,
  );
}

describe("window chrome", () => {
  it("has one toolbar row without the app name, and the window drags from it", () => {
    const client = new FakeClient();
    const { container } = renderShell(client, { overlay_titlebar: false, material: "solid" });
    const toolbar = container.querySelector(".toolbar");
    expect(toolbar).not.toBeNull();
    expect(toolbar?.hasAttribute("data-tauri-drag-region")).toBe(true);
    expect(within(toolbar as HTMLElement).queryByText("Agendar")).toBeNull();
    expect(container.querySelector(".titlebar")).toBeNull();
    expect(screen.getByRole("button", { name: "Hide sidebar" })).toBeInTheDocument();
    // Solid windows never ask for native glass.
    expect(client.calls.some((call) => call.command === "set_glass_regions")).toBe(false);
  });

  it("marks glass and overlay, reports glass regions and clears them on unmount", async () => {
    const client = new FakeClient();
    const { container, unmount } = renderShell(client, {
      overlay_titlebar: true,
      material: "glass",
    });
    const win = container.querySelector(".win");
    expect(win?.getAttribute("data-material")).toBe("glass");
    expect(win?.getAttribute("data-overlay")).toBe("true");
    await waitFor(() =>
      expect(client.calls.some((call) => call.command === "set_glass_regions")).toBe(true),
    );
    const before = client.calls.filter((call) => call.command === "set_glass_regions").length;
    unmount();
    const after = client.calls.filter((call) => call.command === "set_glass_regions");
    expect(after.length).toBe(before + 1);
    expect(after.at(-1)?.request).toEqual({ regions: [] });
  });

  it("makes the whole menu bar popover a glass panel when glass is available", async () => {
    const client = new FakeClient();
    const { container } = render(
      <ClientContext.Provider value={client}>
        <TrayPopover
          snapshot={makeSnapshot()}
          chrome={{ overlay_titlebar: false, material: "glass" }}
          canMutate
          onOpen={vi.fn()}
          onAnswered={vi.fn()}
          onRefresh={vi.fn()}
        />
      </ClientContext.Provider>,
    );
    expect(container.querySelector(".pop")?.getAttribute("data-material")).toBe("glass");
    await waitFor(() =>
      expect(client.calls.some((call) => call.command === "set_glass_regions")).toBe(true),
    );
  });
});

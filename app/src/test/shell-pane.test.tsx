import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createRef } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { PANE_DEFAULT, PANE_MIN, PANE_RAIL, Shell, clampPane } from "../components/Shell";
import { makeSnapshot } from "./fixtures";

function renderShell() {
  return render(
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
    >
      <p>content</p>
    </Shell>,
  );
}

describe("resizable, collapsible sidebar", () => {
  beforeEach(() => window.localStorage.clear());

  it("collapses with the titlebar toggle or ⌘\\ and remembers it", async () => {
    const user = userEvent.setup();
    const { container, unmount } = renderShell();
    expect(container.querySelector(".side")).not.toBeNull();
    await user.click(screen.getByRole("button", { name: "Hide sidebar" }));
    expect(container.querySelector(".win")!.className).toContain("pane-collapsed");
    expect(window.localStorage.getItem("agendar.pane.collapsed")).toBe("1");
    unmount();

    const again = renderShell();
    expect(again.container.querySelector(".win")!.className).toContain("pane-collapsed");
    fireEvent.keyDown(window, { key: "\\", metaKey: true });
    expect(again.container.querySelector(".win")!.className).not.toContain("pane-collapsed");
  });

  it("resizes from the keyboard within its bounds and resets on double click", async () => {
    renderShell();
    const grip = screen.getByRole("separator", { name: "Sidebar width" });
    expect(grip.getAttribute("aria-valuenow")).toBe(String(PANE_DEFAULT));
    fireEvent.keyDown(grip, { key: "ArrowRight" });
    expect(grip.getAttribute("aria-valuenow")).toBe(String(PANE_DEFAULT + 16));
    expect(window.localStorage.getItem("agendar.pane.width")).toBe(String(PANE_DEFAULT + 16));
    for (let step = 0; step < 40; step += 1) fireEvent.keyDown(grip, { key: "ArrowLeft" });
    // Past the narrowest full sidebar it becomes the icon rail…
    expect(grip.getAttribute("aria-valuenow")).toBe(String(PANE_RAIL));
    // …and one step back out is the narrowest full sidebar.
    fireEvent.keyDown(grip, { key: "ArrowRight" });
    expect(grip.getAttribute("aria-valuenow")).toBe(String(PANE_MIN));
    fireEvent.doubleClick(grip);
    expect(grip.getAttribute("aria-valuenow")).toBe(String(PANE_DEFAULT));
  });

  it("snaps a drag below halfway to the narrowest width into the rail", () => {
    expect(clampPane(PANE_RAIL + 10)).toBe(PANE_RAIL);
    expect(clampPane((PANE_RAIL + PANE_MIN) / 2 - 1)).toBe(PANE_RAIL);
    expect(clampPane((PANE_RAIL + PANE_MIN) / 2 + 1)).toBe(PANE_MIN);
    expect(clampPane(10_000)).toBe(520);
  });
});

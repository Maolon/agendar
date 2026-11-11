import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../App";
import { SnapshotStore } from "../api/store";
import { detectMac, resetShortcutPlatform, shortcut } from "../state/shortcut";
import { FakeClient, makeSnapshot } from "./fixtures";

/** Stub the platform on the navigator instance; the prototype getters stay. */
function stubPlatform(nav: { platform: string; userAgent: string }): void {
  for (const [key, value] of Object.entries(nav)) {
    Object.defineProperty(navigator, key, { value, configurable: true });
  }
}

afterEach(() => {
  for (const key of ["platform", "userAgent"]) delete (navigator as unknown as Record<string, unknown>)[key];
  resetShortcutPlatform();
});

describe("shortcut labels", () => {
  it("detects macOS and iOS, preferring userAgentData", () => {
    expect(detectMac({ userAgentData: { platform: "macOS" }, platform: "Win32" })).toBe(true);
    expect(detectMac({ platform: "MacIntel" })).toBe(true);
    expect(detectMac({ userAgent: "Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X)" })).toBe(true);
    expect(detectMac({ userAgentData: { platform: "Windows" }, platform: "MacIntel" })).toBe(false);
    expect(detectMac({ platform: "Win32" })).toBe(false);
    expect(detectMac({ platform: "Linux x86_64" })).toBe(false);
    expect(detectMac(undefined)).toBe(false);
  });

  it("spells glyphs on mac and Ctrl/Enter on Windows and Linux", () => {
    expect(["mod+k", "mod+enter", "enter", "mod+\\", "mod+,"].map((key) => shortcut(key as never, true))).toEqual([
      "⌘K",
      "⌘↵",
      "↵",
      "⌘\\",
      "⌘,",
    ]);
    expect(["mod+k", "mod+enter", "enter", "mod+\\", "mod+,"].map((key) => shortcut(key as never, false))).toEqual([
      "Ctrl+K",
      "Ctrl+Enter",
      "Enter",
      "Ctrl+\\",
      "Ctrl+,",
    ]);
  });

  it.each([
    ["Windows", { platform: "Win32", userAgent: "Mozilla/5.0 (Windows NT 10.0)" }, "Ctrl+K", "Ctrl+Enter"],
    ["Linux", { platform: "Linux x86_64", userAgent: "Mozilla/5.0 (X11; Linux x86_64)" }, "Ctrl+K", "Ctrl+Enter"],
    ["macOS", { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh)" }, "⌘K", "⌘↵"],
  ])("shows the %s form in the search field and the brief", async (_name, nav, searchKey, startKey) => {
    stubPlatform(nav);
    resetShortcutPlatform();
    const client = new FakeClient(makeSnapshot());
    render(<App client={client} store={new SnapshotStore(client)} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });
    expect(document.querySelector(".search kbd")).toHaveTextContent(searchKey);
    expect(screen.getByRole("button", { name: new RegExp(startKey.replace(/[+\\]/g, "\\$&")) })).toBeInTheDocument();
  });
});

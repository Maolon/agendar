import { useEffect, useState, type RefObject } from "react";
import type { AgendarClient } from "../api/client";
import type { GlassRegion, GlassTint, WindowChrome } from "../api/types";

const SOLID: WindowChrome = { overlay_titlebar: false, material: "solid" };

/**
 * Ask once how this window paints its chrome. The answer marks the document
 * (`data-overlay`) so screens other than the board also clear the traffic
 * lights on macOS.
 */
export function useWindowChrome(client: AgendarClient, enabled: boolean): WindowChrome {
  const [chrome, setChrome] = useState<WindowChrome>(SOLID);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    client
      .windowChrome()
      .then((value) => {
        if (live) setChrome(value);
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [client, enabled]);
  useEffect(() => {
    document.documentElement.dataset.overlay = chrome.overlay_titlebar ? "true" : "false";
  }, [chrome.overlay_titlebar]);
  return chrome;
}

/**
 * Page elements that native glass shows through, and the edges they follow.
 * Native glass sits behind the webview, so it only suits surfaces with the
 * window behind them: the sidebar. Toolbar capsules float over page content
 * and use CSS glass instead.
 */
export type GlassTargets = ReadonlyArray<readonly [string, GlassRegion["anchor"]]>;

/** The main window: the floating sidebar. */
export const SHELL_GLASS: GlassTargets = [[".side", "fill"]];

/** The menu bar popover: the whole panel. */
export const POPOVER_GLASS: GlassTargets = [[".pop", "fill"]];

/** How strongly the glass leans toward the sidebar colour, for legible text. */
const TINT_ALPHA = 0.5;

/** The sidebar token as an sRGB tint, or null when it cannot be read. */
function tint(root: HTMLElement, token: string): GlassTint | null {
  const value = getComputedStyle(root).getPropertyValue(token).trim();
  const hex = /^#([0-9a-f]{6})$/i.exec(value)?.[1];
  if (!hex) return null;
  const channel = (offset: number) => Number.parseInt(hex.slice(offset, offset + 2), 16) / 255;
  return { r: channel(0), g: channel(2), b: channel(4), a: TINT_ALPHA };
}

function matching(root: HTMLElement, selector: string): HTMLElement[] {
  return [
    ...(root.matches(selector) ? [root] : []),
    ...root.querySelectorAll<HTMLElement>(selector),
  ];
}

function measure(root: HTMLElement, targets: GlassTargets): GlassRegion[] {
  const regions: GlassRegion[] = [];
  for (const [selector, anchor] of targets) {
    for (const target of matching(root, selector)) {
      const rect = target.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) continue;
      const corner = Number.parseFloat(getComputedStyle(target).borderTopLeftRadius) || 0;
      regions.push({
        x: rect.left,
        y: rect.top,
        width: rect.width,
        height: rect.height,
        radius: Math.min(corner, rect.width / 2, rect.height / 2),
        anchor,
      });
    }
  }
  return regions;
}

/**
 * Keep the native glass under the page's transparent chrome: report the
 * rectangles whenever they move, and clear them when the board unmounts.
 * `layoutKey` changes when the set of chrome elements changes.
 */
export function useGlassRegions(
  client: AgendarClient | null,
  rootRef: RefObject<HTMLElement | null>,
  active: boolean,
  layoutKey: string,
  targets: GlassTargets = SHELL_GLASS,
  tintToken = "--side",
): void {
  useEffect(() => {
    const root = rootRef.current;
    if (!active || !root || !client) return;
    let frame = 0;
    let last = "";
    const send = () => {
      frame = 0;
      const regions = measure(root, targets);
      const glassTint = tint(root, tintToken);
      const key = JSON.stringify([regions, glassTint]);
      if (key === last) return;
      last = key;
      void client.setGlassRegions(regions, glassTint).catch(() => undefined);
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(send);
    };
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    observer?.observe(root);
    for (const [selector] of targets) {
      matching(root, selector).forEach((target) => observer?.observe(target));
    }
    window.addEventListener("resize", schedule);
    // A theme change recolours the tint.
    const themes = new MutationObserver(schedule);
    themes.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const scheme = window.matchMedia?.("(prefers-color-scheme: dark)");
    scheme?.addEventListener?.("change", schedule);
    schedule();
    return () => {
      observer?.disconnect();
      themes.disconnect();
      scheme?.removeEventListener?.("change", schedule);
      window.removeEventListener("resize", schedule);
      if (frame) cancelAnimationFrame(frame);
      void client.setGlassRegions([], null).catch(() => undefined);
    };
  }, [client, rootRef, active, layoutKey, targets, tintToken]);
}

/** Whether this window is the key window (focus follows the native window). */
export function useWindowActive(): boolean {
  const [active, setActive] = useState(() =>
    typeof document === "undefined" ? true : document.hasFocus(),
  );
  useEffect(() => {
    const on = () => setActive(true);
    const off = () => setActive(false);
    window.addEventListener("focus", on);
    window.addEventListener("blur", off);
    return () => {
      window.removeEventListener("focus", on);
      window.removeEventListener("blur", off);
    };
  }, []);
  return active;
}

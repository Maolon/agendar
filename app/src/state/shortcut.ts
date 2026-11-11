/**
 * Keyboard shortcut labels for the platform the app runs on. The handlers
 * accept Meta or Ctrl everywhere; only the hint differs: macOS shows glyphs
 * (⌘K), Windows and Linux spell the key out (Ctrl+K).
 */

export type Shortcut =
  | "mod+k"
  | "mod+enter"
  | "enter"
  | "mod+\\"
  | "mod+,"
  | "m"
  | "h"
  | "mod+="
  | "mod+-"
  | "mod+0"
  | "mod+1"
  | "mod+z"
  | "shift+mod+z"
  | "delete";

const MAC: Record<Shortcut, string> = {
  "mod+k": "⌘K",
  "mod+enter": "⌘↵",
  enter: "↵",
  "mod+\\": "⌘\\",
  "mod+,": "⌘,",
  m: "M",
  h: "H",
  "mod+=": "⌘+",
  "mod+-": "⌘−",
  "mod+0": "⌘0",
  "mod+1": "⌘1",
  "mod+z": "⌘Z",
  "shift+mod+z": "⇧⌘Z",
  delete: "⌫",
};

const OTHER: Record<Shortcut, string> = {
  "mod+k": "Ctrl+K",
  "mod+enter": "Ctrl+Enter",
  enter: "Enter",
  "mod+\\": "Ctrl+\\",
  "mod+,": "Ctrl+,",
  m: "M",
  h: "H",
  "mod+=": "Ctrl++",
  "mod+-": "Ctrl+−",
  "mod+0": "Ctrl+0",
  "mod+1": "Ctrl+1",
  "mod+z": "Ctrl+Z",
  "shift+mod+z": "Ctrl+Shift+Z",
  delete: "Delete",
};

interface NavigatorLike {
  platform?: string;
  userAgent?: string;
  userAgentData?: { platform?: string };
}

/** macOS and iOS read as mac; everything else gets the Ctrl spelling. */
export function detectMac(nav: NavigatorLike | undefined = globalThis.navigator): boolean {
  if (!nav) return false;
  const platform = nav.userAgentData?.platform || nav.platform || nav.userAgent || "";
  return /mac|iphone|ipad|ipod|ios/i.test(platform);
}

let cached: boolean | undefined;

/** The platform, detected once per window. */
export function isMac(): boolean {
  cached ??= detectMac();
  return cached;
}

/** Tests only: detect again on the next call. */
export function resetShortcutPlatform(): void {
  cached = undefined;
}

export function shortcut(key: Shortcut, mac: boolean = isMac()): string {
  return (mac ? MAC : OTHER)[key];
}

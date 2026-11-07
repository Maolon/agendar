import { useEffect, useRef } from "react";
import type { Selection } from "../state/ui";

export interface KeyboardHandlers {
  /** Visible needs then project rows, in shown order. */
  rows: Selection[];
  selection: Selection | null;
  select: (selection: Selection | null) => void;
  /** Number of options on the active decision card, 0 elsewhere. */
  optionCount: number;
  selectOption: (index: number) => void;
  /** Plain Enter may submit only the easy current recommendation. */
  plainEnterEnabled: boolean;
  submit: (source: "enter" | "mod-enter" | "pointer") => void;
  escape: () => void;
  focusSearch: () => void;
  canOpenReport: boolean;
  openReport: () => void;
  scrollRowIntoView: (selection: Selection | null) => void;
  /** ⌘, / Ctrl+, opens Settings. */
  openSettings?: () => void;
}

function isModifier(event: KeyboardEvent): boolean {
  // Meta on macOS, Ctrl elsewhere; accepting both keeps the guard identical
  // across the two native webviews and the jsdom tests.
  return event.metaKey || event.ctrlKey;
}

function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

function isActivatable(target: EventTarget | null): boolean {
  // A focused button/link handles its own Enter; the window policy must not
  // activate it a second time.
  if (!(target instanceof HTMLElement)) return false;
  if (target.tagName === "BUTTON") return true;
  return target.tagName === "A" && target.hasAttribute("href");
}

/**
 * One window-level key handler per mounted view tree.
 *
 * Ignores IME composition, key repeats for submission and editable fields
 * except Cmd/Ctrl-K. Never swallows Enter while the user is typing a report,
 * search or send-back reason; those forms submit through their own form.
 */
export function handleKeyDown(event: KeyboardEvent, handlers: KeyboardHandlers): void {
  if (event.isComposing || event.key === "Process") return;
  const editable = isEditable(event.target);

  if (isModifier(event) && event.key.toLowerCase() === "k") {
    event.preventDefault();
    handlers.focusSearch();
    return;
  }

  if (isModifier(event) && event.key === "," && handlers.openSettings) {
    event.preventDefault();
    handlers.openSettings();
    return;
  }

  if (event.key === "Escape") {
    if (event.defaultPrevented) return;
    if (editable) {
      (event.target as HTMLElement).blur();
      return;
    }
    handlers.escape();
    return;
  }

  if (editable) return;
  if (event.defaultPrevented) return;

  if (isModifier(event) && event.key === "Enter") {
    if (event.repeat) return;
    event.preventDefault();
    handlers.submit("mod-enter");
    return;
  }

  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    const rows = handlers.rows;
    if (!rows.length) return;
    event.preventDefault();
    const index = handlers.selection
      ? rows.findIndex(
          (row) =>
            row.kind === handlers.selection?.kind && row.id === handlers.selection?.id,
        )
      : -1;
    const nextIndex =
      event.key === "ArrowDown"
        ? Math.min(index + 1, rows.length - 1)
        : Math.max(index - 1, 0);
    const next = rows[nextIndex];
    if (!next) return;
    handlers.select(next);
    handlers.scrollRowIntoView(next);
    return;
  }

  if (/^[1-4]$/.test(event.key)) {
    const index = Number(event.key) - 1;
    if (index < handlers.optionCount) {
      event.preventDefault();
      handlers.selectOption(index);
    }
    return;
  }

  if (event.key === "Enter") {
    if (event.repeat) return;
    if (isActivatable(event.target)) return;
    if (!handlers.plainEnterEnabled) return;
    event.preventDefault();
    handlers.submit("enter");
    return;
  }

  if (event.key === " " || event.code === "Space") {
    if (!handlers.canOpenReport) return;
    event.preventDefault();
    handlers.openReport();
  }
}

/**
 * Install the one keyboard policy for this surface. Passing `null` installs
 * nothing (the popover surface has its own policy instance).
 */
export function useKeyboard(handlers: KeyboardHandlers | null): void {
  const latest = useRef(handlers);
  latest.current = handlers;

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent): void {
      const current = latest.current;
      if (!current) return;
      handleKeyDown(event, current);
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);
}

import { createContext, useContext } from "react";
import type { BriefData, BriefSection, NeedsYouItem, Snapshot } from "../api/types";

/**
 * Snooze is a local UI map: an item hides from Needs you,
 * the tray and notifications until its time, with no Vault event and no change
 * to gates or ranking.
 */
export type SnoozeChoice = "hour" | "tomorrow" | "week";

export const SNOOZE_CHOICES: readonly SnoozeChoice[] = ["hour", "tomorrow", "week"];

/** The time an item is snoozed until, or null when it is not snoozed now. */
export function snoozedUntil(snapshot: Snapshot, id: string): string | null {
  const until = snapshot.preferences.snoozed?.[id];
  if (!until) return null;
  return Date.parse(until) > Date.parse(snapshot.state.evaluated_at) ? until : null;
}

/** A hand-off item the user closed for good on this computer. */
export function isDismissed(snapshot: Snapshot, id: string): boolean {
  return snapshot.preferences.dismissed?.includes(id) ?? false;
}

/** Hidden from Needs you: snoozed for now, or closed for good. */
export function isHidden(snapshot: Snapshot, id: string): boolean {
  return isDismissed(snapshot, id) || snoozedUntil(snapshot, id) !== null;
}

export function activeNeeds(snapshot: Snapshot): NeedsYouItem[] {
  return snapshot.needs.filter((item) => !isHidden(snapshot, item.id));
}

export function snoozedNeeds(snapshot: Snapshot): NeedsYouItem[] {
  return snapshot.needs.filter(
    (item) => !isDismissed(snapshot, item.id) && snoozedUntil(snapshot, item.id) !== null,
  );
}

/** The brief without rows that point at a snoozed or closed item. */
export function briefWithoutSnoozed(snapshot: Snapshot): BriefData {
  const entries = Object.entries(snapshot.brief) as [BriefSection, BriefData[BriefSection]][];
  return Object.fromEntries(
    entries.map(([key, rows]) => [
      key,
      rows.filter((row) => !(row.target.kind === "need" && isHidden(snapshot, row.target.id))),
    ]),
  ) as BriefData;
}

/** End of a snooze: an hour, tomorrow at 09:00 local, or just under the host's 7-day cap. */
export function snoozeEnd(choice: SnoozeChoice, now: Date = new Date()): string {
  if (choice === "hour") return new Date(now.getTime() + 60 * 60 * 1000).toISOString();
  if (choice === "week") return new Date(now.getTime() + 7 * 24 * 60 * 60 * 1000 - 60 * 1000).toISOString();
  const next = new Date(now);
  next.setDate(next.getDate() + 1);
  next.setHours(9, 0, 0, 0);
  return next.toISOString();
}

export interface SnoozeApi {
  until: (id: string) => string | null;
  /** The item whose snooze choices are open (UI state `snooze_dialog`). */
  open: string | null;
  setOpen: (id: string | null) => void;
  snooze: (id: string, choice: SnoozeChoice) => Promise<void>;
  restore: (id: string) => Promise<void>;
  dismissed: (id: string) => boolean;
  /** Close a hand-off item for good on this computer; the step is untouched. */
  dismiss: (id: string) => Promise<void>;
}

/** Provided by the main window only; without it no snooze control renders. */
export const SnoozeContext = createContext<SnoozeApi | null>(null);

export function useSnooze(): SnoozeApi | null {
  return useContext(SnoozeContext);
}

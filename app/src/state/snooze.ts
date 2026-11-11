import { createContext, useContext } from "react";
import type { BriefData, BriefSection, NeedsYouItem, Snapshot } from "../api/types";
import { isActiveNeed, isFoldedNeed } from "./claim";

/**
 * A snooze is the owner's `need.snoozed` event in the Vault: the item hides
 * from Needs you, the tray and notifications until its time, on every device,
 * with no change to gates or ranking. The reduced state carries it on the row
 * (`snoozed_until`); nothing about it is stored locally.
 */
export type SnoozeChoice = "hour" | "tomorrow" | "week";

export const SNOOZE_CHOICES: readonly SnoozeChoice[] = ["hour", "tomorrow", "week"];

/** The time an item is snoozed until, or null when it is not snoozed now. */
export function snoozedUntil(snapshot: Snapshot, id: string): string | null {
  const until = snapshot.needs.find((item) => item.id === id)?.snoozed_until;
  if (!until) return null;
  return Date.parse(until) > Date.parse(snapshot.state.evaluated_at) ? until : null;
}

/** A rescue row the user closed for good on this computer. */
export function isDismissed(snapshot: Snapshot, id: string): boolean {
  return snapshot.preferences.dismissed?.includes(id) ?? false;
}

/** Hidden from Needs you: snoozed for now, or closed for good. */
export function isHidden(snapshot: Snapshot, id: string): boolean {
  return isDismissed(snapshot, id) || snoozedUntil(snapshot, id) !== null;
}

/**
 * Needs you now: not hidden, not an information row for a free step, and not
 * a question folded on a closed step.
 */
export function activeNeeds(snapshot: Snapshot): NeedsYouItem[] {
  return snapshot.needs.filter((item) => isActiveNeed(item) && !isHidden(snapshot, item.id));
}

export function snoozedNeeds(snapshot: Snapshot): NeedsYouItem[] {
  return snapshot.needs.filter(
    (item) => isActiveNeed(item) && !isDismissed(snapshot, item.id) && snoozedUntil(snapshot, item.id) !== null,
  );
}

/** Questions on closed steps, folded out of Needs you (still answerable). */
export function closedStepNeeds(snapshot: Snapshot): NeedsYouItem[] {
  return snapshot.needs.filter((item) => isFoldedNeed(item) && !isHidden(snapshot, item.id));
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

/** One choice of the snooze menu: when it ends, and whether the row's limit cut it. */
export interface SnoozeOffer {
  choice: SnoozeChoice;
  until: string;
  cut: boolean;
}

/**
 * The choices to offer for a row. A row with a default time returns at its
 * `snooze_limit` at the latest: the first choice that would run past it ends
 * at the limit instead, later ones are the same snooze and are left out, and
 * nothing is offered once the limit has passed (the host would refuse it).
 */
export function snoozeOffers(limit: string | undefined, now: Date = new Date()): SnoozeOffer[] {
  const offers: SnoozeOffer[] = [];
  for (const choice of SNOOZE_CHOICES) {
    const until = snoozeEnd(choice, now);
    if (!limit || Date.parse(until) <= Date.parse(limit)) {
      offers.push({ choice, until, cut: false });
      continue;
    }
    if (Date.parse(limit) > now.getTime()) offers.push({ choice, until: limit, cut: true });
    break;
  }
  return offers;
}

export interface SnoozeApi {
  until: (id: string) => string | null;
  /** What the menu offers for this item now; empty when it cannot be snoozed. */
  offers: (id: string) => SnoozeOffer[];
  /** The item whose snooze choices are open (UI state `snooze_dialog`). */
  open: string | null;
  setOpen: (id: string | null) => void;
  snooze: (id: string, offer: SnoozeOffer) => Promise<void>;
  restore: (id: string) => Promise<void>;
  dismissed: (id: string) => boolean;
  /** Close a rescue row for good on this computer; the step is untouched. */
  dismiss: (id: string) => Promise<void>;
}

/** Provided by the main window only; without it no snooze control renders. */
export const SnoozeContext = createContext<SnoozeApi | null>(null);

export function useSnooze(): SnoozeApi | null {
  return useContext(SnoozeContext);
}

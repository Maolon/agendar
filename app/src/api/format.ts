import { tx } from "../i18n";
import type {
  DecisionCard,
  DecisionOption,
  DecisionState,
  EtaRange,
  Guard,
  Reversibility,
  TimeRange,
} from "./types";

const RISK_ORDER: Record<Reversibility, number> = { easy: 0, costly: 1, irreversible: 2 };
const RISK_NAMES: Reversibility[] = ["easy", "costly", "irreversible"];

/** Length in Unicode scalar values, never UTF-16 units. */
export function scalarLength(text: string): number {
  return [...text].length;
}

export function trimmedScalarLength(text: string): number {
  return scalarLength(text.trim());
}

/**
 * Effective card risk as far as the client can know it: max of card risk and
 * every option, with external effects at the irreversible floor. The backend
 * rechecks heuristics and policy; this is only the pre-request guard.
 */
export function cardRisk(card: DecisionCard): Reversibility {
  let worst = RISK_ORDER[card.reversibility];
  for (const option of card.options) worst = Math.max(worst, RISK_ORDER[option.reversibility]);
  if (card.external) worst = Math.max(worst, RISK_ORDER.irreversible);
  return RISK_NAMES[worst] ?? "easy";
}

export function guardFor(risk: Reversibility): Guard {
  return risk === "easy" ? "easy" : risk === "costly" ? "choose" : "confirm";
}

export function optionById(card: DecisionCard, id: string | undefined): DecisionOption | undefined {
  if (!id) return undefined;
  return card.options.find((option) => option.id === id);
}

export function isRecommendation(card: DecisionCard, optionId: string | undefined): boolean {
  return optionId === card.recommended.option;
}

/**
 * Tray/popover eligibility: easy effective risk, confidence
 * med/high, still open and unanswered. Never promotes costly or irreversible.
 */
export function isEasyRecommendation(decision: DecisionState | undefined): boolean {
  if (!decision || decision.lifecycle !== "open") return false;
  if (decision.superseded_by || decision.withdrawn_by || decision.effective_receipt) return false;
  const confidence = decision.card.recommended.confidence;
  if (confidence !== "med" && confidence !== "high") return false;
  return cardRisk(decision.card) === "easy";
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

function diffMs(from: string, now: string): number {
  const start = Date.parse(from);
  const end = Date.parse(now);
  if (Number.isNaN(start) || Number.isNaN(end)) return 0;
  return end - start;
}

/** Compact age label; negative or unreadable times never claim freshness. */
/** "just now", "5m ago", "2h ago": an age phrase that never reads "just now ago". */
export function formatAgo(from: string | undefined, now: string): string {
  const age = formatAge(from, now);
  if (!age || age === tx("time.just_now") || age.endsWith(tx("time.ago", { age: "" }).trim())) return age;
  return tx("time.ago", { age });
}

export function formatAge(from: string | undefined, now: string): string {
  if (!from) return "";
  const delta = Math.max(0, diffMs(from, now));
  if (delta < MINUTE) return tx("time.just_now");
  if (delta < HOUR) return tx("time.min", { n: Math.floor(delta / MINUTE) });
  if (delta < DAY) {
    const hours = Math.floor(delta / HOUR);
    const minutes = Math.floor((delta - hours * HOUR) / MINUTE);
    return minutes > 0 ? tx("time.hour_min", { h: hours, m: minutes }) : tx("time.hour", { n: hours });
  }
  const days = Math.floor(delta / DAY);
  if (days < 7) return tx("time.day", { n: days });
  return tx("time.week_ago", { n: Math.floor(days / 7) });
}

/** Remaining time until a deadline, e.g. "3h 22m". */
export function formatRemaining(until: string | undefined, now: string): string {
  if (!until) return "";
  const delta = diffMs(now, until);
  if (Number.isNaN(delta) || delta <= 0) return tx("time.due");
  const hours = Math.floor(delta / HOUR);
  const minutes = Math.floor((delta - hours * HOUR) / MINUTE);
  if (hours > 0) return tx("time.hour_min", { h: hours, m: minutes });
  const seconds = Math.max(1, Math.floor(delta / 1000));
  return minutes > 0 ? tx("time.min", { n: minutes }) : tx("time.sec", { n: seconds });
}

export function formatWaiting(minutes: number): string {
  if (minutes < 60) return tx("time.min", { n: minutes });
  const days = Math.floor(minutes / (60 * 24));
  if (days >= 1) return tx("time.day", { n: days });
  return tx("time.hour", { n: Math.floor(minutes / 60) });
}

function clock(date: Date): string {
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(date);
}

/** Local clock label for an absolute instant, e.g. "16:40". */
export function formatClockTime(value: string | undefined): string {
  if (!value) return "";
  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) return "";
  return clock(new Date(parsed));
}

/** "~16:40" or "~16:40–17:10" for an absolute finish window. */
export function formatFinishWindow(range: TimeRange | undefined): string {
  if (!range) return "";
  const earliest = formatClockTime(range.earliest);
  const latest = formatClockTime(range.latest);
  if (!earliest || !latest) return "";
  return earliest === latest ? `~${earliest}` : `~${earliest}–${latest}`;
}

/** Finish window from an ETA range anchored at a known projection time. */
export function formatEtaFinish(eta: EtaRange | undefined, anchor: string): string {
  if (!eta) return "";
  const base = Date.parse(anchor);
  if (Number.isNaN(base)) return "";
  const earliest = new Date(base + eta.min_s * 1000);
  const latest = new Date(base + eta.max_s * 1000);
  const earliestLabel = clock(earliest);
  const latestLabel = clock(latest);
  if (earliestLabel === latestLabel) return `~${earliestLabel}`;
  return `~${earliestLabel}–${latestLabel}`;
}

/** Minimal Crockford-base32 ULID for idempotent request IDs (JS-safe). */
export function newRequestId(): string {
  const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
  let remain = Date.now();
  const chars: string[] = new Array<string>(10);
  for (let index = 9; index >= 0; index -= 1) {
    chars[index] = alphabet[remain % 32] ?? "0";
    remain = Math.floor(remain / 32);
  }
  // A 48-bit millisecond timestamp never exceeds the ULID 10-char bound.
  chars[0] = alphabet[(chars[0] ? alphabet.indexOf(chars[0]) : 0) % 8] ?? "0";
  const bytes = new Uint8Array(16);
  if (globalThis.crypto?.getRandomValues) globalThis.crypto.getRandomValues(bytes);
  else
    for (let index = 0; index < bytes.length; index += 1)
      bytes[index] = Math.floor(Math.random() * 256);
  for (const byte of bytes) chars.push(alphabet[byte % 32] ?? "0");
  return chars.join("").slice(0, 26);
}

export function formatEtaDuration(eta: EtaRange | undefined): string {
  if (!eta) return "";
  const label = (seconds: number): string => {
    if (seconds >= 3600) {
      const hours = seconds / 3600;
      return `${Number.isInteger(hours) ? hours : hours.toFixed(1)}h`;
    }
    return `${Math.round(seconds / 60)}m`;
  };
  return `${label(eta.min_s)}–${label(eta.max_s)}`;
}

/** "20–45 min", "1.5–4h" or "about 3h" for an amount of work in ms. */
export function formatWorkRange([low, high]: [number, number]): string {
  const minutes = (ms: number) => Math.max(1, Math.round(ms / MINUTE));
  const hours = (ms: number) => Math.max(0.5, Math.round((ms / HOUR) * 2) / 2);
  if (high < HOUR) {
    const [a, b] = [minutes(low), minutes(high)];
    return a === b ? `about ${a} min` : `${a}–${b} min`;
  }
  const [a, b] = [hours(low), hours(high)];
  return a === b ? `about ${a}h` : `${a}–${b}h`;
}

/** Reserved option id for an answer in the human's own words. */
export const OWN_OPTION = "own";
export const OWN_ANSWER_MAX = 600;

/** The option a receipt shows for an answer in the human's own words. */
export function ownOption(card: DecisionCard, text: string): DecisionOption {
  return {
    id: OWN_OPTION,
    label: tx("fmt.own"),
    consequence: text,
    reversibility: card.reversibility,
    external: card.external,
    icon: "user",
  };
}

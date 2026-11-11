/**
 * The home page by state: what is moving (a live agent is on it), what is
 * stalled (claimed, nobody on it), what needs the human, what finished since
 * the last look and what comes next. Pure derivations from the snapshot; the
 * view only lays them out.
 */

import { currentLanguage, tx } from "../i18n";
import { formatClockTime } from "../api/format";
import type {
  FeatureState,
  NeedsYouItem,
  ProjectState,
  ProjectSummary,
  Snapshot,
  UtcTime,
} from "../api/types";
import { orderedProjects } from "../components/Sidebar";
import { demoGap, demoGapLine } from "./demoGap";
import { planSteps } from "./project";
import { activeNeeds, closedStepNeeds, isHidden } from "./snooze";
import { heldBy, hostQuietSince } from "./claim";
import { isMoving, isStalled } from "./moving";

export { isMoving, isStalled, sentBackUnattended } from "./moving";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** At most this many finished items are listed; the rest is "+N more". */
export const DONE_CAP = 6;

export interface EtaBar {
  /** Elapsed share of the window, 0..1 (1 when late). */
  share: number;
  /** Seconds past the window's latest finish; 0 when not late. */
  lateS: number;
  paused: boolean;
  earliest: UtcTime;
  latest: UtcTime;
}

export interface MovingStep {
  project: ProjectSummary;
  feature: FeatureState;
  harness: string | undefined;
  startedAt: UtcTime | undefined;
  bar: EtaBar | undefined;
  /** The agent's latest progress note for this attempt, when projected. */
  note: string | undefined;
  /** Held but quiet inside its ETA: the agent's last word, shown muted. */
  lastWord: UtcTime | undefined;
  /** The session's host has not been heard from for a day: sync may lag. */
  hostHeard: UtcTime | undefined;
}

/**
 * Why nobody is moving a claimed step: quiet / lost / none / paused (no claim
 * from the core), sent back with nobody on it, no word since its ETA passed
 * (stalled), or abandoned (the claim ended).
 */
export type StalledAgent = "quiet" | "lost" | "none" | "paused" | "sent_back" | "silent" | "abandoned";

export interface StalledStep {
  feature: FeatureState;
  agent: StalledAgent;
  /** silent: the last word; abandoned: when the claim ended. */
  since: UtcTime | undefined;
  /** Seconds late (ETA or deadline); 0 when not late. */
  lateS: number;
  /** A future deadline, when not late. */
  due: UtcTime | undefined;
  /** An active rescue need for this step, opened instead of the project. */
  needId: string | undefined;
}

export interface RiskChip {
  text: string;
  level: "red" | "amber";
  /** What is missing and what clears it, for a tooltip. */
  why?: string;
}

export interface StalledGroup {
  project: ProjectSummary;
  risks: RiskChip[];
  steps: StalledStep[];
}

export interface NeedRow {
  need: NeedsYouItem;
  project: ProjectSummary | undefined;
  /** Titles of the steps this need holds up. */
  holds: string[];
  /** When the need goes ahead on its own, and with what. */
  defaultAt: UtcTime | undefined;
  defaultLabel: string | undefined;
  /** Steps on hold because their agent waits for this answer. */
  waiting: string[];
}

export interface DoneItem {
  key: string;
  kind: "accepted" | "done";
  title: string;
  project: ProjectSummary;
  ts: UtcTime | undefined;
}

export interface NextItem {
  project: ProjectSummary;
  feature: FeatureState;
  more: number;
}

export interface NowBoard {
  needs: NeedRow[];
  /** Questions on closed steps, folded out of Needs you. */
  closed: NeedRow[];
  moving: MovingStep[];
  stalled: StalledGroup[];
  done: DoneItem[];
  next: NextItem[];
}

const FINISHED = new Set<FeatureState["progress"]>(["done", "verified", "dropped"]);

function ms(value: string | undefined): number {
  return value ? Date.parse(value) : Number.NaN;
}

export function etaBar(feature: FeatureState, now: UtcTime): EtaBar | undefined {
  const eta = feature.eta;
  if (!eta) return undefined;
  // The same window as everywhere else: history-adjusted when calibrated.
  const window = eta.likely_finish ?? eta.agent_finish;
  const start = ms(eta.anchor);
  const end = ms(window.latest);
  const at = ms(now);
  if ([start, end, at].some(Number.isNaN)) return undefined;
  const span = end - start;
  const share = span > 0 ? Math.max(0, Math.min(1, (at - start) / span)) : 1;
  return {
    share: eta.late_by_s > 0 ? 1 : share,
    lateS: Math.max(0, eta.late_by_s),
    paused: eta.paused || feature.paused,
    earliest: window.earliest,
    latest: window.latest,
  };
}

/** Coarse lateness: "2d", "13h", "25m". */
export function lateText(seconds: number): string {
  const delta = seconds * 1000;
  if (delta >= DAY) return tx("time.day", { n: Math.floor(delta / DAY) });
  if (delta >= HOUR) return tx("time.hour", { n: Math.floor(delta / HOUR) });
  return tx("time.min", { n: Math.max(1, Math.floor(delta / MINUTE)) });
}

/** A future time as the shortest local label: "16:00" today, "Wed" this week, else "12 Feb". */
export function dueText(due: UtcTime, now: UtcTime): string {
  const at = ms(due);
  const base = ms(now);
  if (Number.isNaN(at) || Number.isNaN(base)) return "";
  const date = new Date(at);
  const today = new Date(base);
  if (date.toDateString() === today.toDateString()) return formatClockTime(due);
  // The app's language, not the browser's: "周三" beside Chinese text, not "Wed".
  const locale = currentLanguage();
  if (at - base < 6 * DAY) return new Intl.DateTimeFormat(locale, { weekday: "short" }).format(date);
  return new Intl.DateTimeFormat(locale, { day: "numeric", month: "short" }).format(date);
}

/** "done 19:30–02:30" in local time. */
export function windowText(bar: EtaBar): string {
  const a = formatClockTime(bar.earliest);
  const b = formatClockTime(bar.latest);
  if (!a || !b) return "";
  return a === b ? tx("dash.done_at", { t: a }) : tx("dash.done_at", { t: `${a}–${b}` });
}

/** The latest pulse note of this attempt, from the projected activity feed. */
export function latestNote(project: ProjectState, feature: FeatureState): string | undefined {
  const since = ms(feature.eta?.anchor);
  for (const item of project.activity ?? []) {
    if (item.node !== feature.node || item.msg?.key !== "act.pulse") continue;
    // A note from an earlier attempt says nothing about this one.
    if (!Number.isNaN(since) && ms(item.ts) < since) return undefined;
    const message = item.msg.args?.message;
    return message?.t === "text" && message.v.trim() ? message.v.trim() : undefined;
  }
  return undefined;
}

function harnessOf(project: ProjectState, feature: FeatureState): string | undefined {
  return feature.session ? project.sessions[feature.session]?.harness : undefined;
}

function stalledAgent(project: ProjectState, feature: FeatureState): StalledAgent {
  if (feature.claim) {
    if (feature.phase === "sent_back") return "sent_back";
    if (feature.claim.state === "free") return "abandoned";
    if (feature.claim.state === "stalled") return "silent";
  }
  if (feature.liveness === "lost") return "lost";
  if (feature.phase === "sent_back") return "sent_back";
  if (feature.paused) return "paused";
  if (!feature.session || !project.sessions[feature.session]) return "none";
  return "quiet";
}

function stepLateness(feature: FeatureState, now: UtcTime): { lateS: number; due: UtcTime | undefined } {
  const etaLate = feature.eta?.late_by_s ?? 0;
  const deadline = ms(feature.deadline);
  const at = ms(now);
  const deadlineLate =
    !Number.isNaN(deadline) && !Number.isNaN(at) && at > deadline ? Math.floor((at - deadline) / 1000) : 0;
  const lateS = Math.max(etaLate, deadlineLate);
  return { lateS, due: lateS > 0 ? undefined : feature.deadline };
}

/** Speculation-budget pressure as chips: red at the limit, amber near it. */
export function riskChips(project: ProjectState): RiskChip[] {
  const usage = project.summary.budget;
  const budget = project.policy.value.budget;
  const chips: RiskChip[] = [];
  const level = (used: number, limit: number, over: boolean): RiskChip["level"] | null => {
    if (limit <= 0 || used <= 0) return null;
    if (over) return "red";
    return used * 5 >= limit * 4 ? "amber" : null;
  };
  const gap = level(usage.demo_gap_s, budget.max_demo_gap_s, usage.demo_gap_s >= budget.max_demo_gap_s);
  if (gap) {
    const full = demoGap(project);
    chips.push({
      level: gap,
      text: tx("dash.risk.demo_gap", { d: lateText(usage.demo_gap_s) }),
      ...(full ? { why: demoGapLine(full) } : {}),
    });
  }
  const guesses = level(
    usage.speculative_features,
    budget.max_speculative_features,
    usage.speculative_features > budget.max_speculative_features,
  );
  if (guesses) {
    chips.push({
      level: guesses,
      text: tx("dash.risk.guesses", { n: usage.speculative_features, limit: budget.max_speculative_features }),
    });
  }
  const slices = level(usage.pending_slices, budget.max_pending_slices, usage.pending_slices >= budget.max_pending_slices);
  if (slices) chips.push({ level: slices, text: tx("dash.risk.slices", { n: usage.pending_slices }) });
  return chips;
}

/** The first runnable planned step: open gate, not paused, dependencies finished. */
export function runnableSteps(project: ProjectState): FeatureState[] {
  const finished = (node: string) => {
    const dependency = project.features[node];
    return !dependency || dependency.progress === "done" || dependency.progress === "verified";
  };
  return planSteps(project.features)
    .map((step) => step.feature)
    .filter(
      (feature) =>
        feature.progress === "planned" &&
        feature.gate === "open" &&
        !feature.paused &&
        feature.liveness !== "lost" &&
        feature.depends_on.every(finished),
    );
}

/** Milliseconds encoded in a ULID event id (its first ten characters). */
export function ulidTime(id: string): number {
  const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
  if (id.length < 10) return Number.NaN;
  let value = 0;
  for (const char of id.slice(0, 10).toUpperCase()) {
    const digit = alphabet.indexOf(char);
    if (digit < 0) return Number.NaN;
    value = value * 32 + digit;
  }
  return value;
}

/** A plausible event time inside (since, now], or undefined. */
function eventTime(ids: (string | undefined)[], since: UtcTime | undefined, now: UtcTime): UtcTime | undefined {
  const low = ms(since);
  const high = ms(now) + MINUTE;
  for (const id of ids) {
    if (!id) continue;
    const at = ulidTime(id);
    if (Number.isNaN(at) || at > high || (!Number.isNaN(low) && at <= low)) continue;
    return new Date(at).toISOString();
  }
  return undefined;
}

function needRow(snapshot: Snapshot, need: NeedsYouItem): NeedRow {
  const project = snapshot.state.projects[need.project];
  const holds = need.unblocks
    .map((node) => project?.features[node])
    .filter((feature): feature is FeatureState => Boolean(feature) && !FINISHED.has(feature!.progress))
    .map((feature) => feature.title);
  const decision = project?.decisions[need.source];
  const pending = decision?.lifecycle === "open" && project?.policy.value.allow_timeouts !== false;
  const defaultAt = pending ? decision?.default_at : undefined;
  const defaultLabel = defaultAt
    ? decision?.card.options.find((option) => option.id === decision.card.recommended.option)?.label
    : undefined;
  const waiting = heldBy(project, need).map((feature) => feature.title);
  return { need, project: project?.summary, holds, defaultAt, defaultLabel, waiting };
}

export function nowBoard(snapshot: Snapshot): NowBoard {
  const now = snapshot.state.evaluated_at;
  const since = snapshot.preferences.last_looked_at;
  const needs = activeNeeds(snapshot);
  const projects = orderedProjects(snapshot)
    .map((summary) => snapshot.state.projects[summary.slug])
    .filter((project): project is ProjectState => Boolean(project));

  const moving: MovingStep[] = [];
  const stalled: StalledGroup[] = [];
  const next: NextItem[] = [];
  for (const project of projects) {
    const features = planSteps(project.features).map((step) => step.feature);
    for (const feature of features.filter(isMoving)) {
      moving.push({
        project: project.summary,
        feature,
        harness: harnessOf(project, feature),
        startedAt: feature.eta?.anchor,
        bar: etaBar(feature, now),
        note: latestNote(project, feature),
        lastWord: feature.claim && feature.liveness !== "live" ? feature.claim.last_word : undefined,
        hostHeard: hostQuietSince(project, feature, now),
      });
    }
    const steps = features.filter(isStalled).map((feature): StalledStep => {
      // Free steps' rows are information, not in Needs you, but still open
      // Hand over / Pause from the step.
      const rescue = snapshot.needs.find(
        (need) =>
          need.kind === "rescue" &&
          need.project === project.summary.slug &&
          need.node === feature.node &&
          !isHidden(snapshot, need.id),
      );
      const agent = stalledAgent(project, feature);
      const since =
        agent === "abandoned" ? feature.claim?.since : agent === "silent" ? feature.claim?.last_word : undefined;
      return { feature, agent, since, ...stepLateness(feature, now), needId: rescue?.id };
    });
    if (steps.length) {
      // Latest first: the step most overdue leads its project.
      steps.sort((a, b) => b.lateS - a.lateS);
      stalled.push({ project: project.summary, risks: riskChips(project), steps });
    }
    const runnable = runnableSteps(project);
    if (runnable[0]) next.push({ project: project.summary, feature: runnable[0], more: runnable.length - 1 });
  }

  // Done since the last look: accepted deliveries (the core's watermark rows)
  // and steps the activity feed reports finished after the watermark.
  const done: DoneItem[] = [];
  const seen = new Set<string>();
  for (const row of snapshot.brief.delivered) {
    const project = row.target.kind === "project" ? snapshot.state.projects[row.target.id] : undefined;
    if (!project || project.summary.archived) continue;
    const delivery = row.source_ids.map((id) => project.deliveries[id]).find(Boolean);
    if (!delivery) continue;
    seen.add(`${project.summary.slug}/${delivery.node}`);
    done.push({
      key: row.id,
      kind: "accepted",
      title: delivery.name,
      project: project.summary,
      ts: eventTime([delivery.review_event, delivery.id], since, now),
    });
  }
  const low = ms(since);
  for (const project of projects) {
    for (const item of project.activity ?? []) {
      if (item.msg?.key !== "act.finished" || !item.node) continue;
      if (!Number.isNaN(low) && ms(item.ts) <= low) continue;
      const key = `${project.summary.slug}/${item.node}`;
      if (seen.has(key)) continue;
      seen.add(key);
      done.push({
        key: `done:${key}`,
        kind: "done",
        title: project.features[item.node]?.title ?? item.detail ?? item.node,
        project: project.summary,
        ts: item.ts,
      });
    }
  }
  done.sort((a, b) => (ms(b.ts) || 0) - (ms(a.ts) || 0));

  return {
    needs: needs.filter((need) => !snapshot.state.projects[need.project]?.summary.archived).map((need) => needRow(snapshot, need)),
    closed: closedStepNeeds(snapshot)
      .filter((need) => !snapshot.state.projects[need.project]?.summary.archived)
      .map((need) => needRow(snapshot, need)),
    moving,
    stalled,
    done,
    next,
  };
}

export function boardEmpty(board: NowBoard): boolean {
  return !board.needs.length && !board.moving.length && !board.stalled.length && !board.done.length && !board.next.length;
}

export function stalledCount(board: NowBoard): number {
  return board.stalled.reduce((total, group) => total + group.steps.length, 0);
}

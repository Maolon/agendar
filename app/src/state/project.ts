/**
 * Project-page derivations that the prototype drew directly from its sample
 * data: ordered plan steps, In-flight rows, waits-on/unblocks labels, ETA text,
 * calibration wording and the budget meter. Everything is derived from the
 * projected DTOs; nothing is invented when a field is missing.
 */

import { tx, type MessageKey } from "../i18n";
import { formatFinishWindow, formatWorkRange } from "../api/format";
import type {
  BudgetUsage,
  Calibration,
  FeatureState,
  NodePath,
  ProjectForecast,
  ProjectState,
  SessionState,
} from "../api/types";

/** Progress labels in the active language (read at render time). */
export const PROGRESS_LABEL: Record<FeatureState["progress"], string> = new Proxy(
  {} as Record<FeatureState["progress"], string>,
  { get: (_, progress: string) => tx(`progress.${progress}` as MessageKey) },
);

/**
 * Working means a live agent has a step in progress. A connected agent with
 * nothing in progress (a session idle at its prompt) is idle, not working.
 */
export function workingAgents(project: ProjectState): number {
  const sessions = new Set<string>();
  for (const feature of Object.values(project.features)) {
    if (feature.progress === "doing" && feature.liveness === "live" && feature.session) {
      sessions.add(feature.session);
    }
  }
  return sessions.size;
}

/** Connected agents with no step in progress. */
export function idleAgents(project: ProjectState): number {
  return Math.max(0, project.summary.active_agents - workingAgents(project));
}

/** "2 agents working", "1 agent idle", or null when no agent is connected. */
export function agentsLine(working: number, idle: number): string | null {
  if (working > 0) return working === 1 ? tx("agents.working.one") : tx("agents.working.other", { n: working });
  if (idle > 0) return idle === 1 ? tx("agents.idle.one") : tx("agents.idle.other", { n: idle });
  return null;
}

export function inFlight(feature: FeatureState): boolean {
  if (feature.progress === "doing" || feature.progress === "done") return true;
  if (feature.liveness === "lost") return true;
  return feature.progress === "planned" && feature.gate !== "open";
}

export interface PlanStep {
  feature: FeatureState;
  depth: number;
}

/**
 * Ordered plan steps:
 * upstream dependencies first, declaration order inside a depth, cycles kept
 * visible at depth 0 instead of recursing forever.
 */
export function planSteps(features: Record<NodePath, FeatureState>): PlanStep[] {
  const list = Object.values(features);
  const index = new Map(list.map((feature, position) => [feature.node, position]));
  const depths = new Map<string, number>();
  const visiting = new Set<string>();

  const depthOf = (node: string): number => {
    const known = depths.get(node);
    if (known !== undefined) return known;
    const feature = features[node];
    if (!feature) return 0;
    if (visiting.has(node)) return 0;
    visiting.add(node);
    let depth = 0;
    for (const dependency of feature.depends_on) {
      if (dependency === node || !features[dependency]) continue;
      depth = Math.max(depth, depthOf(dependency) + 1);
    }
    visiting.delete(node);
    depths.set(node, depth);
    return depth;
  };

  return list
    .map((feature) => ({ feature, depth: depthOf(feature.node) }))
    .sort((a, b) => {
      if (a.depth !== b.depth) return a.depth - b.depth;
      return (index.get(a.feature.node) ?? 0) - (index.get(b.feature.node) ?? 0);
    });
}

export function featureTitle(project: ProjectState, node: NodePath): string {
  return project.features[node]?.title ?? node;
}

/** A step's gate in the active language; an unknown gate shows as sent. */
export function gateLabel(gate: string): string {
  const key = `gate.${gate}`;
  return key === "gate.open" || key === "gate.awaiting_decision" || key === "gate.awaiting_upstream"
    ? tx(key)
    : gate.replace("_", " ");
}

/** `feature:x` / `decision:<id>` / `review:<id>` → human text; never a raw id. */
export function waitLabels(feature: FeatureState, project: ProjectState): string[] {
  return feature.waits_on.map((reference) => {
    const separator = reference.indexOf(":");
    const kind = separator > 0 ? reference.slice(0, separator) : reference;
    const id = separator > 0 ? reference.slice(separator + 1) : "";
    if (kind === "feature") return project.features[id]?.title ?? id;
    if (kind === "decision") {
      const ask = project.decisions[id];
      return ask ? tx("wait.your_call_q", { q: ask.card.question }) : tx("wait.your_call");
    }
    if (kind === "review") {
      const delivery = project.deliveries[id];
      return delivery ? tx("wait.review_of", { name: delivery.name }) : tx("wait.review");
    }
    return id || reference;
  });
}

export function unblockLabels(feature: FeatureState, project: ProjectState): string[] {
  return feature.unblocks.map((node) => project.features[node]?.title ?? node);
}

/** Who is on this step: the registered session's host, or an honest absence. */
export function whoLabel(
  feature: FeatureState,
  project: ProjectState,
): { host?: string; text: string } {
  const session: SessionState | undefined = feature.session
    ? project.sessions[feature.session]
    : undefined;
  if (session) return { host: session.host, text: `${session.host} · ${session.harness.replace("_", " ")}` };
  if (feature.progress === "done") return { text: tx("who.waiting") };
  return { text: tx("who.nobody") };
}

/** `1500` → `1.5`, `850` → `0.85`: the calibrated factor as displayed. */
export function ratioFactor(ratioMilli: number): string {
  return (ratioMilli / 1000).toFixed(2).replace(/0+$/, "").replace(/\.$/, "");
}

export interface EtaClaims {
  /** Primary label: calibrated finish, late text or pause text. */
  text: string;
  /** The agent's own claim, shown only when a projection changed it. */
  agent: string;
  /** The applied calibration factor (`1.5×`), straight from `ratio_milli`. */
  ratio: string;
  /** One accessible sentence combining all three, for a tooltip. */
  title: string;
}

/**
 * the calibrated finish is the headline, but the agent's original
 * claim and the applied ratio stay visible. Both come from this feature's
 * `EtaState`; nothing is derived from the project-wide calibration list.
 */
export function etaClaims(feature: FeatureState): EtaClaims {
  const text = etaText(feature);
  const eta = feature.eta;
  const empty = { text, agent: "", ratio: "", title: text };
  if (!eta) return empty;
  const likely = eta.likely_finish ? formatFinishWindow(eta.likely_finish) : "";
  const claimed = formatFinishWindow(eta.agent_finish);
  const agent = likely && claimed && claimed !== likely ? tx("eta.agent_says", { t: claimed }) : "";
  const ratio = eta.ratio_milli && likely ? `${ratioFactor(eta.ratio_milli)}×` : "";
  const title = [text, agent, ratio ? tx("eta.adjusted", { r: ratio }) : ""].filter(Boolean).join(" · ");
  return { text, agent, ratio, title };
}

/** Original or calibrated finish, late and paused states; never a progress bar. */
export function etaText(feature: FeatureState): string {
  const eta = feature.eta;
  if (feature.paused) return tx("eta.paused");
  if (!eta) return "";
  if (eta.paused) return tx("eta.on_hold");
  if (eta.late_by_s > 0) {
    const minutes = Math.round(eta.late_by_s / 60);
    const late =
      minutes >= 60
        ? tx("time.hour_min", { h: Math.floor(minutes / 60), m: minutes % 60 })
        : tx("time.min", { n: minutes });
    return tx("eta.late", { d: late });
  }
  const finish = formatFinishWindow(eta.likely_finish ?? eta.agent_finish);
  return finish ? tx("eta.at", { t: finish }) : "";
}

/**
 * Sidebar label: the earliest calibrated/original finish of work that is
 * actually running, otherwise "paused" when a step is paused, otherwise empty.
 */
export function projectEtaLabel(project: ProjectState): string {
  const features = Object.values(project.features);
  if (features.some((feature) => feature.paused)) {
    const running = features.some(
      (feature) => feature.progress === "doing" && !feature.paused && feature.eta && !feature.eta.paused,
    );
    if (!running) return tx("eta.paused");
  }
  let earliest: string | undefined;
  for (const feature of features) {
    if (feature.progress !== "doing" || feature.paused || !feature.eta || feature.eta.paused) {
      continue;
    }
    const range = feature.eta.likely_finish ?? feature.eta.agent_finish;
    if (!range) continue;
    if (!earliest || Date.parse(range.earliest) < Date.parse(earliest)) earliest = range.earliest;
  }
  if (earliest) return formatFinishWindow({ earliest, latest: earliest });
  const held = features.some((feature) => feature.eta?.paused);
  return held ? tx("eta.held") : "";
}

/**
 * The whole project's finish: a clock window while an agent works, otherwise
 * the work left. Falls back to the step-level paused/on-hold labels.
 */
export function projectFinishLabel(project: ProjectState): string {
  const forecast = project.stats?.forecast;
  if (forecast?.finish) return formatFinishWindow(forecast.finish);
  if (forecast) return tx("eta.left", { work: formatWorkRange(forecast.work_ms) });
  const fallback = projectEtaLabel(project);
  return fallback === tx("eta.paused") || fallback === tx("eta.held") ? fallback : "";
}

/** How the forecast was made, for a tooltip. */
export function forecastBasis(forecast: ProjectForecast): string {
  const open =
    forecast.open_steps === 1 ? tx("fb.open.one") : tx("fb.open.other", { n: forecast.open_steps });
  const split = forecast.agents > 1 ? tx("fb.split", { n: forecast.agents }) : "";
  const idle = forecast.agents === 0 ? tx("fb.idle") : "";
  return tx("fb.basis", {
    open,
    pace: formatWorkRange(forecast.step_ms),
    n: forecast.basis_steps,
    split,
    idle,
  });
}

/** Prototype wording with the projected calibration ratios. */
export function calibrationLine(calibrations: Calibration[]): string {
  const usable = calibrations.filter((item) => item.samples > 0 && item.ratio_milli > 0);
  if (!usable.length) {
    return tx("plan.no_calibration");
  }
  const main = usable.reduce((best, item) => (item.samples > best.samples ? item : best));
  const ratio = ratioFactor(main.ratio_milli);
  const harness = main.harness.replace("_", " ");
  return tx("plan.calibration", { ratio, n: main.samples, harness });
}

export interface BudgetMeter {
  used: number;
  limit: number;
  blocked: boolean;
  reasons: string[];
  label: string;
}

/** The ten-dot meter counts features built on unconfirmed choices. */
export function budgetMeter(project: ProjectState): BudgetMeter {
  const usage: BudgetUsage = project.summary.budget;
  const limit = project.policy.value.budget.max_speculative_features;
  const used = usage.speculative_features;
  const label = `${used} of ${limit}`;
  return { used, limit, blocked: usage.blocked, reasons: usage.reasons, label };
}

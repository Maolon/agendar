/**
 * Assumption accounting for the views.
 *
 * An assumption is a `decision.assumed` receipt that a feature's current
 * attempt still consumes and that no human answer has resolved. Confirm and
 * override are ordinary answers on the same decision: confirm picks the option
 * that was assumed, override picks another one and marks every feature built on
 * it for rework.
 */

import type {
  DecisionOption,
  DecisionState,
  NodePath,
  ProjectState,
  Receipt,
  UtcTime,
} from "../api/types";

export interface LiveAssumption {
  receipt: Receipt;
  decision: DecisionState;
  /** The option the agent proceeded with. */
  assumed: DecisionOption;
  /** Every other option, in card order. */
  alternatives: DecisionOption[];
  /** Features whose current attempt carries this assumption. */
  consumers: NodePath[];
  /** `built_on(d)` as published, deduped. */
  builtOn: NodePath[];
  assumedAt: UtcTime;
  ageSeconds: number;
}

const RESOLVED = new Set<DecisionState["lifecycle"]>([
  "answered",
  "acked",
  "withdrawn",
  "amended",
]);

/**
 * Assumptions the human can still confirm or override, one entry per assumed
 * receipt. Resolved decisions drop out even though their features keep the
 * historical receipt reference; a tainted feature has no Confirm left, it needs
 * a restart.
 */
export function liveAssumptions(project: ProjectState, anchor: UtcTime): LiveAssumption[] {
  const anchors = Date.parse(anchor);
  const byReceipt = new Map<string, LiveAssumption>();

  for (const feature of Object.values(project.features)) {
    for (const receiptId of feature.assumptions) {
      const receipt = project.receipts[receiptId];
      if (!receipt || receipt.source !== "assumed") continue;
      const decision = project.decisions[receipt.ask];
      if (!decision || RESOLVED.has(decision.lifecycle)) continue;
      const existing = byReceipt.get(receiptId);
      if (existing) {
        if (!existing.consumers.includes(feature.node)) existing.consumers.push(feature.node);
        continue;
      }
      const assumedAt = decision.assumed_at ?? receipt.created_at;
      const assumedMs = Date.parse(assumedAt);
      byReceipt.set(receiptId, {
        receipt,
        decision,
        assumed: receipt.chose,
        alternatives: decision.card.options.filter((option) => option.id !== receipt.chose.id),
        consumers: [feature.node],
        builtOn: [...new Set(decision.built_on)].sort(),
        assumedAt,
        ageSeconds:
          Number.isFinite(assumedMs) && Number.isFinite(anchors)
            ? Math.max(0, Math.round((anchors - assumedMs) / 1000))
            : 0,
      });
    }
  }

  return [...byReceipt.values()].sort(
    (a, b) => Date.parse(a.assumedAt) - Date.parse(b.assumedAt) || a.receipt.id.localeCompare(b.receipt.id),
  );
}

/** Every feature an override with `optionId` would mark for rework. */
export function reworkTargets(assumption: LiveAssumption): NodePath[] {
  const union = new Set<NodePath>([...assumption.builtOn, ...assumption.consumers]);
  return [...union].sort();
}

export function featureTitles(project: ProjectState, nodes: readonly NodePath[]): string[] {
  return nodes.map((node) => project.features[node]?.title ?? node);
}

/** `2h`, `1d 3h` — compact age for the assumption strip. */
export function ageLabel(seconds: number): string {
  if (seconds < 60) return `${Math.max(1, Math.round(seconds))}s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (hours < 24) return rest ? `${hours}h ${rest}m` : `${hours}h`;
  const days = Math.floor(hours / 24);
  const rem = hours % 24;
  return rem ? `${days}d ${rem}h` : `${days}d`;
}

/**
 * Resolve a projected `BriefRow` to the entity its target names. The row text
 * is authored by the backend projection; the view only adds the small visuals
 * the prototype drew beside it, and every visual is derived from projected
 * fields (no fixture-only paths).
 */

import type {
  DecisionState,
  NeedsYouItem,
  OnTimeout,
  ProjectState,
  Snapshot,
  UtcTime,
  BriefRow,
} from "../api/types";
import { budgetMeter, type BudgetMeter } from "./project";

export function rowNeed(snapshot: Snapshot, row: BriefRow): NeedsYouItem | undefined {
  if (row.target.kind !== "need") return undefined;
  return snapshot.needs.find((item) => item.id === row.target.id);
}

export function rowProject(snapshot: Snapshot, row: BriefRow): ProjectState | undefined {
  if (row.target.kind === "project") return snapshot.state.projects[row.target.id];
  const need = rowNeed(snapshot, row);
  return need ? snapshot.state.projects[need.project] : undefined;
}

export function rowDecision(snapshot: Snapshot, row: BriefRow): DecisionState | undefined {
  const need = rowNeed(snapshot, row);
  if (need) return snapshot.state.projects[need.project]?.decisions[need.source];
  const project = rowProject(snapshot, row);
  if (!project) return undefined;
  return Object.values(project.decisions).find((decision) => decision.default_at !== undefined);
}

/** The delivery behind a Delivered row, when one is still projected. */
export function rowDelivery(
  snapshot: Snapshot,
  row: BriefRow,
): { project: ProjectState; delivery: ProjectState["deliveries"][string] } | undefined {
  const project = rowProject(snapshot, row);
  if (!project) return undefined;
  // The projection lists the delivery (and its review) in source_ids; prefer
  // that exact delivery over whichever one happens to be projected first.
  for (const source of row.source_ids) {
    const delivery = project.deliveries[source];
    if (delivery) return { project, delivery };
  }
  const need = rowNeed(snapshot, row);
  if (need) {
    const delivery = project.deliveries[need.source];
    if (delivery) return { project, delivery };
  }
  const delivery = Object.values(project.deliveries).find((item) => !item.superseded_by);
  return delivery ? { project, delivery } : undefined;
}

export function waitingAgents(snapshot: Snapshot, row: BriefRow): number {
  const need = rowNeed(snapshot, row);
  if (need) return need.stalled_agents;
  const project = rowProject(snapshot, row);
  if (!project) return 0;
  return project.summary.stalled_agents;
}

export interface DefaultRing {
  since: UtcTime;
  defaultAt: UtcTime;
  onTimeout: OnTimeout;
  chosenLabel: string | undefined;
}

/** The earliest pending default a "By default" row can point at. */
export function defaultRing(snapshot: Snapshot, row: BriefRow): DefaultRing | undefined {
  const need = rowNeed(snapshot, row);
  if (need) {
    const decision = snapshot.state.projects[need.project]?.decisions[need.source];
    if (!decision?.default_at) return undefined;
    return {
      since: need.since,
      defaultAt: decision.default_at,
      onTimeout: decision.card.on_timeout,
      chosenLabel: decision.card.options.find(
        (option) => option.id === decision.card.recommended.option,
      )?.label,
    };
  }
  const project = rowProject(snapshot, row);
  if (!project) return undefined;
  const pending = Object.values(project.decisions)
    .filter((decision) => decision.default_at !== undefined)
    .sort((a, b) => Date.parse(a.default_at ?? "") - Date.parse(b.default_at ?? ""));
  const decision = pending[0];
  if (!decision?.default_at) return undefined;
  const since = project.needs_you.find((item) => item.source === decision.id)?.since;
  return {
    since: since ?? decision.default_at,
    defaultAt: decision.default_at,
    onTimeout: decision.card.on_timeout,
    chosenLabel: decision.card.options.find(
      (option) => option.id === decision.card.recommended.option,
    )?.label,
  };
}

export function rowBudget(snapshot: Snapshot, row: BriefRow): BudgetMeter | undefined {
  const project = rowProject(snapshot, row);
  if (!project) return undefined;
  const meter = budgetMeter(project);
  // A late-only risk row is not a budget row: no empty meter beside it.
  return meter.blocked || meter.used > 0 ? meter : undefined;
}

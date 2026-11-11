/**
 * The claim and the correction record, read for display. The core derives
 * both (held / stalled / free, and why the board shows something other than
 * the agent's last word); the views only choose words and weights from them.
 */

import { tx, type MessageKey } from "../i18n";
import type {
  Correction,
  CorrectionRule,
  DeliveryState,
  FeatureState,
  NeedsYouItem,
  ProjectState,
  UtcTime,
} from "../api/types";

const DAY = 24 * 60 * 60_000;

/**
 * A row the board raised for a free step (its claim ended: the session
 * stopped, or no word for a day). Its own conversation takes the step back by
 * itself and any other agent may start it, so the row is information: it
 * stays openable (its details, Pause) from the step and the corrections list,
 * but it is not in the active Needs-you list and offers no hand-off. Mirrors
 * the core's `is_correction_row`.
 */
export function isInfoNeed(need: NeedsYouItem): boolean {
  return (
    need.kind === "rescue" &&
    (need.id.startsWith("rescue:lost:") || need.id.startsWith("rescue:lost-step:") || need.id.startsWith("rescue:silent:"))
  );
}

/**
 * A row that asks who continues a step (two agents started it, or a takeover
 * missed the earlier agent's last word): the one place the owner still lets
 * another agent continue. A free step's row and a plan-repair row do not.
 */
export function asksWhoContinues(need: NeedsYouItem): boolean {
  return need.kind === "rescue" && !isInfoNeed(need) && !need.id.startsWith("rescue:cycle:");
}

/**
 * A question on a step closed (dropped or accepted) a day ago: folded under
 * "On closed steps", out of the active list. Mirrors the core's `is_folded`.
 */
export function isFoldedNeed(need: NeedsYouItem): boolean {
  return need.closed_step?.folded === true;
}

/** A row of the active Needs-you list: not information, not folded. */
export function isActiveNeed(need: NeedsYouItem): boolean {
  return !isInfoNeed(need) && !isFoldedNeed(need);
}

function ms(value: string | undefined): number {
  return value ? Date.parse(value) : Number.NaN;
}

const REASON: Record<CorrectionRule, MessageKey> = {
  accepted: "corr.accepted",
  sent_back: "corr.sent_back",
  lost: "corr.lost",
  silent: "corr.silent",
  closed_after_accept: "corr.closed_after_accept",
};

/** "closed: you accepted", "freed: no word for 24h", … */
export function correctionReason(correction: Correction): string {
  return tx(REASON[correction.rule]);
}

/**
 * How much weight a step's status carries: solid for an owner fact or a fresh
 * self-report, muted for an ageing self-report, marked for a correction.
 */
export type Weight = "solid" | "muted" | "marked";

export function statusWeight(feature: FeatureState): Weight {
  if (feature.correction) return "marked";
  const claim = feature.claim;
  if (claim && claim.state !== "held") return "muted";
  if (claim && claim.state === "held" && !claim.waiting_on && feature.liveness !== "live") return "muted";
  return "solid";
}

/** Steps held because their session waits on this question. */
export function heldBy(project: ProjectState | undefined, need: NeedsYouItem): FeatureState[] {
  if (!project || need.kind !== "decide") return [];
  return Object.values(project.features).filter(
    (feature) => feature.claim?.state === "held" && feature.claim.waiting_on === need.source && feature.node !== need.node,
  );
}

/** The host of a held step's session, when it has not been heard from for a day. */
export function hostQuietSince(project: ProjectState, feature: FeatureState, now: UtcTime): UtcTime | undefined {
  if (feature.claim?.state !== "held" || !feature.session) return undefined;
  const host = project.sessions[feature.session]?.host;
  const heard = host ? project.summary.host_as_of[host] : undefined;
  if (!heard) return undefined;
  return ms(now) - ms(heard) >= DAY ? heard : undefined;
}

export interface CorrectedStep {
  feature: FeatureState;
  correction: Correction;
  /** The step's information row (why it is free; Pause), when the claim ended. */
  details: NeedsYouItem | undefined;
  /** The accepted delivery the owner can send back, when the accept closed it. */
  sendBack: DeliveryState | undefined;
}

function acceptedDelivery(project: ProjectState, feature: FeatureState): DeliveryState | undefined {
  return Object.values(project.deliveries)
    .filter(
      (delivery) =>
        delivery.attempt === feature.attempt && !delivery.superseded_by && delivery.review === "accepted",
    )
    .sort((a, b) => b.id.localeCompare(a.id))[0];
}

/** Every corrected step of a project, newest correction first. */
export function correctedSteps(project: ProjectState, needs: NeedsYouItem[]): CorrectedStep[] {
  return Object.values(project.features)
    .filter((feature): feature is FeatureState & { correction: Correction } => Boolean(feature.correction))
    .map((feature) => ({
      feature,
      correction: feature.correction,
      details: needs.find(
        (need) => need.kind === "rescue" && need.project === project.summary.slug && need.node === feature.node,
      ),
      sendBack:
        feature.correction.rule === "accepted" || feature.correction.rule === "closed_after_accept"
          ? acceptedDelivery(project, feature)
          : undefined,
    }))
    .sort((a, b) => ms(b.correction.since) - ms(a.correction.since));
}

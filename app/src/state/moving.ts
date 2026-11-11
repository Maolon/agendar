/**
 * The desktop's one statement of which step is moving and which is stalled.
 * Now's lists (`state/now.ts`) and the agent counts of the tray and the
 * sidebar (`state/project.ts`) both read it.
 */

import type { FeatureState } from "../api/types";

const FINISHED = new Set<FeatureState["progress"]>(["done", "verified", "dropped"]);

/**
 * A step the owner sent back that nobody is on: its agent is not live, or its
 * claim is no longer held. "Quiet inside its ETA" does not hold a sent-back
 * step: that ETA was the estimate of the delivery the owner already answered.
 * A step held because its agent waits on a question sits under that question.
 * The same predicate as the core's `reduce::motion::sent_back_unattended`,
 * which a mobile client's Stalled rows come from.
 */
export function sentBackUnattended(feature: FeatureState): boolean {
  if (feature.progress !== "doing" || feature.phase !== "sent_back") return false;
  const claim = feature.claim;
  if (claim?.waiting_on) return false;
  return feature.liveness !== "live" || (claim !== undefined && claim.state !== "held");
}

/**
 * Moving: in progress and held by its agent (live, or quiet inside its ETA).
 * A step in review is the owner's: its Needs-you review row covers it. A
 * finished step (accepted) is not moving. A sent-back step is moving only
 * while its agent is on it. A step held because its agent waits on its own
 * question sits under that question. Without a claim from the core, a live
 * agent decides.
 */
export function isMoving(feature: FeatureState): boolean {
  if (feature.progress !== "doing" || feature.phase === "in_review") return false;
  if (sentBackUnattended(feature)) return false;
  const claim = feature.claim;
  if (!claim) return feature.liveness === "live";
  return claim.state === "held" && !claim.waiting_on;
}

/** Stalled: claimed but nobody on it (past its ETA with no word, abandoned, sent back, paused, or stopped mid-way). */
export function isStalled(feature: FeatureState): boolean {
  if (feature.phase === "in_review") return false;
  if (sentBackUnattended(feature)) return true;
  if (feature.progress === "doing") {
    const claim = feature.claim;
    if (claim) return claim.state !== "held";
    return feature.liveness !== "live";
  }
  return feature.liveness === "lost" && !FINISHED.has(feature.progress);
}

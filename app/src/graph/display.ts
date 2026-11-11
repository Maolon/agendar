/**
 * What one graph node says.
 *
 * Display precedence is: lost → stopped glyph; tainted → rework label; pending
 * human need → amber; verified → green; doing with an open gate and a firm
 * basis → radar; otherwise a static icon. A decision/review node is amber only
 * while the backend publishes a `need_id` for it and its own `state` is still
 * outstanding: a defaulted assumption or an accepted review is history and
 * stays static. The four axes stay readable in the tooltip even though one node
 * shows one line.
 */

import { correctionReason } from "../state/claim";
import { tx } from "../i18n";
import type { FeatureState, GraphNode } from "../api/types";
import type { IconName } from "../components/Icon";
import { PROGRESS_LABEL, etaText } from "../state/project";

export interface NodeDisplay {
  label: string;
  sub: string;
  /** CSS class on the state dot: verified · doing · blocked · todo · stopped · you… */
  dot: string;
  icon?: IconName;
  /** A pending human decision or review: amber, and it opens a card. */
  you: boolean;
  assumed: boolean;
  title: string;
  /** Why the board corrected this step's status, when it did. */
  corrected?: string;
}

/** Decision states that still wait for a human answer. */
const OUTSTANDING_DECISION = new Set(["open", "defaulted", "pending"]);
/** Review states that still wait for a human verdict. */
const OUTSTANDING_REVIEW = new Set(["pending"]);

function decisionText(state: string, actionable: boolean): string {
  switch (state) {
    case "defaulted":
      return actionable ? tx("gd.confirm") : tx("gd.assumed");
    case "answered":
      return tx("gd.answered");
    case "acked":
      return tx("gd.acked");
    case "withdrawn":
      return tx("gd.withdrawn");
    case "amended":
      return tx("gd.amended");
    case "open":
    case "pending":
      return actionable ? tx("gd.your_call") : tx("gd.waiting_call");
    default:
      return actionable ? tx("gd.your_call") : state;
  }
}

function reviewText(state: string, actionable: boolean): string {
  switch (state) {
    case "accepted":
      return tx("dl.rev.accepted");
    case "sent_back":
      return tx("dl.rev.sent_back");
    case "pending":
      return actionable ? tx("gd.ready") : tx("gd.waiting_review");
    default:
      return actionable ? tx("gd.ready") : state;
  }
}

/**
 * A decision or review node. Amber means the row is yours right now, which the
 * backend expresses as a `need_id` plus an outstanding state; everything else
 * (a defaulted assumption, an accepted review) is rendered as static history.
 */
function virtualDisplay(node: GraphNode): NodeDisplay {
  const decision = node.kind === "decision";
  const outstanding = decision
    ? OUTSTANDING_DECISION.has(node.state)
    : OUTSTANDING_REVIEW.has(node.state);
  const actionable = outstanding && Boolean(node.need_id);
  const sub = decision
    ? decisionText(node.state, actionable)
    : reviewText(node.state, actionable);
  const assumed = node.assumed || (decision && node.state === "defaulted");
  const settled = node.state === "acked" || node.state === "accepted";
  const dot = actionable ? "you" : settled ? "verified" : outstanding ? "blocked" : "todo";
  const icon: IconName | undefined = actionable
    ? decision
      ? "hand"
      : "package"
    : settled
      ? "check"
      : decision && node.state === "defaulted"
        ? "timer"
        : undefined;
  const what = decision ? "decision" : "delivery";
  const title = [`${node.label} (${what})`, `state ${node.state}`]
    .concat(actionable ? ["waits for you"] : [])
    .concat(assumed ? ["assumed"] : [])
    .join(" · ");
  return {
    label: node.label,
    sub,
    dot,
    ...(icon ? { icon } : {}),
    you: actionable,
    assumed,
    title,
  };
}

export function nodeDisplay(node: GraphNode, feature: FeatureState | undefined): NodeDisplay {
  const assumed = node.assumed || feature?.basis === "assumed";
  if (node.kind !== "feature" || !feature) {
    return virtualDisplay(node);
  }

  // A session ending only stops unfinished work; finished steps keep their state.
  const finished =
    feature.progress === "done" || feature.progress === "verified" || feature.progress === "dropped";
  // The core's claim decides: a step whose claim ended is stopped, a quiet
  // agent inside its ETA is still at work. Without a claim, liveness does.
  const claim = feature.claim;
  const stopped = !finished && (claim ? claim.state === "free" : feature.liveness === "lost");
  const quiet = claim ? claim.state === "stalled" : feature.liveness === "quiet";
  const corrected = feature.correction ? correctionReason(feature.correction) : undefined;
  const tainted = feature.basis === "tainted";
  const eta = etaText(feature);
  const gate =
    feature.gate === "awaiting_decision"
      ? tx("gd.gate_call")
      : feature.gate === "awaiting_upstream"
        ? tx("gd.gate_up")
        : "";
  let dot = "todo";
  let icon: IconName | undefined;
  let sub = gate;
  if (stopped) {
    dot = "stopped";
    icon = "alert-triangle";
    sub = feature.correction?.rule === "silent" ? tx("dash.agent.abandoned_plain") : tx("rs.stopped");
  } else if (tainted) {
    dot = "tainted";
    icon = "refresh-cw";
    sub = eta ? tx("gd.rework_eta", { eta }) : tx("gd.rework");
  } else if (feature.progress === "verified") {
    dot = "verified";
    icon = "check";
    sub = tx("ov.verified_cap");
  } else if (feature.progress === "done") {
    // A review still pending shows on the delivery's own node.
    dot = "done";
    icon = "check";
    sub = tx("gd.done");
  } else if (feature.progress === "dropped") {
    dot = "dropped";
    icon = "x";
    sub = feature.owner_drop ? tx("plan.dropped_by_you") : tx("progress.dropped");
  } else if (feature.phase === "in_review") {
    dot = "review";
    icon = "package";
    sub = tx("gd.review");
  } else if (feature.phase === "sent_back") {
    dot = "doing";
    sub = tx("dl.rev.sent_back");
  } else if (
    feature.progress === "doing" &&
    feature.gate === "open" &&
    quiet &&
    !feature.paused
  ) {
    // The agent has said nothing for over 15 minutes.
    dot = "quiet";
    sub = tx("gd.quiet");
  } else if (feature.progress === "doing" && feature.gate === "open") {
    dot = "doing";
    sub = eta || PROGRESS_LABEL[feature.progress];
  } else if (feature.gate !== "open") {
    dot = "blocked";
    icon = "lock";
  } else if (feature.progress === "doing") {
    sub = eta || PROGRESS_LABEL[feature.progress];
  } else {
    sub = eta ? `${PROGRESS_LABEL[feature.progress]} · ${eta}` : PROGRESS_LABEL[feature.progress];
  }
  if (!sub) sub = PROGRESS_LABEL[feature.progress];

  const title = [
    `${feature.title} (${feature.node})`,
    `progress ${feature.progress}`,
    `gate ${feature.gate}`,
    `basis ${feature.basis}`,
    `liveness ${feature.liveness}`,
    eta ? eta : "",
    corrected ?? "",
  ]
    .filter(Boolean)
    .join(" · ");

  return {
    label: feature.title,
    sub,
    dot,
    ...(icon ? { icon } : {}),
    you: false,
    assumed,
    title,
    ...(corrected ? { corrected } : {}),
  };
}


/**
 * Whether the ambient radar may run on this node (motion still needs its
 * gate). A step delivered and waiting for the owner's review is not moving:
 * the same rule as Moving now (`state/moving.ts`) and the core's
 * `reduce::motion`.
 */
export function isRadarCandidate(feature: FeatureState | undefined): boolean {
  if (!feature) return false;
  return (
    feature.progress === "doing" &&
    feature.phase !== "in_review" &&
    feature.gate === "open" &&
    feature.basis !== "tainted" &&
    feature.liveness === "live" &&
    !feature.paused
  );
}

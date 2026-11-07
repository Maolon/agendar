/**
 * The small visuals the prototype placed at the end of a Brief row. Each one
 * is derived from projected data: a delivered demo thumbnail, waiting-agent
 * avatars, the default countdown ring and the ten-dot speculation meter.
 */

import { tx } from "../i18n";
import { formatRemaining } from "../api/format";
import type { BriefRow, BriefSection, Demo, Snapshot } from "../api/types";
import { useBlob } from "../hooks/useBlob";
import { defaultRing, rowBudget, rowDelivery, waitingAgents } from "../state/brief";
import { demoAlt, demoBlob, firstDemo, mediaUrl } from "../state/delivery";
import { Icon } from "./Icon";

function Bots({ count }: { count: number }) {
  const shown = Math.min(count, 3);
  if (shown <= 0) return null;
  return (
    <span className="bots" aria-label={`${count} ${count === 1 ? "agent" : "agents"} waiting`}>
      {Array.from({ length: shown }, (_, index) => (
        <span key={index} aria-hidden="true">
          <Icon name="bot" />
        </span>
      ))}
    </span>
  );
}

/** Desaturated mini thumbnail of the delivered demo; presentation only. */
function DeliveredThumb({ snapshot, row }: { snapshot: Snapshot; row: BriefRow }) {
  const found = rowDelivery(snapshot, row);
  const demo = found ? firstDemo(found.delivery) : undefined;
  if (!found || !demo) return null;
  if (demo.kind === "screenshot") {
    return <ScreenshotMini project={found.project.summary.slug} demo={demo} />;
  }
  return <TranscriptMini project={found.project.summary.slug} demo={demo} />;
}

function ScreenshotMini({ project, demo }: { project: string; demo: Demo }) {
  const view = useBlob(project, demoBlob(demo));
  if (view.phase === "ready" && view.token) {
    return (
      <img
        className="mini-img"
        src={mediaUrl(view.token)}
        alt={demoAlt(demo)}
        data-testid="brief-thumbnail"
      />
    );
  }
  return (
    <span className="mini-thumb" data-testid="brief-thumbnail" aria-label={`${demoAlt(demo)} demo`}>
      <Icon name="eye" className="c3" />
    </span>
  );
}

function TranscriptMini({ project, demo }: { project: string; demo: Demo }) {
  const view = useBlob(project, demoBlob(demo));
  const firstLine = view.phase === "ready" && view.text ? view.text.split("\n")[0] : undefined;
  return (
    <span className="mini-thumb" data-testid="brief-thumbnail">
      <Icon name="terminal" className="c3" />
      {firstLine ? <span className="mini-line">{firstLine}</span> : null}
    </span>
  );
}

function DefaultRing({ snapshot, row }: { snapshot: Snapshot; row: BriefRow }) {
  const ring = defaultRing(snapshot, row);
  if (!ring) return null;
  const start = Date.parse(ring.since);
  const deadline = Date.parse(ring.defaultAt);
  const now = Date.parse(snapshot.state.evaluated_at);
  const total = deadline - start;
  const remaining = Math.max(0, deadline - now);
  const fraction = total > 0 ? Math.min(1, Math.max(0, remaining / total)) : 0;
  const circumference = 2 * Math.PI * 10;
  const then =
    ring.onTimeout.kind === "proceed_as_assumption"
      ? tx("cd.goes_with", { label: ring.chosenLabel ?? tx("cd.the_rec") })
      : tx("cd.holds");
  const label = tx("cd.label", { d: formatRemaining(ring.defaultAt, snapshot.state.evaluated_at), then });
  return (
    <span className="brief-ring" aria-label={label} title={label} data-testid="brief-ring">
      <svg className="ring" viewBox="0 0 26 26" aria-hidden="true">
        <circle className="bg" cx="13" cy="13" r="10" />
        <circle
          className="fg"
          cx="13"
          cy="13"
          r="10"
          strokeDasharray={`${(circumference * fraction).toFixed(1)} ${circumference.toFixed(1)}`}
        />
      </svg>
      <span aria-hidden="true">{formatRemaining(ring.defaultAt, snapshot.state.evaluated_at)}</span>
    </span>
  );
}

/** Ten dots: features built on unconfirmed choices against the project cap. */
export function BudgetDots({
  used,
  limit,
  blocked,
}: {
  used: number;
  limit: number;
  blocked?: boolean;
}) {
  const shown = Math.min(Math.max(Math.trunc(limit), 1), 10);
  const safeLimit = Math.max(Math.trunc(limit), 1);
  const filled = Math.min(shown, Math.round((Math.max(used, 0) / safeLimit) * shown));
  const label = tx("bv.budget", { used, limit, over: blocked ? tx("bv.over") : "" });
  return (
    <span className="brief-budget" title={label}>
      <span className="dots" role="img" aria-label={label} data-testid="brief-budget-dots">
        {Array.from({ length: shown }, (_, index) => (
          <i key={index} className={index < filled ? "" : "e"} />
        ))}
      </span>
      <span aria-hidden="true">
        {tx("bv.count", { used, limit })}
      </span>
    </span>
  );
}

export function BriefVisual({ section, row, snapshot }: {
  section: BriefSection;
  row: BriefRow;
  snapshot: Snapshot;
}) {
  switch (section) {
    case "delivered":
      return <DeliveredThumb snapshot={snapshot} row={row} />;
    case "needs_you":
      return <Bots count={waitingAgents(snapshot, row)} />;
    case "by_default":
      return <DefaultRing snapshot={snapshot} row={row} />;
    case "at_risk": {
      const budget = rowBudget(snapshot, row);
      if (!budget) return null;
      return (
        <BudgetDots used={budget.used} limit={budget.limit} blocked={budget.blocked} />
      );
    }
    default:
      return null;
  }
}

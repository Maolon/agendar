import { needQuestion, needReason } from "../i18n/msg";
import { tx } from "../i18n";
import { useRef, useState } from "react";
import { errorCode } from "../api/client";
import { formatAgo, newRequestId } from "../api/format";
import { useClient } from "../api/store";
import type { NeedsYouItem, ProjectState } from "../api/types";
import { Icon } from "../components/Icon";
import { NeedDismiss, NeedSnooze } from "../components/NeedSnooze";
import { gateLabel } from "../state/project";
import { ProjectAvatar } from "../components/ProjectAvatar";

interface RescueProps {
  project: ProjectState;
  need: NeedsYouItem;
  anchor: string;
  canMutate: boolean;
  onResolved: (message: string) => void;
  onRefresh: () => void;
}

/**
 * Rescue view: same spacing and anatomy as a decision, intent-only actions.
 * Handoff and pause patch the plan; nothing here starts, resumes or kills a
 * process. Evidence stays read-only and local.
 */
export function Rescue({ project, need, anchor, canMutate, onResolved, onRefresh }: RescueProps) {
  const client = useClient();
  const [pending, setPending] = useState<"handoff" | "pause" | null>(null);
  const [error, setError] = useState<{ message: string; hint?: string } | null>(null);
  const requestRef = useRef<Record<string, string>>({});
  const feature = need.node ? project.features[need.node] : undefined;
  const session = feature?.session ? project.sessions[feature.session] : undefined;
  const stopped = feature?.liveness === "lost";

  async function resolve(action: "handoff" | "pause"): Promise<void> {
    if (!canMutate || pending) return;
    if (!need.node) {
      setError({
        message: tx("rs.no_node"),
        hint: tx("rs.no_node_hint"),
      });
      return;
    }
    const requestId = requestRef.current[action] ?? newRequestId();
    requestRef.current[action] = requestId;
    setPending(action);
    setError(null);
    try {
      await client.resolveRescue({
        request_id: requestId,
        item_id: need.id,
        expected_rescue_revision: need.revision,
        action,
      });
      delete requestRef.current[action];
      setPending(null);
      onResolved(
        action === "handoff"
          ? tx("rs.handed")
          : tx("rs.paused"),
      );
      onRefresh();
    } catch (cause) {
      setPending(null);
      setError({
        message: cause instanceof Error ? cause.message : tx("tray.failed"),
        hint:
          errorCode(cause) === "E_STALE_ANSWER"
            ? tx("rs.stale")
            : tx("rs.retry"),
      });
      onRefresh();
    }
  }

  return (
    <div className="view">
      <div className="crumb">
        <ProjectAvatar glyph={project.summary.glyph} size="sm" project={project.summary} />
        <span>{project.summary.slug}</span>
        <span className="sep">›</span>
        <Icon name="flag" className="c3" />
        <span>{feature?.slice ?? need.node ?? tx("rs.rescue")}</span>
        <NeedSnooze id={need.id} />
      </div>
      <h1 className="h">{needQuestion(need)}</h1>
      <p className="lede">
        {tx("rs.lede")}
      </p>
      <div className="strip">
        <div className="l">
          <Icon
            name="alert-triangle"
            className={stopped ? "cred" : "c2"}
            strokeWidth={1.9}
          />
          <span>
            {stopped ? <b>{tx("rs.stopped")}</b> : tx("rs.needs")}
            {feature?.session ? tx("rs.session", { id: feature.session }) : ""}
          </span>
        </div>
        <div className="r">
          <span>{formatAgo(need.since, anchor)}</span>
        </div>
      </div>
      <div className="facts" aria-label={tx("rs.evidence")}>
        <span className="lbl">{tx("rs.evidence")}</span>
        <span className="fact">
          <Icon name="eye" className="c2" />
          {needReason(need)}
        </span>
        {session ? (
          <span className="fact">
            <Icon name="server" className="c2" />
            <span className="mono">{session.host}</span> · {formatAgo(session.last_event_ts, anchor)}
          </span>
        ) : null}
        {feature ? (
          <span className="fact">
            <Icon name="git-branch" className="c2" />
            {feature.title} · {gateLabel(feature.gate)}
          </span>
        ) : null}
      </div>
      {error ? (
        <div className="alert" role="alert">
          <Icon name="alert-triangle" />
          <span className="body">
            <span>{error.message}</span>
            {error.hint ? <span className="hint">{error.hint}</span> : null}
          </span>
        </div>
      ) : null}
      <div className="acts">
        <NeedDismiss id={need.id} />
        <button
          type="button"
          className="btn"
          onClick={() => void resolve("pause")}
          disabled={!canMutate || pending !== null}
        >
          <Icon name="pause-circle" />
          {tx("rs.pause")}
        </button>
        <button
          type="button"
          className="btn primary"
          onClick={() => void resolve("handoff")}
          disabled={!canMutate || pending !== null}
        >
          <Icon name="arrow-right" />
          {pending === "handoff" ? tx("rs.recording") : tx("rs.handoff")}
        </button>
      </div>
      {!canMutate ? (
        <p className="muted-note">{tx("dl.disabled")}</p>
      ) : null}
    </div>
  );
}

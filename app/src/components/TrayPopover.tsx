import { needQuestion } from "../i18n/msg";
import { shortcut } from "../state/shortcut";
import { tx } from "../i18n";
import { useRef, useState } from "react";
import { errorCode, isApiError } from "../api/client";
import { formatAgo, isEasyRecommendation, newRequestId, optionById } from "../api/format";
import { useClient } from "../api/store";
import type {
  AnswerDecisionData,
  AnswerDecisionRequest,
  DecisionState,
  NeedsYouItem,
  ProjectState,
  Snapshot,
  WindowChrome,
} from "../api/types";
import { useKeyboard } from "../hooks/useKeyboard";
import { POPOVER_GLASS, useGlassRegions, useWindowActive } from "../hooks/useWindowChrome";
import { agentCounts, agentsLine, etaText, totalAgents, workingAgents } from "../state/project";
import { isMoving as stepMoving, isStalled as stepStalled } from "../state/moving";
import { Icon, type IconName } from "./Icon";
import { NeedDismiss, NeedSnooze } from "./NeedSnooze";
import { AppLogo } from "./BrandMark";
import { updateAvailable } from "./FootStatus";
import { useEnvironment } from "../state/motion";
import { ProjectAvatar } from "./ProjectAvatar";
import { PresenceRows } from "./PresenceRows";
import { ProjectTrack } from "./Sidebar";
import { isActiveNeed } from "../state/claim";

const KIND_ICON: Record<NeedsYouItem["kind"], IconName> = {
  decide: "git-branch",
  review: "package",
  confirm: "circle-dashed",
  rescue: "alert-triangle",
};

interface TrayPopoverProps {
  snapshot: Snapshot;
  /** Glass on macOS 26+: the panel is native Liquid Glass. */
  chrome?: WindowChrome;
  canMutate: boolean;
  /** Open the main window on a need, or just open it when null. */
  onOpen: (need: NeedsYouItem | null) => void;
  /** The backend escalated an answer beyond one key: say why and offer the main window. */
  onNeedsMain?: (need: NeedsYouItem, label: string, guard: "confirm" | "choose") => void;
  onAnswered: (data: AnswerDecisionData) => void;
  onRefresh: () => void;
}

function decisionFor(snapshot: Snapshot, need: NeedsYouItem): DecisionState | undefined {
  return snapshot.state.projects[need.project]?.decisions[need.source];
}

function isStopped(snapshot: Snapshot, need: NeedsYouItem): boolean {
  const feature = need.node ? snapshot.state.projects[need.project]?.features[need.node] : undefined;
  return feature?.liveness === "lost";
}

function localClock(ts: string): string {
  const date = new Date(ts);
  return Number.isNaN(date.getTime())
    ? ""
    : date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
}

const PROGRESS_ROWS = 4;

/** The project needs the owner: a question for you, or an agent waiting on your answer. */
function needsOwner(project: ProjectState): boolean {
  return project.summary.needs_count > 0 || agentCounts(project).waiting > 0;
}

/** Something happens in this project now: it needs you, or an agent is on a moving step. */
function isMoving(project: ProjectState): boolean {
  return needsOwner(project) || workingAgents(project) > 0;
}

/** Connected agents that move nothing and ask nothing: stalled on their step, or idle. */
function restingAgents(project: ProjectState): number {
  const counts = agentCounts(project);
  return counts.stalled + counts.idle;
}

function projectTitle(snapshot: Snapshot, slug: string): string {
  return snapshot.state.projects[slug]?.summary.title ?? slug;
}

/**
 * Progress first: the projects where something happens now (a question for
 * you, an agent at work, a step in progress), one line each with what moves.
 */
function TrayProgress({
  snapshot,
  onOpen,
  rows = PROGRESS_ROWS,
}: {
  snapshot: Snapshot;
  onOpen: () => void;
  rows?: number;
}) {
  const projects = Object.values(snapshot.state.projects).filter((project) => !project.summary.archived);
  // The step the row names is one Now lists under Moving now.
  const doing = (project: ProjectState) => Object.values(project.features).filter(stepMoving);
  const moving = projects
    .filter(isMoving)
    .sort(
      (a, b) =>
        b.summary.needs_count - a.summary.needs_count ||
        Number(needsOwner(b)) - Number(needsOwner(a)) ||
        workingAgents(b) - workingAgents(a) ||
        a.summary.title.localeCompare(b.summary.title),
    );
  // Then projects whose agent is connected but moving no step (idle, or its
  // step stalled): their open steps are waiting for someone, which is worth
  // seeing next to the progress.
  const idle = projects
    .filter((project) => !isMoving(project) && restingAgents(project) > 0)
    .sort((a, b) => a.summary.title.localeCompare(b.summary.title));
  const shown = [...moving, ...idle].slice(0, rows);
  const more = projects.length - shown.length;
  if (!projects.length) return null;
  return (
    <div className="tray-progress">
      {shown.map((project) => {
        const features = Object.values(project.features).filter((feature) => feature.progress !== "dropped");
        const finished = features.filter(
          (feature) => feature.progress === "done" || feature.progress === "verified",
        ).length;
        const current = doing(project)[0];
        const open = features.length - finished;
        // A connected agent whose step stalled is not idle: the row names the step.
        const counts = agentCounts(project);
        const stuck =
          !current && counts.stalled > 0
            ? features.find((feature) => stepStalled(feature) && feature.liveness === "live")
            : undefined;
        const line = current
          ? [current.title, etaText(current)].filter(Boolean).join(" · ")
          : project.summary.needs_count
            ? tx("tray.waiting", { n: project.summary.needs_count })
            : counts.waiting > 0
              ? (agentsLine({ working: 0, stalled: 0, waiting: counts.waiting, idle: 0 }) ?? "")
              : stuck
                ? `${stuck.title} · ${tx("dash.sec.stalled")}`
                : open === 1
                  ? tx("tray.idle.one")
                  : tx("tray.idle.other", { n: open });
        return (
          <button
            key={project.summary.slug}
            type="button"
            className="tray-proj"
            aria-label={tx("tray.open_x", { x: project.summary.title })}
            onClick={onOpen}
          >
            <ProjectAvatar glyph={project.summary.glyph} size="sm" project={project.summary} />
            <span className="tray-proj-main">
              <span className="tray-proj-top">
                <span className="tray-proj-name">{project.summary.title}</span>
                <ProjectTrack features={project.features} />
                <span className="tray-proj-count">
                  {finished}/{features.length}
                </span>
              </span>
              <span className={`tray-proj-line${current && current.eta && current.eta.late_by_s > 0 ? " late" : ""}`}>
                {line}
              </span>
            </span>
          </button>
        );
      })}
      {more > 0 ? (
        <p className="pop-more">
          {more} more project{more === 1 ? "" : "s"}
        </p>
      ) : null}
    </div>
  );
}

/**
 * Tray popover: the total Needs you count, one expanded eligible easy
 * recommendation, every other item compact. Only easy, medium/high-confidence
 * open decisions get a one-key approve; costly and irreversible items never do.
 */
export function TrayPopover({
  snapshot,
  chrome,
  canMutate,
  onOpen,
  onAnswered,
  onRefresh,
  onNeedsMain,
}: TrayPopoverProps) {
  const client = useClient();
  const rootRef = useRef<HTMLDivElement | null>(null);
  const glass = chrome?.material === "glass";
  const windowActive = useWindowActive();
  useGlassRegions(client, rootRef, glass, "popover", POPOVER_GLASS, "--win");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<{ message: string; hint?: string } | null>(null);
  // The complete commit request is retained before the call, so a retry after
  // an uncertain outcome re-sends the identical payload under the same ID.
  const reservedRef = useRef<AnswerDecisionRequest | null>(null);
  // Decisions approved here leave the list at once, before the refreshed
  // snapshot drops them, so the next question moves up for its own approve.
  const [approved, setApproved] = useState<ReadonlySet<string>>(() => new Set());
  const anchor = snapshot.state.evaluated_at;
  const latest = updateAvailable(snapshot) ? snapshot.update!.latest! : null;
  const { reduced } = useEnvironment();
  const animate = !reduced && snapshot.preferences.motion !== false;
  function openAbout(): void {
    void client
      .showMain({ view: "about" })
      .catch(() => undefined)
      .finally(() => {
        void client.hidePopover().catch(() => undefined);
      });
  }
  const active = snapshot.needs.filter((need) => {
    if (!isActiveNeed(need)) return false;
    if (approved.has(need.source)) return false;
    if (snapshot.preferences.dismissed?.includes(need.id)) return false;
    const until = need.snoozed_until;
    return !until || Date.parse(until) <= Date.parse(anchor);
  });
  const eligible = active.filter((need) => {
    const decision = decisionFor(snapshot, need);
    if (!decision) return false;
    if (need.kind !== "decide" && need.kind !== "confirm") return false;
    return isEasyRecommendation(decision);
  });
  const top = eligible[0];
  const topDecision = top ? decisionFor(snapshot, top) : undefined;
  const topOption = topDecision
    ? topDecision.card.options.find((option) => option.id === topDecision.card.recommended.option)
    : undefined;
  const others = active.filter((need) => need.id !== top?.id);

  async function approve(need: NeedsYouItem): Promise<void> {
    const decision = decisionFor(snapshot, need);
    const option = decision ? optionById(decision.card, decision.card.recommended.option) : undefined;
    if (!decision || !option || pending) return;
    if (!canMutate) {
      setError({ message: tx("tray.stale"), hint: tx("tray.stale_hint") });
      return;
    }
    setPending(true);
    setError(null);
    try {
      let request = reservedRef.current;
      if (!request) {
        const prepared = await client.prepareAnswer({
          ask_id: decision.id,
          option_id: option.id,
          expected_decision_revision: decision.revision,
          ...(decision.effective_receipt ? { expected_receipt: decision.effective_receipt } : {}),
        });
        if (prepared.guard !== "easy") {
          // The backend escalated the risk; never one-key approve that. The
          // tray answers easy questions only, so the confirm happens in the
          // main window: an amber toast says why and opens it on this item.
          setPending(false);
          if (onNeedsMain) {
            onNeedsMain(need, prepared.label, prepared.guard === "confirm" ? "confirm" : "choose");
          } else {
            setError({ message: tx("tray.not_one_key"), hint: tx("tray.not_one_key_hint") });
          }
          onRefresh();
          return;
        }
        request = {
          request_id: newRequestId(),
          token: prepared.token,
          echo_label: prepared.label,
          ...(option.eta ? { expected_eta: option.eta } : {}),
        };
        reservedRef.current = request;
      }
      const data = await client.answerDecision(request);
      reservedRef.current = null;
      setApproved((previous) => new Set(previous).add(decision.id));
      setPending(false);
      // The popover stays open: the toast confirms, and the next question can
      // be approved right away. Escape or a click elsewhere closes it.
      onAnswered(data);
    } catch (cause) {
      // Only an uncertain outcome keeps the reservation; a rejected request is new work.
      if (!isApiError(cause) || !cause.retryable) reservedRef.current = null;
      setPending(false);
      setError({
        message: cause instanceof Error ? cause.message : tx("tray.failed"),
        hint: tx("tray.failed_hint", { code: errorCode(cause) }),
      });
      onRefresh();
    }
  }

  // The popover uses the same window keyboard policy as the main window, with
  // its own handlers: IME composition, repeats, editable targets and
  // already-handled events are all rejected before Enter can approve anything.
  useKeyboard({
    rows: [],
    selection: null,
    select: () => undefined,
    optionCount: 0,
    selectOption: () => undefined,
    plainEnterEnabled: canMutate && !pending && top !== undefined,
    submit: (source) => {
      if (source !== "enter" || !top) return;
      void approve(top);
    },
    escape: () => {
      void client.hidePopover();
    },
    focusSearch: () => undefined,
    canOpenReport: false,
    openReport: () => undefined,
    scrollRowIntoView: () => undefined,
  });

  const projects = Object.values(snapshot.state.projects).filter((project) => !project.summary.archived);
  const agents = totalAgents(projects);
  const working = agents.working;
  const status = [
    agentsLine(agents) ?? tx("agents.none"),
    active.length
      ? active.length === 1
        ? tx("tray.needs.one")
        : tx("tray.needs.other", { n: active.length })
      : tx("tray.nothing"),
  ].join(" · ");
  const hasProgress = projects.some((project) => isMoving(project) || restingAgents(project) > 0);

  return (
    <div
      className="pop"
      ref={rootRef}
      data-material={glass ? "glass" : "solid"}
      data-inactive={windowActive ? undefined : ""}
    >
      <header className="pop-head">
        <span className={`pop-mark${working ? " live" : ""}`} aria-hidden="true">
          <AppLogo size={22} />
        </span>
        <span className="pop-title">
          <span className="pop-name">Agendar</span>
          <span className="pop-status">{status}</span>
        </span>
      </header>
      <div className="pop-body">
        {error ? (
          <div className="alert" role="alert">
            <Icon name="alert-triangle" />
            <span className="body">
              <span>{error.message}</span>
              {error.hint ? <span className="hint">{error.hint}</span> : null}
            </span>
          </div>
        ) : null}
        {!hasProgress && !active.length ? (
          <div className="pop-quiet">
            <AppLogo size={44} />
            <span className="pop-quiet-t">{tx("tray.quiet")}</span>
            <span className="pop-quiet-s">{tx("tray.quiet_s")}</span>
          </div>
        ) : (
          <>
            {hasProgress ? (
              <section className="pop-sec" aria-label={tx("side.projects")}>
                <h2 className="pop-h">{tx("side.projects")}</h2>
                {/* With questions waiting, keep them on the first screen. */}
                <TrayProgress snapshot={snapshot} onOpen={() => onOpen(null)} rows={active.length ? 3 : 4} />
              </section>
            ) : null}
            <section className="pop-sec" aria-label={tx("side.needs")}>
              <h2 className="pop-h">
                {tx("side.needs")}
                {active.length ? <span className="pop-badge">{active.length}</span> : null}
              </h2>
              <PresenceRows
                rows={(snapshot.presence ?? []).filter((row) => row.state !== "working")}
                snapshot={snapshot}
                anchor={snapshot.state.evaluated_at}
              />
              {!active.length && !(snapshot.presence ?? []).some((row) => row.state !== "working") ? (
                <p className="pop-clear">
                  <Icon name="check" />
                  {tx("tray.nothing_now")}
                </p>
              ) : null}
              {top && topDecision && topOption ? (
                <div className="pop-ask">
                  <span className="pop-meta">
                    <Icon name={KIND_ICON[top.kind]} />
                    {projectTitle(snapshot, top.project)} · {formatAgo(top.since, anchor)}
                    <NeedSnooze id={top.id} />
                  </span>
                  <span className="pop-q">{topDecision.card.question}</span>
                  <span className="pop-rec">
                    <Icon name="star" />
                    <span>
                      <b>{topOption.label}</b>
                      <span className="pop-rec-why">{topOption.consequence}</span>
                    </span>
                  </span>
                  <div className="pop-acts">
                    <button type="button" className="btn" onClick={() => onOpen(top)}>
                      {tx("tray.open")}
                    </button>
                    <button
                      type="button"
                      className="btn primary"
                      onClick={() => void approve(top)}
                      disabled={!canMutate || pending}
                    >
                      {pending ? tx("tray.saving") : tx("tray.approve")}
                      <kbd>{shortcut("enter")}</kbd>
                    </button>
                  </div>
                </div>
              ) : active.length ? (
                <p className="pop-note">
                  {tx("tray.no_one_key")}
                </p>
              ) : null}
              {others.map((need) => (
                <div key={need.id} className="pop-row-wrap">
                <button
                  type="button"
                  className="pop-row"
                  aria-label={tx("tray.open_in", { q: needQuestion(need), p: need.project })}
                  onClick={() => onOpen(need)}
                >
                  <Icon
                    name={KIND_ICON[need.kind]}
                    className={need.kind === "rescue" && isStopped(snapshot, need) ? "cred" : "c2"}
                  />
                  <span className="pop-row-main">
                    <span className="pop-row-q">{needQuestion(need)}</span>
                    <span className="pop-meta">
                      {projectTitle(snapshot, need.project)} · {formatAgo(need.since, anchor)}
                    </span>
                  </span>
                  <Icon name="chevron-right" className="c3" />
                </button>
                <NeedSnooze id={need.id} compact />
                {need.kind === "rescue" ? <NeedDismiss id={need.id} compact /> : null}
                </div>
              ))}
            </section>
          </>
        )}
      </div>
      <footer className="pop-foot">
        {latest && !pending ? (
          // A newer release replaces the "Updated" time, as in the sidebar;
          // it opens Settings › About in the main window.
          <button
            type="button"
            className={animate ? "foot-face foot-pill pop-update foot-in" : "foot-face foot-pill pop-update"}
            title={`${tx("update.available", { version: latest })} · ${tx("tray.updated", { t: localClock(anchor) })}`}
            onClick={openAbout}
          >
            <Icon name="package" />
            <span className="foot-text">{tx("update.tag", { version: latest })}</span>
          </button>
        ) : (
          <span className="faint" title={tx("tray.as_of", { ts: anchor })}>
            {pending ? tx("tray.saving") : tx("tray.updated", { t: localClock(anchor) })}
          </span>
        )}
        <button type="button" className="btn" onClick={() => onOpen(null)}>
          {tx("tray.open_app")}
        </button>
      </footer>
    </div>
  );
}

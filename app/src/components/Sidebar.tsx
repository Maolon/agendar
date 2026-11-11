import { needQuestion, needReason } from "../i18n/msg";
import { shortcut } from "../state/shortcut";
import { tx, type MessageKey } from "../i18n";
import { useState } from "react";
import { formatAge, formatWaiting } from "../api/format";
import { activeNeeds, closedStepNeeds, snoozedNeeds } from "../state/snooze";
import { isActiveNeed } from "../state/claim";
import type {
  FeatureState,
  NeedsYouItem,
  ProjectState,
  ProjectSummary,
  Snapshot,
  Variance,
} from "../api/types";
import { agentsLine, projectFinishLabel, totalAgents } from "../state/project";
import { demoGap, demoGapLine } from "../state/demoGap";
import { isRadarCandidate } from "../graph/display";
import type { Selection } from "../state/ui";
import { FootStatus } from "./FootStatus";
import { Icon, type IconName } from "./Icon";
import { ProjectAvatar } from "./ProjectAvatar";

const VARIANCE_ORDER: Variance[] = ["rework", "stopped", "no_demo", "active", "quiet"];

const VARIANCE_LABEL: Record<Exclude<Variance, "active">, { icon: IconName; text: MessageKey }> = {
  rework: { icon: "refresh-cw", text: "variance.rework" },
  stopped: { icon: "pause-circle", text: "variance.stopped" },
  no_demo: { icon: "eye-off", text: "variance.no_demo" },
  quiet: { icon: "eye-off", text: "variance.quiet" },
};

const NEED_ICON: Record<NeedsYouItem["kind"], IconName> = {
  decide: "git-branch",
  review: "package",
  confirm: "circle-dashed",
  rescue: "alert-triangle",
};

/** Visible needs then project rows, in shown order (↑/↓ traversal). */
export function sidebarRows(snapshot: Snapshot, quietOpen: boolean): Selection[] {
  const needs = activeNeeds(snapshot);
  const visible = orderedProjects(snapshot).filter(
    (summary) => summary.variance !== "quiet" || quietOpen,
  );
  return [
    ...needs.map((item) => ({ kind: "need" as const, id: item.id })),
    ...visible.map((summary) => ({ kind: "project" as const, id: summary.slug })),
  ];
}

export function orderedProjects(snapshot: Snapshot): ProjectSummary[] {
  const summaries = Object.values(snapshot.state.projects)
    .map((project) => project.summary)
    .filter((summary) => !summary.archived);
  const firstNeed = (slug: string): number => {
    const needs = snapshot.needs.filter((item) => item.project === slug && isActiveNeed(item));
    if (!needs.length) return Number.POSITIVE_INFINITY;
    return Math.min(...needs.map((item) => Date.parse(item.since)).filter((value) => !Number.isNaN(value)));
  };
  return summaries.sort((a, b) => {
    const byVariance = VARIANCE_ORDER.indexOf(a.variance) - VARIANCE_ORDER.indexOf(b.variance);
    if (byVariance !== 0) return byVariance;
    // A project with a need comes before one without; two missing needs fall
    // through to the slug instead of subtracting infinities.
    const aNeed = firstNeed(a.slug);
    const bNeed = firstNeed(b.slug);
    if (aNeed !== bNeed) return aNeed < bNeed ? -1 : 1;
    return a.slug.localeCompare(b.slug);
  });
}

function needIcon(item: NeedsYouItem, snapshot: Snapshot): { icon: IconName; red: boolean } {
  const feature = item.node
    ? snapshot.state.projects[item.project]?.features[item.node]
    : undefined;
  // The same claim the board shows: a step whose claim ended is red.
  const ended = feature?.claim ? feature.claim.state === "free" : feature?.liveness === "lost";
  return { icon: NEED_ICON[item.kind], red: item.kind === "rescue" && ended === true };
}

export function ProjectTrack({
  features,
  slug,
  motion,
  pulses,
}: {
  features: Record<string, FeatureState>;
  slug?: string;
  motion?: boolean;
  pulses?: ReadonlySet<string>;
}) {
  // Finished steps first, then moving ones, then the rest: the strip reads
  // left to right like the project's progress, one discrete segment per step.
  const rank = (feature: FeatureState): number =>
    feature.progress === "verified" || feature.progress === "done"
      ? 0
      : feature.progress === "doing"
        ? 1
        : feature.gate === "awaiting_decision" || feature.gate === "awaiting_upstream"
          ? 2
          : 3;
  const list = Object.values(features)
    .filter((feature) => feature.progress !== "dropped")
    .sort((a, b) => rank(a) - rank(b));
  if (!list.length) return null;
  const label = list
    .map((feature) => `${feature.title}: ${feature.progress}`)
    .join(" · ");
  const stateOf = (feature: FeatureState): string =>
    feature.progress === "verified" || feature.progress === "done"
      ? "d"
      : feature.progress === "doing"
        ? "w"
        : feature.gate === "awaiting_decision" || feature.gate === "awaiting_upstream"
          ? "h"
          : "";
  // Only a live, firm, unpaused doing step breathes: blocked, stopped,
  // tainted and stale work stays still.
  const isLive = (feature: FeatureState): boolean =>
    feature.progress === "doing" &&
    feature.gate === "open" &&
    feature.basis !== "tainted" &&
    feature.liveness === "live" &&
    !feature.paused;
  const isFlashing = (feature: FeatureState): boolean =>
    motion === true && slug !== undefined && pulses?.has(`${slug}/${feature.node}`) === true;
  if (list.length > 8) {
    // Too many steps for one segment each: one block per state, sized by its
    // count, so finished work and the outlined remainder both stay readable.
    const groups: { state: string; steps: FeatureState[] }[] = [];
    for (const feature of list) {
      const state = stateOf(feature);
      const last = groups[groups.length - 1];
      if (last && last.state === state) last.steps.push(feature);
      else groups.push({ state, steps: [feature] });
    }
    return (
      <span className="track dense" title={label} aria-label={label}>
        {groups.map((group) => (
          <i
            key={group.state || "p"}
            data-count={group.steps.length}
            style={{ flexGrow: group.steps.length }}
            className={`${group.state || "p"}${group.steps.some(isLive) ? " live" : ""}${
              group.steps.some(isFlashing) ? " blink" : ""
            }`}
          />
        ))}
      </span>
    );
  }
  return (
    <span className="track" title={label} aria-label={label}>
      {list.map((feature) => (
        <i
          key={feature.node}
          className={`${stateOf(feature)}${isLive(feature) ? " live" : ""}${
            isFlashing(feature) ? " blink" : ""
          }`}
        />
      ))}
    </span>
  );
}

function NeedRow({
  item,
  snapshot,
  selected,
  onSelect,
}: {
  item: NeedsYouItem;
  snapshot: Snapshot;
  selected: boolean;
  onSelect: () => void;
}) {
  const evaluatedAt = snapshot.state.evaluated_at;
  const icon = needIcon(item, snapshot);
  const age = formatAge(item.since, evaluatedAt);
  return (
    <button
      type="button"
      className="row"
      title={needQuestion(item)}
      aria-current={selected}
      data-row={`need:${item.id}`}
      onClick={onSelect}
    >
      <Icon name={icon.icon} className={icon.red ? "cred" : "c2"} />
      <span style={{ minWidth: 0 }}>
        <span className="t" title={needReason(item) || needQuestion(item)}>
          {needQuestion(item)}
        </span>
        <span className="s">
          <ProjectAvatar
            glyph={snapshot.state.projects[item.project]?.summary.glyph}
            size="sm"
            project={snapshot.state.projects[item.project]?.summary}
          />
          {item.project}
          {item.reason ? (
            <span className="why" title={needReason(item)}>
              {needReason(item)}
            </span>
          ) : null}
        </span>
      </span>
      <span className="tr">
        {item.stalled_agents > 0 ? (
          <span className="bots" aria-hidden="true">
            {Array.from({ length: Math.min(item.stalled_agents, 3) }, (_, index) => (
              <span key={index}>
                <Icon name="bot" />
              </span>
            ))}
          </span>
        ) : null}
        {item.waiting_minutes > 0 ? formatWaiting(item.waiting_minutes) : age}
      </span>
    </button>
  );
}

/**
 * The finish in as few characters as possible for a narrow sidebar:
 * "~10:54–11:51" → "~11:51", "1.5h left" → "1.5h".
 */
export function shortFinish(eta: string): string {
  if (eta.endsWith(" left")) return eta.slice(0, -" left".length);
  const parts = eta.split("–");
  if (parts.length < 2 || !/\d:\d\d/.test(eta)) return eta;
  const end = parts[parts.length - 1]?.trim() ?? eta;
  return eta.startsWith("~") ? `~${end}` : end;
}

function ProjectRow({
  project,
  selected,
  motion,
  pulses,
  onSelect,
}: {
  project: ProjectState;
  selected: boolean;
  motion?: boolean;
  pulses?: ReadonlySet<string>;
  onSelect: () => void;
}) {
  const summary = project.summary;
  const variance = summary.variance;
  const eta = projectFinishLabel(project);
  // "No demo" says what is missing and what clears it.
  const gap = variance === "no_demo" ? demoGap(project) : null;
  const why = gap ? demoGapLine(gap) : undefined;
  // A project that still has live work keeps its state; its stopped steps are
  // a count beside it. A project that is only stopped says so by its chip.
  const stoppedSteps = variance === "stopped" ? 0 : (summary.stopped_steps ?? 0);
  const stoppedLabel =
    stoppedSteps === 1
      ? tx("variance.stopped_steps.one")
      : tx("variance.stopped_steps.other", { n: stoppedSteps });
  return (
    <button
      type="button"
      className="row prow"
      title={summary.title}
      aria-description={why}
      aria-current={selected}
      data-row={`project:${summary.slug}`}
      onClick={onSelect}
    >
      <ProjectAvatar glyph={summary.glyph} project={summary} />
      <span className="t" title={summary.title}>
        {summary.title}
      </span>
      <span className="tr">
        {eta ? (
          <span className="pe" title={eta} data-short={shortFinish(eta) !== eta ? "true" : undefined}>
            <Icon name={eta === "paused" ? "pause-circle" : "clock"} />
            <span className="pe-full">{eta}</span>
            {shortFinish(eta) !== eta ? (
              <span className="pe-short" aria-hidden="true">
                {shortFinish(eta)}
              </span>
            ) : null}
          </span>
        ) : null}
        {stoppedSteps > 0 ? (
          <span className="pstop" role="img" aria-label={stoppedLabel} title={stoppedLabel}>
            <Icon name="pause-circle" />
            <span aria-hidden="true">{stoppedSteps}</span>
          </span>
        ) : null}
        {variance === "active" ? (
          <ProjectTrack
            features={project.features}
            slug={summary.slug}
            motion={motion}
            pulses={pulses}
          />
        ) : (
          <span className="pvar" title={why}>
            <Icon name={VARIANCE_LABEL[variance].icon} className="c2" />
            {tx(VARIANCE_LABEL[variance].text)}
          </span>
        )}
      </span>
    </button>
  );
}

interface SidebarProps {
  snapshot: Snapshot;
  selection: Selection | null;
  quietOpen: boolean;
  /** Motion is allowed right now; the track segment flashes on a pulse. */
  motion?: boolean;
  /** `${project}/${node}` keys pulsed in the latest batch. */
  pulses?: ReadonlySet<string>;
  onSelect: (selection: Selection) => void;
  onToggleQuiet: () => void;
  onOpenSettings?: () => void;
  settingsOpen?: boolean;
  /** Flip the stored motion preference (shown when provided). */
}

export function Sidebar({
  snapshot,
  selection,
  quietOpen,
  motion,
  pulses,
  onSelect,
  onToggleQuiet,
  onOpenSettings,
  settingsOpen = false,
}: SidebarProps) {
  const needs = activeNeeds(snapshot);
  // Snoozed items leave Needs you until their time but stay reachable to restore.
  const snoozed = snoozedNeeds(snapshot);
  const [snoozedOpen, setSnoozedOpen] = useState(false);
  // Questions on closed steps fold away; they stay answerable here.
  const closed = closedStepNeeds(snapshot);
  const [closedOpen, setClosedOpen] = useState(false);
  const projects = orderedProjects(snapshot);
  const visibleProjects = projects.filter((summary) => summary.variance !== "quiet" || quietOpen);
  const quiet = projects.filter((summary) => summary.variance === "quiet");
  // Archived projects leave the lists but stay reachable, to look or restore.
  const archived = Object.values(snapshot.state.projects)
    .filter((project) => project.summary.archived)
    .sort((a, b) => a.summary.title.localeCompare(b.summary.title));
  const [archivedOpen, setArchivedOpen] = useState(false);
  const live = projects
    .map((summary) => snapshot.state.projects[summary.slug])
    .filter((project): project is ProjectState => project !== undefined);
  const agents = agentsLine(totalAgents(live)) ?? tx("agents.none");
  const health = snapshot.local.health;
  // The footer radar means the fleet has work in progress: a live agent on a
  // doing feature. Without one it is not decoration, so it is not shown.
  const moving = projects.some((summary) => {
    const project = snapshot.state.projects[summary.slug];
    if (!project) return false;
    // The plan's rule: a step waiting for the owner's review is not moving.
    return Object.values(project.features).some(isRadarCandidate);
  });

  return (
    <aside className="side">
      <div className="side-scroll">
        <div className="gh">
          {tx("side.needs")}{" "}
          <span className={needs.length ? "n on" : "n"}>{needs.length}</span>
        </div>
        {needs.length ? (
          needs.map((item) => (
            <NeedRow
              key={item.id}
              item={item}
              snapshot={snapshot}
              selected={selection?.kind === "need" && selection.id === item.id}
              onSelect={() => onSelect({ kind: "need", id: item.id })}
            />
          ))
        ) : (
          <div className="empty-row">{tx("side.nothing")}</div>
        )}
        {snoozed.length ? (
          <>
            <button type="button" className="more" onClick={() => setSnoozedOpen((open) => !open)}>
              {snoozedOpen ? tx("side.snoozed_hide") : tx("side.snoozed", { n: snoozed.length })}
            </button>
            {snoozedOpen
              ? snoozed.map((item) => (
                  <NeedRow
                    key={item.id}
                    item={item}
                    snapshot={snapshot}
                    selected={selection?.kind === "need" && selection.id === item.id}
                    onSelect={() => onSelect({ kind: "need", id: item.id })}
                  />
                ))
              : null}
          </>
        ) : null}
        {closed.length ? (
          <>
            <button type="button" className="more" onClick={() => setClosedOpen((open) => !open)}>
              {closedOpen ? tx("side.closed_hide") : tx("side.closed", { n: closed.length })}
            </button>
            {closedOpen
              ? closed.map((item) => (
                  <NeedRow
                    key={item.id}
                    item={item}
                    snapshot={snapshot}
                    selected={selection?.kind === "need" && selection.id === item.id}
                    onSelect={() => onSelect({ kind: "need", id: item.id })}
                  />
                ))
              : null}
          </>
        ) : null}
        <div className="gh" style={{ marginTop: 6 }}>
          {tx("side.projects")} <span className="n">{projects.length}</span>
        </div>
        {visibleProjects.map((summary) => {
          const project = snapshot.state.projects[summary.slug];
          if (!project) return null;
          return (
            <ProjectRow
              key={summary.slug}
              project={project}
              selected={selection?.kind === "project" && selection.id === summary.slug}
              motion={motion}
              pulses={pulses}
              onSelect={() => onSelect({ kind: "project", id: summary.slug })}
            />
          );
        })}
        {quiet.length ? (
          <button type="button" className="more" onClick={onToggleQuiet}>
            {quietOpen ? tx("side.quiet_hide") : tx("side.quiet_more", { n: quiet.length })}
          </button>
        ) : null}
        {archived.length ? (
          <>
            <button type="button" className="more" onClick={() => setArchivedOpen((open) => !open)}>
              {archivedOpen ? tx("side.archived_hide") : tx("side.archived", { n: archived.length })}
            </button>
            {archivedOpen
              ? archived.map((project) => (
                  <ProjectRow
                    key={project.summary.slug}
                    project={project}
                    selected={selection?.kind === "project" && selection.id === project.summary.slug}
                    motion={false}
                    onSelect={() => onSelect({ kind: "project", id: project.summary.slug })}
                  />
                ))
              : null}
          </>
        ) : null}
      </div>
      <div className="side-foot">
        <span className="foot-status">
          <span className="foot-item foot-agents" title={agents}>
            {moving ? <span className="radar" aria-hidden="true" /> : null}
            <span className="foot-text">{agents}</span>
          </span>
          <FootStatus snapshot={snapshot} onOpenAbout={() => onSelect({ kind: "settings", id: "about" })} />
          {health !== "healthy" ? <span className="foot-item c3">{health}</span> : null}
        </span>
        {onOpenSettings ? (
          <button
            type="button"
            className="settings-btn"
            aria-label={tx("side.settings")}
            title={tx("side.settings_title", { key: shortcut("mod+,") })}
            aria-current={settingsOpen ? "page" : undefined}
            onClick={onOpenSettings}
          >
            <Icon name="settings" />
          </button>
        ) : null}
      </div>
    </aside>
  );
}

import { needQuestion, needReason } from "../i18n/msg";
import { tx, type MessageKey } from "../i18n";
import { useState } from "react";
import { formatAge, formatAgo, formatWaiting } from "../api/format";
import { activeNeeds, snoozedNeeds } from "../state/snooze";
import type {
  FeatureState,
  NeedsYouItem,
  ProjectState,
  ProjectSummary,
  Snapshot,
  Variance,
} from "../api/types";
import { agentsLine, idleAgents, projectFinishLabel, workingAgents } from "../state/project";
import type { Selection } from "../state/ui";
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

export function sidebarRows(
  snapshot: Snapshot,
  filter: string,
  quietOpen: boolean,
): Selection[] {
  const text = filter.trim().toLowerCase();
  const matches = (value: string): boolean => !text || value.toLowerCase().includes(text);
  const needs = activeNeeds(snapshot).filter(
    (item) => matches(item.question) || matches(item.project),
  );
  const visible = orderedProjects(snapshot).filter(
    (summary) =>
      matches(projectText(snapshot, summary)) &&
      (summary.variance !== "quiet" || quietOpen || text.length > 0),
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
    const needs = snapshot.needs.filter((item) => item.project === slug);
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

function projectText(snapshot: Snapshot, summary: ProjectSummary): string {
  const features = Object.values(snapshot.state.projects[summary.slug]?.features ?? {});
  return [summary.slug, summary.title, ...features.map((feature) => feature.title)].join(" ");
}

function needIcon(item: NeedsYouItem, snapshot: Snapshot): { icon: IconName; red: boolean } {
  const feature = item.node
    ? snapshot.state.projects[item.project]?.features[item.node]
    : undefined;
  return { icon: NEED_ICON[item.kind], red: item.kind === "rescue" && feature?.liveness === "lost" };
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
  return (
    <button
      type="button"
      className="row prow"
      title={summary.title}
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
        {variance === "active" ? (
          <ProjectTrack
            features={project.features}
            slug={summary.slug}
            motion={motion}
            pulses={pulses}
          />
        ) : (
          <>
            <Icon name={VARIANCE_LABEL[variance].icon} className="c2" />
            {tx(VARIANCE_LABEL[variance].text)}
          </>
        )}
      </span>
    </button>
  );
}

interface SidebarProps {
  snapshot: Snapshot;
  selection: Selection | null;
  filter: string;
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
  filter,
  quietOpen,
  motion,
  pulses,
  onSelect,
  onToggleQuiet,
  onOpenSettings,
  settingsOpen = false,
}: SidebarProps) {
  const evaluatedAt = snapshot.state.evaluated_at;
  const text = filter.trim().toLowerCase();
  const matches = (value: string): boolean => !text || value.toLowerCase().includes(text);
  const active = activeNeeds(snapshot);
  const needs = active.filter((item) => matches(item.question) || matches(item.project));
  // Snoozed items leave Needs you until their time but stay reachable to restore.
  const snoozed = snoozedNeeds(snapshot);
  const [snoozedOpen, setSnoozedOpen] = useState(false);
  const projects = orderedProjects(snapshot);
  const visibleProjects = projects.filter(
    (summary) => matches(projectText(snapshot, summary)) && (summary.variance !== "quiet" || quietOpen || text.length > 0),
  );
  const quiet = projects.filter((summary) => summary.variance === "quiet");
  // Archived projects leave the lists but stay reachable, to look or restore.
  const archived = Object.values(snapshot.state.projects)
    .filter((project) => project.summary.archived)
    .sort((a, b) => a.summary.title.localeCompare(b.summary.title));
  const [archivedOpen, setArchivedOpen] = useState(false);
  const live = projects
    .map((summary) => snapshot.state.projects[summary.slug])
    .filter((project): project is ProjectState => project !== undefined);
  const working = live.reduce((total, project) => total + workingAgents(project), 0);
  const idle = live.reduce((total, project) => total + idleAgents(project), 0);
  const health = snapshot.local.health;
  // The footer radar means the fleet has work in progress: a live agent on a
  // doing feature. Without one it is not decoration, so it is not shown.
  const moving = projects.some((summary) => {
    const project = snapshot.state.projects[summary.slug];
    if (!project) return false;
    return Object.values(project.features).some(
      (feature) =>
        feature.progress === "doing" &&
        feature.gate === "open" &&
        feature.liveness === "live" &&
        feature.basis !== "tainted" &&
        !feature.paused,
    );
  });

  return (
    <aside className="side">
      <div className="side-scroll">
        <div className="gh">
          {tx("side.needs")}{" "}
          <span className={active.length ? "n on" : "n"}>{active.length}</span>
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
          <div className="empty-row">{active.length ? tx("side.no_match") : tx("side.nothing")}</div>
        )}
        {!text && snoozed.length ? (
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
        {!text && quiet.length ? (
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
          <span className="foot-item foot-agents" title={agentsLine(working, idle) ?? tx("agents.none")}>
            {moving ? <span className="radar" aria-hidden="true" /> : null}
            <span className="foot-text">{agentsLine(working, idle) ?? tx("agents.none")}</span>
          </span>
          <span
            className="foot-item foot-received"
            title={tx("side.received_title", {
              age: formatAge(snapshot.local.received_at, evaluatedAt),
              ago: formatAgo(snapshot.local.received_at, evaluatedAt),
              time: snapshot.local.received_at,
            })}
          >
            <Icon name="cloud" />
            <span
              className="foot-text"
              aria-label={tx("side.received_label", {
                age: formatAge(snapshot.local.received_at, evaluatedAt),
                ago: formatAgo(snapshot.local.received_at, evaluatedAt),
              })}
            >
              {tx("side.received", {
                age: formatAge(snapshot.local.received_at, evaluatedAt),
                ago: formatAgo(snapshot.local.received_at, evaluatedAt),
              })}
            </span>
          </span>
          {health !== "healthy" ? <span className="foot-item c3">{health}</span> : null}
        </span>
        {onOpenSettings ? (
          <button
            type="button"
            className="settings-btn"
            aria-label={tx("side.settings")}
            title={tx("side.settings_title")}
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

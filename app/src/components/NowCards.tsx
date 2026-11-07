/**
 * "Now": what agents are doing on this project and what waits for the human.
 * One card per step in progress (or stopped mid-way) and one amber card per
 * open item that needs you. The time line marks the agent's window and the
 * current moment; it never claims a percentage done.
 */

import { needQuestion } from "../i18n/msg";
import { tx } from "../i18n";
import type { FeatureState, NeedsYouItem, PresenceRow, ProjectState } from "../api/types";
import { formatAge, formatAgo } from "../api/format";
import { isRadarCandidate } from "../graph/display";
import { unblockLabels } from "../state/project";
import { Icon } from "./Icon";
import { harnessName, orderedPresence, presenceText } from "./PresenceRows";

const HARNESS: Record<string, string> = {
  claude_code: "Claude Code",
  codex: "Codex",
  pi: "pi",
  herdr: "Herdr",
  opencode: "opencode",
  cursor: "Cursor",
  dsh: "dsh",
  agy: "Antigravity",
  gemini: "Gemini CLI",
  generic: "Agent",
};

export function harnessLabel(harness: string | undefined): string {
  return harness ? (HARNESS[harness] ?? harness.replace("_", " ")) : "Agent";
}

/** Steps that are moving now: doing, or stopped before they finished. */
export function isNow(feature: FeatureState): boolean {
  if (feature.progress === "doing") return true;
  return (
    feature.liveness === "lost" &&
    feature.progress !== "done" &&
    feature.progress !== "verified" &&
    feature.progress !== "dropped"
  );
}

function clock(ts: string): string {
  const date = new Date(ts);
  return Number.isNaN(date.getTime())
    ? ""
    : date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
}

function minutes(ms: number): string {
  const total = Math.max(1, Math.round(ms / 60000));
  return total >= 60
    ? tx("time.hour_min", { h: Math.floor(total / 60), m: total % 60 })
    : tx("now.minutes", { n: total });
}

/**
 * A labelled time line: when the step started, the window the agent said it
 * would finish in, and now. Every mark carries its own time, so it reads
 * without a legend; it shows where things are in time, never a percentage.
 */
function Timeline({ feature, anchor }: { feature: FeatureState; anchor: string }) {
  const eta = feature.eta;
  if (!eta) return null;
  // The same window everywhere (sidebar, plan, brief): history-adjusted when
  // calibrated, else the agent's own claim.
  const calibrated = eta.likely_finish !== undefined;
  const window = eta.likely_finish ?? eta.agent_finish;
  const start = Date.parse(eta.anchor);
  const lo = Date.parse(window.earliest);
  const hi = Date.parse(window.latest);
  const now = Date.parse(anchor);
  if ([start, lo, hi, now].some(Number.isNaN)) return null;
  const end = Math.max(hi, now);
  const span = end - start || 1;
  const pct = (value: number) => Math.max(0, Math.min(100, ((value - start) / span) * 100));
  const mid = (pct(lo) + pct(hi)) / 2;
  // A stopped step has nobody on it: time against its window says nothing
  // about progress, so neither "on track" nor "late" is claimed.
  const stopped = feature.liveness === "lost";
  const late = !stopped && now > hi;
  const status = stopped
    ? tx("now.unattended")
    : eta.paused
    ? tx("now.on_hold")
    : late
      ? tx("now.late", { d: minutes(now - hi) })
      : now >= lo
        ? tx("now.inside", { d: minutes(hi - now) })
        : tx("now.on_track", { lo: minutes(lo - now), hi: minutes(hi - now) });
  return (
    <span className="ntl">
      <span
        className="ntl-win-label"
        // Anchor to the nearer edge so a long label never leaves the card.
        style={
          mid < 35
            ? { left: 0, transform: "none" }
            : mid > 65
              ? { right: 0, transform: "none" }
              : { left: `${mid}%` }
        }
      >
        {calibrated ? tx("now.likely") : tx("now.agent_expects")} {clock(window.earliest)}–{clock(window.latest)}
      </span>
      <span
        className="ntl-track"
        role="img"
        aria-label={tx("now.track", {
          start: clock(eta.anchor),
          a: clock(window.earliest),
          b: clock(window.latest),
          now: clock(anchor),
          status,
        })}
      >
        <span className="ntl-win" style={{ left: `${pct(lo)}%`, width: `${pct(hi) - pct(lo)}%` }} />
        <span className={`ntl-now${late ? " late" : ""}`} style={{ left: `${pct(now)}%` }} />
      </span>
      <span className="ntl-ends">
        <span>{tx("now.started", { time: clock(eta.anchor) })}</span>
        <span>{tx("now.now", { time: clock(anchor) })}</span>
      </span>
      <span className={`ntl-status${late ? " late" : ""}`}>
        <Icon name="clock" />
        {status}
      </span>
    </span>
  );
}

/**
 * When the window is history-adjusted, the agent's own claim, the ratio and
 * how many finished steps it rests on stay visible next to it.
 */
function calibratedLine(feature: FeatureState, project: ProjectState): string {
  const eta = feature.eta;
  if (!eta?.likely_finish || !eta.ratio_milli) return "";
  const session = feature.session ? project.sessions[feature.session] : undefined;
  const samples = project.summary.calibration.find((item) => item.harness === session?.harness)?.samples;
  const ratio = (eta.ratio_milli / 1000).toFixed(2).replace(/0+$/, "").replace(/\.$/, "");
  return (
    tx("now.adjusted", { a: clock(eta.agent_finish.earliest), b: clock(eta.agent_finish.latest), ratio }) +
    (samples ? (samples === 1 ? tx("now.adjusted_from_one") : tx("now.adjusted_from", { n: samples })) : "")
  );
}

export function NowCards({
  project,
  anchor,
  needs,
  selected,
  onSelect,
  onOpenNeed,
  handoffs = {},
  pulseAges = {},
  motion = false,
  observed = [],
}: {
  /** Agents in this project's folders seen without the skill. */
  observed?: PresenceRow[];
  /** Motion allowed now: a live step's radar sweeps, as in the plan. */
  motion?: boolean;
  project: ProjectState;
  anchor: string;
  /** Latest pulse time per `${project}/${node}`. */
  pulseAges?: Readonly<Record<string, string>>;
  needs: NeedsYouItem[];
  selected: string | null;
  onSelect: (node: string | null) => void;
  onOpenNeed?: (needId: string) => void;
  /** Stopped steps whose hand-off item was dismissed: node → that item, to reopen it. */
  handoffs?: Readonly<Record<string, string>>;
}) {
  const moving = Object.values(project.features)
    .filter(isNow)
    .sort((a, b) => a.title.localeCompare(b.title));
  if (!moving.length && !needs.length && !observed.length) {
    return <div className="muted-note">Nothing is moving right now.</div>;
  }
  return (
    <div className="now">
      {orderedPresence(observed).map((row) => (
        <div key={row.key} className={`ncard observed${row.state === "working" ? "" : " you"}`}>
          <span className="ntop">
            <span className="chip">
              <Icon name="bot" />
              {harnessName(row.harness)}
            </span>
            <span className="nstate">no skill</span>
          </span>
          <span className="nt">{presenceText(row).replace(/^./, (first) => first.toUpperCase())}</span>
          <span className="nline">
            in {row.folder} · {formatAgo(row.since, anchor)}
          </span>
        </div>
      ))}
      {needs.map((need) => (
        <button
          type="button"
          key={need.id}
          className="ncard you"
          data-need={need.id}
          onClick={() => onOpenNeed?.(need.id)}
        >
          <span className="ntop">
            <span className="gd you" aria-hidden="true">
              <Icon name="hand" />
            </span>
            <span>{tx("now.waits")}</span>
          </span>
          <span className="nt">{needQuestion(need)}</span>
          <span className="nline">
            {need.stalled_agents
              ? need.stalled_agents === 1
                ? tx("now.agents_waiting.one", { ago: formatAgo(need.since, anchor) })
                : tx("now.agents_waiting.other", { n: need.stalled_agents, ago: formatAgo(need.since, anchor) })
              : tx("now.asked", { ago: formatAgo(need.since, anchor) })}
          </span>
          <span className="ndeps nopen">
            {tx("now.open_answer")}
            <Icon name="arrow-right" />
          </span>
        </button>
      ))}
      {moving.map((feature) => {
        const session = feature.session ? project.sessions[feature.session] : undefined;
        const stopped = feature.liveness === "lost";
        // No event from the agent for over 15 minutes: say so instead of "Doing".
        // A paused step or one waiting on the human is quiet on purpose.
        const quiet =
          !stopped && feature.liveness === "quiet" && !feature.paused && feature.gate === "open";
        const silentFor = quiet ? formatAge(session?.last_event_ts, anchor) : "";
        const unblocks = unblockLabels(feature, project);
        const id = `feature:${feature.node}`;
        const pulsed = pulseAges[`${project.summary.slug}/${feature.node}`];
        const handoff = stopped && onOpenNeed ? handoffs[feature.node] : undefined;
        return (
          <button
            type="button"
            key={feature.node}
            className="ncard"
            data-feat={feature.node}
            aria-pressed={selected === id}
            onClick={() => (handoff ? onOpenNeed?.(handoff) : onSelect(selected === id ? null : id))}
          >
            <span className="ntop">
              <span
                className={`gd ${stopped ? "stopped" : quiet ? "quiet" : "doing"}${
                  motion && isRadarCandidate(feature) ? " live" : ""
                }`}
                aria-hidden="true"
              >
                {stopped ? <Icon name="pause-circle" /> : null}
              </span>
              <span className="chip">
                <Icon name="bot" />
                {harnessLabel(session?.harness)}
              </span>
              <span className={`nstate${quiet ? " silent" : ""}`}>
                {stopped ? tx("now.stopped") : quiet && silentFor ? tx("now.no_word", { d: silentFor }) : tx("now.doing")}
              </span>
            </span>
            <span className="nt">{feature.title}</span>
            <Timeline feature={feature} anchor={anchor} />
            {/* Two fixed single-line rows, present on every card so rows align. */}
            <span className="nline" title={calibratedLine(feature, project) || undefined}>
              {calibratedLine(feature, project) || "\u00a0"}
            </span>
            <span className="nline">{pulsed ? tx("now.last_pulse", { ago: formatAgo(pulsed, anchor) }) : "\u00a0"}</span>
            {handoff ? (
              <span className="ndeps nopen">
                {tx("now.handoff")}
                <Icon name="arrow-right" />
              </span>
            ) : unblocks.length ? (
              <span className="ndeps">
                <Icon name="arrow-right" />
                {tx("now.unblocks", { list: unblocks.join(", ") })}
              </span>
            ) : null}
          </button>
        );
      })}
    </div>
  );
}

/**
 * Header statistics: who is working now and how fast steps get finished in
 * wall time. Every number says what it counts; nothing is estimated here.
 */

import { tx } from "../i18n";
import type { ProjectStats } from "../api/types";

function duration(ms: number): string {
  const minutes = Math.round(ms / 60000);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest ? `${hours}h ${rest}m` : `${hours}h`;
}

/** 950, 12.4K, 3.1M, 1.2B */
function compact(value: number): string {
  if (value < 1_000) return String(value);
  const units: [number, string][] = [
    [1e9, "B"],
    [1e6, "M"],
    [1e3, "K"],
  ];
  const [size, unit] = units.find(([size]) => value >= size)!;
  const scaled = value / size;
  return `${scaled >= 100 ? Math.round(scaled) : scaled.toFixed(1).replace(/\.0$/, "")}${unit}`;
}

interface Stat {
  key: string;
  value: string;
  label: string;
  note?: string;
  /** The full breakdown for the hover text, when the note is shortened to fit. */
  detail?: string;
}

export function ProjectStatsRow({ stats }: { stats: ProjectStats | undefined }) {
  if (!stats) return null;
  const items: Stat[] = [
    {
      key: "agents",
      value: String(stats.agents_running),
      label: stats.agents_running === 1 ? tx("stat.agent_working") : tx("stat.agents_working"),
      ...(stats.agents_stalled || stats.agents_idle
        ? {
            note: [
              stats.agents_stalled ? tx("stat.waiting_you", { n: stats.agents_stalled }) : "",
              stats.agents_idle ? tx("stat.idle", { n: stats.agents_idle }) : "",
            ]
              .filter(Boolean)
              .join(" · "),
          }
        : {}),
    },
    { key: "finished", value: String(stats.finished_24h), label: tx("stat.finished") },
    {
      key: "time",
      value: stats.finished_24h ? duration(stats.agent_ms_24h) : "—",
      label: tx("stat.time"),
    },
    {
      key: "median",
      value: stats.median_step_ms !== undefined ? duration(stats.median_step_ms) : "—",
      label: tx("stat.median"),
      note: stats.timed_steps === 1 ? tx("stat.of_step") : tx("stat.of_steps", { n: stats.timed_steps }),
    },
    {
      key: "tokens",
      value: stats.tokens ? compact(stats.tokens.input + stats.tokens.cached + stats.tokens.output) : "—",
      label: tx("stat.tokens"),
      // The tile fits one short note; the output count goes to the hover text.
      note: stats.tokens ? tx("stat.tokens_cached", { cached: compact(stats.tokens.cached) }) : tx("stat.no_logs"),
      ...(stats.tokens
        ? { detail: tx("stat.tokens_note", { cached: compact(stats.tokens.cached), out: compact(stats.tokens.output) }) }
        : {}),
    },
  ];
  return (
    <dl className="pstats">
      {items.map((item) => (
        <div key={item.key} className="pstat" data-stat={item.key} title={item.detail ?? item.note ? `${item.label}: ${item.detail ?? item.note}` : item.label}>
          <dt>{item.label}</dt>
          <dd>
            <span className="pstat-v">{item.value}</span>
            {/* Every tile keeps the note line so values align across the row. */}
            <span className="pstat-n">{item.note ?? "\u00a0"}</span>
          </dd>
        </div>
      ))}
    </dl>
  );
}

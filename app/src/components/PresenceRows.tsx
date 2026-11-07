/**
 * Agents seen without the skill (observe-only hooks). A row says what the
 * agent is doing and since when; it is never a question and Agendar cannot act
 * on it — the human goes to that agent.
 */

import { tx, type MessageKey } from "../i18n";
import { formatAgo } from "../api/format";
import type { PresenceRow, Snapshot } from "../api/types";
import { Icon, type IconName } from "./Icon";

const HARNESS: Record<string, string> = {
  claude_code: "Claude Code",
  codex: "Codex",
  pi: "pi",
  opencode: "opencode",
  cursor: "Cursor",
  dsh: "dsh",
  agy: "Antigravity",
  gemini: "Gemini CLI",
};

const STATE: Record<PresenceRow["state"], { text: MessageKey; icon: IconName }> = {
  waiting: { text: "pres.waiting", icon: "hourglass" },
  permission: { text: "pres.permission", icon: "hand" },
  working: { text: "pres.working", icon: "bot" },
};

export function harnessName(harness: string): string {
  return HARNESS[harness] ?? harness;
}

export function presenceText(row: PresenceRow): string {
  return tx(STATE[row.state].text);
}

/** Waiting and permission rows first: those are the ones that need the human. */
export function orderedPresence(rows: PresenceRow[]): PresenceRow[] {
  const rank = (row: PresenceRow) => (row.state === "permission" ? 0 : row.state === "waiting" ? 1 : 2);
  return [...rows].sort((a, b) => rank(a) - rank(b) || a.since.localeCompare(b.since));
}

export function PresenceRows({
  rows,
  snapshot,
  anchor,
}: {
  rows: PresenceRow[];
  snapshot: Snapshot;
  anchor: string;
}) {
  if (!rows.length) return null;
  return (
    <ul className="presence" aria-label={tx("pres.label")}>
      {orderedPresence(rows).map((row) => {
        const project = row.project ? snapshot.state.projects[row.project]?.summary.title : undefined;
        return (
          <li key={row.key} className={`presence-row ${row.state}`}>
            <Icon name={STATE[row.state].icon} />
            <span className="presence-main">
              <b>{harnessName(row.harness)}</b>
              {tx("pres.in", { where: project ?? row.folder, state: presenceText(row) })}
            </span>
            <span className="presence-age">{formatAgo(row.since, anchor)}</span>
          </li>
        );
      })}
    </ul>
  );
}

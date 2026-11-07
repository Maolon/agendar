/**
 * Reports: what the board cannot say in a line. The app shows the agent's
 * summary; the full document opens in the person's own browser, never inside
 * the app.
 */

import { tx } from "../i18n";
import { useState } from "react";
import { formatAgo } from "../api/format";
import { useClient } from "../api/store";
import type { BlobRef, ReportState } from "../api/types";
import { Icon } from "./Icon";
import { harnessName } from "./PresenceRows";

/** "Open report": hands the Vault document to the system browser. */
export function ReportLink({
  project,
  blob,
  label = tx("rep.open"),
}: {
  project: string;
  blob: BlobRef;
  label?: string;
}) {
  const client = useClient();
  const [failure, setFailure] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  async function open(): Promise<void> {
    setPending(true);
    setFailure(null);
    try {
      await client.openReport({ project, blob });
    } catch (cause) {
      setFailure(cause instanceof Error ? cause.message : tx("rep.failed"));
    } finally {
      setPending(false);
    }
  }
  return (
    <span className="report-link">
      <button type="button" className="link" disabled={pending} onClick={() => void open()}>
        <Icon name="file-text" />
        {label}
      </button>
      {failure ? (
        <span className="c-red" role="alert">
          {failure}
        </span>
      ) : null}
    </span>
  );
}

export function ReportCards({
  project,
  reports,
  anchor,
}: {
  project: string;
  reports: ReportState[];
  anchor: string;
}) {
  const [expanded, setExpanded] = useState(false);
  if (!reports.length) return null;
  // Newest first; collapsed, only the newest shows, on one line.
  const ordered = [...reports].sort((a, b) => Date.parse(b.ts) - Date.parse(a.ts));
  const latest = ordered[0]!;
  const toggle = (
    <button type="button" className="link rtoggle" aria-expanded={expanded} onClick={() => setExpanded((open) => !open)}>
      {expanded
        ? tx("rep.less")
        : ordered.length > 1
          ? tx("rep.all", { n: ordered.length })
          : tx("rep.summary")}
    </button>
  );
  if (!expanded) {
    const firstLine = latest.summary.split("\n").find((line) => line.trim()) ?? "";
    return (
      <div className="rline">
        <span className="rl-text" title={`${latest.title} — ${latest.summary}`}>
          <b>{latest.title}</b>
          {firstLine ? <span className="rl-sum"> — {firstLine}</span> : null}
        </span>
        <span className="rmeta">
          {harnessName(latest.harness)} · {formatAgo(latest.ts, anchor)}
        </span>
        <ReportLink project={project} blob={latest.report} />
        {toggle}
      </div>
    );
  }
  return (
    <>
    <div className="reports">
      {ordered.map((report) => (
        <article key={report.id} className="rcard">
          <h4 className="rt">{report.title}</h4>
          <p className="rs">{report.summary}</p>
          <span className="rmeta">
            {harnessName(report.harness)} · {formatAgo(report.ts, anchor)}
            {report.node ? ` · ${report.node}` : ""}
          </span>
          <ReportLink project={project} blob={report.report} />
        </article>
      ))}
    </div>
    <div className="rtoggle-row">{toggle}</div>
    </>
  );
}

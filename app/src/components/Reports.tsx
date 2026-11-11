/**
 * Reports: what the board cannot say in a line. The app shows the agent's
 * summary; "Read" shows the full document as escaped text in the app, and
 * "Open report" hands it to the person's own browser.
 */

import { tx, type MessageKey } from "../i18n";
import { useState } from "react";
import { formatAgo } from "../api/format";
import { useClient } from "../api/store";
import type { BlobRef, Domain, ReportKind, ReportState } from "../api/types";
import { Icon, type IconName } from "./Icon";
import { harnessName } from "./PresenceRows";
import { ReportPanel } from "./Report";

type Tone = "proposal" | "finding" | "plain";

const KIND: Record<ReportKind | "none", { icon: IconName; label: MessageKey; tone: Tone }> = {
  proposal: { icon: "columns-2", label: "rep.kind.proposal", tone: "proposal" },
  finding: { icon: "search", label: "rep.kind.finding", tone: "finding" },
  none: { icon: "file-text", label: "rep.kicker", tone: "plain" },
};

const DOMAIN: Record<Domain, MessageKey> = {
  arch: "rep.domain.arch",
  code: "rep.domain.code",
  ux: "rep.domain.ux",
};

/** "Proposal · Architecture": what a report asks of you, then what it is about. */
function kindText(kind: ReportKind | undefined, domain: Domain | undefined): string {
  const label = tx(KIND[kind ?? "none"].label);
  return domain ? `${label} · ${tx(DOMAIN[domain])}` : label;
}

/** The kind as a small coloured label with its icon, for lists. */
export function ReportKindLabel({ kind, domain }: { kind?: ReportKind | undefined; domain?: Domain | undefined }) {
  const meta = KIND[kind ?? "none"];
  return (
    <span className={`rkind rk-${meta.tone}`}>
      <Icon name={meta.icon} />
      {kindText(kind, domain)}
    </span>
  );
}

/** Hands a Vault document to the system browser; remembers a failure to show. */
function useOpenReport(project: string, blob: BlobRef) {
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
  return { open: () => void open(), failure, pending };
}

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
  const { open, failure, pending } = useOpenReport(project, blob);
  return (
    <span className="report-link">
      <button type="button" className="link" disabled={pending} onClick={open}>
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

/** The report a question links: title and summary in view, one button opens it. */
export function AttachedReport({
  project,
  blob,
  info,
}: {
  project: string;
  blob: BlobRef;
  /** The published report with this document, when the project lists one. */
  info?: ReportState | undefined;
}) {
  const { open, failure, pending } = useOpenReport(project, blob);
  const title = info?.title ?? tx("rep.attached");
  const meta = KIND[info?.kind ?? "none"];
  // A question's report without a kind still reads as the reason to choose.
  const tone: Tone = meta.tone === "plain" ? "proposal" : meta.tone;
  return (
    <section className="report-att" aria-label={title}>
      <span className={`ra-ic rk-${tone}`} aria-hidden="true">
        <Icon name={meta.icon} size={20} />
      </span>
      <div className="ra-body">
        <span className={`ra-kicker rk-${tone}`}>{kindText(info?.kind, info?.domain)}</span>
        <span className="ra-title">{title}</span>
        {info?.summary ? <p className="ra-sum">{info.summary}</p> : null}
        {failure ? (
          <span className="c-red" role="alert">
            {failure}
          </span>
        ) : null}
      </div>
      <button type="button" className="btn ra-open" disabled={pending} onClick={open}>
        {tx("rep.open")}
        <Icon name="arrow-right" />
      </button>
    </section>
  );
}

/** "Read": the full report as escaped text in the in-app panel. */
function ReadButton({ report, onRead }: { report: ReportState; onRead: (report: ReportState) => void }) {
  return (
    <button
      type="button"
      className="link rread"
      aria-haspopup="dialog"
      aria-label={tx("rep.read_label", { x: report.title })}
      onClick={() => onRead(report)}
    >
      {tx("rep.read")}
    </button>
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
  const [reading, setReading] = useState<ReportState | null>(null);
  if (!reports.length) return null;
  // Newest first; collapsed, only the newest shows, on one line.
  const ordered = [...reports].sort((a, b) => Date.parse(b.ts) - Date.parse(a.ts));
  const latest = ordered[0]!;
  const panel = reading ? (
    <ReportPanel project={project} blob={reading.report} title={reading.title} onClose={() => setReading(null)} />
  ) : null;
  const toggle =
    ordered.length > 1 ? (
      <button type="button" className="link rtoggle" aria-expanded={expanded} onClick={() => setExpanded((open) => !open)}>
        {expanded ? tx("rep.less") : tx("rep.all", { n: ordered.length })}
      </button>
    ) : null;
  if (!expanded) {
    const firstLine = latest.summary.split("\n").find((line) => line.trim()) ?? "";
    return (
      <>
      <div className="rline">
        <ReportKindLabel kind={latest.kind} domain={latest.domain} />
        <span className="rl-text" title={`${latest.title} — ${latest.summary}`}>
          <b>{latest.title}</b>
          {firstLine ? <span className="rl-sum"> — {firstLine}</span> : null}
        </span>
        <span className="rmeta">
          {harnessName(latest.harness)} · {formatAgo(latest.ts, anchor)}
        </span>
        <ReadButton report={latest} onRead={setReading} />
        <ReportLink project={project} blob={latest.report} />
        {toggle}
      </div>
      {panel}
      </>
    );
  }
  return (
    <>
    <div className="reports">
      {ordered.map((report) => (
        <article key={report.id} className="rcard">
          <ReportKindLabel kind={report.kind} domain={report.domain} />
          <h4 className="rt">{report.title}</h4>
          <p className="rs">{report.summary}</p>
          <span className="rmeta">
            {harnessName(report.harness)} · {formatAgo(report.ts, anchor)}
            {report.node ? ` · ${report.node}` : ""}
          </span>
          <span className="ractions">
            <ReadButton report={report} onRead={setReading} />
            <ReportLink project={project} blob={report.report} />
          </span>
        </article>
      ))}
    </div>
    <div className="rtoggle-row">{toggle}</div>
    {panel}
    </>
  );
}

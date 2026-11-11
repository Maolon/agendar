import { useState, type ReactNode } from "react";
import { needQuestion } from "../i18n/msg";
import { shortcut } from "../state/shortcut";
import { tx, type MessageKey } from "../i18n";
import { formatAge, formatAgo, formatClockTime } from "../api/format";
import type { ProjectSummary, Snapshot } from "../api/types";
import { harnessLabel } from "../components/NowCards";
import { Icon } from "../components/Icon";
import { PresenceRows } from "../components/PresenceRows";
import { CorrectionMark } from "../components/CorrectionMark";
import { ProjectAvatar } from "../components/ProjectAvatar";
import {
  DONE_CAP,
  boardEmpty,
  dueText,
  lateText,
  nowBoard,
  stalledCount,
  windowText,
  type MovingStep,
  type NeedRow,
  type StalledStep,
} from "../state/now";
import type { Selection } from "../state/ui";

const AGENT_TEXT: Record<StalledStep["agent"], MessageKey> = {
  quiet: "dash.agent.quiet",
  lost: "dash.agent.lost",
  none: "dash.agent.none",
  paused: "dash.agent.paused",
  sent_back: "dash.agent.sent_back",
  silent: "dash.agent.silent",
  abandoned: "dash.agent.abandoned",
};

/** "no word for 3h", "abandoned 1d ago", or the plain agent label. */
export function stalledText(step: StalledStep, anchor: string): string {
  if ((step.agent === "silent" || step.agent === "abandoned") && step.since) {
    return tx(AGENT_TEXT[step.agent], { d: formatAge(step.since, anchor) });
  }
  return tx(step.agent === "silent" ? "dash.agent.quiet" : step.agent === "abandoned" ? "dash.agent.none" : AGENT_TEXT[step.agent]);
}

interface BriefProps {
  snapshot: Snapshot;
  onSelect: (selection: Selection) => void;
  /** Motion allowed now: the live dot breathes. */
  motion?: boolean;
}

function Avatar({ project }: { project: ProjectSummary }) {
  return <ProjectAvatar glyph={project.glyph} project={project} size="sm" />;
}

function Section({ id, title, hint, children }: { id: string; title: string; hint?: string; children: ReactNode }) {
  return (
    <section className="dash-sec" data-dash={id}>
      <h3 className="dash-h">
        {title}
        {hint ? <span className="dash-hint">{hint}</span> : null}
      </h3>
      {children}
    </section>
  );
}

function NeedLine({ row, anchor, onSelect }: { row: NeedRow; anchor: string; onSelect: BriefProps["onSelect"] }) {
  const extra: string[] = [];
  if (row.holds.length) {
    const first = row.holds[0]!;
    extra.push(tx("dash.holds", { what: row.holds.length > 1 ? `${first} +${row.holds.length - 1}` : first }));
  }
  if (row.waiting.length) {
    const first = row.waiting[0]!;
    extra.push(tx("dash.waiting_answer", { what: row.waiting.length > 1 ? `${first} +${row.waiting.length - 1}` : first }));
  }
  if (row.defaultAt) {
    const t = formatClockTime(row.defaultAt);
    extra.push(row.defaultLabel ? tx("dash.default", { label: row.defaultLabel, t }) : tx("dash.default_rec", { t }));
  }
  return (
    <button
      type="button"
      className="dash-li"
      data-need={row.need.id}
      onClick={() => onSelect({ kind: "need", id: row.need.id })}
    >
      <span className="dash-you" aria-hidden="true">
        <Icon name="hand" />
      </span>
      <span className="dash-main">
        <span className="dash-t" title={needQuestion(row.need)}>
          {needQuestion(row.need)}
        </span>
        {extra.length ? <span className="dash-sub">{extra.join(" · ")}</span> : null}
      </span>
      <span className="dash-meta">
        {row.project ? `${row.project.title} · ` : ""}
        {formatAgo(row.need.since, anchor)}
      </span>
    </button>
  );
}

function MovingCard({
  step,
  anchor,
  motion,
  onSelect,
}: {
  step: MovingStep;
  anchor: string;
  motion: boolean;
  onSelect: BriefProps["onSelect"];
}) {
  const { bar } = step;
  const late = bar ? bar.lateS > 0 : false;
  const pct = bar ? Math.round(bar.share * 100) : 0;
  return (
    <button
      type="button"
      className="dash-move"
      data-feat={`${step.project.slug}/${step.feature.node}`}
      onClick={() => onSelect({ kind: "project", id: step.project.slug })}
    >
      <span className="dash-move-top">
        <Avatar project={step.project} />
        <span className="dash-move-t">{step.feature.title}</span>
        <span className="chip dash-agent">
          <span className={`dash-dot live${motion ? " breathe" : ""}`} aria-hidden="true" />
          {harnessLabel(step.harness)}
        </span>
      </span>
      <span className="dash-move-meta">
        <span>
          {step.startedAt
            ? tx("dash.started", { project: step.project.title, ago: formatAgo(step.startedAt, anchor) })
            : step.project.title}
        </span>
        {bar ? (
          <>
            <span
              className={`dash-bar${late ? " late" : ""}${bar.paused ? " paused" : ""}`}
              role="img"
              aria-label={tx("dash.bar", { pct })}
            >
              <i style={{ width: `${pct}%` }} />
            </span>
            {late ? <span className="chip dash-chip red">{tx("dash.late", { d: lateText(bar.lateS) })}</span> : null}
            {bar.paused ? <span className="chip dash-chip">{tx("dash.paused")}</span> : null}
            <span className="dash-when">{windowText(bar)}</span>
          </>
        ) : null}
      </span>
      {step.lastWord || step.hostHeard ? (
        <span className="dash-aging">
          {[
            step.lastWord ? tx("dash.last_word", { ago: formatAgo(step.lastWord, anchor) }) : "",
            step.hostHeard ? tx("dash.host_heard", { ago: formatAgo(step.hostHeard, anchor) }) : "",
          ]
            .filter(Boolean)
            .join(" · ")}
        </span>
      ) : null}
      {step.note ? <span className="dash-note">{step.note}</span> : null}
    </button>
  );
}

function StalledLine({
  step,
  slug,
  anchor,
  onSelect,
}: {
  step: StalledStep;
  slug: string;
  anchor: string;
  onSelect: BriefProps["onSelect"];
}) {
  const correction = step.feature.correction;
  const dot = step.agent === "lost" || step.agent === "abandoned" ? "lost" : step.agent === "quiet" || step.agent === "silent" ? "quiet" : "none";
  return (
    <button
      type="button"
      className={`dash-li${step.agent === "abandoned" ? " abandoned" : ""}`}
      data-feat={`${slug}/${step.feature.node}`}
      data-agent={step.agent}
      onClick={() =>
        onSelect(step.needId ? { kind: "need", id: step.needId } : { kind: "project", id: slug })
      }
    >
      <span className={`dash-dot ${dot}`} aria-hidden="true" />
      <span className="dash-t" title={step.feature.title}>
        {step.feature.title}
      </span>
      {correction ? <CorrectionMark correction={correction} /> : null}
      {step.lateS > 0 ? (
        <span className="chip dash-chip red">{tx("dash.late", { d: lateText(step.lateS) })}</span>
      ) : step.due ? (
        <span className="chip dash-chip">{tx("dash.due", { t: dueText(step.due, anchor) })}</span>
      ) : null}
      <span className="dash-meta">{stalledText(step, anchor)}</span>
    </button>
  );
}

/**
 * Home: since the last look, by state. Moving steps (a live agent on them) as
 * cards, stalled steps grouped by project, then what needs the human, what
 * finished and what is next. A section with nothing in it is not shown.
 */
export function Brief({ snapshot, onSelect, motion = false }: BriefProps) {
  const { preferences, state, local } = snapshot;
  const anchor = state.evaluated_at;
  const board = nowBoard(snapshot);
  const presence = snapshot.presence ?? [];
  const lookedAge = preferences.last_looked_at ? formatAge(preferences.last_looked_at, anchor) : "";
  const stalled = stalledCount(board);
  const accepted = board.done.filter((item) => item.kind === "accepted").length;
  const finished = board.done.length - accepted;
  const firstNeed = board.needs[0]?.need;
  const [closedOpen, setClosedOpen] = useState(false);

  const heading = (
    <h2 className="h">
      {tx("brief.since")} {lookedAge ? <span className="sub">{lookedAge}</span> : null}
    </h2>
  );

  if (boardEmpty(board) && !presence.length) {
    return (
      <div className="view">
        {heading}
        <p className="dash-quiet">
          {tx("dash.all_quiet")}
          {local.health !== "healthy" ? tx("brief.local", { health: local.health }) : ""}
        </p>
      </div>
    );
  }

  const parts: ReactNode[] = [];
  if (board.moving.length) parts.push(<b key="moving">{tx("dash.moving", { n: board.moving.length })}</b>);
  if (stalled) {
    parts.push(
      <span key="stalled">
        <b>{tx("dash.stalled", { n: stalled })}</b> {tx("dash.stalled_why")}
      </span>,
    );
  }
  parts.push(
    <span key="needs">
      {board.needs.length === 0
        ? tx("dash.needs.none")
        : board.needs.length === 1
          ? tx("dash.needs.one")
          : tx("dash.needs.other", { n: board.needs.length })}
    </span>,
  );
  if (accepted) parts.push(<span key="accepted">{tx("dash.accepted", { n: accepted })}</span>);
  if (finished) parts.push(<span key="finished">{tx("dash.finished", { n: finished })}</span>);

  const doneShown = board.done.slice(0, DONE_CAP);
  const doneMore = board.done.length - doneShown.length;

  return (
    <div className="view dash">
      <div>
        {heading}
        <p className="dash-sum" data-testid="dash-summary">
          {parts.flatMap((part, index) => (index ? [<span key={`sep${index}`}> · </span>, part] : [part]))}
        </p>
      </div>

      {board.needs.length ? (
        <Section id="needs" title={tx("dash.sec.needs")}>
          <div className="dash-list">
            {board.needs.map((row) => (
              <NeedLine key={row.need.id} row={row} anchor={anchor} onSelect={onSelect} />
            ))}
          </div>
          {firstNeed ? (
            <div className="acts" style={{ justifyContent: "flex-start" }}>
              <button
                type="button"
                className="btn primary"
                onClick={() => onSelect({ kind: "need", id: firstNeed.id })}
              >
                {tx("brief.start")} <kbd>{shortcut("mod+enter")}</kbd>
              </button>
            </div>
          ) : null}
        </Section>
      ) : null}

      {board.closed.length ? (
        <div className="dash-closed">
          <button type="button" className="more" onClick={() => setClosedOpen((open) => !open)}>
            {closedOpen ? tx("side.closed_hide") : tx("side.closed", { n: board.closed.length })}
          </button>
          {closedOpen ? (
            <div className="dash-list">
              {board.closed.map((row) => (
                <NeedLine key={row.need.id} row={row} anchor={anchor} onSelect={onSelect} />
              ))}
            </div>
          ) : null}
        </div>
      ) : null}

      {board.moving.length ? (
        <Section id="moving" title={tx("dash.sec.moving")} hint={tx("dash.hint.moving")}>
          <div className="dash-cards">
            {board.moving.map((step) => (
              <MovingCard
                key={`${step.project.slug}/${step.feature.node}`}
                step={step}
                anchor={anchor}
                motion={motion}
                onSelect={onSelect}
              />
            ))}
          </div>
        </Section>
      ) : null}

      {presence.length ? (
        <Section id="presence" title={tx("pres.label")}>
          <PresenceRows rows={presence} snapshot={snapshot} anchor={anchor} />
        </Section>
      ) : null}

      {board.stalled.length ? (
        <Section id="stalled" title={tx("dash.sec.stalled")} hint={tx("dash.hint.stalled")}>
          <div className="dash-list">
            {board.stalled.map((group) => (
              <div key={group.project.slug} className="dash-grp" data-project={group.project.slug}>
                <button
                  type="button"
                  className="dash-grp-h"
                  onClick={() => onSelect({ kind: "project", id: group.project.slug })}
                >
                  <Avatar project={group.project} />
                  <span className="dash-grp-t">{group.project.title}</span>
                  {group.risks.map((risk) => (
                    <span key={risk.text} className={`chip dash-chip ${risk.level}`} title={risk.why}>
                      {risk.text}
                    </span>
                  ))}
                </button>
                {group.steps.map((step) => (
                  <StalledLine
                    key={step.feature.node}
                    step={step}
                    slug={group.project.slug}
                    anchor={anchor}
                    onSelect={onSelect}
                  />
                ))}
              </div>
            ))}
          </div>
        </Section>
      ) : null}

      {board.done.length ? (
        <Section id="done" title={tx("dash.sec.done")}>
          <div className="dash-list">
            {doneShown.map((item) => (
              <button
                type="button"
                key={item.key}
                className="dash-li"
                onClick={() => onSelect({ kind: "project", id: item.project.slug })}
              >
                <span className="dash-check" aria-hidden="true">
                  <Icon name="check" />
                </span>
                <span className="dash-t" title={item.title}>
                  {item.title}
                </span>
                <span className="dash-meta">
                  {item.project.title} ·{" "}
                  {item.kind === "accepted"
                    ? item.ts
                      ? tx("dash.accepted_ago", { ago: formatAgo(item.ts, anchor) })
                      : tx("dash.accepted_plain")
                    : item.ts
                      ? tx("dash.finished_ago", { ago: formatAgo(item.ts, anchor) })
                      : tx("dash.finished_plain")}
                </span>
              </button>
            ))}
            {doneMore > 0 ? <div className="dash-more">{tx("dash.more", { n: doneMore })}</div> : null}
          </div>
        </Section>
      ) : null}

      {board.next.length ? (
        <Section id="next" title={tx("dash.sec.next")}>
          <div className="dash-list">
            {board.next.map((item) => (
              <button
                type="button"
                key={item.project.slug}
                className="dash-li"
                data-feat={`${item.project.slug}/${item.feature.node}`}
                onClick={() => onSelect({ kind: "project", id: item.project.slug })}
              >
                <Avatar project={item.project} />
                <span className="dash-t" title={item.feature.title}>
                  {item.feature.title}
                </span>
                <span className="dash-meta">
                  {item.project.title}
                  {item.more ? ` · +${item.more}` : ""}
                </span>
              </button>
            ))}
          </div>
        </Section>
      ) : null}
    </div>
  );
}

import { tx } from "../i18n";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { formatAgo } from "../api/format";
import type { DeliveryState, Demo, SentBackNote } from "../api/types";
import { pastComments } from "../state/comments";
import {
  anchorQuote,
  demoLabel,
  placeLabel,
  sinceRows,
  type Redelivery,
} from "../state/redelivery";
import { DemoView } from "./Demo";
import { DisclosureView } from "./Disclosure";
import { Icon } from "./Icon";
import { useReturnFocus } from "./MediaViewer";

/**
 * Top of a redelivery's page: the send-back it answers, each comment beside
 * the agent's reply. A comment opens the previous version at its spot.
 */
export function SinceSendBack({
  delivery,
  redelivery,
  anchor,
  onOpenPrevious,
}: {
  delivery: DeliveryState;
  redelivery: Redelivery;
  anchor: string;
  /** With a comment's number, the previous version opens at that comment. */
  onOpenPrevious: (comment?: number) => void;
}) {
  const { previous, note } = redelivery;
  const rows = sinceRows(note, delivery.replies);
  return (
    <section className="rd" aria-label={tx("rd.title")}>
      <div className="rd-h">
        <Icon name="refresh-cw" className="c2" />
        <span className="rd-h-text">
          <b>{tx("rd.title")}</b>
          <span className="rd-h-meta">{tx("rd.replaces", { ago: formatAgo(note.at, anchor) })}</span>
        </span>
        <button type="button" className="btn" onClick={() => onOpenPrevious()}>
          {tx("rd.previous")}
        </button>
      </div>
      {rows.length ? (
        <ol className="rd-rows">
          {rows.map((row, index) => (
            <li className="rd-row" key={row.n ?? `${row.kind}-${index}`}>
              {row.kind === "comment" && row.comment ? (
                <button
                  type="button"
                  className="rd-said rd-jump"
                  title={tx("rd.open_spot")}
                  onClick={() => onOpenPrevious(row.n)}
                >
                  <span className="rd-place">
                    <span className="cm-pin">{row.n}</span>
                    <span className="cm-loc">{placeLabel(row.comment.anchor, previous.demos)}</span>
                    <Icon name="arrow-right" className="rd-go" />
                  </span>
                  {anchorQuote(row.comment.anchor) ? (
                    <code className="rd-quote">{anchorQuote(row.comment.anchor)}</code>
                  ) : null}
                  <span className="rd-text">{row.text}</span>
                </button>
              ) : (
                <div className="rd-said">
                  <span className="rd-place">
                    <span className="cm-loc">{row.kind === "reason" ? tx("rd.reason") : tx("rd.note")}</span>
                    {row.text ? <span className="rd-text">{row.text}</span> : null}
                  </span>
                </div>
              )}
              <div className={row.reply ? "rd-reply" : "rd-reply none"} aria-label={tx("rd.reply")}>
                <Icon name="corner-down-left" />
                <span>{row.reply ?? tx("rd.no_reply")}</span>
              </div>
            </li>
          ))}
        </ol>
      ) : null}
    </section>
  );
}

/** An unchanged demo: one line that opens in place. */
export function SameDemo({ demo, children }: { demo: Demo; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="rd-same">
      <button
        type="button"
        className="rd-same-row"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <Icon name="chevron-right" className={open ? "rd-chev open" : "rd-chev"} />
        <span className="rd-same-label">{tx("rd.same")}</span>
        <span className="rd-same-name">{demoLabel(demo)}</span>
      </button>
      {open ? children : null}
    </div>
  );
}

/** The "New" tag on a demo or a disclosure line. */
export function NewTag() {
  return <span className="rd-new">{tx("rd.new")}</span>;
}

/**
 * The replaced delivery, read-only, over the whole window: its demos carry
 * the comments it was sent back with. `open` is the comment it opens on.
 */
export function PreviousVersion({
  project,
  previous,
  note,
  open = null,
  onClose,
}: {
  project: string;
  previous: DeliveryState;
  note: SentBackNote;
  open?: number | null;
  onClose: () => void;
}) {
  const closeRef = useRef<HTMLButtonElement | null>(null);
  useReturnFocus();
  useEffect(() => {
    // A comment opened on (its line, or the image viewer) keeps the focus it took.
    if (document.activeElement?.closest("[role='dialog']")) return;
    closeRef.current?.focus();
  }, []);
  const comments = note.comments ?? [];
  const title = tx("rd.prev_title", { x: previous.name });
  return createPortal(
    <div
      className="rd-backdrop"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          // Keep the window policy from also clearing the selected need.
          event.stopPropagation();
          event.preventDefault();
          onClose();
          return;
        }
        if (["ArrowDown", "ArrowUp", " ", "PageUp", "PageDown"].includes(event.key)) {
          event.stopPropagation();
        }
      }}
    >
      <section className="rd-prev" role="dialog" aria-modal="true" aria-label={title} tabIndex={-1}>
        <div className="report-h">
          <h3>{title}</h3>
          <button
            type="button"
            className="btn"
            ref={closeRef}
            onClick={onClose}
            aria-label={tx("rd.close_label")}
          >
            <Icon name="x" />
            {tx("rp.close")}
          </button>
        </div>
        <div className="rd-prev-body">
          {previous.demos.map((demo, index) => (
            <DemoView
              key={`${previous.id}-demo-${index}`}
              project={project}
              demo={demo}
              comments={comments.length ? pastComments(comments, index, open) : undefined}
            />
          ))}
          <DisclosureView disclosure={previous.disclosure} />
        </div>
      </section>
    </div>,
    document.body,
  );
}

import { tx } from "../i18n";
import { useState } from "react";
import type { DeliveryState, Demo, NeedsYouItem } from "../api/types";
import { useBlob } from "../hooks/useBlob";
import {
  deliveryStatus,
  demoAlt,
  demoBlob,
  mediaUrl,
  newestFirst,
  thumbnailDemo,
} from "../state/delivery";
import { diffBody, parseHunks, splitPath, type DiffLine } from "../state/diff";
import { Icon } from "./Icon";
import { MediaViewer, type ViewerItem } from "./MediaViewer";

/** Cards shown before "Show all": two rows of three, three rows of two. */
export const EVIDENCE_SHOWN = 6;

/**
 * The project's delivery cards, newest first, the latest few at a time. A
 * delivery still waiting for review is never folded away.
 */
export function EvidenceCards({
  project,
  deliveries,
  needs,
  onOpenNeed,
}: {
  project: string;
  deliveries: DeliveryState[];
  needs: NeedsYouItem[];
  onOpenNeed?: (needId: string) => void;
}) {
  const [all, setAll] = useState(false);
  if (!deliveries.length) return null;
  const ordered = newestFirst(deliveries);
  const shown = all
    ? ordered
    : ordered.filter((delivery, index) => index < EVIDENCE_SHOWN || delivery.review === "pending");
  return (
    <>
      <div className="evs">
        {shown.map((delivery) => {
          const need = needs.find((item) => item.kind === "review" && item.source === delivery.id);
          return (
            <EvidenceCard
              key={delivery.id}
              project={project}
              delivery={delivery}
              {...(need && onOpenNeed ? { onOpen: () => onOpenNeed(need.id) } : {})}
            />
          );
        })}
      </div>
      {ordered.length > EVIDENCE_SHOWN ? (
        <button
          type="button"
          className="link evs-more"
          aria-expanded={all}
          onClick={() => setAll((value) => !value)}
        >
          {all ? tx("ev.less") : tx("ev.all", { n: ordered.length })}
        </button>
      ) : null}
    </>
  );
}

interface EvidenceCardProps {
  project: string;
  delivery: DeliveryState;
  /** Navigate to the review when this delivery still needs a human. */
  onOpen?: () => void;
}

/**
 * One "What changed" evidence card. The thumbnail keeps the image's own
 * colours, so it matches the full view; clicking it opens the demo full-window (image
 * viewer, transcript reader or diff). The caption leads with the human's
 * verdict once there is one; while the delivery waits, with how proven it is.
 */
export function EvidenceCard({ project, delivery, onOpen }: EvidenceCardProps) {
  const status = deliveryStatus(delivery);
  const thumbnail = <Thumbnail project={project} delivery={delivery} />;
  return (
    <div className="ev" data-delivery={delivery.id} data-review={delivery.review}>
      {thumbnail}
      <div className="cap">
        <Icon name={status.icon} className={status.tone} />
        <span className="cap-t">
          <span className="cap-name">{delivery.name}</span>
          <span className="cap-meta">{status.text}</span>
        </span>
        {onOpen ? (
          <button type="button" className="link" onClick={onOpen}>
            {tx("tray.open")}
          </button>
        ) : null}
      </div>
    </div>
  );
}

function Thumbnail({ project, delivery }: { project: string; delivery: DeliveryState }) {
  const demo = thumbnailDemo(delivery);
  const [viewing, setViewing] = useState(false);
  if (!demo) {
    return (
      <div className="thumb thumb-empty">
        <Icon name="file-text" className="c3" />
        <span>{tx("ev.no_demo")}</span>
      </div>
    );
  }
  if (demo.kind !== "screenshot" && demo.kind !== "cli_transcript" && demo.kind !== "diff") {
    return (
      <div className="thumb thumb-empty">
        <Icon name="file-video" className="c3" />
        <span>{tx("ev.demo", { kind: demo.kind.replace("_", " ") })}</span>
      </div>
    );
  }
  // Every demo of the same kind opens in one viewer, ←/→ between them.
  const items: ViewerItem[] = delivery.demos
    .filter((item) => item.kind === demo.kind)
    .map((item): ViewerItem =>
      item.kind === "screenshot"
        ? { kind: "image", blob: demoBlob(item), title: demoAlt(item) }
        : item.kind === "diff"
          ? { kind: "diff", demo: item, title: `${delivery.name} · ${tx("mv.diff")}` }
          : { kind: "text", blob: demoBlob(item), title: `${delivery.name} · ${tx("mv.transcript")}` },
    );
  const open = () => setViewing(true);
  return (
    <>
      {demo.kind === "screenshot" ? (
        <ScreenshotThumb project={project} demo={demo} onOpen={open} />
      ) : demo.kind === "diff" ? (
        <DiffThumb project={project} demo={demo} onOpen={open} />
      ) : (
        <TranscriptThumb project={project} demo={demo} onOpen={open} />
      )}
      {viewing ? <MediaViewer project={project} items={items} onClose={() => setViewing(false)} /> : null}
    </>
  );
}

interface ThumbProps {
  project: string;
  demo: Demo;
  onOpen: () => void;
}

function ScreenshotThumb({ project, demo, onOpen }: ThumbProps) {
  const view = useBlob(project, demoBlob(demo));
  const alt = demoAlt(demo);
  if (view.phase === "loading") return <div className="thumb thumb-empty">{tx("shell.loading")}</div>;
  if (view.phase === "error" || !view.token) {
    return (
      <div className="thumb thumb-empty" role="note">
        <Icon name="file-text" className="c3" />
        <span>{view.message ?? tx("ev.shot_unreadable")}</span>
      </div>
    );
  }
  return (
    <button
      type="button"
      className="ev-thumb"
      aria-haspopup="dialog"
      aria-label={tx("ev.open_shot", { x: alt })}
      onClick={onOpen}
    >
      <img className="ev-img" src={mediaUrl(view.token)} alt={alt} loading="lazy" />
    </button>
  );
}

function TranscriptThumb({ project, demo, onOpen }: ThumbProps) {
  const view = useBlob(project, demoBlob(demo));
  const lines = (view.text ?? "").split("\n").slice(0, 3);
  return (
    <button
      type="button"
      className="ev-thumb"
      aria-haspopup="dialog"
      aria-label={tx("ev.expand")}
      onClick={onOpen}
    >
      <span className="thumb th-term">
        {view.phase === "loading"
          ? tx("ev.reading")
          : view.phase === "error"
            ? (view.message ?? tx("ev.read_failed"))
            : lines.join("\n")}
      </span>
    </button>
  );
}

/** Lines a diff thumbnail shows: the first shown hunk's opening rows. */
const DIFF_THUMB_LINES = 5;
const DIFF_MARK: Record<DiffLine["kind"], string> = { add: "+", del: "-", ctx: " ", note: "" };

/** The rows around the first change: one line of context (unless blank), then the change. */
export function thumbLines(lines: DiffLine[]): DiffLine[] {
  const change = lines.findIndex((line) => line.kind === "add" || line.kind === "del");
  const before = lines[change - 1];
  const start = before && before.text.trim() ? change - 1 : Math.max(0, change);
  return lines.slice(start, start + DIFF_THUMB_LINES);
}

function DiffThumb({ project, demo, onOpen }: ThumbProps) {
  const body = diffBody(demo);
  const view = useBlob(project, body?.blob);
  const first = body?.shown[0];
  const base = first ? splitPath(first.path)[1] : "";
  const lines = view.text === undefined ? [] : thumbLines(parseHunks(view.text)[0]?.lines ?? []);
  return (
    <button
      type="button"
      className="ev-thumb"
      aria-haspopup="dialog"
      aria-label={tx("ev.open_diff", { x: base || tx("mv.diff") })}
      onClick={onOpen}
    >
      <span className="thumb th-diff">
        {base ? <span className="th-diff-file">{base}</span> : null}
        {!body ? (
          <span className="th-diff-note">{tx("demo.diff.failed")}</span>
        ) : view.phase === "loading" ? (
          <span className="th-diff-note">{tx("demo.diff.reading")}</span>
        ) : view.phase === "error" || view.text === undefined ? (
          <span className="th-diff-note">{view.message ?? tx("demo.diff.failed")}</span>
        ) : (
          lines.map((line, index) => (
            <span key={index} className={`th-diff-row ${line.kind}`}>
              {DIFF_MARK[line.kind]}
              {line.text}
            </span>
          ))
        )}
      </span>
    </button>
  );
}

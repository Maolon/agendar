import { tx } from "../i18n";
import { useState } from "react";
import type { DeliveryState } from "../api/types";
import { useBlob } from "../hooks/useBlob";
import {
  deliveryProvenance,
  demoAlt,
  demoBlob,
  firstDemo,
  mediaUrl,
  provenanceIcon,
  provenanceLabel,
  reviewLabel,
} from "../state/delivery";
import { Icon } from "./Icon";

interface EvidenceCardProps {
  project: string;
  delivery: DeliveryState;
  /** Navigate to the review when this delivery still needs a human. */
  onOpen?: () => void;
}

/**
 * One "What changed" evidence card. The thumbnail is
 * desaturated with the prototype's `saturate(.35)` and shows its original
 * colours once opened; the caption says whether the app saw the evidence or
 * only has the agent's word for it.
 */
export function EvidenceCard({ project, delivery, onOpen }: EvidenceCardProps) {
  const provenance = deliveryProvenance(delivery);
  const thumbnail = <Thumbnail project={project} delivery={delivery} />;
  return (
    <div className="ev">
      {thumbnail}
      <div className="cap">
        <Icon name={provenanceIcon(provenance)} className="c2" />
        <span className="cap-t">
          <span className="cap-name">{delivery.name}</span>
          <span className="cap-meta">
            {provenanceLabel(provenance)} · {reviewLabel(delivery.review)}
          </span>
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
  const demo = firstDemo(delivery);
  const [open, setOpen] = useState(false);
  if (!demo) {
    return (
      <div className="thumb thumb-empty">
        <Icon name="file-text" className="c3" />
        <span>{tx("ev.no_demo")}</span>
      </div>
    );
  }
  if (demo.kind === "screenshot") {
    return <ScreenshotThumb project={project} delivery={delivery} open={open} setOpen={setOpen} />;
  }
  if (demo.kind === "cli_transcript") {
    return <TranscriptThumb project={project} delivery={delivery} open={open} setOpen={setOpen} />;
  }
  return (
    <div className="thumb thumb-empty">
      <Icon name="file-video" className="c3" />
      <span>{tx("ev.demo", { kind: demo.kind.replace("_", " ") })}</span>
    </div>
  );
}

interface ThumbProps {
  project: string;
  delivery: DeliveryState;
  open: boolean;
  setOpen: (open: boolean) => void;
}

function ScreenshotThumb({ project, delivery, open, setOpen }: ThumbProps) {
  const demo = firstDemo(delivery);
  const blob = demo ? demoBlob(demo) : undefined;
  const view = useBlob(project, blob);
  const alt = demo ? demoAlt(demo) : tx("dl.screenshot");
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
      aria-expanded={open}
      aria-label={open ? `Desaturate ${alt}` : `Open ${alt} at full colour`}
      onClick={() => setOpen(!open)}
    >
      <img className="ev-img" src={mediaUrl(view.token)} alt={alt} loading="lazy" />
    </button>
  );
}

function TranscriptThumb({ project, delivery, open, setOpen }: ThumbProps) {
  const demo = firstDemo(delivery);
  const blob = demo ? demoBlob(demo) : undefined;
  const view = useBlob(project, blob);
  const lines = (view.text ?? "").split("\n");
  const shown = open ? lines : lines.slice(0, 3);
  return (
    <button
      type="button"
      className="ev-thumb"
      aria-expanded={open}
      aria-label={open ? tx("ev.collapse") : tx("ev.expand")}
      onClick={() => setOpen(!open)}
    >
      <span className="thumb th-term">
        {view.phase === "loading"
          ? tx("ev.reading")
          : view.phase === "error"
            ? (view.message ?? tx("ev.read_failed"))
            : shown.join("\n")}
      </span>
    </button>
  );
}

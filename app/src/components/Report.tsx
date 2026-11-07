import { tx } from "../i18n";
import { useEffect, useRef } from "react";
import type { BlobRef } from "../api/types";
import { useBlob } from "../hooks/useBlob";
import { isHtmlBlob } from "../state/delivery";
import { Icon } from "./Icon";

interface ReportPanelProps {
  project: string;
  blob: BlobRef;
  title: string;
  onClose: () => void;
}

/**
 * Report source panel. The bytes are escaped text children
 * in `<pre>`: no HTML rendering, no `innerHTML`, no iframe, no Markdown
 * extension and no automatic fetch of links or images.
 */
export function ReportPanel({ project, blob, title, onClose }: ReportPanelProps) {
  const view = useBlob(project, blob);
  const closeRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    closeRef.current?.focus();
  }, []);

  return (
    <div
      className="report-backdrop"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          // Keep the window policy from also clearing the selected need.
          event.stopPropagation();
          event.preventDefault();
          onClose();
          return;
        }
        // The view behind the modal must not move with these keys either.
        if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === " ") {
          event.stopPropagation();
          return;
        }
        if (event.key !== "Tab") return;
        // Two-stop focus loop: the close button is the only other stop.
        const panel = event.currentTarget.querySelector<HTMLElement>(".report");
        if (!panel) return;
        const stops = [...panel.querySelectorAll<HTMLElement>("button, [tabindex='0']")];
        if (!stops.length) return;
        const first = stops[0];
        const last = stops[stops.length - 1];
        if (!first || !last) return;
        if (event.shiftKey && document.activeElement === first) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault();
          first.focus();
        }
      }}
    >
      <section
        className="report"
        role="dialog"
        aria-modal="true"
        aria-label={tx("rp.label", { x: title })}
        tabIndex={-1}
      >
        <div className="report-h">
          <h3>{title}</h3>
          <button
            type="button"
            className="btn"
            ref={closeRef}
            onClick={onClose}
            aria-label={tx("rp.close_label")}
          >
            <Icon name="x" />
            {tx("rp.close")}
          </button>
        </div>
        <p className="ghint">
          {isHtmlBlob(blob)
            ? tx("rp.html")
            : tx("rp.source")}
        </p>
        {view.phase === "loading" ? (
          <p className="muted-note">{tx("rp.reading")}</p>
        ) : view.phase === "error" ? (
          <p className="alert" role="alert">
            <Icon name="alert-triangle" />
            <span className="body">
              <span>{view.message ?? tx("rp.failed")}</span>
              <span className="hint">{tx("rp.failed_hint")}</span>
            </span>
          </p>
        ) : view.text === undefined ? (
          <p className="muted-note">{tx("rp.no_text")}</p>
        ) : (
          <pre className="report-body">{view.text}</pre>
        )}
      </section>
    </div>
  );
}

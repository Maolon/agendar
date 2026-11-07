import { tx } from "../i18n";
import { useEffect, useRef, useState } from "react";
import type { BlobRef, DecisionOption } from "../api/types";
import { useBlob } from "../hooks/useBlob";
import { mediaUrl } from "../state/delivery";
import { Icon } from "./Icon";

/**
 * Option images (a visual choice). They are what is being decided, so they
 * stay at full colour, unlike evidence thumbnails. Each image is read through
 * `read_report` like any other referenced blob.
 */

interface PictureProps {
  project: string;
  option: DecisionOption;
  selected: boolean;
  onOpen: () => void;
}

/** The tile above one option card; an option without an image keeps the row aligned. */
export function OptionPicture({ project, option, selected, onOpen }: PictureProps) {
  if (!option.image) return <div className="opt-pic none" aria-hidden="true" />;
  return (
    <button
      type="button"
      className="opt-pic"
      data-selected={selected ? "true" : "false"}
      aria-label={tx("oi.view", { x: option.label })}
      onClick={onOpen}
    >
      <BlobImage project={project} blob={option.image} alt={option.label} />
    </button>
  );
}

const RETRY_MS = 5_000;
const RETRIES = 24;

function BlobImage({ project, blob, alt }: { project: string; blob: BlobRef; alt: string }) {
  const [attempt, setAttempt] = useState(0);
  const view = useBlob(project, blob, attempt);
  const failed = view.phase === "error" || (view.phase === "ready" && !view.token);
  useEffect(() => {
    // The image may still be syncing from the machine that asked.
    if (!failed || attempt >= RETRIES) return;
    const timer = window.setTimeout(() => setAttempt((value) => value + 1), RETRY_MS);
    return () => window.clearTimeout(timer);
  }, [failed, attempt]);
  if (view.phase === "loading") {
    return <span className="pic-state">{attempt > 0 ? "Waiting for the image to sync…" : ""}</span>;
  }
  if (failed || !view.token) {
    return (
      <span className="pic-state">
        {attempt < RETRIES ? tx("oi.syncing") : (view.message ?? tx("oi.unavailable"))}
      </span>
    );
  }
  return <img src={mediaUrl(view.token)} alt={alt} draggable={false} />;
}

interface CompareProps {
  project: string;
  options: DecisionOption[];
  /** Index into `options` of the image to show first. */
  start: number;
  selected: string | undefined;
  recommended: string;
  onSelect: (optionId: string) => void;
  onClose: () => void;
}

/**
 * Full-size viewer. Every image is mounted at once in the same frame and only
 * the current one is visible, so flipping with ← → shows the difference in
 * place without a loading flash.
 */
export function ImageCompare({
  project,
  options,
  start,
  selected,
  recommended,
  onSelect,
  onClose,
}: CompareProps) {
  const pictured = options
    .map((option, index) => ({ option, index }))
    .filter((entry) => entry.option.image);
  const [at, setAt] = useState(() =>
    Math.max(
      0,
      pictured.findIndex((entry) => entry.index === start),
    ),
  );
  const dialogRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialogRef.current?.focus();
    return () => opener?.focus();
  }, []);

  const current = pictured[at];
  if (!current) return null;
  const count = pictured.length;
  const go = (step: number) => setAt((value) => (value + step + count) % count);
  const choose = () => {
    onSelect(current.option.id);
    onClose();
  };

  return (
    <div
      className="compare-backdrop"
      onKeyDown={(event) => {
        // The view behind must not react to keys meant for the viewer.
        const key = event.key;
        if (key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          onClose();
          return;
        }
        if (key === "ArrowLeft" || key === "ArrowRight") {
          event.preventDefault();
          event.stopPropagation();
          go(key === "ArrowRight" ? 1 : -1);
          return;
        }
        if (/^[1-4]$/.test(key)) {
          event.preventDefault();
          event.stopPropagation();
          const target = pictured.findIndex((entry) => entry.index === Number(key) - 1);
          if (target >= 0) setAt(target);
          return;
        }
        if (key === "Enter" && !event.metaKey && !event.ctrlKey) {
          if (event.target instanceof HTMLButtonElement) {
            event.stopPropagation();
            return;
          }
          event.preventDefault();
          event.stopPropagation();
          choose();
          return;
        }
        if (key === "Tab") {
          // Keep focus inside the viewer.
          const stops = [...event.currentTarget.querySelectorAll<HTMLElement>("button")];
          const first = stops[0];
          const last = stops[stops.length - 1];
          if (!first || !last) return;
          const active = document.activeElement;
          if (event.shiftKey && (active === first || active === dialogRef.current)) {
            event.preventDefault();
            last.focus();
          } else if (!event.shiftKey && active === last) {
            event.preventDefault();
            first.focus();
          }
          return;
        }
        if (key === "ArrowDown" || key === "ArrowUp" || key === " " || event.metaKey || event.ctrlKey) {
          event.stopPropagation();
        }
      }}
    >
      <section
        className="compare"
        role="dialog"
        aria-modal="true"
        aria-label={tx("oi.compare")}
        tabIndex={-1}
        ref={dialogRef}
      >
        <header className="compare-h">
          <span className="key" aria-hidden="true">
            {current.index + 1}
          </span>
          <h3>{current.option.label}</h3>
          {current.option.id === recommended ? (
            <span className="tag">
              <Icon name="star" />
              Recommended
            </span>
          ) : null}
          {current.option.id === selected ? (
            <span className="tag">
              <Icon name="check" />
              Selected
            </span>
          ) : null}
          <span className="count">
            {at + 1} / {count}
          </span>
          <button type="button" className="btn" onClick={onClose} aria-label="Close the images">
            <Icon name="x" />
          </button>
        </header>
        <div className="compare-stage">
          {pictured.map((entry, index) => (
            <div
              key={entry.option.id}
              className="compare-frame"
              data-current={index === at ? "true" : "false"}
              aria-hidden={index === at ? undefined : true}
            >
              {entry.option.image ? (
                <BlobImage project={project} blob={entry.option.image} alt={entry.option.label} />
              ) : null}
            </div>
          ))}
          {count > 1 ? (
            <>
              <button
                type="button"
                className="compare-nav prev"
                onClick={() => go(-1)}
                aria-label={tx("oi.prev")}
              >
                <Icon name="chevron-left" size={22} />
              </button>
              <button
                type="button"
                className="compare-nav next"
                onClick={() => go(1)}
                aria-label={tx("oi.next")}
              >
                <Icon name="chevron-right" size={22} />
              </button>
            </>
          ) : null}
        </div>
        <footer className="compare-f">
          <p className="sub">{current.option.consequence}</p>
          <div className="compare-dots" role="group" aria-label="Images">
            {pictured.map((entry, index) => (
              <button
                key={entry.option.id}
                type="button"
                className="compare-dot"
                aria-pressed={index === at}
                onClick={() => setAt(index)}
              >
                <span className="k">{entry.index + 1}</span>
                {entry.option.label}
              </button>
            ))}
          </div>
          <button type="button" className="btn" onClick={choose}>
            {current.option.id === selected ? tx("oi.keep") : tx("oi.choose", { x: current.option.label })}
            <kbd>↵</kbd>
          </button>
        </footer>
      </section>
    </div>
  );
}

/**
 * The sidebar footer's one status tag: the sync label ("received just now"),
 * "Not uploaded yet" while this computer's own writes have not left it,
 * "Checking for updates…" while a check runs, or an accent "Update 0.2.0" pill
 * when a release build has a newer release. Each change crossfades and morphs
 * the pill's width; with reduced motion it swaps at once.
 */

import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { formatAge, formatAgo, formatClockTime } from "../api/format";
import type { Snapshot } from "../api/types";
import { tx } from "../i18n";
import { useEnvironment } from "../state/motion";
import { Icon } from "./Icon";

/** How long a crossfade and width morph take (matches the CSS). */
export const FOOT_MORPH_MS = 200;

export type FootTagState = "sync" | "upload" | "checking" | "update";

/**
 * A newer release to offer: a release build only, with checks on and a
 * strictly newer version. A local (dev) build never gets the Update tag.
 */
export function updateAvailable(snapshot: Snapshot): boolean {
  const update = snapshot.update;
  return Boolean(
    update && update.channel === "release" && update.available && update.enabled && update.latest,
  );
}

/**
 * Which face the tag shows: writes still not uploaded after two minutes win
 * (another device cannot have them), then a running check, then an update; "not
 * uploaded yet" only takes the sync label's place.
 */
export function footTagState(snapshot: Snapshot): FootTagState {
  if (snapshot.local.upload?.state === "late") return "upload";
  if (snapshot.update?.checking) return "checking";
  if (updateAvailable(snapshot)) return "update";
  if (snapshot.local.upload) return "upload";
  return "sync";
}

function syncTitle(snapshot: Snapshot): string {
  const at = snapshot.local.received_at;
  const now = snapshot.state.evaluated_at;
  return tx("side.received_title", { age: formatAge(at, now), ago: formatAgo(at, now), time: at });
}

/** One face of the tag; `key` changes whenever what it shows changes. */
function face(snapshot: Snapshot, state: FootTagState, onOpenAbout: () => void): { key: string; node: ReactNode } {
  if (state === "checking") {
    // A short label fits beside "N agents working"; the full one is the
    // tooltip and what a screen reader hears.
    const label = tx("update.checking_footer");
    return {
      key: "checking",
      node: (
        <span className="foot-face" role="status" title={label} aria-label={label}>
          <span className="foot-spin" aria-hidden="true" />
          <span className="foot-text" aria-hidden="true">
            {tx("update.checking_short")}
          </span>
        </span>
      ),
    };
  }
  if (state === "upload") {
    const upload = snapshot.local.upload!;
    const late = upload.state === "late";
    const time = formatClockTime(upload.since);
    const label = late ? tx("side.upload_late_label", { time }) : tx("side.upload_waiting");
    return {
      key: `upload:${upload.state}`,
      node: (
        <span className="foot-face" role="status" title={label} aria-label={label} data-upload={upload.state}>
          <Icon name="cloud" />
          <span className="foot-text" aria-hidden="true">
            {late ? tx("side.upload_late", { time }) : label}
          </span>
        </span>
      ),
    };
  }
  if (state === "update") {
    const version = snapshot.update!.latest!;
    const label = tx("update.tag", { version });
    return {
      key: `update:${version}`,
      node: (
        <button
          type="button"
          className="foot-face foot-pill"
          aria-label={label}
          title={`${tx("update.available", { version })} · ${syncTitle(snapshot)}`}
          onClick={onOpenAbout}
        >
          <Icon name="package" />
          <span className="foot-text">{label}</span>
        </button>
      ),
    };
  }
  const at = snapshot.local.received_at;
  const now = snapshot.state.evaluated_at;
  const vars = { age: formatAge(at, now), ago: formatAgo(at, now) };
  return {
    key: "sync",
    node: (
      <span className="foot-face" title={syncTitle(snapshot)}>
        <Icon name="cloud" />
        <span className="foot-text" aria-label={tx("side.received_label", vars)}>
          {tx("side.received", vars)}
        </span>
      </span>
    ),
  };
}

export function FootStatus({ snapshot, onOpenAbout }: { snapshot: Snapshot; onOpenAbout: () => void }) {
  // The system's reduce-motion setting and the app's own Motion preference
  // ("Animate live changes") both turn the morph into an instant swap.
  const { reduced } = useEnvironment();
  const animate = !reduced && snapshot.preferences.motion !== false;
  const state = footTagState(snapshot);
  const current = face(snapshot, state, onOpenAbout);

  // The face that is fading out, kept for one morph. `lastFace` is the face
  // of the previous render (updated after the morph effect reads it).
  const [leaving, setLeaving] = useState<{ key: string; node: ReactNode } | null>(null);
  const lastFace = useRef(current);
  useEffect(() => {
    const previous = lastFace.current;
    if (!animate || previous.key === current.key) return;
    setLeaving(previous);
    const timer = setTimeout(() => setLeaving(null), FOOT_MORPH_MS + 40);
    return () => clearTimeout(timer);
    // Only a change of face starts a morph; new text on the same face just
    // updates in place.
  }, [current.key, animate]);
  useEffect(() => {
    lastFace.current = current;
  });

  // Width morph: the slot holds an explicit width that follows the incoming
  // face's natural width, so the neighbours glide instead of jumping.
  const inner = useRef<HTMLSpanElement>(null);
  const [width, setWidth] = useState<number | null>(null);
  useLayoutEffect(() => {
    if (!animate) {
      setWidth(null);
      return;
    }
    const element = inner.current;
    if (!element) return;
    const measure = () => {
      // The natural width, even while the slot is narrower and truncates:
      // the layer is a shrinkable flex item capped by the slot, so lift both
      // the cap and the shrink for the measurement.
      const cap = element.style.maxWidth;
      const flex = element.style.flex;
      element.style.maxWidth = "none";
      element.style.flex = "none";
      const next = Math.max(element.scrollWidth, Math.ceil(element.getBoundingClientRect().width));
      element.style.maxWidth = cap;
      element.style.flex = flex;
      setWidth(next > 0 ? next : null);
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    // The layer is capped by the slot, so a wider sidebar (or a label the
    // narrow layout hid coming back) does not resize it: watch the row too.
    const row = element.closest(".foot-status");
    if (row) observer.observe(row);
    return () => observer.disconnect();
  }, [animate, current.key]);

  const classes = [
    "foot-item",
    "foot-tag",
    `foot-tag-${state}`,
    // Narrow sidebars hide the plain sync label, as before.
    state === "sync" ? "foot-received" : "",
    animate ? "foot-morph" : "foot-still",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <span
      className={classes}
      data-state={state}
      style={animate && width !== null ? { width } : undefined}
    >
      <span ref={inner} key={current.key} className={animate ? "foot-layer foot-in" : "foot-layer"}>
        {current.node}
      </span>
      {animate && leaving && leaving.key !== current.key ? (
        <span key={`out:${leaving.key}`} className="foot-layer foot-out" aria-hidden="true" inert>
          {leaving.node}
        </span>
      ) : null}
    </span>
  );
}

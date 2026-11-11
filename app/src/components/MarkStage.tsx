import { tx } from "../i18n";
import type { CSSProperties, SyntheticEvent } from "react";
import type { Pin } from "../state/comments";
import { CommentBox } from "./Comments";

/** A per-mille rectangle of an image box. */
export type Region = { x: number; y: number; w: number; h: number };

/** The whole image, for "Comment on image". */
export const WHOLE_IMAGE: Region = { x: 0, y: 0, w: 1000, h: 1000 };

/** What the full-screen viewer needs to show and add area comments on one image. */
export interface ImageMarks {
  /** Region pins with their page-wide numbers. */
  pins: Pin[];
  /** False when the delivery is not live or the comment limit is reached: no drawing. */
  enabled: boolean;
  /** False when the delivery is not live or a request is pending: no edits or removals. */
  editable: boolean;
  /** Names the comment box ("an area of …"). */
  label: string;
  onAdd: (region: Region, text: string) => void;
  /** Removes the comment with this page-wide number. */
  onRemove: (n: number) => void;
  onEdit: (n: number, text: string) => void;
}

export function areaStyle(region: Region): CSSProperties {
  return {
    left: `${region.x / 10}%`,
    top: `${region.y / 10}%`,
    width: `${region.w / 10}%`,
    height: `${region.h / 10}%`,
  };
}

/** Numbered amber areas over an image whose box they share. */
export function RegionPins({ pins }: { pins: Pin[] }) {
  return (
    <>
      {pins.map(({ n, comment }) =>
        comment.anchor.kind === "region" ? (
          <span key={n} className="cm-area" style={areaStyle(comment.anchor)} title={comment.text}>
            <span className="cm-pin">{n}</span>
          </span>
        ) : null,
      )}
    </>
  );
}

/**
 * Where the comment box sits: under the marked area, else above it, else
 * docked at the bottom of the viewer, so it does not cover what was marked.
 */
function boxPlace(region: Region): { docked: boolean; style?: CSSProperties } {
  const left = `clamp(0px, ${region.x / 10}%, calc(100% - var(--mv-cm-w)))`;
  if (region.y + region.h <= 800) {
    return { docked: false, style: { left, top: `calc(${(region.y + region.h) / 10}% + 6px)` } };
  }
  if (region.y >= 200) {
    return { docked: false, style: { left, bottom: `calc(${(1000 - region.y) / 10}% + 6px)` } };
  }
  return { docked: true };
}

/** Keeps pointer gestures on a box or popover from drawing or panning the stage. */
const stay = {
  onPointerDown: (event: SyntheticEvent) => event.stopPropagation(),
  onDoubleClick: (event: SyntheticEvent) => event.stopPropagation(),
};

/** Buttons that keep focus on the stage when clicked, so its keys keep working. */
const keepFocus = (event: SyntheticEvent) => event.preventDefault();

/** True when the per-mille point lies inside the region. */
export function regionHas(region: Region, x: number, y: number): boolean {
  return x >= region.x && x <= region.x + region.w && y >= region.y && y <= region.y + region.h;
}

/** The pin under a per-mille point: the smallest area wins. */
export function pinAt(pins: Pin[], x: number, y: number): number | null {
  let best: { n: number; size: number } | null = null;
  for (const { n, comment } of pins) {
    const anchor = comment.anchor;
    if (anchor.kind !== "region" || !regionHas(anchor, x, y)) continue;
    const size = anchor.w * anchor.h;
    if (!best || size <= best.size) best = { n, size };
  }
  return best?.n ?? null;
}

/**
 * What sits on the viewer's image when it carries area comments: numbered
 * areas whose pins select them, the draft area with its comment box, and the
 * selected comment's popover (text, Edit, Delete).
 */
export function MarkLayer({
  marks,
  draft,
  selected,
  editing,
  hot,
  onSelect,
  onSaveDraft,
  onCancelDraft,
  onEdit,
  onEditDone,
  onDelete,
}: {
  marks: ImageMarks;
  draft: Region | null;
  selected: number | null;
  editing: boolean;
  /** Highlighted from the comments list. */
  hot: number | null;
  onSelect: (n: number) => void;
  onSaveDraft: (text: string) => void;
  onCancelDraft: () => void;
  onEdit: () => void;
  /** The edited text, or null when cancelled. */
  onEditDone: (text: string | null) => void;
  onDelete: (n: number) => void;
}) {
  const place = draft ? boxPlace(draft) : null;
  const pick = marks.pins.find((pin) => pin.n === selected && pin.comment.anchor.kind === "region");
  const picked = pick && pick.comment.anchor.kind === "region" ? { pin: pick, place: boxPlace(pick.comment.anchor) } : null;
  return (
    <>
      {marks.pins.map(({ n, comment }) =>
        comment.anchor.kind === "region" ? (
          <span
            key={n}
            className={`cm-area${n === selected ? " sel" : ""}${n === hot ? " hot" : ""}`}
            style={areaStyle(comment.anchor)}
          >
            <button
              type="button"
              className="cm-pin"
              aria-label={tx("vt.comment_n", { n })}
              aria-pressed={n === selected}
              title={comment.text}
              onMouseDown={keepFocus}
              onClick={() => onSelect(n)}
              {...stay}
            >
              {n}
            </button>
          </span>
        ) : null,
      )}
      {draft ? <span className="cm-area drawing" style={areaStyle(draft)} /> : null}
      {draft && place ? (
        <div className={place.docked ? "mv-cm docked" : "mv-cm"} style={place.style} {...stay}>
          <CommentBox
            // A new region is a new box, focused.
            key={`${draft.x},${draft.y},${draft.w},${draft.h}`}
            label={marks.label}
            onCancel={onCancelDraft}
            onSave={onSaveDraft}
          />
        </div>
      ) : null}
      {picked && !draft ? (
        <div
          className={picked.place.docked ? "mv-cm mv-pop docked" : "mv-cm mv-pop"}
          style={picked.place.style}
          role="group"
          aria-label={tx("vt.comment_n", { n: picked.pin.n })}
          {...stay}
        >
          {editing ? (
            <CommentBox
              label={marks.label}
              ariaLabel={tx("vt.edit_n", { n: picked.pin.n })}
              initial={picked.pin.comment.text}
              onCancel={() => onEditDone(null)}
              onSave={(text) => onEditDone(text)}
            />
          ) : (
            <>
              <p className="mv-pop-text">
                <span className="cm-pin">{picked.pin.n}</span>
                <span>{picked.pin.comment.text}</span>
              </p>
              {marks.editable ? (
                <div className="mv-pop-acts">
                  <button type="button" onMouseDown={keepFocus} onClick={onEdit}>
                    {tx("vt.edit")}
                  </button>
                  <button
                    type="button"
                    className="danger"
                    onMouseDown={keepFocus}
                    onClick={() => onDelete(picked.pin.n)}
                  >
                    {tx("vt.delete")}
                  </button>
                </div>
              ) : null}
            </>
          )}
        </div>
      ) : null}
    </>
  );
}

import { tx } from "../i18n";
import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type MutableRefObject,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import type { BlobRef, Demo } from "../api/types";
import { useBlob } from "../hooks/useBlob";
import { MAX_COMMENTS } from "../state/comments";
import { mediaUrl } from "../state/delivery";
import { DiffDemoView } from "./DiffDemo";
import { Icon } from "./Icon";
import { ImageViewport, useViewport, type Tool, type Viewport } from "./ImageViewport";
import { MarkLayer, pinAt, WHOLE_IMAGE, type ImageMarks, type Region } from "./MarkStage";
import { CommentRail, ViewerToolbar } from "./ViewerTools";

/** One thing the viewer can show: an image, a text blob read in full, or a diff demo. */
export type ViewerItem =
  | { kind: "image"; blob: BlobRef | undefined; title: string; marks?: ImageMarks }
  | { kind: "text"; blob: BlobRef | undefined; title: string }
  | { kind: "diff"; demo: Demo; title: string };

interface MediaViewerProps {
  project: string;
  items: ViewerItem[];
  /** Index of the item shown first. */
  start?: number;
  /** A region whose comment box is open from the start ("Comment on image"). */
  initialMark?: Region;
  /** A comment the viewer opens on: its area selected and zoomed to. */
  initialPin?: number;
  /** Focused on close instead of whatever opened the viewer. */
  returnFocus?: RefObject<HTMLElement | null>;
  onClose: () => void;
}

/** Keys that would scroll or act on the view behind the viewer. */
const SCROLL_KEYS = new Set(["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End"]);

/**
 * Puts focus back on whatever opened a modal once it closes (a thumbnail, a
 * "Read" link), so keyboard users land where they were.
 */
export function useReturnFocus(target?: RefObject<HTMLElement | null>): void {
  // Read while rendering, before a child's autofocus moves focus inside.
  const [opener] = useState(() => (document.activeElement instanceof HTMLElement ? document.activeElement : null));
  useEffect(() => {
    return () => {
      const back = target?.current ?? opener;
      if (back?.isConnected) back.focus();
    };
  }, [opener, target]);
}

/** A mark added in this viewer session, for Undo and Redo. */
interface Added {
  region: Region;
  text?: string;
}

const sameRegion = (a: Region, b: Region) => a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;

/** Pan step of the arrow keys, in pixels. */
const ARROW_PX = 60;

/**
 * Full-window viewer for delivery demos. Text blobs are escaped children of a
 * `<pre>`, never HTML; a diff renders as the review's diff view. Escape, the
 * close button or a click outside the content closes it; ←/→ step through
 * several items.
 *
 * An image sits on a stage that zooms and pans, with a side toolbar (Hand,
 * zoom, 100%, Fit). An image with `marks` adds Mark (drag a region, then a
 * comment box), Undo/Redo, "Comment on image" and a comments list; a pin
 * opens a popover to edit or delete its comment. Text fields own the keys
 * while focused; Escape closes a box or edit, then deselects, then closes.
 */
export function MediaViewer({
  project,
  items,
  start = 0,
  initialMark,
  initialPin,
  returnFocus,
  onClose,
}: MediaViewerProps) {
  const count = items.length;
  const [at, setAt] = useState(() => Math.min(Math.max(start, 0), Math.max(count - 1, 0)));
  const closeRef = useRef<HTMLButtonElement | null>(null);
  const textRef = useRef<HTMLPreElement | null>(null);
  const [draft, setDraft] = useState<Region | null>(initialMark ?? null);
  const [selected, setSelected] = useState<number | null>(initialPin ?? null);
  const [editing, setEditing] = useState(false);
  const [hot, setHot] = useState<number | null>(null);
  // The comment list opens by itself once there is a comment, so an image
  // with none gets the full width; a toggle by hand wins from then on.
  const [railChoice, setRailChoice] = useState<boolean | null>(null);
  // The tool picked on the toolbar; null until then (Mark when it can be used).
  const [chosen, setChosen] = useState<Tool | null>(null);
  // Space held: Hand until it is released.
  const [space, setSpace] = useState(false);
  const [history, setHistory] = useState<{ added: Added[]; undone: Added[] }>({ added: [], undone: [] });
  const swallowClick = useRef(false);
  const viewport = useViewport();
  useReturnFocus(returnFocus);

  const current = items[at];
  const image = current?.kind === "image";
  const marks = current?.kind === "image" ? current.marks : undefined;
  const editable = Boolean(marks?.editable);
  const canMark = Boolean(marks?.enabled);
  // No drawing, and no open box, once commenting stops.
  const openDraft = canMark ? draft : null;
  const base = chosen ?? (canMark ? "mark" : "hand");
  const tool: Tool = !space && base === "mark" && canMark ? "mark" : "hand";
  const pick = selected !== null && marks?.pins.some((pin) => pin.n === selected) ? selected : null;
  const stageOrClose = useRef(() => viewport.stage() ?? closeRef.current);
  stageOrClose.current = () => viewport.stage() ?? closeRef.current;

  useEffect(() => {
    // An initial comment box keeps its focus; otherwise the image (its keys work at once).
    if (!initialMark) stageOrClose.current()?.focus();
  }, [initialMark]);

  useEffect(() => {
    // Stepping replaces the stage: keep the keys inside the viewer.
    const active = document.activeElement;
    if (!active || active === document.body) stageOrClose.current()?.focus();
  }, [at]);

  // The opening comment's area, once the image has a size.
  const measured = viewport.view !== null;
  const zoomed = useRef(false);
  useEffect(() => {
    if (!measured || zoomed.current || initialPin === undefined) return;
    zoomed.current = true;
    const anchor = marks?.pins.find((pin) => pin.n === initialPin)?.comment.anchor;
    if (anchor?.kind === "region") viewport.focus(anchor);
    // Once, when the image is first measured.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [measured]);

  useEffect(() => {
    const release = () => setSpace(false);
    window.addEventListener("blur", release);
    return () => window.removeEventListener("blur", release);
  }, []);

  if (!current) return null;

  const focusStage = () => stageOrClose.current()?.focus();

  const go = (step: number) => {
    viewport.reset();
    setDraft(null);
    setSelected(null);
    setEditing(false);
    setAt((value) => (value + step + count) % count);
  };

  function closeDraft(): void {
    setDraft(null);
    focusStage();
  }

  function dismiss(): void {
    if (openDraft) closeDraft();
    else if (editing) {
      setEditing(false);
      focusStage();
    } else if (pick !== null) setSelected(null);
    else onClose();
  }

  function select(n: number | null): void {
    setSelected(n);
    setEditing(false);
  }

  function saveDraft(text: string): void {
    if (!marks || !openDraft) return;
    marks.onAdd(openDraft, text);
    const region = openDraft;
    setHistory((value) => ({ added: [...value.added, { region }], undone: [] }));
    closeDraft();
  }

  function remove(n: number): void {
    if (!marks || !editable) return;
    marks.onRemove(n);
    select(null);
    setHot(null);
    focusStage();
  }

  function undo(): void {
    if (!marks || !editable) return;
    const added = [...history.added];
    while (added.length) {
      const entry = added.pop()!;
      const pin = [...marks.pins]
        .reverse()
        .find(({ comment: { anchor } }) => anchor.kind === "region" && sameRegion(anchor, entry.region));
      if (!pin) continue;
      marks.onRemove(pin.n);
      select(null);
      setHistory({ added, undone: [...history.undone, { region: entry.region, text: pin.comment.text }] });
      return;
    }
    setHistory({ added: [], undone: history.undone });
  }

  function redo(): void {
    const entry = history.undone[history.undone.length - 1];
    if (!marks || !canMark || !entry?.text) return;
    marks.onAdd(entry.region, entry.text);
    setHistory({ added: [...history.added, { region: entry.region }], undone: history.undone.slice(0, -1) });
  }

  function onTap(point: { x: number; y: number }): void {
    if (openDraft) setDraft(null);
    select(marks ? pinAt(marks.pins, point.x * 1000, point.y * 1000) : null);
  }

  function onPick(n: number): void {
    const anchor = marks?.pins.find((pin) => pin.n === n)?.comment.anchor;
    setDraft(null);
    select(n);
    if (anchor?.kind === "region") viewport.reveal(anchor);
    focusStage();
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>): void {
    const key = event.key;
    if (key === "Tab") {
      trapTab(event);
      return;
    }
    // Nothing behind the viewer reacts while it is open (window keyboard policy included).
    event.stopPropagation();
    // A comment box or edit field owns its keys (Enter saves, Escape cancels, ⌘Z undoes typing).
    if (event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement) return;
    if (key === "Escape") {
      event.preventDefault();
      dismiss();
      return;
    }
    if (image && imageKey(event)) {
      event.preventDefault();
      return;
    }
    if ((key === "ArrowLeft" || key === "ArrowRight") && count > 1) {
      event.preventDefault();
      go(key === "ArrowRight" ? 1 : -1);
      return;
    }
    // Scroll keys move the text or the diff, never the page behind.
    if (!image && SCROLL_KEYS.has(key) && event.target instanceof HTMLButtonElement) {
      event.preventDefault();
      const scroller = textRef.current ?? event.currentTarget.querySelector<HTMLElement>(".mv-diff");
      scroller?.focus();
    }
  }

  /** The image's shortcuts; true when the key was one of them. */
  function imageKey(event: KeyboardEvent<HTMLDivElement>): boolean {
    const key = event.key;
    const lower = key.toLowerCase();
    if (event.altKey) return false;
    if (event.metaKey || event.ctrlKey) {
      if (key === "=" || key === "+") viewport.zoomIn();
      else if (key === "-" || key === "_") viewport.zoomOut();
      else if (key === "0") viewport.fit();
      else if (key === "1") viewport.actual();
      else if (lower === "z" && marks && editable) (event.shiftKey ? redo : undo)();
      else return false;
      return true;
    }
    if (key === " ") {
      // A focused button keeps Space for itself.
      if (event.target instanceof HTMLButtonElement) return false;
      setSpace(true);
      return true;
    }
    if (lower === "m" && marks && editable) {
      if (canMark) setChosen("mark");
      return true;
    }
    if (lower === "h") {
      setChosen("hand");
      return true;
    }
    if ((key === "Backspace" || key === "Delete") && pick !== null && editable) {
      remove(pick);
      return true;
    }
    const pan: Record<string, [number, number]> = {
      ArrowUp: [0, ARROW_PX],
      ArrowDown: [0, -ARROW_PX],
      ...(count > 1 ? {} : { ArrowLeft: [ARROW_PX, 0], ArrowRight: [-ARROW_PX, 0] }),
    };
    const step = pan[key];
    if (!step || event.target instanceof HTMLButtonElement) return false;
    viewport.panBy(step[0], step[1]);
    return true;
  }

  function onClick(event: MouseEvent<HTMLDivElement>): void {
    // The click that ends a drag lands outside the image when the drag does.
    if (swallowClick.current) {
      swallowClick.current = false;
      return;
    }
    if (event.target instanceof HTMLElement && event.target.dataset.dismiss !== undefined) dismiss();
  }

  const layer = marks ? (
    <MarkLayer
      marks={marks}
      draft={openDraft}
      selected={pick}
      editing={editing}
      hot={hot}
      onSelect={(n) => {
        setDraft(null);
        select(pick === n ? null : n);
      }}
      onSaveDraft={saveDraft}
      onCancelDraft={closeDraft}
      onEdit={() => setEditing(true)}
      onEditDone={(text) => {
        if (text !== null && pick !== null) marks.onEdit(pick, text);
        setEditing(false);
        focusStage();
      }}
      onDelete={remove}
    />
  ) : null;

  const viewer = (
    <div
      className="mv-backdrop"
      data-dismiss=""
      onKeyDown={onKeyDown}
      onKeyUp={(event) => {
        if (event.key === " ") setSpace(false);
      }}
      onClick={onClick}
    >
      <section
        className={image ? "mv has-tools" : "mv"}
        role="dialog"
        aria-modal="true"
        aria-label={current.title}
        data-dismiss=""
      >
        <header className="mv-h">
          <h3>{current.title}</h3>
          {count > 1 ? (
            <span className="mv-count">
              {at + 1} / {count}
            </span>
          ) : null}
          <button
            type="button"
            className="mv-btn"
            ref={closeRef}
            onClick={dismiss}
            aria-label={tx("mv.close")}
            title={tx("mv.close")}
          >
            <Icon name="x" />
          </button>
        </header>
        {current.kind === "image" ? (
          <div className="mv-body">
            <ViewerToolbar
              tool={tool}
              onTool={setChosen}
              mark={marks && editable ? { disabled: !canMark, reason: tx("vt.full", { n: MAX_COMMENTS }) } : null}
              percent={viewport.percent}
              fitted={viewport.fitted}
              onZoomIn={viewport.zoomIn}
              onZoomOut={viewport.zoomOut}
              onActual={viewport.actual}
              onFit={viewport.fit}
              history={
                marks && editable
                  ? {
                      canUndo: history.added.length > 0,
                      canRedo: canMark && history.undone.length > 0,
                      onUndo: undo,
                      onRedo: redo,
                    }
                  : null
              }
              whole={
                marks && editable
                  ? {
                      disabled: !canMark,
                      onClick: () => {
                        select(null);
                        setDraft(WHOLE_IMAGE);
                      },
                    }
                  : null
              }
            />
            <ImageStage
              key={at}
              project={project}
              item={current}
              viewport={viewport}
              tool={tool}
              onDraw={
                canMark
                  ? (region) => {
                      select(null);
                      setDraft(region);
                    }
                  : undefined
              }
              onTap={onTap}
              swallowClick={swallowClick}
              layer={layer}
            />
            {marks ? (
              <CommentRail
                pins={marks.pins}
                editable={editable}
                open={railChoice ?? marks.pins.length > 0}
                selected={pick}
                onToggle={() => setRailChoice((value) => !(value ?? marks.pins.length > 0))}
                onHot={setHot}
                onPick={onPick}
                onRemove={remove}
              />
            ) : null}
          </div>
        ) : current.kind === "diff" ? (
          <DiffStage key={at} project={project} demo={current.demo} />
        ) : (
          <TextStage key={at} project={project} item={current} textRef={textRef} />
        )}
        {count > 1 ? (
          <>
            <button type="button" className="mv-nav prev" onClick={() => go(-1)} aria-label={tx("mv.prev")}>
              <Icon name="chevron-left" size={22} />
            </button>
            <button type="button" className="mv-nav next" onClick={() => go(1)} aria-label={tx("mv.next")}>
              <Icon name="chevron-right" size={22} />
            </button>
          </>
        ) : null}
      </section>
    </div>
  );
  return createPortal(viewer, document.body);
}

/** Keeps Tab and Shift+Tab inside the viewer. */
function trapTab(event: KeyboardEvent<HTMLDivElement>): void {
  event.stopPropagation();
  const stops = [
    ...event.currentTarget.querySelectorAll<HTMLElement>(
      "button:not([tabindex='-1']):not(:disabled), input, [tabindex='0']",
    ),
  ];
  const first = stops[0];
  const last = stops[stops.length - 1];
  if (!first || !last) return;
  const active = document.activeElement;
  if (event.shiftKey && (active === first || !stops.includes(active as HTMLElement))) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && (active === last || !stops.includes(active as HTMLElement))) {
    event.preventDefault();
    first.focus();
  }
}

function ImageStage({
  project,
  item,
  viewport,
  tool,
  onDraw,
  onTap,
  swallowClick,
  layer,
}: {
  project: string;
  item: Extract<ViewerItem, { kind: "image" }>;
  viewport: Viewport;
  tool: Tool;
  onDraw?: (region: Region) => void;
  onTap: (point: { x: number; y: number }) => void;
  swallowClick: MutableRefObject<boolean>;
  layer: ReactNode;
}) {
  const view = useBlob(project, item.blob);
  const note =
    view.phase === "loading" ? (
      <p className="mv-note">{tx("mv.reading")}</p>
    ) : view.phase === "error" || !view.token ? (
      <p className="mv-note" role="alert">
        {view.message ?? tx("mv.failed")}
      </p>
    ) : null;
  return (
    <ImageViewport
      src={view.token ? mediaUrl(view.token) : ""}
      title={item.title}
      note={note}
      viewport={viewport}
      tool={tool}
      onDraw={onDraw}
      onTap={onTap}
      swallowClick={swallowClick}
    >
      {layer}
    </ImageViewport>
  );
}

function TextStage({
  project,
  item,
  textRef,
}: {
  project: string;
  item: Extract<ViewerItem, { kind: "text" }>;
  textRef: RefObject<HTMLPreElement | null>;
}) {
  const view = useBlob(project, item.blob);
  return (
    <div className="mv-stage mv-reader" data-dismiss="">
      {view.phase === "loading" ? (
        <p className="mv-note">{tx("mv.reading")}</p>
      ) : view.phase === "error" ? (
        <p className="mv-note" role="alert">
          {view.message ?? tx("mv.failed")}
        </p>
      ) : view.text === undefined ? (
        <p className="mv-note">{tx("mv.no_text")}</p>
      ) : (
        <pre className="mv-text" ref={textRef} tabIndex={0} aria-label={item.title}>
          {view.text}
        </pre>
      )}
    </div>
  );
}

/** The diff in the review's own rendering, on a page-coloured sheet that scrolls. */
function DiffStage({ project, demo }: { project: string; demo: Demo }) {
  return (
    <div className="mv-stage mv-reader" data-dismiss="">
      <div className="mv-diff" tabIndex={0}>
        <DiffDemoView project={project} demo={demo} />
      </div>
    </div>
  );
}

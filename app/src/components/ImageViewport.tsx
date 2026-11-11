import { useCallback, useEffect, useRef, useState, type CSSProperties, type MutableRefObject, type ReactNode } from "react";
import { regionFrom } from "../state/comments";
import {
  actualView,
  clampPan,
  fitView,
  focusArea,
  isFit,
  panBy,
  revealArea,
  toggleZoomAt,
  wheelFactor,
  zoomAt,
  zoomStep,
  ZOOM_STEP,
  type Area,
  type Point,
  type Size,
  type View,
} from "../state/viewport";
import { areaStyle, type Region } from "./MarkStage";

export type Tool = "mark" | "hand";

/** Zoom and pan state of the viewer's image, and the commands that change it. */
export interface Viewport {
  /** Callback ref for the stage element. */
  stageRef: (element: HTMLDivElement | null) => void;
  stage: () => HTMLDivElement | null;
  /** Null until both the image and the stage have a size. */
  view: View | null;
  /** The image's own size, once loaded. */
  size: Size | null;
  /** The view changes by a discrete step: animate it. */
  animate: boolean;
  /** Zoom in percent, or null before the image is measured. */
  percent: number | null;
  fitted: boolean;
  onImageLoad: (image: HTMLImageElement) => void;
  zoomIn: () => void;
  zoomOut: () => void;
  actual: () => void;
  fit: () => void;
  /** Zoom by a factor keeping the stage point fixed (pinch, Ctrl/⌘ + wheel). */
  zoomBy: (factor: number, point: Point) => void;
  /** Double-click: 2× at the point, or back to Fit. */
  toggleAt: (point: Point) => void;
  panBy: (dx: number, dy: number) => void;
  reveal: (area: Area) => void;
  /** Open on an area: centred and zoomed in on it. */
  focus: (area: Area) => void;
  /** A new image: forget the size and go back to Fit. */
  reset: () => void;
}

export function useViewport(): Viewport {
  const element = useRef<HTMLDivElement | null>(null);
  const observer = useRef<ResizeObserver | null>(null);
  const [image, setImage] = useState<Size | null>(null);
  const [stage, setStage] = useState<Size | null>(null);
  // Null means Fit, recomputed when the window changes size.
  const [manual, setManual] = useState<View | null>(null);
  const [animate, setAnimate] = useState(false);

  const measure = useCallback(() => {
    const box = element.current?.getBoundingClientRect();
    if (!box) return;
    setStage((last) => (last && last.w === box.width && last.h === box.height ? last : { w: box.width, h: box.height }));
  }, []);

  const stageRef = useCallback(
    (node: HTMLDivElement | null) => {
      observer.current?.disconnect();
      observer.current = null;
      element.current = node;
      if (!node) return;
      measure();
      if (typeof ResizeObserver !== "undefined") {
        observer.current = new ResizeObserver(measure);
        observer.current.observe(node);
      }
    },
    [measure],
  );

  useEffect(() => {
    window.addEventListener("resize", measure);
    return () => {
      window.removeEventListener("resize", measure);
      observer.current?.disconnect();
    };
  }, [measure]);

  const ready = image && stage && image.w > 0 && image.h > 0 && stage.w > 0 && stage.h > 0 ? { image, stage } : null;
  const view = ready ? (manual ? clampPan(manual, ready.image, ready.stage) : fitView(ready.image, ready.stage)) : null;

  // Latest values for the commands, which stay stable for listeners.
  const latest = useRef({ ready, view });
  latest.current = { ready, view };

  const change = useCallback((next: (view: View, image: Size, stage: Size) => View, smooth: boolean) => {
    const { ready: now, view: current } = latest.current;
    if (!now || !current) return;
    const result = next(current, now.image, now.stage);
    latest.current = { ready: now, view: result };
    setAnimate(smooth);
    // At the fit scale the view is the fit view: keep following the window.
    setManual(isFit(result, now.image, now.stage) ? null : result);
  }, []);

  return {
    stageRef,
    stage: () => element.current,
    view,
    size: ready ? ready.image : null,
    animate,
    percent: view ? Math.round(view.scale * 100) : null,
    fitted: ready && view ? manual === null || isFit(view, ready.image, ready.stage) : true,
    onImageLoad: (img) => {
      setImage({ w: img.naturalWidth, h: img.naturalHeight });
      measure();
    },
    zoomIn: () => change((v, i, s) => zoomStep(v, ZOOM_STEP, i, s), true),
    zoomOut: () => change((v, i, s) => zoomStep(v, 1 / ZOOM_STEP, i, s), true),
    actual: () => change((v, i, s) => actualView(v, i, s), true),
    fit: () => {
      setAnimate(true);
      setManual(null);
    },
    zoomBy: (factor, point) => change((v, i, s) => zoomAt(v, v.scale * factor, point, i, s), false),
    toggleAt: (point) => change((v, i, s) => toggleZoomAt(v, point, i, s), true),
    panBy: (dx, dy) => change((v, i, s) => panBy(v, dx, dy, i, s), false),
    reveal: (area) => change((v, i, s) => revealArea(v, area, i, s), true),
    focus: (area) => change((_, i, s) => focusArea(area, i, s), false),
    reset: () => {
      setImage(null);
      setManual(null);
      setAnimate(false);
    },
  };
}

/** A drag shorter than this many pixels is a click. */
const DRAG_PX = 4;
const LINE_PX = 16;

/** A pointer position as fractions (0..1) of the element's box. */
function fraction(event: { clientX: number; clientY: number }, element: HTMLElement): Point {
  const box = element.getBoundingClientRect();
  return {
    x: box.width ? (event.clientX - box.left) / box.width : 0,
    y: box.height ? (event.clientY - box.top) / box.height : 0,
  };
}

function stagePoint(event: { clientX: number; clientY: number }, stage: HTMLElement): Point {
  const box = stage.getBoundingClientRect();
  return { x: event.clientX - box.left, y: event.clientY - box.top };
}

interface Gesture {
  start: Point;
  px: Point;
  last: Point;
  moved: boolean;
  pan: boolean;
}

/** Safari's trackpad pinch (not in the DOM typings). */
type PinchEvent = Event & { scale: number; clientX: number; clientY: number };

/**
 * The viewer's image on a stage that zooms and pans: Hand (or Space, or the
 * middle button) drags to pan, Mark drags a region, a double-click zooms at
 * the point, pinch and Ctrl/⌘ + wheel zoom at the pointer, a plain wheel pans.
 * Regions are fractions of the rendered image box, so any zoom gives the same
 * per-mille anchor.
 */
export function ImageViewport({
  src,
  title,
  note,
  viewport,
  tool,
  onDraw,
  onTap,
  swallowClick,
  children,
}: {
  src: string;
  title: string;
  /** Shown instead of the image while it loads or when it cannot be read. */
  note?: ReactNode;
  viewport: Viewport;
  tool: Tool;
  /** A region drawn with Mark. */
  onDraw?: (region: Region) => void;
  /** A click that did not drag, as fractions of the image. */
  onTap?: (point: Point) => void;
  /** Set after a drag so the click that ends it does not dismiss. */
  swallowClick: MutableRefObject<boolean>;
  /** Pins, boxes and popovers laid over the image. */
  children?: ReactNode;
}) {
  const image = useRef<HTMLImageElement | null>(null);
  const gesture = useRef<Gesture | null>(null);
  const [drawing, setDrawing] = useState<Region | null>(null);
  const [panning, setPanning] = useState(false);
  const pinch = useRef<number | null>(null);
  const live = useRef(viewport);
  live.current = viewport;

  // Wheel and pinch need non-passive listeners to keep the page from zooming.
  useEffect(() => {
    const stage = live.current.stage();
    if (!stage) return;
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      const unit = event.deltaMode === 1 ? LINE_PX : event.deltaMode === 2 ? stage.clientHeight : 1;
      if (event.ctrlKey || event.metaKey) {
        // Safari pinches through gesture events; ignore its wheel echo.
        if (pinch.current !== null) return;
        live.current.zoomBy(wheelFactor(event.deltaY * unit), stagePoint(event, stage));
        return;
      }
      let dx = event.deltaX * unit;
      let dy = event.deltaY * unit;
      if (event.shiftKey && dx === 0) {
        dx = dy;
        dy = 0;
      }
      live.current.panBy(-dx, -dy);
    };
    const onPinchStart = (event: Event) => {
      event.preventDefault();
      pinch.current = 1;
    };
    const onPinch = (event: Event) => {
      event.preventDefault();
      const { scale, clientX, clientY } = event as PinchEvent;
      if (pinch.current === null || !scale) return;
      live.current.zoomBy(scale / pinch.current, stagePoint({ clientX, clientY }, stage));
      pinch.current = scale;
    };
    const onPinchEnd = (event: Event) => {
      event.preventDefault();
      pinch.current = null;
    };
    stage.addEventListener("wheel", onWheel, { passive: false });
    stage.addEventListener("gesturestart", onPinchStart);
    stage.addEventListener("gesturechange", onPinch);
    stage.addEventListener("gestureend", onPinchEnd);
    return () => {
      stage.removeEventListener("wheel", onWheel);
      stage.removeEventListener("gesturestart", onPinchStart);
      stage.removeEventListener("gesturechange", onPinch);
      stage.removeEventListener("gestureend", onPinchEnd);
    };
  }, [viewport.stageRef]);

  function onPointerDown(event: React.PointerEvent<HTMLDivElement>): void {
    swallowClick.current = false;
    const middle = event.button === 1;
    if ((event.button !== 0 && !middle) || !image.current) return;
    const px = { x: event.clientX, y: event.clientY };
    gesture.current = {
      start: fraction(event, image.current),
      px,
      last: px,
      moved: false,
      pan: middle || tool === "hand",
    };
    if (middle) event.preventDefault();
    event.currentTarget.setPointerCapture?.(event.pointerId);
  }

  function onPointerMove(event: React.PointerEvent<HTMLDivElement>): void {
    const current = gesture.current;
    if (!current || !image.current) return;
    const far = Math.abs(event.clientX - current.px.x) > DRAG_PX || Math.abs(event.clientY - current.px.y) > DRAG_PX;
    if (!current.moved && !far) return;
    current.moved = true;
    if (current.pan) {
      setPanning(true);
      viewport.panBy(event.clientX - current.last.x, event.clientY - current.last.y);
      current.last = { x: event.clientX, y: event.clientY };
      return;
    }
    setDrawing(regionFrom(current.start, fraction(event, image.current)));
  }

  function onPointerUp(event: React.PointerEvent<HTMLDivElement>): void {
    const current = gesture.current;
    gesture.current = null;
    setPanning(false);
    if (!current || !image.current) return;
    if (!current.moved) {
      if (event.button === 0) onTap?.(fraction(event, image.current));
      return;
    }
    swallowClick.current = true;
    if (current.pan) return;
    setDrawing(null);
    onDraw?.(regionFrom(current.start, fraction(event, image.current)));
  }

  const view = viewport.view;
  const canvas: CSSProperties | undefined =
    view && viewport.size
      ? {
          width: `${viewport.size.w * view.scale}px`,
          height: `${viewport.size.h * view.scale}px`,
          // Offsets, not a transform: a docked comment box stays fixed to the window.
          left: `${view.x}px`,
          top: `${view.y}px`,
        }
      : undefined;
  return (
    <div
      className="mv-stage mv-port"
      ref={viewport.stageRef}
      tabIndex={0}
      aria-label={title}
      data-tool={panning ? "panning" : tool}
      data-fit={viewport.fitted ? "true" : "false"}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={() => {
        gesture.current = null;
        setDrawing(null);
        setPanning(false);
      }}
      onDoubleClick={(event) => {
        const stage = viewport.stage();
        if (stage) viewport.toggleAt(stagePoint(event, stage));
      }}
    >
      {note ?? (
      <div
        className="mv-canvas"
        data-ready={canvas ? "true" : "false"}
        data-animate={viewport.animate ? "true" : "false"}
        style={canvas}
      >
        <img
          ref={image}
          src={src}
          alt={title}
          draggable={false}
          onLoad={(event) => viewport.onImageLoad(event.currentTarget)}
        />
        {children}
        {drawing ? <span className="cm-area drawing" style={areaStyle(drawing)} /> : null}
      </div>
      )}
    </div>
  );
}

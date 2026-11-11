/**
 * Zoom and pan of one image inside the viewer's stage: pure math, in stage
 * pixels. `scale` is rendered pixels per image pixel (1 = 100%); `x`/`y` is
 * where the image's top-left corner sits in the stage.
 */

export interface Size {
  w: number;
  h: number;
}

export interface Point {
  x: number;
  y: number;
}

export interface View {
  scale: number;
  x: number;
  y: number;
}

/** Per-mille rectangle of the image, as comment anchors use. */
export interface Area {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Zoom range: 25%–800%, and never above Fit at the low end. */
export const MIN_SCALE = 0.25;
export const MAX_SCALE = 8;
/** One step of the zoom buttons and keys. */
export const ZOOM_STEP = 1.25;

const EPSILON = 1e-6;

const clamp = (value: number, low: number, high: number) => Math.min(high, Math.max(low, value));

/** The scale that shows the whole image; small images keep their actual size. */
export function fitScale(image: Size, stage: Size): number {
  if (image.w <= 0 || image.h <= 0 || stage.w <= 0 || stage.h <= 0) return 1;
  return Math.min(stage.w / image.w, stage.h / image.h, 1);
}

/** Zoom limits for this image: a huge image may fit below 25%. */
export function scaleRange(image: Size, stage: Size): { min: number; max: number } {
  return { min: Math.min(MIN_SCALE, fitScale(image, stage)), max: MAX_SCALE };
}

export function clampScale(scale: number, image: Size, stage: Size): number {
  const { min, max } = scaleRange(image, stage);
  return clamp(scale, min, max);
}

/**
 * Keeps the image on screen: an axis smaller than the stage is centred, a
 * larger one may not leave a gap at either edge.
 */
export function clampPan(view: View, image: Size, stage: Size): View {
  const axis = (offset: number, length: number, room: number) =>
    length <= room ? (room - length) / 2 : clamp(offset, room - length, 0);
  return {
    scale: view.scale,
    x: axis(view.x, image.w * view.scale, stage.w),
    y: axis(view.y, image.h * view.scale, stage.h),
  };
}

/** The whole image, centred. */
export function fitView(image: Size, stage: Size): View {
  return clampPan({ scale: fitScale(image, stage), x: 0, y: 0 }, image, stage);
}

/** Actual size (100%), centred on the stage's centre. */
export function actualView(view: View, image: Size, stage: Size): View {
  return zoomAt(view, 1, { x: stage.w / 2, y: stage.h / 2 }, image, stage);
}

/**
 * Zoom to `scale` so the image point under `point` stays under it (as far as
 * clamping allows).
 */
export function zoomAt(view: View, scale: number, point: Point, image: Size, stage: Size): View {
  const next = clampScale(scale, image, stage);
  const u = (point.x - view.x) / view.scale;
  const v = (point.y - view.y) / view.scale;
  return clampPan({ scale: next, x: point.x - u * next, y: point.y - v * next }, image, stage);
}

/** Zoom by a factor around the stage's centre (buttons and keys). */
export function zoomStep(view: View, factor: number, image: Size, stage: Size): View {
  return zoomAt(view, view.scale * factor, { x: stage.w / 2, y: stage.h / 2 }, image, stage);
}

export function panBy(view: View, dx: number, dy: number, image: Size, stage: Size): View {
  return clampPan({ scale: view.scale, x: view.x + dx, y: view.y + dy }, image, stage);
}

/** True when the view shows the image at its fit scale. */
export function isFit(view: View, image: Size, stage: Size): boolean {
  return Math.abs(view.scale - fitScale(image, stage)) < EPSILON;
}

/**
 * Double-click: below 2× Fit, zoom in 2× around the point; at 2× Fit or more,
 * back to Fit.
 */
export function toggleZoomAt(view: View, point: Point, image: Size, stage: Size): View {
  const fit = fitScale(image, stage);
  if (view.scale >= fit * 2 - EPSILON) return fitView(image, stage);
  return zoomAt(view, view.scale * 2, point, image, stage);
}

/**
 * Pinch or Ctrl/⌘ + wheel: a zoom factor from a wheel delta. Large mouse-wheel
 * notches are capped so one notch is one moderate step.
 */
export function wheelFactor(deltaY: number): number {
  return Math.exp(-clamp(deltaY, -50, 50) * 0.01);
}

/**
 * Brings a per-mille area into view, centred; zooms in or out only when it
 * does not fit at the current scale or is a speck.
 */
export function revealArea(view: View, area: Area, image: Size, stage: Size): View {
  const w = (area.w / 1000) * image.w;
  const h = (area.h / 1000) * image.h;
  let scale = view.scale;
  const room = Math.min((stage.w * 0.8) / Math.max(w, 1), (stage.h * 0.8) / Math.max(h, 1));
  if (w * scale > stage.w * 0.9 || h * scale > stage.h * 0.9) scale = room;
  scale = clampScale(Math.max(scale, fitScale(image, stage)), image, stage);
  const cx = ((area.x + area.w / 2) / 1000) * image.w;
  const cy = ((area.y + area.h / 2) / 1000) * image.h;
  return clampPan({ scale, x: stage.w / 2 - cx * scale, y: stage.h / 2 - cy * scale }, image, stage);
}

/**
 * Opens on a per-mille area: centred, and zoomed in until it fills about half
 * the stage, never past actual size.
 */
export function focusArea(area: Area, image: Size, stage: Size): View {
  const w = Math.max((area.w / 1000) * image.w, 1);
  const h = Math.max((area.h / 1000) * image.h, 1);
  const fit = fitScale(image, stage);
  const half = Math.min((stage.w * 0.5) / w, (stage.h * 0.5) / h);
  const scale = clampScale(Math.max(fit, Math.min(half, 1)), image, stage);
  const cx = ((area.x + area.w / 2) / 1000) * image.w;
  const cy = ((area.y + area.h / 2) / 1000) * image.h;
  return clampPan({ scale, x: stage.w / 2 - cx * scale, y: stage.h / 2 - cy * scale }, image, stage);
}

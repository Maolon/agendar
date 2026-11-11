import { describe, expect, it } from "vitest";
import {
  actualView,
  clampPan,
  fitScale,
  fitView,
  focusArea,
  isFit,
  panBy,
  revealArea,
  scaleRange,
  toggleZoomAt,
  wheelFactor,
  zoomAt,
  type View,
} from "../state/viewport";

const IMAGE = { w: 1600, h: 800 };
const STAGE = { w: 800, h: 400 };

/** The image point under a stage point. */
const under = (view: View, x: number, y: number) => ({ u: (x - view.x) / view.scale, v: (y - view.y) / view.scale });

describe("viewport math", () => {
  it("fits large images and keeps small ones at actual size", () => {
    expect(fitScale(IMAGE, STAGE)).toBe(0.5);
    expect(fitScale({ w: 200, h: 100 }, STAGE)).toBe(1);
    expect(fitView(IMAGE, STAGE)).toEqual({ scale: 0.5, x: 0, y: 0 });
    // A small image is centred.
    expect(fitView({ w: 200, h: 100 }, STAGE)).toEqual({ scale: 1, x: 300, y: 150 });
    expect(actualView(fitView(IMAGE, STAGE), IMAGE, STAGE).scale).toBe(1);
  });

  it("zooms around a point and keeps that image point under it", () => {
    const fit = fitView(IMAGE, STAGE);
    const before = under(fit, 200, 100);
    const zoomed = zoomAt(fit, 1.5, { x: 200, y: 100 }, IMAGE, STAGE);
    expect(zoomed.scale).toBe(1.5);
    const after = under(zoomed, 200, 100);
    expect(after.u).toBeCloseTo(before.u);
    expect(after.v).toBeCloseTo(before.v);
  });

  it("clamps zoom to 25%–800%, or below 25% when that is what fits", () => {
    const fit = fitView(IMAGE, STAGE);
    expect(zoomAt(fit, 40, { x: 0, y: 0 }, IMAGE, STAGE).scale).toBe(8);
    expect(zoomAt(fit, 0.01, { x: 0, y: 0 }, IMAGE, STAGE).scale).toBe(0.25);
    const huge = { w: 8000, h: 4000 };
    expect(scaleRange(huge, STAGE).min).toBe(0.1);
    expect(wheelFactor(-1000)).toBeCloseTo(Math.exp(0.5));
  });

  it("clamps panning so the image cannot leave the stage", () => {
    const view = zoomAt(fitView(IMAGE, STAGE), 1, { x: 0, y: 0 }, IMAGE, STAGE);
    expect(panBy(view, 5000, 5000, IMAGE, STAGE)).toEqual({ scale: 1, x: 0, y: 0 });
    expect(panBy(view, -5000, -5000, IMAGE, STAGE)).toEqual({ scale: 1, x: -800, y: -400 });
    // An axis smaller than the stage stays centred.
    expect(clampPan({ scale: 0.25, x: -50, y: 90 }, IMAGE, STAGE)).toEqual({ scale: 0.25, x: 200, y: 100 });
  });

  it("toggles on double-click: 2× at the point, then back to Fit", () => {
    const fit = fitView(IMAGE, STAGE);
    const zoomed = toggleZoomAt(fit, { x: 200, y: 100 }, IMAGE, STAGE);
    expect(zoomed).toEqual({ scale: 1, x: -200, y: -100 });
    expect(isFit(zoomed, IMAGE, STAGE)).toBe(false);
    expect(toggleZoomAt(zoomed, { x: 10, y: 10 }, IMAGE, STAGE)).toEqual(fit);
    // At or past 2× Fit (from the wheel), a double-click returns to Fit too.
    expect(toggleZoomAt({ scale: 3, x: 0, y: 0 }, { x: 10, y: 10 }, IMAGE, STAGE)).toEqual(fit);
  });

  it("reveals an area: centred, zooming in only when it does not fit", () => {
    const fit = fitView(IMAGE, STAGE);
    // A small area at fit: same scale, centred as far as clamping allows.
    expect(revealArea(fit, { x: 0, y: 0, w: 100, h: 100 }, IMAGE, STAGE)).toEqual(fit);
    const close = { scale: 4, x: 0, y: 0 };
    const shown = revealArea(close, { x: 500, y: 500, w: 200, h: 200 }, IMAGE, STAGE);
    // 320×160 px of image fits 80% of the stage at 2×.
    expect(shown.scale).toBe(2);
    const centre = under(shown, 400, 200);
    expect(centre.u).toBeCloseTo(960);
    expect(centre.v).toBeCloseTo(480);
  });

  it("opens on an area: centred, zoomed in to at most actual size", () => {
    // 160×80 px of image would fill half the stage at 2.5×: capped at 100%.
    const shown = focusArea({ x: 450, y: 450, w: 100, h: 100 }, IMAGE, STAGE);
    expect(shown.scale).toBe(1);
    const centre = under(shown, 400, 200);
    expect(centre.u).toBeCloseTo(800);
    expect(centre.v).toBeCloseTo(400);
    // A corner area is clamped to the image's edge.
    expect(focusArea({ x: 0, y: 0, w: 100, h: 100 }, IMAGE, STAGE)).toEqual({ scale: 1, x: 0, y: 0 });
    // The whole image stays at Fit.
    expect(focusArea({ x: 0, y: 0, w: 1000, h: 1000 }, IMAGE, STAGE)).toEqual(fitView(IMAGE, STAGE));
  });
});

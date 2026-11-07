// @vitest-environment node
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const TOKENS = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../styles/tokens.css",
);

function linear(channel: number): number {
  return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
}

function luminance(hex: string): number {
  const value = hex.replace("#", "");
  const r = parseInt(value.slice(0, 2), 16) / 255;
  const g = parseInt(value.slice(2, 4), 16) / 255;
  const b = parseInt(value.slice(4, 6), 16) / 255;
  return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
}

function contrast(a: string, b: string): number {
  const first = luminance(a);
  const second = luminance(b);
  const lighter = Math.max(first, second);
  const darker = Math.min(first, second);
  return (lighter + 0.05) / (darker + 0.05);
}

describe("token set", () => {
  it("keeps the approved opaque label on the unchanged amber button in both themes", async () => {
    const css = await readFile(TOKENS, "utf8");
    const onAccent = [...css.matchAll(/--on-accent:\s*(#[0-9a-fA-F]{6})/g)].map(
      (match) => match[1] ?? "",
    );
    expect(onAccent.length).toBeGreaterThanOrEqual(2);
    expect(new Set(onAccent.map((value) => value.toLowerCase()))).toEqual(new Set(["#1d1d1f"]));

    const amber = [...css.matchAll(/--amber:[^#]*(#[0-9a-fA-F]{6})/g)].map(
      (match) => (match[1] ?? "").toLowerCase(),
    );
    expect(amber).toContain("#d9820b");
    expect(amber).toContain("#e8890c");

    expect(contrast("#1d1d1f", "#d9820b")).toBeCloseTo(5.7288, 3);
    expect(contrast("#1d1d1f", "#e8890c")).toBeCloseTo(6.4232, 3);
  });
});

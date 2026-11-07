// @vitest-environment node
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const BASE = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../styles/base.css",
);

describe("motion preference stylesheet", () => {
  it("disables transitions as well as keyframes under the still root", async () => {
    const css = await readFile(BASE, "utf8");
    // `.gnode` keeps a 0.15 s opacity transition; turning the preference off
    // must stop that movement too, including on pseudo-elements.
    const selector = css.match(/:root\.still[^{]*\{/)?.[0] ?? "";
    expect(selector).toContain("*::before");
    expect(selector).toContain("*::after");
    const rules = [...css.matchAll(/:root\.still[^{]*\{([^}]*)\}/g)].map(
      (match) => match[1] ?? "",
    );
    expect(rules.length).toBeGreaterThan(0);
    const still = rules.join("\n");
    expect(still).toMatch(/animation:\s*none\s*!important/);
    expect(still).toMatch(/transition:\s*none\s*!important/);
  });
});

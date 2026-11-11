// @vitest-environment node
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const LAYOUT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../styles/layout.css");

/** The declarations of the first top-level rule with exactly this selector. */
function rule(css: string, selector: string): string {
  const start = css.indexOf(`\n${selector} {`);
  if (start < 0) throw new Error(`no rule ${selector}`);
  const open = css.indexOf("{", start);
  return css.slice(open + 1, css.indexOf("}", open));
}

describe("sidebar footer layout", () => {
  it("never wraps, so a status tag change cannot make the footer taller", async () => {
    const css = await readFile(LAYOUT, "utf8");
    const row = rule(css, ".foot-status");
    expect(row).toMatch(/flex-wrap:\s*nowrap/);
    expect(row).toMatch(/white-space:\s*nowrap/);
    expect(css).not.toMatch(/\.foot-status\s*\{[^}]*flex-wrap:\s*wrap/);
    expect(rule(css, ".foot-item .foot-text")).toMatch(/text-overflow:\s*ellipsis/);
  });

  it("lets the agents label give way first: the status tag never shrinks beside it", async () => {
    const css = await readFile(LAYOUT, "utf8");
    // Weighted shrink factors (e.g. 10 vs 1) share any shortfall, so the sync
    // label would truncate too: only a non-shrinking tag makes it "first".
    const agents = rule(css, ".foot-item.foot-agents");
    expect(agents).toMatch(/flex:\s*0 1 auto/);
    expect(agents).toMatch(/overflow:\s*hidden/);
    expect(rule(css, ".foot-item.foot-tag")).toMatch(/flex:\s*0 0 auto/);
    // A tag wider than the whole row is still capped by it.
    expect(rule(css, ".foot-tag")).toMatch(/max-width:\s*100%/);
    // No override anywhere (a container query included) lets the tag shrink
    // or the agents label stop shrinking. Every tag state (sync, checking,
    // update) is the same `.foot-item.foot-tag` slot.
    for (const match of css.matchAll(/([^{}]*\.foot-(?:tag|agents)[^{}]*)\{([^}]*)\}/g)) {
      const [, selector, body] = match;
      if (/\bflex(-shrink)?:/.test(body!)) {
        expect(`${selector!.trim()} {${body}}`).toMatch(
          /foot-agents[^{]*\{[^}]*flex:\s*0 1 auto|foot-tag[^{]*\{[^}]*flex:\s*0 0 auto/,
        );
      }
    }
  });
});

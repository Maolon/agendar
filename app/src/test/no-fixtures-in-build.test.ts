// @vitest-environment node
import { mkdir, mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { describe, expect, it } from "vitest";

const APP_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
// Build directories live under the repository target/ tree (AGENTS.md).
const TARGET_ROOT = path.resolve(APP_ROOT, "../target");

async function collectJs(directory: string): Promise<{ name: string; text: string }[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  const files: { name: string; text: string }[] = [];
  for (const entry of entries) {
    const full = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await collectJs(full)));
    } else if (entry.name.endsWith(".js")) {
      files.push({ name: entry.name, text: await readFile(full, "utf8") });
    }
  }
  return files;
}

describe("production build", () => {
  it(
    "contains no fixture transport data",
    async () => {
      await mkdir(TARGET_ROOT, { recursive: true });
      const outDir = await mkdtemp(path.join(TARGET_ROOT, "agendar-fixture-build-"));
      const previousNodeEnv = process.env.NODE_ENV;
      try {
        // `vite build` runs with NODE_ENV=production; vitest sets it to "test",
        // which would make import.meta.env.DEV true inside the child build.
        process.env.NODE_ENV = "production";
        await build({
          root: APP_ROOT,
          configFile: path.join(APP_ROOT, "vite.config.ts"),
          mode: "production",
          logLevel: "silent",
          build: { outDir, emptyOutDir: true, write: true },
        });
        process.env.NODE_ENV = previousNodeEnv;
        const files = await collectJs(outDir);
        expect(files.length).toBeGreaterThan(0);
        expect(files.some((file) => file.name.toLowerCase().includes("fixture"))).toBe(false);
        const bundle = files.map((file) => file.text).join("\n");
        expect(bundle).toContain("Agendar");
        for (const sentinel of [
          "help-site",
          "fixture-token-",
          "h-01h0000000000000000000000",
          "How should answers reach idle agents?",
          // Delivery fixtures: the review view and its check profile.
          "search-v2",
          "Alias search results in the terminal",
          "respect-daily-limit",
          "alias table hit",
          // Twelve-project fixtures: project names and brief wording.
          "screen-clips",
          "relay-box",
          "toolbelt",
          "Encode the model",
          "9 features are built on unconfirmed choices",
          "Interview notes pipeline",
          // Showcase data for the README screenshots.
          "How should two devices settle an edit conflict?",
          "A calm reading app for long articles",
          "Shipping quotes for small online shops",
          // Assumption fixture: only the dev transport may hold it.
          "Keep WebM as the capture format?",
          "The recorder has to pick a container",
        ]) {
          expect(bundle).not.toContain(sentinel);
        }
      } finally {
        process.env.NODE_ENV = previousNodeEnv;
        await rm(outDir, { recursive: true, force: true });
      }
    },
    120_000,
  );
});

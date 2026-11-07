#!/usr/bin/env node
/**
 * Unsigned native build check: stage the sidecar, then run
 * `tauri build --no-bundle` from the app directory.
 *
 * This is the release-build check; a plain Vite build does not
 * exercise the Rust app or the asset embedding.
 *
 * Usage: node scripts/check-build.mjs [--target <triple>]
 */
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const appDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function run(command, args) {
  const result = spawnSync(command, args, {
    stdio: "inherit",
    cwd: appDir,
    env: process.env,
    shell: process.platform === "win32",
  });
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

const passthrough = process.argv.slice(2);
run(process.execPath, [resolve(appDir, "scripts/bundle-cli.mjs"), ...passthrough]);
const pnpm = process.platform === "win32" ? "pnpm.cmd" : "pnpm";
run(pnpm, ["exec", "tauri", "build", "--no-bundle", ...passthrough]);

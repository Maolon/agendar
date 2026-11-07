#!/usr/bin/env node
/**
 * Stage the bundled agendar CLI as the Tauri sidecar.
 *
 * `tauri.conf.json` declares `bundle.externalBin = ["binaries/agendar"]`, so the
 * CLI binary must be named `binaries/agendar-<triple>[.exe]` before a Tauri
 * build. The file is generated (ignored by Git); no download and no unsigned
 * prebuilt binary is used.
 *
 * Usage: node scripts/bundle-cli.mjs [--target <triple>]
 * Set AGENDAR_BUNDLE_ZIGBUILD=1 with --target to use `cargo zigbuild` for a
 * cross-target staging check (the release path uses the native C toolchain).
 */
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, copyFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const appDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = resolve(appDir, "..");

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    stdio: "inherit",
    cwd: options.cwd ?? repoRoot,
    env: process.env,
    shell: process.platform === "win32",
  });
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

function rustcHost() {
  const result = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
  if (result.status !== 0) {
    console.error("bundle:cli cannot read rustc -vV");
    process.exit(result.status ?? 1);
  }
  const line = result.stdout
    .split(/\r?\n/)
    .find((candidate) => candidate.startsWith("host: "));
  if (!line) {
    console.error("bundle:cli cannot find the rustc host triple");
    process.exit(1);
  }
  return line.slice("host: ".length).trim();
}

let target = null;
const args = process.argv.slice(2);
for (let index = 0; index < args.length; index += 1) {
  if (args[index] === "--target") {
    target = args[index + 1];
    index += 1;
    if (!target) {
      console.error("bundle:cli: --target needs a triple");
      process.exit(1);
    }
  } else if (args[index] === "--help" || args[index] === "-h") {
    console.log("Usage: node scripts/bundle-cli.mjs [--target <triple>]");
    process.exit(0);
  } else {
    console.error(`bundle:cli: unknown argument ${args[index]}`);
    process.exit(1);
  }
}

const triple = target ?? rustcHost();
const useZig = process.env.AGENDAR_BUNDLE_ZIGBUILD === "1" && target;
if (useZig) {
  run("cargo", [
    "zigbuild",
    "--locked",
    "--release",
    "-p",
    "agendar-cli",
    "--target",
    triple,
  ]);
} else {
  const cargoArgs = ["build", "--locked", "--release", "-p", "agendar-cli"];
  if (target) cargoArgs.push("--target", triple);
  run("cargo", cargoArgs);
}

const exe = process.platform === "win32" || triple.includes("windows") ? ".exe" : "";
const built = target
  ? join(repoRoot, "target", triple, "release", `agendar${exe}`)
  : join(repoRoot, "target", "release", `agendar${exe}`);
if (!existsSync(built)) {
  console.error(`bundle:cli: expected CLI at ${built}`);
  process.exit(1);
}
const destinationDir = join(appDir, "src-tauri", "binaries");
mkdirSync(destinationDir, { recursive: true });
const destination = join(destinationDir, `agendar-${triple}${exe}`);
copyFileSync(built, destination);
console.log(`bundle:cli: staged ${destination}`);

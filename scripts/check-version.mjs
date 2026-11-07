#!/usr/bin/env node
/**
 * Every place that carries the release version must agree. The workspace
 * version in Cargo.toml is the source; release PRs bump all of them together.
 *
 * Usage: node scripts/check-version.mjs [--expect <version>]
 */
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const read = (path) => readFileSync(resolve(root, path), "utf8");

function workspaceVersion() {
  const section = read("Cargo.toml").split(/^\[/m).find((part) => part.startsWith("workspace.package]"));
  const match = section?.match(/^version\s*=\s*"([^"]+)"/m);
  if (!match) throw new Error("Cargo.toml has no [workspace.package] version");
  return match[1];
}

const found = [["Cargo.toml [workspace.package]", workspaceVersion()]];
found.push(["app/package.json", JSON.parse(read("app/package.json")).version]);
found.push(["app/src-tauri/tauri.conf.json", JSON.parse(read("app/src-tauri/tauri.conf.json")).version]);
found.push([".release-please-manifest.json", JSON.parse(read(".release-please-manifest.json"))["."]]);
for (const [, name, version] of read("Cargo.lock").matchAll(/^name = "(agendar-[a-z-]+)"\nversion = "([^"]+)"/gm)) {
  found.push([`Cargo.lock ${name}`, version]);
}

const args = process.argv.slice(2);
const expect = args[0] === "--expect" ? args[1] : found[0][1];
const wrong = found.filter(([, version]) => version !== expect);
for (const [where, version] of found) console.log(`${version === expect ? "ok " : "BAD"} ${where}: ${version}`);
if (found.filter(([where]) => where.startsWith("Cargo.lock")).length < 5) {
  console.error("Cargo.lock is missing workspace members");
  process.exit(1);
}
if (wrong.length) {
  console.error(`version mismatch: expected ${expect} everywhere`);
  process.exit(1);
}

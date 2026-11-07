/**
 * Canonical JSON and SHA-256 for the revision preconditions the frontend has
 * to compute itself.
 *
 * `set_rule` needs the revision of the addressed rule identity. A rule that has
 * no projection entry is the literal `"absent"` sentinel hashed together with
 * the command discriminator, the project and the rule id, so the client can
 * send an exact precondition for a rule it is about to create.
 *
 * `approve_check` needs the same for a check profile: the digest of the exact
 * argv/cwd/timeout and the revision of "this profile is not approved yet".
 */

import type { RuleState } from "../api/types";

/** The profile fields one approval binds; nothing else is hashed. */
export interface CheckProfileHashInput {
  argv: string[];
  cwd: string;
  timeout_s: number;
}

/** Compact canonical JSON: sorted object keys, arrays keep their order, no LF. */
export function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value) ?? "null";
  if (Array.isArray(value)) return `[${value.map((item) => canonicalJson(item)).join(",")}]`;
  const entries = Object.entries(value as Record<string, unknown>)
    .filter(([, item]) => item !== undefined)
    .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
  return `{${entries
    .map(([key, item]) => `${JSON.stringify(key)}:${canonicalJson(item)}`)
    .join(",")}}`;
}

function toHex(buffer: ArrayBuffer): string {
  return [...new Uint8Array(buffer)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Lowercase SHA-256 of the UTF-8 bytes of `text`. */
export async function sha256Hex(text: string): Promise<string> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) throw new Error("SHA-256 is not available on this host.");
  const digest = await subtle.digest("SHA-256", new TextEncoder().encode(text));
  return toHex(digest);
}

/** Revision of the rule identity as it exists now, or its `absent` sentinel. */
export function ruleRevision(rules: RuleState[], project: string, ruleId: string): Promise<string> {
  const existing = rules.find((entry) => entry.value.rule_id === ruleId);
  if (existing) return Promise.resolve(existing.revision);
  return sha256Hex(canonicalJson(["rule", project, ruleId, "absent"]));
}

/**
 * Digest binding one normalized check profile, exactly as `profile_digest` in
 * `crates/agendar-core/src/runtime/checks.rs` computes it.
 */
export function checkProfileDigest(profile: CheckProfileHashInput): Promise<string> {
  return sha256Hex(
    canonicalJson(["check-profile", profile.argv, profile.cwd, profile.timeout_s]),
  );
}

/**
 * Revision of "this exact profile has no approval yet", the precondition for
 * the first `approve_check` (the counterpart of `absent_revision` in the same
 * core module). A changed argv or cwd changes it, so approval stays exact.
 */
export async function absentApprovalRevision(profile: CheckProfileHashInput): Promise<string> {
  const digest = await checkProfileDigest(profile);
  return sha256Hex(canonicalJson(["check-approval", "absent", digest]));
}

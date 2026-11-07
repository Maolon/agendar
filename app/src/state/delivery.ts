/**
 * Pure helpers over the delivery projection.
 *
 * Nothing here invents Vault facts: every value is read from the snapshot, and
 * anything the wire contract does not declare renders as "not declared" in the
 * views instead of being guessed.
 */

import { tx, type MessageKey } from "../i18n";
import { scalarLength } from "../api/format";
import type {
  BlobRef,
  CriterionId,
  DeliveryState,
  Demo,
  ProjectState,
  ProjectedCriterion,
  Provenance,
} from "../api/types";

/**
 * A check a delivery proposes for one of its criteria.
 *
 * The carrier is the model's declared `DemoCheck` on a demo body:
 * `{kind:"cli_transcript", body:{blob:<BlobRef>, check:{criterion,argv,cwd,timeout_s}}}`
 * (`Demo.body.check` in `types.ts`, written by `agendar deliver --json` and
 * serialized into the snapshot by the delivery projection). The reader still
 * validates the shape because the payload crosses the IPC boundary, but it
 * never adds a field the model does not declare.
 */
export interface CheckProposal {
  criterion: CriterionId;
  /** Exact argv as it will be executed: no shell, no substitutions. */
  argv: string[];
  /** Working directory, always inside a linked repository. */
  cwd: string;
  timeout_s: number;
}

const MAX_TIMEOUT_S = 120;

function readProposal(value: unknown): CheckProposal | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  const criterion = record.criterion;
  const argv = record.argv;
  const cwd = record.cwd;
  if (typeof criterion !== "string" || !criterion) return null;
  if (typeof cwd !== "string" || !cwd.trim()) return null;
  if (!Array.isArray(argv) || argv.length === 0) return null;
  if (!argv.every((item): item is string => typeof item === "string" && item.length > 0)) {
    return null;
  }
  // A profile the model would refuse is not shown at all: the view never
  // displays (and therefore never approves) a timeout the agent did not propose.
  const timeout = record.timeout_s;
  if (
    typeof timeout !== "number" ||
    !Number.isInteger(timeout) ||
    timeout < 1 ||
    timeout > MAX_TIMEOUT_S
  ) {
    return null;
  }
  return { criterion, argv, cwd, timeout_s: timeout };
}

/**
 * Every well-formed check the delivery proposes, in delivery order.
 *
 * Two declared model members can carry one, and a real delivery uses whichever
 * the harness wrote:
 * 1. `Demo.body.check` — the immutable proposal recorded with the delivery
 *    event (what `agendar deliver --json` writes on a demo body);
 * 2. `delivery.check_proposals` — this host's stored proposals, projected by the
 *    app from the local journal (`checks::proposals` in the core).
 * Identical profiles collapse, so one check is never shown twice.
 */
export function checkProposals(delivery: DeliveryState): CheckProposal[] {
  const proposals: CheckProposal[] = [];
  const seen = new Set<string>();
  const push = (candidate: unknown): void => {
    const proposal = readProposal(candidate);
    if (!proposal) return;
    const key = proposalKey(proposal);
    if (seen.has(key)) return;
    seen.add(key);
    proposals.push(proposal);
  };
  for (const demo of delivery.demos) push(demo.body?.check);
  for (const row of delivery.check_proposals ?? []) push(row);
  return proposals;
}

/** Stable identity of one proposal: the profile the human is asked to approve. */
export function proposalKey(proposal: CheckProposal): string {
  return [proposal.criterion, proposal.cwd, ...proposal.argv, String(proposal.timeout_s)].join("\u0000");
}

/**
 * The profile object sent to `approve_check`. The frontend displays exactly
 * these fields and never adds an argument behind the human's back.
 */
export function profileFor(proposal: CheckProposal): Record<string, unknown> {
  return {
    id: proposalKey(proposal),
    criterion: proposal.criterion,
    argv: proposal.argv,
    cwd: proposal.cwd,
    timeout_s: proposal.timeout_s,
  };
}

/**
 * The exact argv with every argument boundary visible, never a shell string.
 * Arguments that could be confused with whitespace or quotes are JSON-quoted,
 * so `["--actual", "one two.txt"]` and `["--actual", "one", "two.txt"]` can
 * never render the same way.
 */
export function argvLine(argv: string[]): string {
  return argv.map(quoteArgument).join(" ");
}

const BARE_ARGUMENT = /^[A-Za-z0-9_@%+=:,./-]+$/;

function quoteArgument(argument: string): string {
  return BARE_ARGUMENT.test(argument) ? argument : JSON.stringify(argument);
}

export function criterionFor(
  delivery: DeliveryState,
  criterion: CriterionId,
): ProjectedCriterion | undefined {
  return delivery.criteria.find((entry) => entry.criterion.id === criterion);
}

const PROVENANCE_LABEL: Record<Provenance, MessageKey> = {
  seen: "dl.prov.seen",
  agent_says: "dl.prov.agent",
  not_proven_yet: "dl.prov.unproven",
};

export function provenanceLabel(provenance: Provenance): string {
  return tx(PROVENANCE_LABEL[provenance] ?? PROVENANCE_LABEL.not_proven_yet);
}

/** Icon for a provenance value. */
export function provenanceIcon(provenance: Provenance): "eye" | "message-square" | "help-circle" {
  if (provenance === "seen") return "eye";
  if (provenance === "agent_says") return "message-square";
  return "help-circle";
}

/** A delivery is Seen only when every criterion has an app-observed result. */
export function deliveryProvenance(delivery: DeliveryState): Provenance {
  if (!delivery.criteria.length) return "not_proven_yet";
  const seen = delivery.criteria.filter((entry) => entry.provenance === "seen").length;
  if (seen === delivery.criteria.length) return "seen";
  return seen > 0 ? "agent_says" : "not_proven_yet";
}

const REVIEW_LABEL: Record<DeliveryState["review"], MessageKey> = {
  pending: "dl.rev.pending",
  accepted: "dl.rev.accepted",
  sent_back: "dl.rev.sent_back",
};

export function reviewLabel(review: DeliveryState["review"]): string {
  const key = REVIEW_LABEL[review];
  return key ? tx(key) : tx("dl.rev.none");
}

/** The first demo, used as the card thumbnail on the project page. */
export function firstDemo(delivery: DeliveryState): Demo | undefined {
  return delivery.demos[0];
}

export function demoBlob(demo: Demo): BlobRef | undefined {
  const blob = demo.body?.blob;
  if (!blob || typeof blob !== "object") return undefined;
  const record = blob as Record<string, unknown>;
  if (typeof record.sha256 !== "string" || typeof record.ext !== "string") return undefined;
  return {
    host: String(record.host ?? ""),
    sha256: record.sha256,
    ext: record.ext,
    bytes: typeof record.bytes === "number" ? record.bytes : 0,
  };
}

export function demoAlt(demo: Demo): string {
  const alt = demo.body?.alt;
  return typeof alt === "string" && alt ? alt : tx("dl.screenshot");
}

export function demoUrl(demo: Demo): string | undefined {
  const url = demo.body?.url;
  return typeof url === "string" && url ? url : undefined;
}

/** HTML reports are labelled as source: rendering stays unsupported (§9.4). */
export function isHtmlBlob(blob: BlobRef | undefined): boolean {
  if (!blob) return false;
  const ext = blob.ext.toLowerCase();
  return ext === "html" || ext === "htm";
}

/** Send-back reasons are 1..500 scalars (ReviewResult). */
export function reasonProblem(note: string): string | null {
  const trimmed = note.trim();
  if (!trimmed) return tx("dl.reason_blank");
  if (scalarLength(trimmed) > 500) return tx("dl.reason_long");
  return null;
}

/** Human-readable house-rule id derived from the note the human typed. */
export function ruleIdFrom(text: string, fallback: string): string {
  const slug = text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
  return slug || `rule-${fallback}`;
}

/** Inline media URL for a verified blob token. */
export function mediaUrl(token: string): string {
  if (!token) return "";
  const scope = window as unknown as {
    __TAURI_INTERNALS__?: { convertFileSrc?: (path: string, protocol?: string) => string };
  };
  const convert = scope.__TAURI_INTERNALS__?.convertFileSrc;
  if (convert) return convert(token, "agendar-media");
  // Browser fixture preview has no protocol handler; the URL is only shape-correct.
  return `agendar-media://localhost/${encodeURIComponent(token)}`;
}

/** Features whose plan depends on the delivered node ("accepting it unblocks …"). */
export function dependents(project: ProjectState, node: string): string[] {
  return Object.values(project.features)
    .filter((feature) => feature.depends_on.includes(node))
    .map((feature) => feature.title);
}

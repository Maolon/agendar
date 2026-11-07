/**
 * Development-only transport: answers the typed IPC contract from an in-memory
 * fixture snapshot. Loaded only when `import.meta.env.DEV` and
 * `VITE_AGENDAR_FIXTURES=1`; a production build cannot reach this module.
 */

import type {
  AnswerDecisionData,
  ApiEnvelope,
  ChangedEvent,
  CheckResult,
  DecisionState,
  DeliveryState,
  FeatureState,
  NeedsYouItem,
  PolicySet,
  PrepareAnswerData,
  ProjectState,
  Receipt,
  Reversibility,
  RuleState,
  Snapshot,
  Theme,
} from "../api/types";
import type { Transport } from "../api/client";
import { buildFixtureGraph, buildFixtureSnapshot, FIXTURE_STARTED_SESSION } from "./fixtures";
import { buildShowcaseSnapshot } from "./showcase";

const RISK: Record<Reversibility, number> = { easy: 0, costly: 1, irreversible: 2 };
const RISK_NAME: Reversibility[] = ["easy", "costly", "irreversible"];

function cardRisk(card: DecisionState["card"]): Reversibility {
  let worst = RISK[card.reversibility];
  for (const option of card.options) worst = Math.max(worst, RISK[option.reversibility]);
  if (card.external) worst = Math.max(worst, RISK.irreversible);
  return RISK_NAME[worst] ?? "easy";
}

function ok<T>(data: T): ApiEnvelope<T> {
  return { v: 1, ok: true, data };
}

function notImplemented(command: string): ApiEnvelope<never> {
  return {
    v: 1,
    ok: false,
    error: {
      code: "E_NOT_IMPLEMENTED",
      message: `Capability ${command} is not implemented in this version.`,
      hint: "Use the main decision, rescue and brief views.",
      retryable: false,
    },
  };
}

function findDecision(
  snapshot: Snapshot,
  askId: string,
): { project: ProjectState; ask: DecisionState; need?: NeedsYouItem } | undefined {
  for (const project of Object.values(snapshot.state.projects)) {
    const ask = project.decisions[askId];
    if (!ask) continue;
    // An assumption answered from the assumptions view has no Needs-you row.
    const need = snapshot.needs.find((item) => item.source === askId);
    return need ? { project, ask, need } : { project, ask };
  }
  return undefined;
}

function releaseAfter(project: ProjectState, node: string): string[] {
  return Object.values(project.features)
    .filter((item) => item.depends_on.includes(node))
    .map((item) => item.node);
}

/** Deterministic digest of the dev-only blobs (mirrors `fixtures.ts`). */
function digest(n: number): string {
  return n.toString(16).padStart(2, "0").repeat(32);
}

/** Dev-only ULID-shaped id for an event this transport pretends to append. */
function ulid(n: number): string {
  return `01J${String(Math.floor(n)).padStart(23, "0")}`;
}

let revisionCounter = 0xb0;

/** A fresh deterministic-looking revision for a field or policy this fixture writes. */
function nextRevision(): string {
  revisionCounter = (revisionCounter + 1) % 256;
  return digest(revisionCounter);
}

/**
 * Dev-only blob bodies. `read_report` returns text for transcripts/reports and
 * a media token for images; a real Vault would verify hash and size first.
 */
const REPORT_TEXT: Record<string, { mime: string; text?: string }> = {
  [digest(0x81)]: {
    mime: "text/plain",
    text: [
      '$ shelf search --author "Mark Twain"',
      "1  The Adventures of Huckleberry Finn",
      "2  The Prince and the Pauper",
      "3  A Connecticut Yankee in King Arthur's Court",
      "",
      "4 books · 38ms · alias table hit",
    ].join("\n"),
  },
  [digest(0x83)]: {
    mime: "text/html",
    text: [
      "<h1>Alias search v2</h1>",
      "<p>Pen names resolve to the real author before the query runs.</p>",
      '<p class="unproven">Speed on a 100k-book library is not proven yet.</p>',
    ].join("\n"),
  },
};

function findDelivery(
  snapshot: Snapshot,
  deliveryId: string,
): { project: ProjectState; delivery: DeliveryState } | undefined {
  for (const project of Object.values(snapshot.state.projects)) {
    const delivery = project.deliveries[deliveryId];
    if (delivery) return { project, delivery };
  }
  return undefined;
}

export function createFixtureTransport(dataset: "test" | "showcase" = "test"): Transport {
  const snapshot = dataset === "showcase" ? buildShowcaseSnapshot() : buildFixtureSnapshot();
  const listeners = new Map<string, Set<(payload: unknown) => void>>();
  const tokens = new Map<
    string,
    { askId: string; optionId: string; label: string; expiresAt: number }
  >();
  const approvals = new Map<string, unknown>();
  let tokenCounter = 0;

  function emit(event: string, payload: unknown): void {
    for (const handler of listeners.get(event) ?? []) handler(payload);
  }

  function publish(
    project: string,
    batch: ChangedEvent["batch"],
    pulses: ChangedEvent["pulses"] = [],
  ): void {
    snapshot.generation += 1;
    snapshot.state.evaluated_at = new Date().toISOString();
    snapshot.local.received_at = snapshot.state.evaluated_at;
    emit("agendar://changed", {
      v: 1,
      epoch: snapshot.epoch,
      generation: snapshot.generation,
      event_set_digest: snapshot.state.event_set_digest,
      as_of: snapshot.state.evaluated_at,
      projects: [project],
      pulses,
      batch,
    } satisfies ChangedEvent);
  }

  function dropNeed(need: NeedsYouItem): void {
    snapshot.needs = snapshot.needs.filter((item) => item.id !== need.id);
    const project = snapshot.state.projects[need.project];
    if (!project) return;
    project.needs_you = project.needs_you.filter((item) => item.id !== need.id);
    project.summary.needs_count = project.needs_you.length;
  }

  async function invoke(command: string, request: unknown): Promise<unknown> {
    const input = (request ?? {}) as Record<string, unknown>;
    switch (command) {
      case "get_snapshot":
        return ok(snapshot);
      case "get_setup_locations":
        return ok({ locations: [] });
      case "open_report":
        return ok({ opened: true });
      case "get_bindings":
        return ok({
          cli_path: "/Applications/Agendar.app/Contents/MacOS/agendar",
          bindings: [
            { harness: "claude_code", kind: "hooks", state: "bound", found: true, path: "~/.claude/settings.json", detail: "" },
            { harness: "codex", kind: "hooks", state: "stale", found: true, path: "~/.codex/hooks.json", detail: "" },
            { harness: "cursor", kind: "mcp", state: "bound", found: true, path: "~/.cursor/mcp.json", detail: "" },
            { harness: "gemini", kind: "mcp", state: "not_bound", found: true, path: "~/.gemini/settings.json", detail: "" },
            { harness: "agy", kind: "mcp", state: "unreadable", found: true, path: "~/.gemini/config/mcp_config.json", detail: "" },
            { harness: "opencode", kind: "mcp", state: "not_bound", found: true, path: "~/.config/opencode/opencode.json", detail: "" },
            { harness: "pi", kind: "mcp", state: "not_bound", found: false, path: "~/.pi/agent/mcp.json", detail: "" },
            { harness: "dsh", kind: "manual", state: "manual", found: true, detail: "" },
          ],
        });
      case "set_binding":
        return ok({ binding: { harness: "cursor", kind: "mcp", state: "bound", found: true, detail: "" } });
      case "test_notice":
        return ok({ sent: true });
      case "get_local_config":
        return ok({
          vault: "/srv/fixture/Agendar",
          data_dir: "/srv/fixture/agendar-data",
          config_revision: "0".repeat(64),
          repos: { "/srv/fixture/code/agendar": "agendar" },
          codex_bin: null,
          cli_path: "/Applications/Agendar.app/Contents/MacOS/agendar",
          app_version: "0.1.0",
        });
      case "get_project": {
        const project = snapshot.state.projects[String(input.project)];
        if (!project) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_NOT_FOUND",
              message: `Project ${String(input.project)} was not found.`,
              hint: "Pick a project from the sidebar.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        return ok({ project, graph: buildFixtureGraph(project) });
      }
      case "search": {
        const text = String(input.text ?? "").toLowerCase();
        const matches = (value: string) => value.toLowerCase().includes(text);
        const needs = snapshot.needs.filter(
          (item) => matches(item.question) || matches(item.project),
        );
        const projects = Object.values(snapshot.state.projects)
          .map((project) => project.summary)
          .filter(
            (summary) =>
              matches(summary.slug) ||
              matches(summary.title) ||
              Object.values(snapshot.state.projects[summary.slug]?.features ?? {}).some(
                (feature) => matches(feature.title),
              ),
          );
        return ok({ needs, projects });
      }
      case "prepare_answer": {
        const askId = String(input.ask_id ?? "");
        const found = findDecision(snapshot, askId);
        if (!found) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_WITHDRAWN",
              message: "Decision was withdrawn or superseded.",
              hint: "Open the current card.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        if (input.expected_decision_revision !== found.ask.revision) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_STALE_ANSWER",
              message: "Answer is no longer current.",
              hint: "Refresh the card before answering.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const option = found.ask.card.options.find((item) => item.id === input.option_id);
        if (!option) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_VALIDATION",
              message: "That option does not belong to this card.",
              field: "/option_id",
              hint: "Pick an option shown on the card.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        tokenCounter += 1;
        const token = `fixture-token-${tokenCounter}`;
        const expiresAt = Date.now() + 60_000;
        tokens.set(token, {
          askId,
          optionId: option.id,
          label: option.label,
          expiresAt,
        });
        const risk = cardRisk(found.ask.card);
        const data: PrepareAnswerData = {
          token,
          expires_at: new Date(expiresAt).toISOString(),
          risk,
          label: option.label,
          receipt: {
            preview: true,
            ask: askId,
            node: found.ask.node,
            source: "human",
            chose: option,
            ruled_out: found.ask.card.options.filter((item) => item.id !== option.id),
            done_when: found.ask.card.done_when,
            ...(option.eta ? { expected_eta: option.eta } : {}),
          },
          guard: risk === "easy" ? "easy" : risk === "costly" ? "choose" : "confirm",
        };
        return ok(data);
      }
      case "answer_decision": {
        const token = String(input.token ?? "");
        const prepared = tokens.get(token);
        if (!prepared || prepared.expiresAt < Date.now()) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_STALE_ANSWER",
              message: "Answer is no longer current.",
              hint: "Refresh the card before answering.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        if (input.echo_label !== prepared.label) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_STALE_ANSWER",
              message: "The chosen action changed.",
              field: "/echo_label",
              hint: "Pick the option again.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const found = findDecision(snapshot, prepared.askId);
        if (!found) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_WITHDRAWN",
              message: "Decision was withdrawn or superseded.",
              hint: "Open the current card.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const option = found.ask.card.options.find((item) => item.id === prepared.optionId);
        if (!option) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_VALIDATION",
              message: "That option does not belong to this card.",
              field: "/token",
              hint: "Pick an option shown on the card.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        tokens.delete(token);
        const receiptId = `01J${String(Date.now()).padStart(23, "0").slice(0, 23)}`;
        const receipt: Receipt = {
          id: receiptId,
          ask: found.ask.id,
          node: found.ask.node,
          source: "human",
          chose: option,
          ruled_out: found.ask.card.options.filter((item) => item.id !== option.id),
          done_when: found.ask.card.done_when,
          ...(option.eta ? { expected_eta: option.eta } : {}),
          created_at: new Date().toISOString(),
        };
        found.ask.lifecycle = "answered";
        found.ask.effective_receipt = receiptId;
        found.ask.answer_history = [...found.ask.answer_history, receiptId];
        found.project.receipts[receiptId] = receipt;
        for (const item of Object.values(found.project.features)) {
          if (item.waits_on.includes(`decision:${found.ask.id}`)) {
            item.gate = "open";
            item.waits_on = item.waits_on.filter((ref) => ref !== `decision:${found.ask.id}`);
          }
        }
        if (found.need) dropNeed(found.need);
        // Dev-only fixture: the answered ask releases its dependents and they
        // start with a live session, so the demo can show the unblock moment and
        // one real pulse. A production build never contains this module.
        const released = releaseAfter(found.project, found.ask.node);
        const at = new Date().toISOString();
        const pulses: ChangedEvent["pulses"] = [];
        for (const node of released) {
          const feature = found.project.features[node];
          if (!feature) continue;
          const pulseId = ulid(Date.now() + pulses.length + 1);
          feature.progress = "doing";
          feature.gate = "open";
          feature.paused = false;
          feature.liveness = "live";
          feature.session = FIXTURE_STARTED_SESSION;
          feature.last_pulse = pulseId;
          pulses.push({
            id: pulseId,
            project: found.project.summary.slug,
            node,
            host: snapshot.local.host,
            event_ts: at,
          });
        }
        if (pulses.length) {
          found.project.sessions[FIXTURE_STARTED_SESSION] = {
            id: FIXTURE_STARTED_SESSION,
            host: snapshot.local.host,
            harness: "claude_code",
            last_event: pulses[0]!.id,
            last_event_ts: at,
            current_node: pulses[0]!.node,
            liveness: "live",
            stalled_on: [],
          };
          found.project.summary.active_agents += 1;
        }
        publish(found.project.summary.slug, "local", pulses);
        emit("agendar://unblock", {
          v: 1,
          answer_id: receiptId,
          project: found.project.summary.slug,
          from: `feature:${found.ask.node}`,
          released,
          phase: "answered",
        });
        const data: AnswerDecisionData = {
          event_id: receiptId,
          receipt,
          awaiting_ack: true,
          released,
        };
        return ok(data);
      }
      case "resolve_rescue": {
        const itemId = String(input.item_id ?? "");
        const need = snapshot.needs.find((item) => item.id === itemId);
        if (!need) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_WITHDRAWN",
              message: "Rescue item is no longer current.",
              hint: "Refresh the brief.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        if (input.expected_rescue_revision !== need.revision) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_STALE_ANSWER",
              message: "Answer is no longer current.",
              hint: "Refresh the card before acting.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        if (input.action !== "handoff" && input.action !== "pause") {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_VALIDATION",
              message: "Unknown rescue action.",
              field: "/action",
              hint: "Choose hand off or pause.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const project = snapshot.state.projects[need.project];
        const feature: FeatureState | undefined = need.node
          ? project?.features[need.node]
          : undefined;
        if (feature) {
          if (input.action === "pause") {
            feature.paused = true;
            feature.gate = "awaiting_decision";
          } else {
            feature.gate = "open";
            feature.liveness = "quiet";
          }
        }
        dropNeed(need);
        if (project) publish(project.summary.slug, "local");
        return ok({ event_id: `01J${String(Date.now()).padStart(23, "0").slice(0, 23)}` });
      }
      case "review_delivery": {
        const verdict = input.verdict;
        if (verdict !== "accept" && verdict !== "send_back") {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_VALIDATION",
              message: "Unknown review verdict.",
              field: "/verdict",
              hint: "Accept or send back.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const found = findDelivery(snapshot, String(input.delivery_id ?? ""));
        if (!found) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_WITHDRAWN",
              message: "Delivery is no longer current.",
              hint: "Refresh the brief.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        if (input.expected_delivery_revision !== found.delivery.revision) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_STALE_ANSWER",
              message: "The delivery changed since it was shown.",
              hint: "Review the refreshed evidence.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        if (verdict === "send_back") {
          const reason = String(input.reason ?? "").trim();
          if (!reason) {
            return {
              v: 1,
              ok: false,
              error: {
                code: "E_VALIDATION",
                message: "A send-back needs a reason.",
                field: "/reason",
                hint: "Write what should change.",
                retryable: false,
              },
            } satisfies ApiEnvelope<never>;
          }
        }
        const reviewEvent = ulid(Date.now());
        found.delivery.review = verdict === "accept" ? "accepted" : "sent_back";
        found.delivery.review_event = reviewEvent;
        found.delivery.revision = digest(Date.now() % 256);
        const need = snapshot.needs.find((item) => item.source === found.delivery.id);
        if (need) dropNeed(need);
        found.project.needs_you = found.project.needs_you.filter(
          (item) => item.source !== found.delivery.id,
        );
        found.project.summary.needs_count = found.project.needs_you.length;
        if (verdict === "accept") {
          for (const item of Object.values(found.project.features)) {
            if (item.depends_on.includes(found.delivery.node) && item.gate === "awaiting_upstream") {
              item.gate = "open";
            }
          }
        }
        publish(found.project.summary.slug, "local");
        return ok({ event_id: reviewEvent, released: releaseAfter(found.project, found.delivery.node) });
      }
      case "set_rule": {
        const project = snapshot.state.projects[String(input.project)];
        if (!project) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_NOT_CONFIGURED",
              message: "That project is not available.",
              hint: "Pick a project from the sidebar.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const rule = input.rule as RuleState["value"] | undefined;
        if (!rule || !rule.rule_id || !rule.text) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_VALIDATION",
              message: "A rule needs an id and text.",
              field: "/rule",
              hint: "Write the rule first.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const index = project.rules.findIndex((entry) => entry.value.rule_id === rule.rule_id);
        const expected = index >= 0 ? project.rules[index]?.revision : undefined;
        if (expected !== undefined && input.expected_revision !== expected) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_STALE_ANSWER",
              message: "The rule changed since it was shown.",
              hint: "Refresh before saving.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const next: RuleState = { value: rule, revision: digest(0x53) };
        if (index >= 0) project.rules[index] = next;
        else project.rules.push(next);
        publish(project.summary.slug, "local");
        return ok({ event_id: ulid(Date.now()) });
      }
      case "set_policy": {
        const project = snapshot.state.projects[String(input.project)];
        if (!project) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_NOT_CONFIGURED",
              message: "That project is not available.",
              hint: "Pick a project from the sidebar.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        if (input.expected_revision !== project.policy.revision) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_STALE_ANSWER",
              message: "The policy changed since it was shown.",
              hint: "Refresh before saving.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const policy = input.policy as PolicySet | undefined;
        if (!policy || !policy.cells || !policy.budget) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_VALIDATION",
              message: "A policy needs cells and a budget.",
              field: "/policy",
              hint: "Reload the panel and try again.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const locked: PolicySet = {
          ...policy,
          cells: {
            ...policy.cells,
            irreversible_local: "ask",
            irreversible_wide: "ask",
          },
        };
        project.policy = { value: locked, revision: nextRevision() };
        publish(project.summary.slug, "local");
        return ok({ event_id: ulid(Date.now()) });
      }
      case "update_project": {
        const project = snapshot.state.projects[String(input.project)];
        if (!project) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_NOT_CONFIGURED",
              message: "That project is not available.",
              hint: "Pick a project from the sidebar.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const expected = (input.expected_fields ?? {}) as Record<string, string>;
        for (const [field, revision] of Object.entries(expected)) {
          if (project.field_revisions[field] !== revision) {
            return {
              v: 1,
              ok: false,
              error: {
                code: "E_STALE_ANSWER",
                message: `The ${field} field changed since it was shown.`,
                hint: "Refresh before saving.",
                retryable: false,
              },
            } satisfies ApiEnvelope<never>;
          }
        }
        const patch = (input.patch ?? {}) as Record<string, unknown>;
        if (!Object.keys(patch).length) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_VALIDATION",
              message: "Nothing to update.",
              hint: "Change a field first.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        if (patch.goal !== undefined) {
          const goal = String(patch.goal).trim();
          if (!goal || [...goal].length > 160) {
            return {
              v: 1,
              ok: false,
              error: {
                code: "E_VALIDATION",
                message: "A goal needs 1–160 characters.",
                field: "/patch/goal",
                hint: "Shorten the goal and save again.",
                retryable: false,
              },
            } satisfies ApiEnvelope<never>;
          }
          project.summary.goal = goal;
        }
        if (patch.title !== undefined) project.summary.title = String(patch.title);
        if (patch.brief !== undefined) project.summary.brief = String(patch.brief);
        for (const field of Object.keys(patch)) project.field_revisions[field] = nextRevision();
        publish(project.summary.slug, "local");
        return ok({ event_id: ulid(Date.now()) });
      }
      case "update_feature": {
        const project = snapshot.state.projects[String(input.project)];
        const feature = project?.features[String(input.node)];
        if (!project || !feature) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_NOT_FOUND",
              message: "That step is not projected.",
              hint: "Refresh the project.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const expected = (input.expected_fields ?? {}) as Record<string, string>;
        for (const [field, revision] of Object.entries(expected)) {
          if (feature.field_revisions[field] !== revision) {
            return {
              v: 1,
              ok: false,
              error: {
                code: "E_STALE_ANSWER",
                message: `The ${field} field changed since it was shown.`,
                hint: "Refresh before saving.",
                retryable: false,
              },
            } satisfies ApiEnvelope<never>;
          }
        }
        const patch = (input.patch ?? {}) as Record<string, unknown>;
        const event = ulid(Date.now());
        if (patch.title !== undefined) {
          const title = String(patch.title).trim();
          if (!title || [...title].length > 80) {
            return {
              v: 1,
              ok: false,
              error: {
                code: "E_VALIDATION",
                message: "A feature title needs 1–80 characters.",
                field: "/patch/title",
                hint: "Shorten the title and save again.",
                retryable: false,
              },
            } satisfies ApiEnvelope<never>;
          }
          feature.title = title;
          feature.field_sources.title = event;
          feature.field_revisions.title = nextRevision();
        }
        if (patch.deadline !== undefined) {
          const update = patch.deadline as { op?: string; value?: string };
          if (update.op === "clear") {
            delete feature.deadline;
          } else if (update.op === "set" && update.value && !Number.isNaN(Date.parse(update.value))) {
            feature.deadline = new Date(update.value).toISOString();
          } else {
            return {
              v: 1,
              ok: false,
              error: {
                code: "E_VALIDATION",
                message: "A deadline update is set or clear.",
                field: "/patch/deadline",
                hint: "Use a date and time.",
                retryable: false,
              },
            } satisfies ApiEnvelope<never>;
          }
          feature.field_sources.deadline = event;
          feature.field_revisions.deadline = nextRevision();
        }
        if (patch.priority !== undefined) {
          const priority = Number(patch.priority);
          if (!Number.isInteger(priority) || priority < 0 || priority > 3) {
            return {
              v: 1,
              ok: false,
              error: {
                code: "E_VALIDATION",
                message: "Priority is 0 (highest) to 3.",
                field: "/patch/priority",
                hint: "Use a whole number from 0 to 3.",
                retryable: false,
              },
            } satisfies ApiEnvelope<never>;
          }
          feature.priority = priority;
          feature.field_sources.priority = event;
          feature.field_revisions.priority = nextRevision();
        }
        publish(project.summary.slug, "local");
        return ok({ event_id: event });
      }
      case "read_report": {
        const blob = input.blob as { sha256?: string; ext?: string } | undefined;
        const sha256 = String(blob?.sha256 ?? "");
        const known = REPORT_TEXT[sha256];
        if (known) return ok({ mime: known.mime, text: known.text, sha256 });
        if (!/^[0-9a-f]{64}$/.test(sha256)) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_VALIDATION",
              message: "That blob reference is not readable.",
              hint: "Refresh the delivery.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        // Images are served through the media protocol by opaque token.
        return ok({ mime: "image/png", media_token: sha256, sha256 });
      }
      case "approve_check": {
        if (input.consent !== true) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_VALIDATION",
              message: "Approving a check needs explicit consent.",
              field: "/consent",
              hint: "Review the argv, then approve it.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const expected = String(input.expected_approval_revision ?? "");
        // The native command parses this field as a SHA-256 revision; the
        // fixture refuses anything else instead of hiding a placeholder token.
        if (!/^[0-9a-f]{64}$/.test(expected)) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_VALIDATION",
              message: "The approval revision must be a SHA-256 precondition.",
              field: "/expected_approval_revision",
              hint: "Refresh the delivery and approve the profile again.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const approvalId = (approvals.size + 1).toString(16).padStart(64, "0");
        approvals.set(approvalId, input.profile ?? {});
        return ok({ approval_id: approvalId });
      }
      case "run_check": {
        const approvalId = String(input.approval_id ?? "");
        if (!approvals.has(approvalId)) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_UNAPPROVED",
              message: "That check has not been approved.",
              hint: "Approve the exact argv and cwd first.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        if (String(input.expected_approval_revision ?? "") !== approvalId) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_STALE_ANSWER",
              message: "The approval changed since it was shown.",
              hint: "Approve the exact argv and cwd again.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const found = findDelivery(snapshot, String(input.delivery_id ?? ""));
        if (!found) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_WITHDRAWN",
              message: "Delivery is no longer current.",
              hint: "Refresh the brief.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        if (input.expected_delivery_revision !== found.delivery.revision) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_STALE_ANSWER",
              message: "The delivery changed since it was shown.",
              hint: "Review the refreshed evidence.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const criterion = found.delivery.criteria.find(
          (entry) => entry.criterion.id === String(input.criterion ?? ""),
        );
        if (!criterion) {
          return {
            v: 1,
            ok: false,
            error: {
              code: "E_VALIDATION",
              message: "That criterion does not belong to this delivery.",
              field: "/criterion",
              hint: "Pick a criterion shown on the card.",
              retryable: false,
            },
          } satisfies ApiEnvelope<never>;
        }
        const observationId = ulid(Date.now());
        const result: CheckResult = "passed";
        criterion.seen_result = result;
        criterion.provenance = "seen";
        criterion.observation = observationId;
        criterion.observation_history = [...criterion.observation_history, observationId];
        criterion.observation_frontier = [observationId];
        criterion.conflict = criterion.agent_result === "failed";
        publish(found.project.summary.slug, "local");
        return ok({ observation_id: observationId, result });
      }
      case "set_preferences": {
        const theme = input.theme as Theme | undefined;
        if (theme) snapshot.preferences.theme = theme;
        if (typeof input.motion === "boolean") snapshot.preferences.motion = input.motion;
        if (typeof input.language === "string") {
          snapshot.preferences.language = input.language as NonNullable<typeof snapshot.preferences.language>;
        }
        if (typeof input.notifications === "boolean") snapshot.preferences.notifications = input.notifications;
        snapshot.preferences.revision = snapshot.preferences.revision
          .split("")
          .reverse()
          .join("");
        return ok({ preferences: snapshot.preferences });
      }
      case "dismiss_need": {
        const itemId = String(input.item_id);
        snapshot.preferences.dismissed = [...new Set([...(snapshot.preferences.dismissed ?? []), itemId])];
        snapshot.preferences.revision = snapshot.preferences.revision.split("").reverse().join("");
        return ok({ preferences: snapshot.preferences });
      }
      case "snooze_need": {
        const itemId = String(input.item_id);
        const until = String(input.until);
        const kept = Object.entries({ ...snapshot.preferences.snoozed, [itemId]: until }).filter(
          ([, end]) => Date.parse(end) > Date.now(),
        );
        snapshot.preferences.snoozed = Object.fromEntries(kept);
        snapshot.preferences.revision = snapshot.preferences.revision.split("").reverse().join("");
        return ok({ preferences: snapshot.preferences });
      }
      case "show_main":
      case "hide_popover":
      case "set_glass_regions":
        return ok({});
      case "window_chrome":
        return ok({ overlay_titlebar: false, material: "solid" });
      default:
        return notImplemented(command);
    }
  }

  return {
    invoke,
    async listen(event, handler) {
      const set = listeners.get(event) ?? new Set<(payload: unknown) => void>();
      set.add(handler as (payload: unknown) => void);
      listeners.set(event, set);
      return () => {
        set.delete(handler as (payload: unknown) => void);
      };
    },
  };
}

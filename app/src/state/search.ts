/**
 * Search over what the snapshot already holds: needs, projects, steps,
 * decisions, reports and deliveries of every project. Client-side, so it
 * answers on each keystroke without a round trip.
 *
 * Matching is case-, accent- and width-insensitive (NFKD folding with marks
 * removed); every word of the query must appear in some field of the item.
 * Chinese text has no word breaks, so a query matches as a substring.
 */

import { needQuestion, needReason } from "../i18n/msg";
import { tx, type MessageKey } from "../i18n";
import type {
  DecisionLifecycle,
  NeedKind,
  ReportState,
  Snapshot,
} from "../api/types";
import { reviewLabel } from "./delivery";
import { PROGRESS_LABEL } from "./project";
import { activeNeeds } from "./snooze";

export type HitKind = "need" | "project" | "step" | "decision" | "report" | "delivery";

/** Fixed group order in the results panel. */
export const HIT_KINDS: readonly HitKind[] = ["need", "project", "step", "decision", "report", "delivery"];

/** Rows per group before "N more". */
export const GROUP_CAP = 8;

export type SearchTarget =
  | { kind: "need"; id: string }
  | { kind: "project"; project: string }
  | { kind: "step"; project: string; node: string }
  | { kind: "decision"; project: string; id: string }
  | { kind: "report"; project: string; report: ReportState }
  | { kind: "delivery"; project: string; id: string };

/** A folded string and, per folded UTF-16 unit, the original span it came from. */
export interface Folded {
  text: string;
  starts: number[];
  ends: number[];
}

export interface SearchItem {
  key: string;
  kind: HitKind;
  title: string;
  /** Quiet context parts: project · node or milestone · state. */
  context: string[];
  ts?: string | undefined;
  /** Open or pending work ranks before finished work. */
  open: boolean;
  /** Variant for the row icon (need kind, step progress). */
  variant?: string;
  target: SearchTarget;
  folded: Folded;
  body: string;
}

export interface SearchHit {
  item: SearchItem;
  /** Matched spans of the title, in original string offsets. */
  ranges: [number, number][];
  score: number;
}

export interface SearchGroup {
  kind: HitKind;
  hits: SearchHit[];
}

/** Fold one string for matching, keeping a map back to the original. */
export function fold(value: string): Folded {
  let text = "";
  const starts: number[] = [];
  const ends: number[] = [];
  let index = 0;
  for (const char of value) {
    const folded = char.normalize("NFKD").replace(/\p{M}/gu, "").toLowerCase();
    for (let unit = 0; unit < folded.length; unit += 1) {
      starts.push(index);
      ends.push(index + char.length);
    }
    text += folded;
    index += char.length;
  }
  return { text, starts, ends };
}

/** The query as folded words; empty when there is nothing to search. */
export function queryTokens(query: string): string[] {
  return fold(query)
    .text.split(/\s+/)
    .filter((token) => token.length > 0);
}

const NEED_STATE: Record<NeedKind, MessageKey> = {
  decide: "search.need.decide",
  review: "search.need.review",
  confirm: "search.need.confirm",
  rescue: "search.need.rescue",
};

const DECISION_STATE: Record<DecisionLifecycle, MessageKey> = {
  open: "search.dec.open",
  defaulted: "search.dec.open",
  answered: "search.dec.answered",
  acked: "search.dec.answered",
  withdrawn: "search.dec.withdrawn",
  amended: "search.dec.amended",
};

function item(
  partial: Omit<SearchItem, "folded" | "body">,
  body: (string | undefined | null)[],
): SearchItem {
  return {
    ...partial,
    folded: fold(partial.title),
    body: fold(body.filter(Boolean).join("\n")).text,
  };
}

/** Everything searchable in one snapshot, built once per snapshot and language. */
export function buildSearchIndex(snapshot: Snapshot): SearchItem[] {
  const items: SearchItem[] = [];
  const projects = snapshot.state.projects;
  const shownNeeds = activeNeeds(snapshot);
  /** Decisions and deliveries already listed under Needs you. */
  const covered = new Set<string>();
  for (const need of shownNeeds) {
    covered.add(need.source);
    const receipt = projects[need.project]?.receipts[need.source];
    if (receipt) covered.add(receipt.ask);
  }

  for (const need of shownNeeds) {
    const project = projects[need.project];
    items.push(
      item(
        {
          key: `need:${need.id}`,
          kind: "need",
          title: needQuestion(need),
          context: [project?.summary.title ?? need.project, need.node ?? "", tx(NEED_STATE[need.kind])],
          ts: need.since,
          open: true,
          variant: need.kind,
          target: { kind: "need", id: need.id },
        },
        [need.project, project?.summary.title, need.node, needReason(need)],
      ),
    );
  }

  for (const [slug, project] of Object.entries(projects)) {
    const summary = project.summary;
    const projectTitle = summary.title || slug;
    items.push(
      item(
        {
          key: `project:${slug}`,
          kind: "project",
          title: projectTitle,
          context: [
            slug,
            summary.archived
              ? tx("search.archived")
              : summary.variance === "active"
                ? ""
                : tx(`variance.${summary.variance}` as MessageKey),
          ],
          ts: summary.last_event,
          open: !summary.archived,
          target: { kind: "project", project: slug },
        },
        [slug, summary.brief, summary.goal],
      ),
    );

    for (const feature of Object.values(project.features)) {
      items.push(
        item(
          {
            key: `step:${slug}/${feature.node}`,
            kind: "step",
            title: feature.title,
            context: [projectTitle, feature.node, PROGRESS_LABEL[feature.progress]],
            open: feature.progress === "planned" || feature.progress === "doing",
            variant: feature.progress,
            target: { kind: "step", project: slug, node: feature.node },
          },
          [feature.node, feature.milestone, feature.slice],
        ),
      );
    }

    for (const decision of Object.values(project.decisions)) {
      if (covered.has(decision.id)) continue;
      const receipt = decision.effective_receipt ? project.receipts[decision.effective_receipt] : undefined;
      const chose = receipt?.chose;
      // An answer in the human's own words keeps its text in `consequence`.
      const own = chose?.id === "own" ? chose.consequence : undefined;
      const state = chose
        ? tx("search.dec.chose", { x: own ?? chose.label })
        : tx(DECISION_STATE[decision.lifecycle]);
      items.push(
        item(
          {
            key: `decision:${slug}/${decision.id}`,
            kind: "decision",
            title: decision.card.question,
            context: [projectTitle, decision.node, state],
            ts: receipt?.created_at ?? decision.default_at,
            open: decision.lifecycle === "open" || decision.lifecycle === "defaulted",
            target: { kind: "decision", project: slug, id: decision.id },
          },
          [decision.node, chose?.label, own],
        ),
      );
    }

    for (const report of project.reports ?? []) {
      const kind = tx(report.kind ? (`rep.kind.${report.kind}` as MessageKey) : "rep.kicker");
      const domain = report.domain ? tx(`rep.domain.${report.domain}` as MessageKey) : "";
      items.push(
        item(
          {
            key: `report:${slug}/${report.id}`,
            kind: "report",
            title: report.title,
            context: [projectTitle, report.node ?? "", domain ? `${kind} · ${domain}` : kind],
            ts: report.ts,
            open: true,
            target: { kind: "report", project: slug, report },
          },
          [report.summary, kind, domain, report.node],
        ),
      );
    }

    for (const delivery of Object.values(project.deliveries)) {
      if (delivery.superseded_by || covered.has(delivery.id)) continue;
      items.push(
        item(
          {
            key: `delivery:${slug}/${delivery.id}`,
            kind: "delivery",
            title: delivery.name,
            context: [projectTitle, delivery.node, reviewLabel(delivery.review)],
            open: delivery.review === "pending",
            target: { kind: "delivery", project: slug, id: delivery.id },
          },
          [delivery.node],
        ),
      );
    }
  }
  return items;
}

function titleRanges(folded: Folded, tokens: string[]): [number, number][] {
  const spans: [number, number][] = [];
  for (const token of tokens) {
    let from = folded.text.indexOf(token);
    while (from !== -1) {
      spans.push([folded.starts[from]!, folded.ends[from + token.length - 1]!]);
      from = folded.text.indexOf(token, from + token.length);
    }
  }
  spans.sort((a, b) => a[0] - b[0]);
  const merged: [number, number][] = [];
  for (const span of spans) {
    const last = merged[merged.length - 1];
    if (last && span[0] <= last[1]) last[1] = Math.max(last[1], span[1]);
    else merged.push([span[0], span[1]]);
  }
  return merged;
}

/**
 * Score: a title holding every word 0, some words 1, body only 2; finished
 * work adds 1.5, so an open body match still follows a finished title match
 * and an open partial-title match leads both.
 */
export function matchItem(item: SearchItem, tokens: string[]): SearchHit | null {
  if (!tokens.length) return null;
  let inTitle = 0;
  for (const token of tokens) {
    if (item.folded.text.includes(token)) inTitle += 1;
    else if (!item.body.includes(token)) return null;
  }
  const rank = inTitle === tokens.length ? 0 : inTitle > 0 ? 1 : 2;
  return {
    item,
    ranges: inTitle ? titleRanges(item.folded, tokens) : [],
    score: rank + (item.open ? 0 : 1.5),
  };
}

function compareHits(a: SearchHit, b: SearchHit): number {
  if (a.score !== b.score) return a.score - b.score;
  const at = a.item.ts ? Date.parse(a.item.ts) : Number.NEGATIVE_INFINITY;
  const bt = b.item.ts ? Date.parse(b.item.ts) : Number.NEGATIVE_INFINITY;
  if (at !== bt) return bt > at ? 1 : -1;
  return a.item.title.localeCompare(b.item.title);
}

/** Ranked hits per type, in the fixed group order; empty groups are left out. */
export function runSearch(index: SearchItem[], query: string): SearchGroup[] {
  const tokens = queryTokens(query);
  if (!tokens.length) return [];
  const byKind = new Map<HitKind, SearchHit[]>();
  for (const entry of index) {
    const hit = matchItem(entry, tokens);
    if (!hit) continue;
    const list = byKind.get(entry.kind);
    if (list) list.push(hit);
    else byKind.set(entry.kind, [hit]);
  }
  return HIT_KINDS.flatMap((kind) => {
    const hits = byKind.get(kind);
    return hits ? [{ kind, hits: hits.sort(compareHits) }] : [];
  });
}

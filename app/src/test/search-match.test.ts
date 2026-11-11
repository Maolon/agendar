import { describe, expect, it } from "vitest";
import type { FeatureState, Snapshot } from "../api/types";
import { buildSearchIndex, fold, queryTokens, runSearch } from "../state/search";
import { makeProject, makeSnapshot, textBlob } from "./fixtures";

function step(node: string, title: string, progress: FeatureState["progress"], extra: Partial<FeatureState> = {}): FeatureState {
  return { ...makeProject("x").features["core/one"]!, node, title, progress, gate: "open", ...extra };
}

function snapshotWith(features: FeatureState[]): Snapshot {
  const snapshot = makeSnapshot();
  snapshot.state.projects.delta!.features = Object.fromEntries(features.map((feature) => [feature.node, feature]));
  return snapshot;
}

function titles(snapshot: Snapshot, query: string, kind = "step"): string[] {
  return (
    runSearch(buildSearchIndex(snapshot), query)
      .find((group) => group.kind === kind)
      ?.hits.map((hit) => hit.item.title) ?? []
  );
}

describe("search matching", () => {
  it("folds case, accents and full-width characters, keeping a map to the original", () => {
    const folded = fold("Café ＡＢＣ");
    expect(folded.text).toBe("cafe abc");
    expect(queryTokens("  CAFÉ   abc ")).toEqual(["cafe", "abc"]);
    // "é" folds to "e": the span still covers the original character.
    expect(folded.starts[3]).toBe(3);
    expect(folded.ends[3]).toBe(4);
  });

  it("needs every word, in any field of the item", () => {
    const snapshot = snapshotWith([
      step("docs/review", "Review copy", "doing"),
      step("docs/write", "Write copy", "doing"),
    ]);
    expect(titles(snapshot, "review copy")).toEqual(["Review copy"]);
    // "docs" is only in the node path; "write" is in the title.
    expect(titles(snapshot, "docs write")).toEqual(["Write copy"]);
    expect(titles(snapshot, "REVIEW")).toEqual(["Review copy"]);
    expect(titles(snapshot, "review missing")).toEqual([]);
  });

  it("matches a node path and a milestone", () => {
    const snapshot = snapshotWith([step("ui/search/panel", "Results panel", "planned", { milestone: "Beta" })]);
    expect(titles(snapshot, "search/panel")).toEqual(["Results panel"]);
    expect(titles(snapshot, "beta results")).toEqual(["Results panel"]);
  });

  it("matches Chinese text as a substring and marks the matched part", () => {
    const snapshot = snapshotWith([step("notes/summary", "访谈要点整理", "doing")]);
    const group = runSearch(buildSearchIndex(snapshot), "要点").find((entry) => entry.kind === "step");
    expect(group?.hits[0]?.item.title).toBe("访谈要点整理");
    expect(group?.hits[0]?.ranges).toEqual([[2, 4]]);
  });

  it("ranks open work before finished work, and title matches before body matches", () => {
    const snapshot = snapshotWith([
      step("a/done", "Review done", "verified"),
      step("a/open", "Review open", "doing"),
      step("review/body", "Body only", "doing"),
      step("review/old", "Old body", "dropped"),
    ]);
    expect(titles(snapshot, "review")).toEqual(["Review open", "Review done", "Body only", "Old body"]);
  });

  it("finds answered decisions by their chosen option, reports and deliveries", () => {
    const snapshot = makeSnapshot();
    const alpha = snapshot.state.projects.alpha!;
    alpha.decisions.done = {
      ...alpha.decisions[Object.keys(alpha.decisions)[0]!]!,
      id: "done",
      lifecycle: "answered",
      effective_receipt: "r1",
      card: { ...alpha.decisions[Object.keys(alpha.decisions)[0]!]!.card, question: "Which store?" },
    };
    alpha.receipts.r1 = {
      id: "r1",
      ask: "done",
      node: "core/one",
      source: "human",
      chose: { id: "own", label: "Own answer", consequence: "Use plain files", reversibility: "easy", external: false },
      ruled_out: [],
      done_when: [],
      created_at: "2026-02-10T14:00:00.000Z",
    };
    alpha.reports = [
      {
        id: "rep",
        title: "Storage compared",
        summary: "Plain files win on sync",
        report: textBlob(),
        kind: "finding",
        domain: "arch",
        ts: "2026-02-10T14:10:00.000Z",
        session: "s",
        harness: "claude_code",
      },
    ];
    expect(titles(snapshot, "plain files", "decision")).toEqual(["Which store?"]);
    expect(titles(snapshot, "plain files", "report")).toEqual(["Storage compared"]);
    expect(titles(snapshot, "architecture", "report")).toEqual(["Storage compared"]);
  });

  it("lists a question under Needs you only, not again under Decisions", () => {
    const snapshot = makeSnapshot();
    const groups = runSearch(buildSearchIndex(snapshot), "idle agents");
    expect(groups.map((group) => group.kind)).toEqual(["need"]);
  });

  it("stays fast on a few hundred steps", () => {
    const many = Array.from({ length: 300 }, (_, i) => step(`m/${i}`, `Step number ${i} review`, i % 3 ? "doing" : "verified"));
    const snapshot = snapshotWith(many);
    const started = performance.now();
    const index = buildSearchIndex(snapshot);
    for (const query of ["r", "re", "rev", "revi", "review", "review 12"]) runSearch(index, query);
    expect(performance.now() - started).toBeLessThan(200);
  });
});

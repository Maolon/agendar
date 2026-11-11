import { useEffect, useRef, useState } from "react";
import { newRequestId } from "../api/format";
import { useClient } from "../api/store";
import type { FeatureState, ProjectState } from "../api/types";
import { laneLabel, UNSCHEDULED } from "../graph/lanes";
import { tx } from "../i18n";
import { expectedFields, featureFields, featurePatch, keepEdit } from "../state/agenda";
import { DROP_NOTE_MAX, dropRequests, refusedIndex } from "../state/drop";
import { Icon } from "./Icon";

interface UntouchedProps {
  project: ProjectState;
  steps: FeatureState[];
  selected: string | null;
  onSelect: (node: string | null) => void;
  canMutate: boolean;
  onRefresh?: (() => void) | undefined;
}

type Choice = "keep" | "drop";

function without<T>(map: Record<string, T>, nodes: string[]): Record<string, T> {
  const next = { ...map };
  for (const node of nodes) delete next[node];
  return next;
}

function applyLabel(drops: number, keeps: number): string {
  if (drops && keeps) return tx("plan.tidy_both", { d: drops, k: keeps });
  return keeps ? tx("plan.tidy_keep", { n: keeps }) : tx("plan.tidy_drop", { n: drops });
}

/**
 * The last lane of the plan: planned steps only agents wrote that nobody
 * touched for 14 days. Folded; still work left. Keep puts a step back in its
 * milestone for good. Tidy goes down the list with Keep / Drop on each row,
 * then one confirm sends every Drop in a single `drop_steps` call and each
 * Keep as the usual patch; a row without a choice stays as it is.
 */
export function Untouched({ project, steps, selected, onSelect, canMutate, onRefresh }: UntouchedProps) {
  const client = useClient();
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState<string | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [tidy, setTidy] = useState(false);
  const [choices, setChoices] = useState<Record<string, Choice>>({});
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  /** Rows the last confirm could not apply, with the reason. */
  const [refused, setRefused] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<string | null>(null);
  const request = useRef(dropRequests());
  const holdsSelection = steps.some((step) => `feature:${step.node}` === selected);
  useEffect(() => {
    if (holdsSelection) setOpen(true);
  }, [holdsSelection]);
  if (!steps.length) return null;

  const slug = project.summary.slug;
  const drops = steps.filter((step) => choices[step.node] === "drop");
  const keeps = steps.filter((step) => choices[step.node] === "keep");

  function keepRequest(step: FeatureState) {
    const edit = keepEdit(step);
    return {
      request_id: newRequestId(),
      project: slug,
      node: step.node,
      expected_fields: expectedFields(step.field_revisions, featureFields(edit)),
      patch: featurePatch(edit),
    };
  }

  async function keep(step: FeatureState): Promise<void> {
    setPending(step.node);
    setFailed(null);
    try {
      await client.updateFeature(keepRequest(step));
      onRefresh?.();
    } catch {
      setFailed(step.node);
    } finally {
      setPending(null);
    }
  }

  function choose(node: string, choice: Choice): void {
    setChoices((current) => (current[node] === choice ? without(current, [node]) : { ...current, [node]: choice }));
    setRefused((current) => without(current, [node]));
    setFailure(null);
  }

  function leave(): void {
    setTidy(false);
    setChoices({});
    setRefused({});
    setFailure(null);
    setNote("");
  }

  async function apply(): Promise<void> {
    if (busy || !(drops.length || keeps.length)) return;
    setBusy(true);
    setRefused({});
    setFailure(null);
    const reason = (cause: unknown): string => (cause instanceof Error ? cause.message : tx("plan.drop_failed"));
    try {
      const nodes = drops.map((step) => step.node);
      if (nodes.length) {
        try {
          await client.dropSteps(request.current(slug, nodes, note));
        } catch (cause) {
          // All or none: nothing was dropped. The named row is marked; every choice stays.
          const node = nodes[refusedIndex(cause) ?? -1];
          if (node) setRefused({ [node]: reason(cause) });
          else setFailure(reason(cause));
          return;
        }
        setChoices((current) => without(current, nodes));
        setNote("");
      }
      const kept: string[] = [];
      const unkept: Record<string, string> = {};
      for (const step of keeps) {
        try {
          await client.updateFeature(keepRequest(step));
          kept.push(step.node);
        } catch (cause) {
          unkept[step.node] = reason(cause);
        }
      }
      setChoices((current) => without(current, kept));
      setRefused(unkept);
      if (!Object.keys(unkept).length) setTidy(false);
      onRefresh?.();
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="plane plane-untouched" data-testid="untouched" aria-label={tx("plan.untouched")}>
      <span className="plane-dot gd todo" aria-hidden="true" />
      <div className="plane-top">
        <button type="button" className="plane-h" aria-expanded={open} onClick={() => setOpen(!open)}>
          <Icon name="chevron-right" className="plane-chev" />
          <span className="plane-name">{tx("plan.untouched")}</span>
          <span className="plane-count">{steps.length}</span>
        </button>
        {open && !tidy ? (
          <button type="button" className="link tidy-start" disabled={!canMutate} onClick={() => setTidy(true)}>
            {tx("plan.tidy")}
          </button>
        ) : null}
      </div>
      {open ? (
        <ul className="untouched-list">
          {steps.map((step) => {
            const id = `feature:${step.node}`;
            const choice = choices[step.node];
            const why = refused[step.node];
            return (
              <li
                key={step.node}
                data-untouched={step.node}
                data-choice={tidy ? choice : undefined}
                data-refused={why ? "" : undefined}
                aria-current={selected === id ? "true" : undefined}
              >
                <button type="button" className="untouched-title" onClick={() => onSelect(selected === id ? null : id)}>
                  {step.title}
                </button>
                {step.milestone ? (
                  <span className="untouched-lane">{laneLabel(step.milestone ?? UNSCHEDULED)}</span>
                ) : null}
                {tidy ? (
                  <span className="tidy-choice" role="group" aria-label={step.title}>
                    {(["keep", "drop"] as const).map((option) => (
                      <button
                        key={option}
                        type="button"
                        className={option}
                        aria-pressed={choice === option}
                        disabled={busy || !canMutate}
                        onClick={() => choose(step.node, option)}
                      >
                        {tx(option === "keep" ? "plan.keep" : "plan.drop")}
                      </button>
                    ))}
                  </span>
                ) : (
                  <button
                    type="button"
                    className="link"
                    disabled={!canMutate || pending === step.node}
                    onClick={() => void keep(step)}
                  >
                    {tx(failed === step.node ? "plan.keep_retry" : "plan.keep")}
                  </button>
                )}
                {tidy && why ? (
                  <span className="tidy-refused" role="alert">
                    {why}
                  </span>
                ) : null}
              </li>
            );
          })}
        </ul>
      ) : null}
      {open && tidy ? (
        <form
          className="tidy-foot"
          onSubmit={(event) => {
            event.preventDefault();
            void apply();
          }}
        >
          {drops.length ? (
            <input
              type="text"
              aria-label={tx("plan.drop_note")}
              placeholder={tx("plan.drop_note")}
              value={note}
              maxLength={DROP_NOTE_MAX}
              disabled={busy}
              onChange={(event) => setNote(event.target.value)}
            />
          ) : null}
          <button type="button" className="link" disabled={busy} onClick={leave}>
            {tx("action.cancel")}
          </button>
          <button
            type="submit"
            className={drops.length ? "btn sm danger" : "btn sm"}
            disabled={busy || !canMutate || !(drops.length || keeps.length)}
          >
            {applyLabel(drops.length, keeps.length)}
          </button>
          {failure ? (
            <span className="error" role="alert">
              {failure}
            </span>
          ) : null}
        </form>
      ) : null}
    </section>
  );
}

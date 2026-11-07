/**
 * Assumption strip: what the agent assumed, how long ago, what is
 * built on it, and the two human moves — confirm the assumed option, or
 * override it after seeing which features will be marked for rework.
 *
 * Both moves are the existing answer path (`prepare_answer` → `answer_decision`)
 * on the same decision; nothing here decides anything locally.
 */

import { tx } from "../i18n";
import { useState } from "react";
import { cardRisk, guardFor } from "../api/format";
import type { DecisionState, ProjectState } from "../api/types";
import { useAnswerState, type AnswerAction } from "../state/answer";
import {
  ageLabel,
  featureTitles,
  liveAssumptions,
  reworkTargets,
  type LiveAssumption,
} from "../state/assumptions";
import { Icon } from "./Icon";

interface AssumptionsProps {
  project: ProjectState;
  anchor: string;
  canMutate: boolean;
  action: AnswerAction;
  /** Restrict the strip to one decision (the decision view). */
  only?: string;
}

/**
 * Every unresolved assumption in the project, or only the one behind `only`.
 * The component renders nothing when there is nothing to confirm, so a card
 * without an assumption is unchanged.
 */
export function Assumptions({ project, anchor, canMutate, action, only }: AssumptionsProps) {
  const state = useAnswerState(action);
  const assumptions = liveAssumptions(project, anchor).filter(
    (assumption) => !only || assumption.decision.id === only,
  );
  if (!assumptions.length) return null;
  return (
    <div className="assumptions" data-assumptions>
      {assumptions.map((assumption) => (
        <AssumptionRow
          key={assumption.receipt.id}
          assumption={assumption}
          project={project}
          canMutate={canMutate}
          action={action}
          pending={state.pending}
          confirmAsk={state.ask}
          confirmLabel={state.confirm?.label ?? null}
        />
      ))}
    </div>
  );
}

export function assumptionSummary(assumption: LiveAssumption): string {
  const builtOn = assumption.builtOn.length || assumption.consumers.length;
  const age = ageLabel(assumption.ageSeconds);
  return builtOn === 1 ? tx("asm.summary.one", { age }) : tx("asm.summary.other", { age, n: builtOn });
}

interface RowProps {
  assumption: LiveAssumption;
  project: ProjectState;
  canMutate: boolean;
  action: AnswerAction;
  pending: boolean;
  /** Ask whose confirmation is currently armed, if any. */
  confirmAsk: string | null;
  confirmLabel: string | null;
}

function AssumptionRow({
  assumption,
  project,
  canMutate,
  action,
  pending,
  confirmAsk,
  confirmLabel,
}: RowProps) {
  const [overrideId, setOverrideId] = useState<string | null>(null);
  /** Option this row submitted last, so the echoed confirm re-sends the same one. */
  const [chosenId, setChosenId] = useState<string | null>(null);
  const decision: DecisionState = assumption.decision;
  const targets = reworkTargets(assumption);
  const titles = featureTitles(project, targets);
  const override = assumption.alternatives.find((option) => option.id === overrideId);
  const guard = guardFor(cardRisk(decision.card));
  const disabled = !canMutate || pending;
  const armed = confirmAsk === decision.id && confirmLabel !== null;
  const armedOption =
    decision.card.options.find((option) => option.id === chosenId) ??
    decision.card.options.find((option) => option.label === confirmLabel);

  function submit(optionId: string | undefined): void {
    if (!optionId) return;
    const option = decision.card.options.find((entry) => entry.id === optionId);
    if (!option) return;
    setChosenId(option.id);
    void action.submit({ decision, option, explicit: true, source: "pointer", enabled: !disabled });
  }

  return (
    <div className="assumption" data-assumption={assumption.receipt.id}>
      <div className="ahead">
        <Icon name="circle-dashed" className="c2" />
        <span className="grow">
          <b>{decision.card.question}</b>
          <small>{assumptionSummary(assumption)}</small>
        </span>
        <span className="ae">{assumption.assumed.label}</span>
      </div>
      <p className="abuilt">
        Built on by{" "}
        {titles.length ? <b>{titles.join(", ")}</b> : <span className="muted">no feature yet</span>}
        {assumption.builtOn.length ? null : (
          <span className="muted"> · not declared by the projection</span>
        )}
      </p>
      {armed ? (
        <div className="acts">
          <span className="hint">{tx("asm.no_undo")}</span>
          <button
            type="button"
            className="btn"
            onClick={() => action.cancelConfirm()}
            disabled={pending}
          >
            Cancel
          </button>
          <button
            type="button"
            className="btn primary"
            onClick={() => submit(armedOption?.id)}
            disabled={pending || !canMutate}
            data-assumption-confirm
          >
            {tx("asm.confirm_x", { x: confirmLabel ?? "" })}
          </button>
        </div>
      ) : (
        <div className="acts">
          <button
            type="button"
            className="btn"
            onClick={() => submit(assumption.assumed.id)}
            disabled={disabled}
            data-assumption-keep
          >
            {tx("asm.confirm", { x: assumption.assumed.label })}
          </button>
          {assumption.alternatives.length ? (
            <label className="aoverride">
              <span className="muted">{tx("asm.override_with")}</span>
              <select
                value={overrideId ?? ""}
                disabled={disabled}
                onChange={(event) => setOverrideId(event.target.value || null)}
                aria-label={tx("asm.override_q", { q: decision.card.question })}
              >
                <option value="">{tx("asm.choose")}</option>
                {assumption.alternatives.map((option) => (
                  <option key={option.id} value={option.id}>
                    {option.label}
                  </option>
                ))}
              </select>
            </label>
          ) : null}
        </div>
      )}
      {override && !armed ? (
        <div className="rework" role="status">
          <Icon name="refresh-cw" className="c2" />
          <span>
            {tx("asm.rework.pre")}
            <b>{override.label}</b>
            {tx("asm.rework.mid")}
            <b>{titles.length}</b>
            {titles.length === 1 ? tx("asm.rework.one") : tx("asm.rework.other")}
            {titles.length ? tx("asm.rework.list", { list: titles.join(", ") }) : ""}
            {tx("asm.rework.end")}
          </span>
          <button
            type="button"
            className="btn"
            onClick={() => submit(override.id)}
            disabled={disabled}
            data-assumption-override
          >
            {tx("asm.override_x", { x: override.label })}
          </button>
        </div>
      ) : null}
      {guard === "confirm" && !armed ? (
        <p className="muted-note">This card asks for the echoed confirmation after you choose.</p>
      ) : null}
    </div>
  );
}

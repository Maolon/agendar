import { tx } from "../i18n";
import { useEffect, useState } from "react";
import { isApiError } from "../api/client";
import { newRequestId } from "../api/format";
import { useClient } from "../api/store";
import type { Mode, PolicySet, ProjectState } from "../api/types";
import {
  budgetDraft,
  budgetWith,
  CELL_GROUPS,
  MODE_LABEL,
  normalizePolicy,
  parseBudget,
  PRESET_LABEL,
  PRESET_NAMES,
  setCell,
  applyPreset,
  type BudgetDraft,
  type CellKey,
} from "../state/policy";
import { Icon } from "./Icon";

const MODES: Mode[] = ["auto", "assume", "ask"];

interface PolicyPanelProps {
  project: ProjectState;
  canMutate: boolean;
  onRefresh?: () => void;
}

interface Failure {
  message: string;
  hint?: string;
}

/**
 * Per-project policy editor: preset buttons, the reversibility ×
 * reach matrix with irreversible locked to `ask`, and the speculation budget
 * numbers. Saving replaces the whole `PolicySet` with the revision that was
 * rendered.
 */
export function PolicyPanel({ project, canMutate, onRefresh }: PolicyPanelProps) {
  const client = useClient();
  const [draft, setDraft] = useState<PolicySet>(project.policy.value);
  const [budget, setBudget] = useState<BudgetDraft>(budgetDraft(project.policy.value));
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<Failure | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    setDraft(project.policy.value);
    setBudget(budgetDraft(project.policy.value));
    setFailure(null);
    setSaved(false);
    // A new revision means a saved policy replaced the draft; unrelated
    // snapshot refreshes keep whatever the human is editing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project.policy.revision]);

  function chooseCell(key: CellKey, mode: Mode): void {
    setSaved(false);
    setDraft((current) => setCell(current, key, mode));
  }

  function choosePreset(preset: (typeof PRESET_NAMES)[number]): void {
    setSaved(false);
    setDraft((current) => applyPreset(current, preset));
  }

  async function save(): Promise<void> {
    if (!canMutate || pending) return;
    const parsed = parseBudget(budget);
    if (!parsed.ok) {
      setSaved(false);
      setFailure({ message: parsed.message });
      return;
    }
    const policy = normalizePolicy(budgetWith(draft, parsed.budget));
    setPending(true);
    setFailure(null);
    try {
      await client.setPolicy({
        request_id: newRequestId(),
        project: project.summary.slug,
        expected_revision: project.policy.revision,
        policy,
      });
      setDraft(policy);
      setSaved(true);
      onRefresh?.();
    } catch (cause) {
      setSaved(false);
      setFailure({
        message: cause instanceof Error ? cause.message : tx("policy.failed"),
        ...(isApiError(cause) ? { hint: cause.hint } : {}),
      });
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="policy">
      <div className="presets" role="group" aria-label={tx("policy.preset")}>
        {PRESET_NAMES.map((preset) => (
          <button
            key={preset}
            type="button"
            className="btn"
            aria-pressed={draft.preset === preset}
            disabled={!canMutate || pending}
            onClick={() => choosePreset(preset)}
          >
            {PRESET_LABEL[preset]}
          </button>
        ))}
        <span className="hint" data-testid="policy-preset">
          {draft.preset === "custom" ? tx("policy.custom") : PRESET_LABEL[draft.preset]}
        </span>
      </div>

      <table className="policy-matrix">
        <caption>{tx("policy.caption")}</caption>
        <thead>
          <tr>
            <th scope="col">{tx("policy.col.rev")}</th>
            <th scope="col">{tx("policy.col.local")}</th>
            <th scope="col">{tx("policy.col.wide")}</th>
          </tr>
        </thead>
        <tbody>
          {CELL_GROUPS.map((group) => (
            <tr key={group.label}>
              <th scope="row">{group.label}</th>
              {([group.local, group.wide] as CellKey[]).map((key, index) => (
                <td key={key}>
                  {group.locked ? (
                    <span className="locked" title={tx("policy.always_ask")}>
                      <Icon name="lock" className="c2" />
                      {MODE_LABEL.ask}
                    </span>
                  ) : (
                    <select
                      aria-label={`${group.label}, ${index === 0 ? tx("policy.local_l") : tx("policy.wide_l")}`}
                      value={draft.cells[key]}
                      disabled={!canMutate || pending}
                      onChange={(event) => chooseCell(key, event.target.value as Mode)}
                    >
                      {MODES.map((mode) => (
                        <option key={mode} value={mode}>
                          {MODE_LABEL[mode]}
                        </option>
                      ))}
                    </select>
                  )}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      <p className="hint">
        {tx("policy.timeouts", { t: draft.allow_timeouts ? tx("policy.timeouts.allowed") : tx("policy.timeouts.hold") })}
      </p>

      <div className="budget-grid">
        <label>
          <span>{tx("policy.b.features")}</span>
          <input
            type="number"
            min={0}
            step={1}
            value={budget.features}
            disabled={!canMutate || pending}
            onChange={(event) => {
              setSaved(false);
              setBudget({ ...budget, features: event.target.value });
            }}
          />
        </label>
        <label>
          <span>{tx("policy.b.slices")}</span>
          <input
            type="number"
            min={0}
            step={1}
            value={budget.slices}
            disabled={!canMutate || pending}
            onChange={(event) => {
              setSaved(false);
              setBudget({ ...budget, slices: event.target.value });
            }}
          />
        </label>
        <label>
          <span>{tx("policy.b.hours")}</span>
          <input
            type="number"
            min={0}
            step={0.25}
            value={budget.hours}
            disabled={!canMutate || pending}
            onChange={(event) => {
              setSaved(false);
              setBudget({ ...budget, hours: event.target.value });
            }}
          />
        </label>
      </div>

      {failure ? (
        <div className="alert" role="alert">
          <Icon name="alert-triangle" />
          <span className="body">
            <span>{failure.message}</span>
            {failure.hint ? <span className="hint">{failure.hint}</span> : null}
          </span>
        </div>
      ) : null}
      {saved ? (
        <p className="setup-note" role="status">
          {tx("policy.saved", { x: project.summary.slug })}
        </p>
      ) : null}

      <div className="acts" style={{ justifyContent: "flex-end" }}>
        <button
          type="button"
          className="btn primary"
          onClick={() => void save()}
          disabled={!canMutate || pending}
        >
          {pending ? tx("tray.saving") : tx("policy.save")}
        </button>
      </div>
    </div>
  );
}

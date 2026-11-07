/**
 * Policy editing helpers. Presets are exact tables, a
 * preset becomes `custom` as soon as an editable cell or timeout differs, and
 * the irreversible cells are a hard floor that the UI cannot weaken.
 */

import { tx, type MessageKey } from "../i18n";
import type { Mode, PolicyCells, PolicySet, Preset } from "../api/types";

export type EditablePreset = Exclude<Preset, "custom">;

export const PRESET_NAMES: EditablePreset[] = ["strict", "balanced", "autopilot"];

/** Labels in the active language, read at render time. */
export const PRESET_LABEL: Record<EditablePreset, string> = new Proxy(
  {} as Record<EditablePreset, string>,
  { get: (_, preset: string) => tx(`policy.preset.${preset}` as MessageKey) },
);

interface PresetTable {
  preset: EditablePreset;
  cells: PolicyCells;
  allow_timeouts: boolean;
}

/** Exact preset table; irreversible is always ask. */
export const PRESETS: Record<EditablePreset, PresetTable> = {
  balanced: {
    preset: "balanced",
    cells: {
      easy_local: "auto",
      easy_wide: "assume",
      costly_local: "assume",
      costly_wide: "ask",
      irreversible_local: "ask",
      irreversible_wide: "ask",
    },
    allow_timeouts: true,
  },
  strict: {
    preset: "strict",
    cells: {
      easy_local: "ask",
      easy_wide: "ask",
      costly_local: "ask",
      costly_wide: "ask",
      irreversible_local: "ask",
      irreversible_wide: "ask",
    },
    allow_timeouts: false,
  },
  autopilot: {
    preset: "autopilot",
    cells: {
      easy_local: "auto",
      easy_wide: "auto",
      costly_local: "assume",
      costly_wide: "assume",
      irreversible_local: "ask",
      irreversible_wide: "ask",
    },
    allow_timeouts: true,
  },
};

export type CellKey = keyof PolicyCells;

export interface CellGroup {
  readonly label: string;
  local: CellKey;
  wide: CellKey;
  locked: boolean;
}

/** Reversibility × reach matrix rows. */
export const CELL_GROUPS: CellGroup[] = [
  { get label() { return tx("policy.row.easy"); }, local: "easy_local", wide: "easy_wide", locked: false },
  { get label() { return tx("policy.row.costly"); }, local: "costly_local", wide: "costly_wide", locked: false },
  { get label() { return tx("policy.row.irreversible"); }, local: "irreversible_local", wide: "irreversible_wide", locked: true },
];

export const CELL_ROWS: { key: CellKey; readonly label: string; locked: boolean }[] = (
  [
    ["easy_local", false],
    ["easy_wide", false],
    ["costly_local", false],
    ["costly_wide", false],
    ["irreversible_local", true],
    ["irreversible_wide", true],
  ] as const
).map(([key, locked]) => ({
  key,
  get label() {
    return tx(`policy.cell.${key}`);
  },
  locked,
}));

export const MODE_LABEL: Record<Mode, string> = new Proxy({} as Record<Mode, string>, {
  get: (_, mode: string) => tx(`policy.mode.${mode}` as MessageKey),
});

/** The irreversible cells are locked to `ask`; UI and payload both enforce it. */
export function lockIrreversible(cells: PolicyCells): PolicyCells {
  return { ...cells, irreversible_local: "ask", irreversible_wide: "ask" };
}

export function presetMatches(policy: PolicySet, preset: EditablePreset): boolean {
  const table = PRESETS[preset];
  return (
    policy.allow_timeouts === table.allow_timeouts &&
    CELL_ROWS.every(({ key }) => policy.cells[key] === table.cells[key])
  );
}

/** A preset name only when every cell and the timeout flag match its table. */
export function presetOf(policy: PolicySet): Preset {
  const found = PRESET_NAMES.find((preset) => presetMatches(policy, preset));
  return found ?? "custom";
}

export function applyPreset(policy: PolicySet, preset: EditablePreset): PolicySet {
  const table = PRESETS[preset];
  return {
    ...policy,
    preset,
    cells: { ...table.cells },
    allow_timeouts: table.allow_timeouts,
  };
}

export function setCell(policy: PolicySet, key: CellKey, mode: Mode): PolicySet {
  const locked = key === "irreversible_local" || key === "irreversible_wide";
  if (locked) return policy;
  const next: PolicySet = { ...policy, cells: lockIrreversible({ ...policy.cells, [key]: mode }) };
  return { ...next, preset: presetOf(next) };
}

/** Trimmed policy for a save: hard floor applied, preset recomputed. */
export function normalizePolicy(policy: PolicySet): PolicySet {
  const cells = lockIrreversible(policy.cells);
  const normalized: PolicySet = { ...policy, cells };
  return { ...normalized, preset: presetOf(normalized) };
}

export interface BudgetDraft {
  features: string;
  slices: string;
  hours: string;
}

export function budgetDraft(policy: PolicySet): BudgetDraft {
  return {
    features: String(policy.budget.max_speculative_features),
    slices: String(policy.budget.max_pending_slices),
    hours: trimNumber(policy.budget.max_demo_gap_s / 3600),
  };
}

function trimNumber(value: number): string {
  return Number.isInteger(value) ? String(value) : String(Number(value.toFixed(2)));
}

function parseCount(value: string): number | null {
  if (!/^\d+$/.test(value.trim())) return null;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : null;
}

function parseHours(value: string): number | null {
  if (!/^\d+(\.\d{1,2})?$/.test(value.trim())) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

export type BudgetResult =
  | { ok: true; budget: PolicySet["budget"] }
  | { ok: false; field: "features" | "slices" | "hours"; message: string };

/** Budget numbers are non-negative integers; the demo gap is edited in hours. */
export function parseBudget(draft: BudgetDraft): BudgetResult {
  const features = parseCount(draft.features);
  if (features === null) {
    return { ok: false, field: "features", message: tx("policy.bad.features") };
  }
  const slices = parseCount(draft.slices);
  if (slices === null) {
    return { ok: false, field: "slices", message: tx("policy.bad.slices") };
  }
  const hours = parseHours(draft.hours);
  if (hours === null) {
    return { ok: false, field: "hours", message: tx("policy.bad.hours") };
  }
  return {
    ok: true,
    budget: {
      max_speculative_features: features,
      max_pending_slices: slices,
      max_demo_gap_s: Math.round(hours * 3600),
    },
  };
}

export function budgetWith(policy: PolicySet, budget: PolicySet["budget"]): PolicySet {
  return { ...policy, budget, preset: policy.preset };
}

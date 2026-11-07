/**
 * Local UI reducer. Domain data never lives here: this state
 * only records what the human is looking at, filters, transient forms and
 * per-ask option choices. A choice is not an answer.
 */

export type SelectionKind = "need" | "project" | "settings";

export interface Selection {
  kind: SelectionKind;
  id: string;
}

/** The one orchestrated moment: “Unblock”. */
export interface UnblockPlay {
  project: string;
  from: string;
  released: string[];
  answer_id: string;
  phase: "answered" | "acked";
  /** Arrival time of the answered transition, in epoch milliseconds. */
  at: number;
  /** The animation ran (or was suppressed by motion settings); never replay. */
  played: boolean;
}

export interface UiState {
  selection: Selection | null;
  filter: string;
  quiet_open: boolean;
  option_by_ask: Record<string, string | undefined>;
  explicit_selection_by_ask: Record<string, boolean | undefined>;
  send_back: boolean;
  agents_open: ReadonlySet<string>;
  selected_graph_node_by_project: Record<string, string | undefined>;
  report_open: boolean;
  snooze_dialog: string | null;
  unblock_play: UnblockPlay | null;
}

export const initialUiState: UiState = {
  selection: null,
  filter: "",
  quiet_open: false,
  option_by_ask: {},
  explicit_selection_by_ask: {},
  send_back: false,
  agents_open: new Set<string>(),
  selected_graph_node_by_project: {},
  report_open: false,
  snooze_dialog: null,
  unblock_play: null,
};

export type UiAction =
  | { type: "select"; selection: Selection | null }
  | { type: "filter"; text: string }
  | { type: "toggle_quiet" }
  | { type: "choose_option"; ask: string; option: string; explicit: boolean }
  | { type: "clear_choice"; ask: string }
  | { type: "set_send_back"; open: boolean }
  | { type: "toggle_agents"; project: string }
  | { type: "select_graph_node"; project: string; node: string | null }
  | { type: "set_report_open"; open: boolean }
  | { type: "set_snooze_dialog"; item: string | null }
  | { type: "unblock_answered"; play: Omit<UnblockPlay, "phase" | "played" | "at"> & { at?: number } }
  | { type: "unblock_acked"; answer_id: string }
  | { type: "unblock_played"; answer_id: string };

export function uiReducer(state: UiState, action: UiAction): UiState {
  switch (action.type) {
    case "select":
      return { ...state, selection: action.selection, send_back: false };
    case "filter":
      return { ...state, filter: action.text };
    case "toggle_quiet":
      return { ...state, quiet_open: !state.quiet_open };
    case "choose_option":
      return {
        ...state,
        option_by_ask: { ...state.option_by_ask, [action.ask]: action.option },
        explicit_selection_by_ask: {
          ...state.explicit_selection_by_ask,
          [action.ask]: action.explicit,
        },
      };
    case "clear_choice": {
      const option_by_ask = { ...state.option_by_ask };
      const explicit_selection_by_ask = { ...state.explicit_selection_by_ask };
      delete option_by_ask[action.ask];
      delete explicit_selection_by_ask[action.ask];
      return { ...state, option_by_ask, explicit_selection_by_ask };
    }
    case "set_send_back":
      return { ...state, send_back: action.open };
    case "toggle_agents": {
      const agents_open = new Set(state.agents_open);
      if (agents_open.has(action.project)) agents_open.delete(action.project);
      else agents_open.add(action.project);
      return { ...state, agents_open };
    }
    case "select_graph_node":
      return {
        ...state,
        selected_graph_node_by_project: {
          ...state.selected_graph_node_by_project,
          [action.project]: action.node ?? undefined,
        },
      };
    case "set_report_open":
      return { ...state, report_open: action.open };
    case "set_snooze_dialog":
      return { ...state, snooze_dialog: action.item };
    case "unblock_answered":
      return {
        ...state,
        unblock_play: {
          project: action.play.project,
          from: action.play.from,
          released: action.play.released,
          answer_id: action.play.answer_id,
          phase: "answered",
          at: action.play.at ?? Date.now(),
          played: false,
        },
      };
    case "unblock_acked":
      if (!state.unblock_play || state.unblock_play.answer_id !== action.answer_id) return state;
      return { ...state, unblock_play: { ...state.unblock_play, phase: "acked" } };
    case "unblock_played":
      if (!state.unblock_play || state.unblock_play.answer_id !== action.answer_id) return state;
      return { ...state, unblock_play: { ...state.unblock_play, played: true } };
    default:
      return state;
  }
}

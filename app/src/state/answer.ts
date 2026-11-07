/**
 * Shared answer-action state for one surface.
 *
 * The keyboard handler, the decision view and the receipt editor all read the
 * same authoritative guard: the stricter of the client-derived card risk and
 * the guard the backend returned from `prepare_answer`. A token, the request
 * payload and the edited criteria live here so a retry can re-send an immutable
 * request under the same request ID and so drafts cannot
 * leak between asks.
 */

import { tx } from "../i18n";
import { useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import { errorCode, isApiError, type AgendarClient } from "../api/client";
import {
  cardRisk,
  guardFor as clientGuardFor,
  isRecommendation,
  newRequestId,
  OWN_OPTION,
} from "../api/format";
import type {
  AnswerDecisionData,
  AnswerDecisionRequest,
  Criterion,
  DecisionState,
  DecisionOption,
  Guard,
} from "../api/types";

const GUARD_ORDER: Record<Guard, number> = { easy: 0, choose: 1, confirm: 2 };

/** The stricter of the client-derived and the backend-prepared guard. */
export function stricterGuard(client: Guard, prepared: Guard | null | undefined): Guard {
  if (!prepared || GUARD_ORDER[prepared] <= GUARD_ORDER[client]) return client;
  return prepared;
}

export type SubmitSource = "enter" | "mod-enter" | "pointer";

export interface AnswerError {
  message: string;
  hint?: string;
}

export interface AnswerConfirm {
  token: string;
  label: string;
  requestId: string;
  expiresAt: string;
}

export interface AnswerState {
  /** Ask whose backend guard and edited criteria this state belongs to. */
  ask: string | null;
  /** Backend-prepared guard retained for `ask`; never weaker than the card. */
  preparedGuard: Guard | null;
  pending: boolean;
  error: AnswerError | null;
  confirm: AnswerConfirm | null;
  /** Edited done-when drafts, isolated per ask id. */
  criteria: Readonly<Record<string, Criterion[] | undefined>>;
}

export interface AnswerSubmitInput {
  decision: DecisionState;
  option: DecisionOption;
  /** The human's own words when `option` is the reserved `own`. */
  ownAnswer?: string;
  explicit: boolean;
  source: SubmitSource;
  enabled?: boolean;
}

export interface AnswerHandlers {
  onAnswered: (data: AnswerDecisionData) => void;
  onRefresh: () => void;
}

export interface AnswerActionApi {
  action: AnswerAction;
  state: AnswerState;
}

interface Reservation {
  ask: string;
  revision: string;
  option: string;
  /** The own-words text baked into the token, for option `own`. */
  own?: string;
  /** Signature of the edited criteria that were baked into `request`. */
  criteria: string;
  label: string;
  expiresAt: string;
  request: AnswerDecisionRequest;
}

const INITIAL: AnswerState = {
  ask: null,
  preparedGuard: null,
  pending: false,
  error: null,
  confirm: null,
  criteria: {},
};

function criteriaSignature(criteria: Criterion[] | undefined): string {
  return JSON.stringify(criteria ?? null);
}

export function staleHint(code: string): string | undefined {
  switch (code) {
    case "E_STALE_ANSWER":
      return tx("ans.stale");
    case "E_WITHDRAWN":
      return tx("ans.withdrawn");
    case "E_REQUEST_CONFLICT":
      return tx("ans.conflict");
    case "E_NOT_IMPLEMENTED":
      return tx("ans.not_impl");
    case "E_POLICY":
      return tx("ans.policy");
    default:
      return undefined;
  }
}

/**
 * One answer action per surface. It never commits optimistically: a receipt
 * exists only after `answer_decision` resolves, and the row disappears only
 * when the refreshed snapshot says so.
 */
export class AnswerAction {
  private state: AnswerState = INITIAL;
  private readonly listeners = new Set<() => void>();
  private reservation: Reservation | null = null;
  private inflight = false;
  private expiry: number | null = null;

  constructor(
    private readonly client: AgendarClient,
    private readonly handlers: { current: AnswerHandlers | null },
  ) {}

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  getState = (): AnswerState => this.state;

  dispose = (): void => {
    this.clearExpiry();
  };

  private set(patch: Partial<AnswerState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }

  /** Effective guard: the backend may only ever make a card stricter. */
  guard = (decision: DecisionState): Guard => {
    const prepared = this.state.ask === decision.id ? this.state.preparedGuard : null;
    return stricterGuard(clientGuardFor(cardRisk(decision.card)), prepared);
  };

  criteria = (decision: DecisionState): Criterion[] =>
    this.state.criteria[decision.id] ?? decision.card.done_when;

  /** A deliberate edit of the criteria is a new action: disarm the token. */
  setCriteria = (ask: string, next: Criterion[] | null): void => {
    this.disarm(ask);
    const criteria = { ...this.state.criteria };
    if (next) criteria[ask] = next;
    else delete criteria[ask];
    this.set({ criteria });
  };

  /** Changing the option or the revision invalidates the prepared token. */
  invalidate = (ask: string): void => {
    this.disarm(ask);
  };

  cancelConfirm = (): void => {
    if (this.state.ask) this.disarm(this.state.ask);
    this.set({ error: null });
  };

  /** Selection changed to another ask: drafts stay, tokens and guards do not. */
  askChanged = (ask: string | null): void => {
    if (this.state.ask === ask) return;
    this.reservation = null;
    this.clearExpiry();
    this.set({ ask, preparedGuard: null, confirm: null, error: null });
  };

  /** A decision revision change disarms the prepared token, never the guard. */
  revisionChanged = (ask: string, revision: string): void => {
    if (!this.reservation || this.reservation.ask !== ask) return;
    if (this.reservation.revision === revision) return;
    this.disarm(ask);
  };

  /**
   * Plain-Enter eligibility for one resolved guard. Deliberately ignores the
   * `pending`/`confirm` state so the post-preparation re-check can reuse the
   * same rule without disabling every submission while this action is running.
   */
  private plainEnterAllowed(
    decision: DecisionState,
    option: DecisionOption,
    guard: Guard,
  ): boolean {
    if (guard !== "easy") return false;
    const confidence = decision.card.recommended.confidence;
    return (
      isRecommendation(decision.card, option.id) &&
      (confidence === "med" || confidence === "high")
    );
  }

  /** Plain Enter is only ever the guarded shortcut for the current easy recommendation. */
  canPlainEnter = (
    decision: DecisionState | undefined,
    option: DecisionOption | undefined,
  ): boolean => {
    if (!decision || !option) return false;
    if (this.state.pending || this.state.confirm) return false;
    return this.plainEnterAllowed(decision, option, this.guard(decision));
  };

  async submit(input: AnswerSubmitInput): Promise<void> {
    const { decision, option, explicit, source, enabled = true } = input;
    const own = option.id === OWN_OPTION ? (input.ownAnswer ?? "").trim() : undefined;
    if (this.inflight) return;
    if (!enabled) {
      this.set({
        error: {
          message: tx("tray.stale"),
          hint: tx("ans.refresh_first"),
        },
      });
      return;
    }
    if (this.state.ask !== decision.id) this.askChanged(decision.id);

    const guard = this.guard(decision);
    if (guard !== "easy" && !explicit) {
      this.set({
        error: {
          message: tx("ans.explicit"),
          hint: tx("ans.explicit_hint"),
        },
      });
      return;
    }
    if (source === "enter" && !this.canPlainEnter(decision, option)) {
      // Plain Enter never submits a costed, escalated or non-recommended card.
      return;
    }

    this.inflight = true;
    this.set({ pending: true, error: null });
    try {
      const edited = this.state.criteria[decision.id];
      const signature = criteriaSignature(edited);
      let reservation = this.reservation;
      if (
        reservation &&
        (reservation.ask !== decision.id ||
          reservation.revision !== decision.revision ||
          reservation.option !== option.id ||
          reservation.own !== own ||
          reservation.criteria !== signature)
      ) {
        this.reservation = null;
        reservation = null;
      }
      if (!reservation) {
        const prepared = await this.client.prepareAnswer({
          ask_id: decision.id,
          option_id: option.id,
          ...(own ? { own_answer: own } : {}),
          expected_decision_revision: decision.revision,
          ...(decision.effective_receipt ? { expected_receipt: decision.effective_receipt } : {}),
        });
        this.set({ preparedGuard: stricterGuard(this.guard(decision), prepared.guard) });
        reservation = {
          ask: decision.id,
          revision: decision.revision,
          option: option.id,
          ...(own !== undefined ? { own } : {}),
          criteria: signature,
          label: prepared.label,
          expiresAt: prepared.expires_at,
          request: {
            request_id: newRequestId(),
            token: prepared.token,
            echo_label: prepared.label,
            ...(edited ? { done_when: edited } : {}),
            ...(option.eta ? { expected_eta: option.eta } : {}),
          },
        };
        // Retain the complete immutable request before any commit attempt.
        this.reservation = reservation;
      }

      const effective = this.guard(decision);
      if (effective !== "easy" && !explicit) {
        // The backend escalated above the card; an implicit default is not a choice.
        this.armExpiry(reservation);
        this.set({
          pending: false,
          error: {
            message: tx("ans.explicit_now"),
            hint: tx("ans.explicit_retry"),
          },
        });
        return;
      }
      if (source === "enter" && !this.plainEnterAllowed(decision, option, effective)) {
        // The backend tightened the guard after the source was checked: plain
        // Enter stays inert even when the recommendation was already selected.
        // The prepared transaction is retained for a deliberate submit.
        this.armExpiry(reservation);
        this.set({
          pending: false,
          error: {
            message: tx("ans.deliberate"),
            hint: tx("ans.deliberate_hint"),
          },
        });
        return;
      }
      if (effective === "confirm" && this.state.confirm?.token !== reservation.request.token) {
        // Arm the echoed confirmation; only a second deliberate step commits.
        this.armExpiry(reservation);
        this.set({
          pending: false,
          confirm: {
            token: reservation.request.token,
            label: reservation.label,
            requestId: reservation.request.request_id,
            expiresAt: reservation.expiresAt,
          },
        });
        return;
      }

      const data = await this.client.answerDecision(reservation.request);
      this.reservation = null;
      this.clearExpiry();
      this.set({ pending: false, confirm: null, error: null });
      this.handlers.current?.onAnswered(data);
    } catch (cause) {
      const code = errorCode(cause);
      const retryable = isApiError(cause) ? cause.retryable : true;
      if (!retryable) {
        this.reservation = null;
        this.clearExpiry();
      }
      this.set({
        pending: false,
        error: {
          message: cause instanceof Error ? cause.message : tx("tray.failed"),
          hint: staleHint(code),
        },
        ...(retryable ? {} : { confirm: null }),
      });
      if (code === "E_STALE_ANSWER" || code === "E_WITHDRAWN" || code === "E_REQUEST_CONFLICT") {
        this.handlers.current?.onRefresh();
      }
    } finally {
      this.inflight = false;
    }
  }

  private disarm(ask: string): void {
    if (this.reservation?.ask === ask) {
      this.reservation = null;
      this.clearExpiry();
    }
    if (this.state.confirm) this.set({ confirm: null });
  }

  private armExpiry(reservation: Reservation): void {
    this.clearExpiry();
    const delay = Math.max(0, Date.parse(reservation.expiresAt) - Date.now());
    this.expiry = window.setTimeout(() => {
      this.expiry = null;
      if (this.reservation?.request.token === reservation.request.token) this.reservation = null;
      if (this.state.confirm?.token === reservation.request.token) this.set({ confirm: null });
    }, delay);
  }

  private clearExpiry(): void {
    if (this.expiry !== null) {
      window.clearTimeout(this.expiry);
      this.expiry = null;
    }
  }
}

export function useAnswerState(action: AnswerAction): AnswerState {
  return useSyncExternalStore(action.subscribe, action.getState);
}

export function useAnswerAction(
  client: AgendarClient,
  handlers: AnswerHandlers,
): AnswerActionApi {
  const handlersRef = useRef<AnswerHandlers | null>(handlers);
  handlersRef.current = handlers;
  const action = useMemo(() => new AnswerAction(client, handlersRef), [client]);
  const state = useAnswerState(action);
  useEffect(() => () => action.dispose(), [action]);
  return { action, state };
}

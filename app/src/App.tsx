import { needQuestion } from "./i18n/msg";
import { I18nProvider, tx } from "./i18n";
import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import type { AgendarClient, Surface } from "./api/client";
import { cardRisk, guardFor, newRequestId, optionById } from "./api/format";
import { ClientContext, StoreContext, type SnapshotStore, useStoreState } from "./api/store";
import type {
  AnswerDecisionData,
  DecisionState,
  NeedsYouItem,
  ProjectState,
} from "./api/types";
import { ErrorState } from "./components/ErrorState";
import { Setup } from "./components/Setup";
import { Shell } from "./components/Shell";
import { sidebarRows } from "./components/Sidebar";
import { Toast, type ToastAction, type ToastMessage } from "./components/Toast";
import { TrayPopover } from "./components/TrayPopover";
import { useKeyboard } from "./hooks/useKeyboard";
import { useWindowChrome } from "./hooks/useWindowChrome";
import { useAnswerAction } from "./state/answer";
import { useMotionAllowed, usePulseFeed } from "./state/motion";
import { openedMessage } from "./state/setup";
import {
  SnoozeContext,
  activeNeeds,
  isDismissed,
  snoozeEnd,
  snoozedUntil,
  type SnoozeApi,
  type SnoozeChoice,
} from "./state/snooze";
import { initialUiState, uiReducer, type Selection } from "./state/ui";
import type { MessageKey } from "./i18n/messages";
import { Brief } from "./views/Brief";
import { Decision } from "./views/Decision";
import { Delivery } from "./views/Delivery";
import { Project } from "./views/Project";
import { Settings } from "./views/Settings";
import { Rescue } from "./views/Rescue";

const SNOOZED_TOAST: Record<SnoozeChoice, MessageKey> = {
  hour: "snooze.done.hour",
  tomorrow: "snooze.done.tomorrow",
  week: "snooze.done.week",
};

interface AppProps {
  client: AgendarClient;
  store: SnapshotStore;
  surface: Surface;
}

const SETUP_CODES = new Set(["E_CONTEXT", "E_NOT_CONFIGURED", "E_SETUP_REQUIRED"]);

function matchesFilter(value: string, text: string): boolean {
  return !text || value.toLowerCase().includes(text);
}

/**
 * A `decide` need points at its ask; a `confirm` need may point at the assumed
 * event instead, so resolve through the receipt and then the node before giving
 * up with an honest "not in this snapshot".
 */
export function decisionForNeed(
  project: ProjectState | undefined,
  need: NeedsYouItem,
): DecisionState | undefined {
  if (!project) return undefined;
  const direct = project.decisions[need.source];
  if (direct) return direct;
  const assumed = project.receipts[need.source];
  if (assumed) return project.decisions[assumed.ask];
  if (need.node) {
    const candidates = Object.values(project.decisions).filter(
      (decision) =>
        decision.node === need.node &&
        (decision.lifecycle === "open" || decision.lifecycle === "defaulted"),
    );
    if (candidates.length === 1) return candidates[0];
  }
  return undefined;
}

export function App({ client, store, surface }: AppProps) {
  const state = useStoreState(store);
  const [ui, dispatch] = useReducer(uiReducer, initialUiState);
  const [toast, setToast] = useState<ToastMessage | null>(null);
  /** Vault folder configured in this session; setup stays up for its optional step. */
  const [configuredVault, setConfiguredVault] = useState<string | null>(null);
  const toastTimer = useRef<number | null>(null);
  const toastSeq = useRef(0);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const chrome = useWindowChrome(client, true);
  const markedRef = useRef<Set<string>>(new Set());
  const previousNeeds = useRef<string[]>([]);
  /** Accept action of the delivery view, for Cmd/Ctrl-Enter. */
  const reviewPrimary = useRef<(() => void) | null>(null);

  const snapshot = state.snapshot;
  const canMutate = state.phase === "ready" && snapshot !== null;
  const setupRequired =
    state.phase === "error" && state.error !== null && SETUP_CODES.has(state.error.code);
  /** Main-window setup, including the optional step after the snapshot has loaded. */
  const setupShown =
    surface !== "popover" &&
    (setupRequired || (configuredVault !== null && snapshot !== null));
  /** After Open, move focus to the main window once Setup has unmounted. */
  const restoreMainFocus = useRef(false);
  // The Motion setting lives on the Settings page; the system's reduce-motion
  // setting, window visibility and data freshness still win.
  const motion = useMotionAllowed(
    snapshot?.preferences.motion ?? false,
    state.phase === "ready",
  );
  const pulseFeed = usePulseFeed(state.change, snapshot, motion);

  const answer = useAnswerAction(client, {
    onAnswered: (data) => handleAnswered(data),
    onRefresh: () => void store.refresh(),
  });
  const action = answer.action;

  const showToast = useCallback(
    (
      message: string,
      options?: { tone?: "status" | "alert" | "warn"; action?: ToastAction; duration?: number },
    ) => {
      toastSeq.current += 1;
      setToast({
        id: toastSeq.current,
        message,
        tone: options?.tone ?? "status",
        ...(options?.action ? { action: options.action } : {}),
      });
      if (toastTimer.current) window.clearTimeout(toastTimer.current);
      const duration = options?.duration ?? (options?.action ? 5000 : 2200);
      toastTimer.current = window.setTimeout(() => setToast(null), duration);
    },
    [],
  );

  // A costly or irreversible answer asks twice. The second step says why, out
  // loud, instead of a small hint beside the buttons.
  // The warning stays up for as long as the confirm step is open.
  const confirmLabel = answer.state.confirm?.label;
  useEffect(() => {
    if (confirmLabel) {
      showToast(tx("dc.confirm_toast", { label: confirmLabel }), { tone: "warn", duration: 10 * 60 * 1000 });
      return;
    }
    setToast((current) => (current?.tone === "warn" ? null : current));
  }, [confirmLabel, showToast]);

  /** Write one snooze entry; it needs no whole-preferences revision. */
  const writeSnooze = useCallback(
    async (itemId: string, until: string): Promise<void> => {
      const data = await client.snoozeNeed({ request_id: newRequestId(), item_id: itemId, until });
      store.applyPreferences(data.preferences);
    },
    [client, store],
  );

  const snoozeApi: SnoozeApi | null = snapshot
    ? {
        until: (id) => snoozedUntil(snapshot, id),
        open: ui.snooze_dialog,
        setOpen: (id) => dispatch({ type: "set_snooze_dialog", item: id }),
        snooze: async (id, choice) => {
          try {
            await writeSnooze(id, snoozeEnd(choice));
            dispatch({ type: "set_snooze_dialog", item: null });
            showToast(tx(SNOOZED_TOAST[choice]));
          } catch (cause) {
            showToast(cause instanceof Error ? cause.message : tx("tray.failed"), { tone: "alert" });
          }
        },
        dismissed: (id) => isDismissed(snapshot, id),
        dismiss: async (id) => {
          try {
            const data = await client.dismissNeed({ request_id: newRequestId(), item_id: id });
            store.applyPreferences(data.preferences);
            showToast(tx("dismiss.done"));
          } catch (cause) {
            showToast(cause instanceof Error ? cause.message : tx("tray.failed"), { tone: "alert" });
          }
        },
        restore: async (id) => {
          try {
            // A time already passed ends the snooze; the host drops it.
            await writeSnooze(id, snapshot.state.evaluated_at);
            showToast(tx("snooze.restored"));
          } catch (cause) {
            showToast(cause instanceof Error ? cause.message : tx("tray.failed"), { tone: "alert" });
          }
        },
      }
    : null;

  useEffect(() => {
    return () => {
      if (toastTimer.current) window.clearTimeout(toastTimer.current);
    };
  }, []);

  useEffect(() => {
    store.start();
    return () => store.stop();
  }, [store]);

  // Theme follows the preference; `system` removes the explicit override.
  useEffect(() => {
    const theme = snapshot?.preferences.theme ?? "system";
    if (theme === "system") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = theme;
  }, [snapshot?.preferences.theme]);

  // Motion off (preference, reduced motion, hidden window or stale data) is a
  // document-wide switch, so CSS animations stop even outside this subtree.
  useEffect(() => {
    document.documentElement.classList.toggle("still", !motion);
  }, [motion]);

  // Mark read only after a rendered, focused review — never a background event,
  // and never while Setup is covering the board.
  useEffect(() => {
    if (setupShown) return;
    if (!snapshot || !ui.selection || ui.selection.kind !== "need") return;
    if (markedRef.current.has(ui.selection.id)) return;
    if (typeof document !== "undefined") {
      if (document.visibilityState !== "visible" || !document.hasFocus()) return;
    }
    markedRef.current.add(ui.selection.id);
    void client
      .setPreferences({
        request_id: newRequestId(),
        expected_revision: snapshot.preferences.revision,
        mark_looked: true,
      })
      .then((data) => store.applyPreferences(data.preferences))
      .catch(() => {
        /* A local watermark failure never blocks the view. */
      });
  }, [client, setupShown, store, snapshot, ui.selection]);

  // Process-navigation ordering: apply only a newer (epoch, sequence) target.
  // Setup owns the window until Open, so a notification must not navigate under it.
  useEffect(() => {
    if (setupShown) return;
    const focus = state.focus;
    if (!focus || !snapshot) return;
    if (focus.navigation_epoch !== snapshot.navigation_epoch) {
      store.acknowledgeFocus(focus.sequence);
      return;
    }
    const need = snapshot.needs.find((item) => item.id === focus.item_id);
    if (focus.item_id === `project:${focus.project}` && snapshot.state.projects[focus.project]) {
      // A progress notice opens its project.
      dispatch({ type: "select", selection: { kind: "project", id: focus.project } });
    } else if (need) {
      dispatch({ type: "select", selection: { kind: "need", id: need.id } });
      scrollRowIntoView({ kind: "need", id: need.id });
    } else if (snapshot.state.projects[focus.project]) {
      dispatch({ type: "select", selection: { kind: "project", id: focus.project } });
      showToast(tx("app.stale_item"));
    } else {
      showToast(tx("app.gone_item"), { tone: "alert" });
    }
    store.acknowledgeFocus(focus.sequence);
  }, [setupShown, showToast, snapshot, state.focus, store]);

  useEffect(() => {
    if (!restoreMainFocus.current || setupShown || surface === "popover") return;
    restoreMainFocus.current = false;
    if (searchRef.current) {
      searchRef.current.focus();
      return;
    }
    const frame = document.querySelector<HTMLElement>(".win");
    if (!frame) return;
    if (!frame.hasAttribute("tabindex")) frame.tabIndex = -1;
    frame.focus();
  }, [setupShown, surface]);

  // A real unblock event is the only trigger for the "See it unblock" toast.
  // `answered` opens the one orchestrated moment; a later `acked` updates it and
  // never plays a second pulse.
  useEffect(() => {
    const unblock = state.unblock;
    if (!unblock) return;
    if (unblock.phase === "answered") {
      dispatch({
        type: "unblock_answered",
        play: {
          project: unblock.project,
          from: unblock.from,
          released: unblock.released,
          answer_id: unblock.answer_id,
        },
      });
      showToast(tx("app.receipt_unblock"), {
        action: {
          label: tx("app.see_unblock"),
          onClick: () =>
            dispatch({ type: "select", selection: { kind: "project", id: unblock.project } }),
        },
        duration: 5000,
      });
    } else {
      dispatch({ type: "unblock_acked", answer_id: unblock.answer_id });
      showToast(tx("app.acked"));
    }
    store.clearUnblock();
  }, [showToast, state.unblock, store]);

  // Follow a removed selected row to the next ranked visible need, else Brief.
  // A row just snoozed counts as removed; one opened from "N snoozed" stays.
  useEffect(() => {
    if (!snapshot) return;
    const active = activeNeeds(snapshot);
    const ids = active.map((item) => item.id);
    if (!ui.selection || ui.selection.kind !== "need") {
      previousNeeds.current = ids;
      return;
    }
    const selected = ui.selection.id;
    const openedSnoozed =
      !previousNeeds.current.includes(selected) && snapshot.needs.some((item) => item.id === selected);
    if (ids.includes(selected) || openedSnoozed) {
      previousNeeds.current = ids;
      return;
    }
    const index = previousNeeds.current.indexOf(selected);
    const text = ui.filter.trim().toLowerCase();
    const visible = active.filter(
      (item) => matchesFilter(item.question, text) || matchesFilter(item.project, text),
    );
    const fallback =
      visible[Math.min(Math.max(index, 0), visible.length - 1)] ?? visible[0] ?? null;
    previousNeeds.current = ids;
    dispatch({
      type: "select",
      selection: fallback ? { kind: "need", id: fallback.id } : null,
    });
  }, [snapshot, ui.filter, ui.selection]);

  const need: NeedsYouItem | undefined =
    ui.selection?.kind === "need" && snapshot
      ? snapshot.needs.find((item) => item.id === ui.selection?.id)
      : undefined;
  const projectState =
    ui.selection?.kind === "project" && snapshot
      ? snapshot.state.projects[ui.selection.id]
      : undefined;
  const decision: DecisionState | undefined =
    need && snapshot ? decisionForNeed(snapshot.state.projects[need.project], need) : undefined;
  const chosenOptionId = decision
    ? (ui.option_by_ask[decision.id] ??
      (guardFor(cardRisk(decision.card)) === "confirm"
        ? undefined
        : decision.card.recommended.option))
    : undefined;
  const chosen = decision ? optionById(decision.card, chosenOptionId) : undefined;
  const explicit = decision ? (ui.explicit_selection_by_ask[decision.id] ?? false) : false;
  const plainEnter = canMutate && action.canPlainEnter(decision, chosen);

  const rows = snapshot ? sidebarRows(snapshot, ui.filter, ui.quiet_open) : [];
  const optionCount = decision?.card.options.length ?? 0;

  function scrollRowIntoView(selection: Selection | null): void {
    if (!selection) return;
    const target = `${selection.kind}:${selection.id}`;
    for (const element of document.querySelectorAll<HTMLElement>("[data-row]")) {
      if (element.dataset.row === target) {
        element.scrollIntoView?.({ block: "nearest" });
        return;
      }
    }
  }

  function select(selection: Selection | null): void {
    dispatch({ type: "select", selection });
    scrollRowIntoView(selection);
  }

  function selectOption(optionId: string): void {
    if (!decision) return;
    // A new selection is a new action: it disarms the prepared token.
    if (optionId !== chosenOptionId) action.invalidate(decision.id);
    dispatch({ type: "choose_option", ask: decision.id, option: optionId, explicit: true });
  }

  useKeyboard(surface === "popover" || setupShown ? null : {
    openSettings: () => select({ kind: "settings", id: "local" }),
    rows,
    selection: ui.selection,
    select,
    optionCount,
    selectOption: (index) => {
      const option = decision?.card.options[index];
      if (option) selectOption(option.id);
    },
    plainEnterEnabled: plainEnter,
    submit: (source) => {
      if (source === "mod-enter" && need?.kind === "review") {
        reviewPrimary.current?.();
        return;
      }
      if (decision && chosen) {
        void action.submit({ decision, option: chosen, explicit, source, enabled: canMutate });
        return;
      }
      if (source === "mod-enter" && snapshot?.needs[0]) {
        select({ kind: "need", id: snapshot.needs[0].id });
      }
    },
    escape: () => {
      if (ui.report_open) {
        dispatch({ type: "set_report_open", open: false });
        return;
      }
      if (ui.snooze_dialog) {
        dispatch({ type: "set_snooze_dialog", item: null });
        return;
      }
      if (ui.send_back) {
        dispatch({ type: "set_send_back", open: false });
        return;
      }
      if (ui.selection?.kind === "project") {
        const node = ui.selected_graph_node_by_project[ui.selection.id];
        if (node) {
          dispatch({ type: "select_graph_node", project: ui.selection.id, node: null });
          return;
        }
      }
      dispatch({ type: "select", selection: null });
    },
    focusSearch: () => {
      searchRef.current?.focus();
      searchRef.current?.select();
    },
    canOpenReport: false,
    openReport: () => undefined,
    scrollRowIntoView,
  });

  function handleAnswered(data: AnswerDecisionData): void {
    // The unblock transition already owns the toast when it arrived with the
    // persisted answer; otherwise say honestly that the ack is still missing.
    if (ui.unblock_play?.answer_id !== data.receipt.id) {
      const listening = Object.values(snapshot?.state.projects ?? {})
        .map((project) => project.listening?.[data.receipt.ask])
        .find(Boolean);
      showToast(
        listening === "waiting"
          ? tx("app.r.waiting")
          : listening === "away"
            ? tx("app.r.away")
            : listening === "connected"
              ? tx("app.r.connected")
              : tx("app.r.pending"),
      );
    }
    if (data.released.length) previousNeeds.current = snapshot?.needs.map((item) => item.id) ?? [];
    void store.refresh();
  }

  function handleResolved(message: string): void {
    showToast(message);
    void store.refresh();
  }

  function openFromTray(need?: NeedsYouItem | null): void {
    void client
      .showMain(need ? { project: need.project, item_id: need.id } : undefined)
      .catch(() => undefined)
      .finally(() => {
        void client.hidePopover().catch(() => undefined);
      });
  }

  function renderView() {
    if (!snapshot) {
      return (
        <div className="view">
          <p className="muted-note">{tx("app.loading")}</p>
        </div>
      );
    }
    if (ui.selection?.kind === "settings") {
      return (
        <Settings
          snapshot={snapshot}
          onPreferences={(preferences) => store.applyPreferences(preferences)}
          onRefresh={() => void store.refresh()}
        />
      );
    }
    const project = need ? snapshot.state.projects[need.project] : undefined;
    if (need && need.kind === "review") {
      const delivery = project?.deliveries[need.source];
      if (!project || !delivery) {
        return (
          <div className="view">
            <h1 className="h">{needQuestion(need)}</h1>
            <p className="muted">
              {tx("app.no_delivery")}
            </p>
            <div className="acts" style={{ justifyContent: "flex-start" }}>
              <button
                type="button"
                className="btn"
                onClick={() => dispatch({ type: "select", selection: null })}
              >
                {tx("app.back_brief")}
              </button>
            </div>
          </div>
        );
      }
      return (
        <Delivery
          key={delivery.id}
          project={project}
          delivery={delivery}
          need={need}
          anchor={snapshot.state.evaluated_at}
          canMutate={canMutate}
          sendBackOpen={ui.send_back}
          onSendBackOpen={(open) => dispatch({ type: "set_send_back", open })}
          onResolved={handleResolved}
          onRefresh={() => void store.refresh()}
          primaryRef={reviewPrimary}
        />
      );
    }
    if (need && need.kind === "rescue") {
      if (!project) {
        return (
          <div className="view">
            <p className="muted-note">Project {need.project} is not available.</p>
          </div>
        );
      }
      return (
        <Rescue
          project={project}
          need={need}
          anchor={snapshot.state.evaluated_at}
          canMutate={canMutate}
          onResolved={handleResolved}
          onRefresh={() => void store.refresh()}
        />
      );
    }
    if (need && decision && project) {
      return (
        <Decision
          key={decision.id}
          project={project}
          decision={decision}
          need={need}
          anchor={snapshot.state.evaluated_at}
          canMutate={canMutate}
          ui={ui}
          onSelectOption={selectOption}
          action={action}
          onSeeGraph={() => {
            dispatch({ type: "select", selection: { kind: "project", id: need.project } });
            dispatch({
              type: "select_graph_node",
              project: need.project,
              node: `decision:${decision.id}`,
            });
          }}
        />
      );
    }
    if (need && !decision) {
      return (
        <div className="view">
          <h1 className="h">{needQuestion(need)}</h1>
          <p className="muted">
            {tx("app.no_decision")}
          </p>
          <div className="acts" style={{ justifyContent: "flex-start" }}>
            <button
              type="button"
              className="btn"
              onClick={() => dispatch({ type: "select", selection: null })}
            >
              {tx("app.back_brief")}
            </button>
          </div>
        </div>
      );
    }
    if (ui.selection?.kind === "project") {
      if (!projectState) {
        return (
          <div className="view">
            <p className="muted-note">Project {ui.selection.id} was not found.</p>
          </div>
        );
      }
      return (
        <Project
          key={projectState.summary.slug}
          project={projectState}
          presence={(snapshot.presence ?? []).filter((row) => row.project === projectState.summary.slug)}
          anchor={snapshot.state.evaluated_at}
          canMutate={canMutate}
          agentsOpen={ui.agents_open.has(projectState.summary.slug)}
          onToggleAgents={() =>
            dispatch({ type: "toggle_agents", project: projectState.summary.slug })
          }
          onOpenNeed={(needId) => select({ kind: "need", id: needId })}
          onRefresh={() => void store.refresh()}
          selectedNode={ui.selected_graph_node_by_project[projectState.summary.slug]}
          onSelectNode={(node) =>
            dispatch({
              type: "select_graph_node",
              project: projectState.summary.slug,
              node,
            })
          }
          onOpenCard={(needId) => select({ kind: "need", id: needId })}
          action={action}
          motion={motion}
          pulses={pulseFeed.pulses}
          pulseToken={pulseFeed.token}
          pulseAges={pulseFeed.ages}
          unblock={ui.unblock_play}
          onUnblockPlayed={(answerId) =>
            dispatch({ type: "unblock_played", answer_id: answerId })
          }
        />
      );
    }
    return <Brief snapshot={snapshot} onSelect={select} />;
  }

  const setupDetail = (field: string): string | undefined =>
    state.error?.details?.find((detail) => detail.field === field)?.message;

  if (surface === "popover") {
    return (
      <I18nProvider preference={snapshot?.preferences.language}>
      <ClientContext.Provider value={client}>
        <StoreContext.Provider value={store}>
        <SnoozeContext.Provider value={snoozeApi}>
          <div className="app">
            {snapshot ? (
              <TrayPopover
                snapshot={snapshot}
                chrome={chrome}
                canMutate={canMutate}
                onOpen={openFromTray}
                onAnswered={handleAnswered}
                onNeedsMain={(need, label, guard) =>
                  showToast(
                    guard === "confirm"
                      ? tx("tray.confirm_in_main", { label })
                      : tx("tray.choose_in_main"),
                    {
                      tone: "warn",
                      duration: 10 * 60 * 1000,
                      action: {
                        label: guard === "confirm" ? tx("tray.open_to_confirm") : tx("tray.open_to_choose"),
                        onClick: () => openFromTray(need),
                      },
                    },
                  )
                }
                onRefresh={() => void store.refresh()}
              />
            ) : state.phase === "error" && state.error ? (
              <ErrorState
                title={tx("app.unavailable")}
                message={state.error.message}
                hint={state.error.hint}
                retryable={state.error.retryable}
                onRetry={() => void store.refresh()}
              />
            ) : (
              <p className="muted" style={{ padding: 16 }}>
                Loading…
              </p>
            )}
            <Toast toast={toast} />
          </div>
        </SnoozeContext.Provider>
        </StoreContext.Provider>
      </ClientContext.Provider>
      </I18nProvider>
    );
  }

  // The optional connection step stays on screen after the Vault is configured,
  // even once the snapshot loads, until the user opens Agendar.
  if (setupShown) {
    return (
      <I18nProvider preference={snapshot?.preferences.language}>
      <ClientContext.Provider value={client}>
        <StoreContext.Provider value={store}>
          <div className="app">
            <Setup
              configRevision={setupDetail("expected_config_revision")}
              preferencesRevision={
                snapshot?.preferences.revision ?? setupDetail("expected_preferences_revision")
              }
              notificationsConsent={
                snapshot ? snapshot.preferences.notifications === true : undefined
              }
              notificationPermission={snapshot?.local.notifications}
              ready={snapshot !== null}
              resumeVaultPath={configuredVault ?? undefined}
              onConfigured={(_data, folder) => {
                setConfiguredVault(folder);
                void store.refresh();
              }}
              onPreferences={(preferences) => store.applyPreferences(preferences)}
              onRefresh={() => void store.refresh()}
              onOpen={(summary) => {
                restoreMainFocus.current = true;
                setConfiguredVault(null);
                showToast(openedMessage(summary), { duration: 6000 });
              }}
            />
            <Toast toast={toast} />
          </div>
        </StoreContext.Provider>
      </ClientContext.Provider>
      </I18nProvider>
    );
  }

  if (!snapshot && state.phase === "error" && state.error) {
    return (
      <I18nProvider preference={undefined}>
      <ClientContext.Provider value={client}>
        <StoreContext.Provider value={store}>
          <div className="app">
            <ErrorState
              title={tx("app.unreachable")}
              message={state.error.message}
              hint={state.error.hint}
              retryable={state.error.retryable}
              onRetry={() => void store.refresh()}
            />
            <Toast toast={toast} />
          </div>
        </StoreContext.Provider>
      </ClientContext.Provider>
      </I18nProvider>
    );
  }

  return (
    <I18nProvider preference={snapshot?.preferences.language}>
      <ClientContext.Provider value={client}>
      <StoreContext.Provider value={store}>
      <SnoozeContext.Provider value={snoozeApi}>
        <div className="app">
          <Shell
            snapshot={snapshot}
            phase={state.phase}
            error={state.error}
            filter={ui.filter}
            selection={ui.selection}
            quietOpen={ui.quiet_open}
            devFixture={client.mode === "fixture"}
            motion={motion}
            pulses={pulseFeed.flashes}
            searchRef={searchRef}
            onFilter={(text) => dispatch({ type: "filter", text })}
            onSelect={select}
            onToggleQuiet={() => dispatch({ type: "toggle_quiet" })}
            onOpenSettings={() => select({ kind: "settings", id: "local" })}
            settingsOpen={ui.selection?.kind === "settings"}
            onRefresh={() => void store.refresh()}
            chrome={chrome}
          >
            {renderView()}
          </Shell>
          <Toast toast={toast} />
        </div>
      </SnoozeContext.Provider>
      </StoreContext.Provider>
    </ClientContext.Provider>
      </I18nProvider>
  );
}

import { createContext, useContext, useSyncExternalStore } from "react";
import type { AgendarClient } from "./client";
import { ApiError } from "./client";
import type {
  ChangedEvent,
  HealthEvent,
  NotificationFocus,
  Preferences,
  Snapshot,
  UnblockEvent,
} from "./types";

export type StorePhase = "loading" | "ready" | "stale" | "error";

export interface StoreState {
  phase: StorePhase;
  snapshot: Snapshot | null;
  error: ApiError | null;
  /** Highest generation announced by push events. */
  observedGeneration: number;
  /** Focus target awaiting UI application; ordered by navigation epoch + sequence. */
  focus: NotificationFocus | null;
  /** Latest real unblock transition; the shell turns it into a 5s toast. */
  unblock: UnblockEvent | null;
  /**
   * Latest committed change batch. `seq` increments once per event so motion
   * can animate a batch exactly once instead of on every render.
   */
  change: { event: ChangedEvent; seq: number } | null;
}

const INITIAL: StoreState = {
  phase: "loading",
  snapshot: null,
  error: null,
  observedGeneration: 0,
  focus: null,
  unblock: null,
  change: null,
};

const FOCUS_BUFFER_LIMIT = 32;

/**
 * Authoritative snapshot subscription.
 *
 * - subscribes to push events before the first fetch;
 * - coalesces event bursts into at most one in-flight `get_snapshot`;
 * - discards responses older than an observed epoch/generation and performs
 *   one more fetch when a change arrived while a response was in flight;
 * - keeps the last snapshot with a stale banner when an invoke fails;
 * - applies notification-focus targets only when
 *   (navigation_epoch, sequence) is strictly newer than the last applied one.
 */
export class SnapshotStore {
  private state: StoreState = INITIAL;
  private readonly listeners = new Set<() => void>();
  private readonly client: AgendarClient;
  private cleanups: (() => void)[] = [];
  private started = false;
  private inflight: Promise<void> | null = null;
  private dirty = false;
  private observedEpoch: string | null = null;
  private observedGeneration = 0;
  private navigationEpoch: string | null = null;
  private lastAppliedSequence = 0;
  private pendingFocus: NotificationFocus[] = [];
  private changeSeq = 0;

  constructor(client: AgendarClient) {
    this.client = client;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  getState = (): StoreState => this.state;

  start = (): void => {
    if (this.started) return;
    this.started = true;
    void this.attach();
  };

  stop = (): void => {
    this.started = false;
    for (const cleanup of this.cleanups) cleanup();
    this.cleanups = [];
  };

  refresh = async (): Promise<void> => {
    this.dirty = true;
    await this.pump();
  };

  private async attach(): Promise<void> {
    const subscriptions: Promise<() => void>[] = [
      this.client.onChanged((event) => this.onChanged(event)),
      this.client.onHealth((event) => this.onHealth(event)),
      this.client.onUnblock((event) => this.onUnblock(event)),
      this.client.onNotificationFocus((event) => this.onFocusEvent(event)),
    ];
    const results = await Promise.allSettled(subscriptions);
    const cleanups: (() => void)[] = [];
    for (const result of results) {
      if (result.status === "fulfilled") cleanups.push(result.value);
    }
    // Local state such as snoozes changes without a Vault event, and the main
    // window and the tray each hold their own copy: re-read on return.
    if (typeof window !== "undefined" && typeof document !== "undefined") {
      const onReturn = () => {
        if (document.visibilityState === "visible") void this.refresh();
      };
      window.addEventListener("focus", onReturn);
      document.addEventListener("visibilitychange", onReturn);
      cleanups.push(() => {
        window.removeEventListener("focus", onReturn);
        document.removeEventListener("visibilitychange", onReturn);
      });
    }
    if (this.started) this.cleanups.push(...cleanups);
    else for (const cleanup of cleanups) cleanup();
    if (!this.started) return;
    await this.refresh();
  }

  private set(patch: Partial<StoreState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }

  private onChanged(event: ChangedEvent): void {
    if (this.observedEpoch !== null && event.epoch !== this.observedEpoch) {
      // Index epochs are independent; generation restarts with the new epoch.
      this.observedGeneration = event.generation;
    } else {
      this.observedGeneration = Math.max(this.observedGeneration, event.generation);
    }
    this.observedEpoch = event.epoch;
    this.changeSeq += 1;
    this.set({ change: { event, seq: this.changeSeq } });
    this.dirty = true;
    void this.pump();
  }

  private onHealth(event: HealthEvent): void {
    const snapshot = this.state.snapshot;
    if (!snapshot) return;
    this.set({
      snapshot: {
        ...snapshot,
        local: { ...snapshot.local, health: event.health, diagnostics: event.diagnostics },
      },
    });
  }

  private onUnblock(event: UnblockEvent): void {
    this.set({ unblock: event });
  }

  clearUnblock = (): void => {
    if (this.state.unblock) this.set({ unblock: null });
  };

  private onFocusEvent(event: NotificationFocus): void {
    if (this.navigationEpoch && event.navigation_epoch === this.navigationEpoch) {
      if (event.sequence > this.lastAppliedSequence) {
        this.set({ focus: newestFocus(this.state.focus, event) });
      }
      return;
    }
    if (event.navigation_epoch !== this.navigationEpoch) {
      this.pendingFocus.push(event);
      if (this.pendingFocus.length > FOCUS_BUFFER_LIMIT) this.pendingFocus.shift();
    }
  }

  /** Called by the shell after it has navigated to the focused target. */
  acknowledgeFocus = (sequence: number): void => {
    this.lastAppliedSequence = Math.max(this.lastAppliedSequence, sequence);
    if (this.state.focus && this.state.focus.sequence <= this.lastAppliedSequence) {
      this.set({ focus: null });
    }
  };

  applyPreferences = (preferences: Preferences): void => {
    const snapshot = this.state.snapshot;
    if (!snapshot) return;
    this.set({ snapshot: { ...snapshot, preferences } });
  };

  /** Coalesce: one in-flight fetch; requests during it collapse into one retry. */
  private async pump(): Promise<void> {
    if (this.inflight) return this.inflight;
    this.inflight = (async () => {
      try {
        // One fetch, plus at most one catch-up fetch for a burst observed while
        // the response was in flight. Never a polling loop.
        for (let fetches = 0; fetches < 2; fetches += 1) {
          this.dirty = false;
          const ok = await this.fetchOnce();
          if (!ok) return;
          const applied = this.state.snapshot;
          const behind =
            applied === null ||
            applied.epoch !== this.observedEpoch ||
            this.observedGeneration > applied.generation;
          if (!this.dirty && !behind) return;
        }
      } finally {
        this.inflight = null;
      }
      if (this.dirty) void this.pump();
    })();
    return this.inflight;
  }

  private async fetchOnce(): Promise<boolean> {
    try {
      const response = await this.client.getSnapshot();
      if (this.observedEpoch !== null && response.epoch !== this.observedEpoch) {
        // An event announced a different epoch while this response was in
        // flight; the response is discarded and the catch-up fetch resolves it.
        return true;
      }
      if (response.generation < this.observedGeneration) {
        // Observed a newer generation than this response; discard it.
        return true;
      }
      this.applySnapshot(response);
      return true;
    } catch (error) {
      const apiError =
        error instanceof ApiError
          ? error
          : new ApiError({
              code: "E_IO",
              message: error instanceof Error ? error.message : "Cannot reach the app.",
              hint: "Check that Agendar is running, then refresh.",
              retryable: true,
            });
      if (this.state.snapshot) {
        this.set({ phase: "stale", error: apiError });
      } else {
        this.set({ phase: "error", error: apiError });
      }
      return false;
    }
  }

  private applySnapshot(snapshot: Snapshot): void {
    this.observedEpoch = snapshot.epoch;
    this.observedGeneration = snapshot.generation;
    const epochChanged =
      this.navigationEpoch !== null && this.navigationEpoch !== snapshot.navigation_epoch;
    if (epochChanged) {
      this.pendingFocus = [];
      this.lastAppliedSequence = 0;
    }
    this.navigationEpoch = snapshot.navigation_epoch;

    const candidates = [...this.pendingFocus, ...(snapshot.notification_focus ? [snapshot.notification_focus] : [])];
    this.pendingFocus = [];
    let focus = this.state.focus;
    for (const candidate of candidates) {
      if (candidate.navigation_epoch !== this.navigationEpoch) continue;
      if (candidate.sequence <= this.lastAppliedSequence) continue;
      focus = newestFocus(focus, candidate);
    }
    this.set({
      snapshot,
      phase: "ready",
      focus,
      error: null,
    });
  }
}

function newestFocus(
  current: NotificationFocus | null,
  candidate: NotificationFocus,
): NotificationFocus | null {
  if (!current) return candidate;
  if (current.navigation_epoch !== candidate.navigation_epoch) return candidate;
  return candidate.sequence > current.sequence ? candidate : current;
}

// React plumbing -------------------------------------------------------------

export const ClientContext = createContext<AgendarClient | null>(null);
export const StoreContext = createContext<SnapshotStore | null>(null);

export function useClient(): AgendarClient {
  const client = useContext(ClientContext);
  if (!client) throw new Error("AgendarClient is not available in this tree");
  return client;
}

export function useSnapshotStore(): SnapshotStore {
  const store = useContext(StoreContext);
  if (!store) throw new Error("SnapshotStore is not available in this tree");
  return store;
}

export function useStoreState(store: SnapshotStore): StoreState {
  return useSyncExternalStore(store.subscribe, store.getState, store.getState);
}

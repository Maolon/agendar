import { act, render, renderHook, screen } from "@testing-library/react";
import { ClientContext } from "../api/store";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ChangedEvent, FeatureState, Graph, Snapshot } from "../api/types";
import { Graph as GraphView, type GraphPlay } from "../components/Graph";
import type { MotionPulse } from "../graph/motion";
import { useMotionAllowed, usePulseFeed } from "../state/motion";
import { Project } from "../views/Project";
import { ProjectTrack } from "../components/Sidebar";
import { EVALUATED_AT, FakeClient, makeSnapshot, projectProps, ulid } from "./fixtures";
import { planGraph, planProject } from "./graph-fixtures";

const SESSION = ulid(201);

function eligibleSnapshot(): Snapshot {
  const snapshot = makeSnapshot();
  const project = snapshot.state.projects.alpha!;
  project.features["core/one"] = {
    ...project.features["core/one"]!,
    progress: "doing",
    gate: "open",
    basis: "firm",
    liveness: "live",
    paused: false,
    session: SESSION,
  } satisfies FeatureState;
  project.sessions[SESSION] = {
    id: SESSION,
    host: "h-fixture",
    harness: "claude_code",
    last_event: ulid(302),
    last_event_ts: "2026-02-10T14:59:00.000Z",
    current_node: "core/one",
    liveness: "live",
    stalled_on: [],
  };
  return snapshot;
}

function changed(pulseId: string, generation: number, overrides: Partial<ChangedEvent> = {}): ChangedEvent {
  return {
    v: 1,
    epoch: ulid(900),
    generation,
    event_set_digest: "0".repeat(64),
    as_of: EVALUATED_AT,
    projects: ["alpha"],
    pulses: [
      {
        id: pulseId,
        project: "alpha",
        node: "core/one",
        host: "h-fixture",
        event_ts: EVALUATED_AT,
      },
    ],
    batch: "local",
    ...overrides,
  };
}

const pulse: MotionPulse = {
  id: "pulse-1",
  project: "alpha",
  node: "core/ask",
  host: "h-fixture",
  event_ts: EVALUATED_AT,
};

function graphHarness(options: {
  motion: boolean;
  pulses?: MotionPulse[];
  pulseToken?: number;
  play?: GraphPlay | null;
  onPlayed?: (answerId: string) => void;
  graph?: Graph;
}) {
  const props = {
    graph: options.graph ?? planGraph(),
    project: planProject(),
    selected: null as string | null,
    onSelect: () => undefined,
    onOpenNeed: () => undefined,
    motion: options.motion,
    pulses: options.pulses ?? [],
    pulseToken: options.pulseToken ?? 0,
    play: options.play ?? null,
    onPlayed: options.onPlayed ?? (() => undefined),
  };
  const view = render(<GraphView {...props} />);
  return {
    rerender(next: Partial<typeof props>) {
      view.rerender(<GraphView {...props} {...next} />);
    },
  };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("pulse feed", () => {
  it("animates a pulse once and consumes its id even when motion is off", () => {
    const snapshot = eligibleSnapshot();
    const first = changed("pulse-1", snapshot.generation);
    const { result, rerender } = renderHook(
      (props: { change: { event: ChangedEvent; seq: number }; snapshot: Snapshot; allowed: boolean }) =>
        usePulseFeed(props.change, props.snapshot, props.allowed),
      {
        initialProps: {
          change: { event: first, seq: 1 },
          snapshot,
          allowed: true,
        },
      },
    );
    expect(result.current.pulses.map((entry) => entry.id)).toEqual(["pulse-1"]);
    expect(result.current.flashes.has("alpha/core/one")).toBe(true);
    // The age shown for a sync batch is the event's own timestamp.
    expect(result.current.ages["alpha/core/one"]).toBe(EVALUATED_AT);

    // Same pulse arriving again (a re-delivered batch) never animates twice.
    const again = eligibleSnapshot();
    again.generation = snapshot.generation + 1;
    rerender({
      change: { event: changed("pulse-1", again.generation), seq: 2 },
      snapshot: again,
      allowed: true,
    });
    expect(result.current.pulses).toEqual([]);
    expect(result.current.flashes.size).toBe(0);

    // Motion off: ids are consumed with no animation, so enabling it later
    // cannot replay the batch.
    const third = eligibleSnapshot();
    third.generation = again.generation + 1;
    rerender({
      change: { event: changed("pulse-2", third.generation), seq: 3 },
      snapshot: third,
      allowed: false,
    });
    expect(result.current.pulses).toEqual([]);
    const fourth = eligibleSnapshot();
    fourth.generation = third.generation + 1;
    rerender({
      change: { event: changed("pulse-2", fourth.generation), seq: 4 },
      snapshot: fourth,
      allowed: true,
    });
    expect(result.current.pulses).toEqual([]);
  });

  it("waits for the projection that contains the pulse before judging it", () => {
    const snapshot = eligibleSnapshot();
    const event = changed("pulse-3", snapshot.generation + 1);
    const { result, rerender } = renderHook(
      (props: { change: { event: ChangedEvent; seq: number }; snapshot: Snapshot; allowed: boolean }) =>
        usePulseFeed(props.change, props.snapshot, props.allowed),
      { initialProps: { change: { event, seq: 1 }, snapshot, allowed: true } },
    );
    // The snapshot is one generation behind: nothing animates yet.
    expect(result.current.pulses).toEqual([]);
    const next = eligibleSnapshot();
    next.generation = event.generation;
    rerender({ change: { event, seq: 1 }, snapshot: next, allowed: true });
    expect(result.current.pulses.map((entry) => entry.id)).toEqual(["pulse-3"]);
  });

  it("never animates a pulse for stale or ineligible work", () => {
    const snapshot = eligibleSnapshot();
    snapshot.state.projects.alpha!.features["core/one"] = {
      ...snapshot.state.projects.alpha!.features["core/one"]!,
      liveness: "quiet",
    };
    const { result } = renderHook(
      () => usePulseFeed({ event: changed("pulse-4", snapshot.generation), seq: 1 }, snapshot, true),
    );
    expect(result.current.pulses).toEqual([]);
  });
});

describe("motion preference and reduced motion", () => {
  function stubMatchMedia(reduced: boolean): void {
    window.matchMedia = ((query: string) => ({
      matches: reduced && query.includes("prefers-reduced-motion"),
      media: query,
      onchange: null,
      addListener: () => undefined,
      removeListener: () => undefined,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
      dispatchEvent: () => false,
    })) as unknown as typeof window.matchMedia;
  }

  it("requires the preference, no reduced-motion request, a visible window and fresh data", () => {
    stubMatchMedia(false);
    const allowed = renderHook(() => useMotionAllowed(true, true));
    expect(allowed.result.current).toBe(true);
    allowed.unmount();

    const stillPreference = renderHook(() => useMotionAllowed(false, true));
    expect(stillPreference.result.current).toBe(false);
    stillPreference.unmount();

    const stale = renderHook(() => useMotionAllowed(true, false));
    expect(stale.result.current).toBe(false);
    stale.unmount();

    stubMatchMedia(true);
    const reduced = renderHook(() => useMotionAllowed(true, true));
    expect(reduced.result.current).toBe(false);
    reduced.unmount();
  });

  it("stops for a hidden window", () => {
    stubMatchMedia(false);
    const original = Object.getOwnPropertyDescriptor(Document.prototype, "visibilityState");
    Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
    const hidden = renderHook(() => useMotionAllowed(true, true));
    expect(hidden.result.current).toBe(false);
    hidden.unmount();
    if (original) Object.defineProperty(Document.prototype, "visibilityState", original);
    else Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
  });
});

describe("graph motion", () => {
  /** Record every SMIL element the view starts explicitly. */
  function stubBeginElement(): { started: Element[]; restore: () => void } {
    const proto = window.SVGElement.prototype as SVGElement & {
      beginElement?: () => void;
    };
    const had = Object.getOwnPropertyDescriptor(proto, "beginElement");
    const started: Element[] = [];
    Object.defineProperty(proto, "beginElement", {
      configurable: true,
      value(this: Element) {
        started.push(this);
      },
    });
    return {
      started,
      restore: () => {
        if (had) Object.defineProperty(proto, "beginElement", had);
        else Reflect.deleteProperty(proto, "beginElement");
      },
    };
  }

  function packetAnimation(edge: string): Element | null {
    return document.querySelector(`[data-packet="${edge}"]`)?.firstElementChild ?? null;
  }

  it("starts each packet at its own insertion time, not at SVG document time zero", () => {
    vi.useFakeTimers();
    const begin = stubBeginElement();
    try {
      const harness = graphHarness({ motion: true, pulseToken: 0, pulses: [] });
      harness.rerender({ pulseToken: 1, pulses: [pulse] });
      const first = packetAnimation("feature:core/ask->feature:core/ipc");
      // Document-time `begin="0s"` would freeze this packet at its destination.
      expect(first?.getAttribute("begin")).toBe("indefinite");
      expect(begin.started).toEqual([first]);

      act(() => vi.advanceTimersByTime(90));
      const second = packetAnimation("feature:core/ask->feature:core/menubar");
      expect(begin.started).toEqual([first, second]);
    } finally {
      begin.restore();
    }
  });

  it("starts the unblock packet when the moment inserts it", () => {
    vi.useFakeTimers();
    const begin = stubBeginElement();
    try {
      const play: GraphPlay = {
        answerId: "answer-1",
        from: "feature:core/ipc",
        released: ["core/wake"],
        played: false,
      };
      graphHarness({ motion: true, play });
      act(() => vi.advanceTimersByTime(350));
      const packet = packetAnimation("feature:core/ipc->feature:core/wake");
      expect(packet?.getAttribute("begin")).toBe("indefinite");
      expect(begin.started).toEqual([packet]);
    } finally {
      begin.restore();
    }
  });

  it("draws one blip per pulse and a packet on each outgoing edge, once", () => {
    vi.useFakeTimers();
    const harness = graphHarness({ motion: true, pulseToken: 0, pulses: [] });
    harness.rerender({ pulseToken: 1, pulses: [pulse] });

    expect(document.querySelectorAll("[data-blip]")).toHaveLength(1);
    // core/ask has two outgoing edges: one starts now, the next 90 ms later.
    expect(document.querySelectorAll("[data-packet]")).toHaveLength(1);
    act(() => vi.advanceTimersByTime(90));
    expect(document.querySelectorAll("[data-packet]")).toHaveLength(2);
    expect(
      document.querySelector('[data-packet="feature:core/ask->feature:core/ipc"]'),
    ).not.toBeNull();
    expect(
      document.querySelector('[data-packet="feature:core/ask->feature:core/menubar"]'),
    ).not.toBeNull();

    act(() => vi.advanceTimersByTime(1300));
    expect(document.querySelectorAll("[data-blip]")).toHaveLength(0);
    expect(document.querySelectorAll("[data-packet]")).toHaveLength(0);

    // The same batch token is never animated again on re-render.
    harness.rerender({ pulseToken: 1, pulses: [pulse] });
    expect(document.querySelectorAll("[data-blip]")).toHaveLength(0);
  });

  it("moves nothing when motion is off", () => {
    vi.useFakeTimers();
    const harness = graphHarness({ motion: false, pulseToken: 0, pulses: [] });
    harness.rerender({ pulseToken: 1, pulses: [pulse] });
    act(() => vi.advanceTimersByTime(2000));
    expect(document.querySelectorAll("[data-blip]")).toHaveLength(0);
    expect(document.querySelectorAll("[data-packet]")).toHaveLength(0);
  });

  it("plays the unblock moment from the event: packet at 350 ms, released blips at 1050 ms", () => {
    vi.useFakeTimers();
    const onPlayed = vi.fn();
    const play: GraphPlay = {
      answerId: "answer-1",
      from: "feature:core/ipc",
      released: ["core/wake"],
      played: false,
    };
    graphHarness({ motion: true, play, onPlayed });
    expect(onPlayed).toHaveBeenCalledWith("answer-1");
    expect(document.querySelectorAll("[data-packet]")).toHaveLength(0);

    act(() => vi.advanceTimersByTime(349));
    expect(document.querySelectorAll("[data-packet]")).toHaveLength(0);
    act(() => vi.advanceTimersByTime(1));
    expect(
      document.querySelector('[data-packet="feature:core/ipc->feature:core/wake"]'),
    ).not.toBeNull();
    // Only the released-set node blips, and only at 1050 ms.
    expect(document.querySelectorAll("[data-blip]")).toHaveLength(0);
    act(() => vi.advanceTimersByTime(700));
    const blips = [...document.querySelectorAll<HTMLElement>("[data-blip]")];
    expect(blips).toHaveLength(1);
    expect(blips[0]?.dataset.blip).toBe("feature:core/wake");

    act(() => vi.advanceTimersByTime(1300));
    expect(document.querySelectorAll("[data-blip]")).toHaveLength(0);
    expect(document.querySelectorAll("[data-packet]")).toHaveLength(0);
  });

  it("keeps the unblock information static without motion", () => {
    vi.useFakeTimers();
    const onPlayed = vi.fn();
    graphHarness({
      motion: false,
      play: {
        answerId: "answer-2",
        from: "feature:core/ipc",
        released: ["core/wake"],
        played: false,
      },
      onPlayed,
    });
    expect(onPlayed).toHaveBeenCalledWith("answer-2");
    act(() => vi.advanceTimersByTime(2000));
    expect(document.querySelectorAll("[data-blip]")).toHaveLength(0);
    expect(document.querySelectorAll("[data-packet]")).toHaveLength(0);
  });

  it("runs the radar only on a live, firm, doing feature", async () => {
    const project = planProject();
    const client = new FakeClient();
    client.graph = planGraph();
    const view = render(
      <ClientContext.Provider value={client}>
        <Project {...projectProps(project, client, { motion: true })} />
      </ClientContext.Provider>,
    );
    const dot = () =>
      document
        .querySelector('[data-graph-node="feature:core/menubar"] .gd')
        ?.getAttribute("class") ?? "";
    await screen.findByRole("button", { name: /Menu bar triage/ });
    expect(dot()).toContain("live");
    // A tainted feature never sweeps even while it is doing and live.
    const tainted = document.querySelector('[data-graph-node="feature:core/async"] .gd');
    expect(tainted?.getAttribute("class") ?? "").not.toContain("live");

    view.rerender(
      <ClientContext.Provider value={client}>
        <Project {...projectProps(project, client, { motion: false })} />
      </ClientContext.Provider>,
    );
    expect(dot()).not.toContain("live");
  });

  it("labels a sync pulse with its own event time instead of 'just now'", async () => {
    const client = new FakeClient();
    client.graph = planGraph();
    const project = planProject();
    render(
      <ClientContext.Provider value={client}>
        <Project
          {...projectProps(project, client, {
            pulseAges: { "alpha/core/menubar": "2026-02-10T14:56:00.000Z" },
          })}
        />
      </ClientContext.Provider>,
    );
    expect(await screen.findByText(/pulse 4m ago/)).toBeInTheDocument();
  });

  it("breathes only on live work and flashes the pulsed segment", () => {
    const project = planProject();
    const { container } = render(
      <ProjectTrack
        features={{
          "core/menubar": project.features["core/menubar"]!,
          "core/async": project.features["core/async"]!,
        }}
        slug="alpha"
        motion
        pulses={new Set(["alpha/core/menubar"])}
      />,
    );
    const segments = [...container.querySelectorAll("i")];
    expect(segments[0]?.className).toBe("w live blink");
    // Tainted work never breathes, even while it is doing.
    expect(segments[1]?.className).toBe("w");
  });

  it("shows the awaiting-ack state statically and updates it on the ack", () => {
    const client = new FakeClient();
    client.graph = planGraph();
    const project = planProject();
    const answered = {
      project: "alpha",
      from: `feature:core/ipc`,
      released: ["core/wake"],
      answer_id: "answer-3",
      phase: "answered" as const,
      at: Date.now(),
      played: true,
    };
    const view = render(
      <ClientContext.Provider value={client}>
        <Project {...projectProps(project, client, { unblock: answered })} />
      </ClientContext.Provider>,
    );
    expect(screen.getByText("Awaiting acknowledgement from the agent.")).toBeInTheDocument();
    view.rerender(
      <ClientContext.Provider value={client}>
        <Project
          {...projectProps(project, client, { unblock: { ...answered, phase: "acked" } })}
        />
      </ClientContext.Provider>,
    );
    expect(screen.getByText("The agent acknowledged the receipt.")).toBeInTheDocument();
  });
});

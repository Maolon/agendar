import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ApiError, createClient, type Transport } from "../api/client";
import { ClientContext } from "../api/store";
import type { FeatureState, Graph, ProjectState, SessionState } from "../api/types";
import { PlanMap } from "../components/PlanMap";
import { StepDrop } from "../components/StepDrop";
import { createFixtureTransport } from "../dev/fixtureTransport";
import { nodeDisplay } from "../graph/display";
import { MESSAGES } from "../i18n/messages";
import { dropRequests, droppedLabel, refusedIndex } from "../state/drop";
import { Project } from "../views/Project";
import { FakeClient, makeProject, projectProps, ulid } from "./fixtures";

function feature(node: string, overrides: Partial<FeatureState> = {}): FeatureState {
  const base = makeProject("alpha").features["core/one"]!;
  const { claim: _claim, phase: _phase, session: _session, ...rest } = base;
  return { ...rest, node, title: node, depends_on: [], unblocks: [], progress: "planned", milestone: "Beta", ...overrides };
}

function graphOf(features: Record<string, FeatureState>): Graph {
  return {
    nodes: Object.keys(features).map((node) => ({
      id: `feature:${node}`,
      kind: "feature" as const,
      label: node,
      feature: node,
      state: features[node]!.progress,
      assumed: false,
    })),
    edges: [],
  };
}

function refusal(index: number): ApiError {
  return new ApiError({
    code: "E_VALIDATION",
    message: "b/old is already dropped.",
    field: `/nodes/${index}`,
    hint: "",
    retryable: false,
  });
}

describe("dropSteps on the client", () => {
  it("invokes drop_steps with the request and returns one event per node", async () => {
    const invoke = vi.fn(async () => ({ v: 1, ok: true, data: { event_ids: ["e1", "e2"] } }));
    const client = createClient({ invoke, listen: async () => () => {} } as unknown as Transport);
    const request = { request_id: ulid(1), project: "alpha", nodes: ["a/b", "a/c"], note: "out of scope" };
    await expect(client.dropSteps(request)).resolves.toEqual({ event_ids: ["e1", "e2"] });
    expect(invoke).toHaveBeenCalledWith("drop_steps", request);
  });

  it("repeats the request id for the same steps and note, and takes a new one when they change", () => {
    const build = dropRequests();
    const first = build("alpha", ["a/b"], " late ");
    expect(first).toEqual({ request_id: first.request_id, project: "alpha", nodes: ["a/b"], note: "late" });
    expect(build("alpha", ["a/b"], "late").request_id).toBe(first.request_id);
    expect(build("alpha", ["a/b"], "").request_id).not.toBe(first.request_id);
    expect(build("alpha", ["a/b"], "")).not.toHaveProperty("note");
  });

  it("reads the refused step from the error's field", () => {
    expect(refusedIndex(refusal(2))).toBe(2);
    expect(refusedIndex(new ApiError({ code: "E_VALIDATION", message: "", field: "/note", hint: "", retryable: false }))).toBeNull();
    expect(refusedIndex(new Error("io"))).toBeNull();
  });
});

describe("drop_steps on the dev fixture transport", () => {
  it("applies the drop, replays a retry and refuses the whole list on one closed step", async () => {
    const client = createClient(createFixtureTransport(), "fixture");
    const before = (await client.getSnapshot()).state.projects["ledger"]!;
    const untouched = Object.values(before.features).filter((step) => step.untouched).map((step) => step.node);
    expect(untouched.length).toBeGreaterThan(1);
    const [first, second] = untouched as [string, string];

    const request = { request_id: ulid(9001), project: "ledger", nodes: [first], note: "not this quarter" };
    const data = await client.dropSteps(request);
    expect(data.event_ids).toHaveLength(1);
    await expect(client.dropSteps(request)).resolves.toEqual(data);

    const dropped = (await client.getSnapshot()).state.projects["ledger"]!.features[first]!;
    expect(dropped.progress).toBe("dropped");
    expect(dropped.untouched).toBeUndefined();
    expect(dropped.owner_drop).toMatchObject({ event: data.event_ids[0], note: "not this quarter" });

    // The second node is fine, the first is closed: nothing is written.
    const error = await client
      .dropSteps({ request_id: ulid(9002), project: "ledger", nodes: [second, first] })
      .catch((cause: unknown) => cause);
    expect(refusedIndex(error)).toBe(1);
    expect((await client.getSnapshot()).state.projects["ledger"]!.features[second]!.progress).toBe("planned");
  });
});

describe("Drop on a step's row", () => {
  const session: SessionState = {
    id: "s-1",
    host: "h-fixture",
    harness: "codex",
    last_event: ulid(1),
    last_event_ts: "2026-02-10T14:59:00.000Z",
    liveness: "live",
    stalled_on: [],
  };

  function setup(step: FeatureState, canMutate = true) {
    const project: ProjectState = {
      ...makeProject("alpha"),
      features: { [step.node]: step },
      sessions: { "s-1": session },
    };
    const client = new FakeClient();
    const onRefresh = vi.fn();
    render(
      <ClientContext.Provider value={client}>
        <StepDrop project={project} feature={step} canMutate={canMutate} onRefresh={onRefresh} />
      </ClientContext.Provider>,
    );
    const sent = () => client.calls.filter((call) => call.command === "drop_steps").map((call) => call.request);
    return { client, onRefresh, sent };
  }

  it("asks once, then drops the step with the note", async () => {
    const { onRefresh, sent } = setup(feature("a/b", { title: "Export" }));
    fireEvent.click(screen.getByRole("button", { name: "Drop Export" }));
    expect(sent()).toHaveLength(0);
    // A planned step has no holder to name.
    expect(screen.queryByText(/is working on this/)).toBeNull();
    fireEvent.change(screen.getByLabelText("Note (optional)"), { target: { value: " replaced by import " } });
    fireEvent.click(screen.getByRole("button", { name: "Drop step" }));
    await waitFor(() => expect(onRefresh).toHaveBeenCalled());
    expect(sent()).toEqual([
      expect.objectContaining({ project: "alpha", nodes: ["a/b"], note: "replaced by import" }),
    ]);
    expect(screen.queryByRole("button", { name: "Drop step" })).toBeNull();
  });

  it("names who holds a step in progress, and Cancel or Escape sends nothing", () => {
    const { sent } = setup(feature("a/b", { title: "Export", progress: "doing", liveness: "live", session: "s-1" }));
    fireEvent.click(screen.getByRole("button", { name: "Drop Export" }));
    expect(screen.getByText("Codex is working on this")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("button", { name: "Drop step" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Drop Export" }));
    fireEvent.keyDown(screen.getByLabelText("Note (optional)"), { key: "Escape" });
    expect(screen.queryByRole("button", { name: "Drop step" })).toBeNull();
    expect(sent()).toHaveLength(0);
  });

  it("keeps the confirm open on a refusal and retries with the same request id", async () => {
    const { client, onRefresh, sent } = setup(feature("a/b", { title: "Export" }));
    client.dropError = new Error("The Vault is busy.");
    fireEvent.click(screen.getByRole("button", { name: "Drop Export" }));
    fireEvent.click(screen.getByRole("button", { name: "Drop step" }));
    expect((await screen.findByRole("alert")).textContent).toBe("The Vault is busy.");
    expect(onRefresh).not.toHaveBeenCalled();
    client.dropError = null;
    fireEvent.click(screen.getByRole("button", { name: "Drop step" }));
    await waitFor(() => expect(onRefresh).toHaveBeenCalled());
    const [first, second] = sent() as { request_id: string }[];
    expect(second!.request_id).toBe(first!.request_id);
  });

  it("is not offered on a finished or dropped step, and is disabled when the app cannot write", () => {
    for (const progress of ["done", "verified", "dropped"] as const) {
      const view = render(
        <ClientContext.Provider value={new FakeClient()}>
          <StepDrop project={makeProject("alpha")} feature={feature("a/b", { progress })} canMutate />
        </ClientContext.Provider>,
      );
      expect(view.container.querySelector("button")).toBeNull();
      view.unmount();
    }
    setup(feature("a/b", { title: "Export" }), false);
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Drop Export" }).disabled).toBe(true);
  });

  it("sits beside Edit on the selected step of the plan", async () => {
    const features = {
      "a/live": feature("a/live", { title: "Live step" }),
      "a/gone": feature("a/gone", {
        title: "Gone step",
        progress: "dropped",
        owner_drop: { event: ulid(7), at: "2026-02-10T12:00:00.000Z", note: "merged into Live step" },
      }),
    };
    const project: ProjectState = { ...makeProject("alpha"), features };
    const client = new FakeClient();
    client.graph = graphOf(features);
    client.snapshot.state.projects["alpha"] = project;
    const view = render(
      <ClientContext.Provider value={client}>
        <Project {...projectProps(project, client, { selectedNode: "feature:a/live" })} />
      </ClientContext.Provider>,
    );
    const detail = await waitFor(() => {
      const found = view.container.querySelector('[data-graph-detail="a/live"]');
      expect(found).not.toBeNull();
      return found!;
    });
    expect(detail.querySelector("button.step-drop")?.textContent).toBe("Drop");

    view.rerender(
      <ClientContext.Provider value={client}>
        <Project {...projectProps(project, client, { selectedNode: "feature:a/gone" })} />
      </ClientContext.Provider>,
    );
    const gone = view.container.querySelector('[data-graph-detail="a/gone"]')!;
    expect(gone.querySelector(".gd-dropped")?.textContent).toBe("Dropped by you · merged into Live step");
    expect(gone.querySelector("button.step-drop")).toBeNull();
  });
});

describe("Tidy on the Untouched group", () => {
  function setup() {
    const features = {
      "a/live": feature("a/live"),
      "a/old": feature("a/old", { untouched: true, priority: 2, field_revisions: { priority: "p-rev" } }),
      "b/old": feature("b/old", { untouched: true, milestone: "Later" }),
      "c/old": feature("c/old", { untouched: true }),
      "d/old": feature("d/old", { untouched: true }),
    };
    const project: ProjectState = { ...makeProject("alpha"), features };
    const client = new FakeClient();
    const onRefresh = vi.fn();
    const view = render(
      <ClientContext.Provider value={client}>
        <PlanMap
          graph={graphOf(features)}
          project={project}
          selected={null}
          onSelect={() => {}}
          onOpenNeed={() => {}}
          motion={false}
          pulses={[]}
          pulseToken={0}
          play={null}
          onPlayed={() => {}}
          canMutate
          onRefresh={onRefresh}
        />
      </ClientContext.Provider>,
    );
    fireEvent.click(screen.getByTestId("untouched").querySelector(".plane-h")!);
    fireEvent.click(screen.getByRole("button", { name: "Tidy" }));
    const pick = (node: string, choice: "Keep" | "Drop") =>
      fireEvent.click(
        [...view.container.querySelectorAll(`[data-untouched="${node}"] .tidy-choice button`)].find(
          (button) => button.textContent === choice,
        )!,
      );
    const row = (node: string) => view.container.querySelector<HTMLElement>(`[data-untouched="${node}"]`)!;
    const sent = (command: string) =>
      client.calls.filter((call) => call.command === command).map((call) => call.request);
    return { client, onRefresh, pick, row, sent };
  }

  it("sends every Drop in one call and each Keep as the keep patch; an unmarked row is left alone", async () => {
    const { onRefresh, pick, row, sent } = setup();
    const confirm = () => screen.getByTestId("untouched").querySelector<HTMLButtonElement>(".tidy-foot [type=submit]")!;
    expect(confirm().disabled).toBe(true);
    pick("a/old", "Keep");
    pick("b/old", "Drop");
    pick("c/old", "Drop");
    expect(row("b/old").getAttribute("data-choice")).toBe("drop");
    expect(row("b/old").querySelector('[aria-pressed="true"]')?.textContent).toBe("Drop");
    expect(confirm().textContent).toBe("Drop 2, keep 1");
    expect(sent("drop_steps")).toHaveLength(0);
    fireEvent.change(screen.getByLabelText("Note (optional)"), { target: { value: "stale" } });
    fireEvent.click(confirm());
    await waitFor(() => expect(onRefresh).toHaveBeenCalled());
    expect(sent("drop_steps")).toEqual([
      expect.objectContaining({ project: "alpha", nodes: ["c/old", "b/old"], note: "stale" }),
    ]);
    expect(sent("update_feature")).toEqual([
      expect.objectContaining({ node: "a/old", patch: { priority: 2 }, expected_fields: { priority: "p-rev" } }),
    ]);
    // Tidy is over: the rows are back to the plain Keep link.
    await waitFor(() => expect(screen.getByRole("button", { name: "Tidy" })).toBeInTheDocument());
  });

  it("a second click on a choice takes it back", () => {
    const { pick, row } = setup();
    pick("a/old", "Drop");
    pick("a/old", "Drop");
    expect(row("a/old").hasAttribute("data-choice")).toBe(false);
    expect(row("a/old").querySelector('[aria-pressed="true"]')).toBeNull();
  });

  it("marks the row the error names, keeps the other choices and keeps nothing", async () => {
    const { client, onRefresh, pick, row, sent } = setup();
    pick("a/old", "Keep");
    pick("b/old", "Drop");
    pick("c/old", "Drop");
    // Rows follow the lanes, so the Drops go out as c/old, b/old: /nodes/1 is b/old.
    client.dropError = refusal(1);
    fireEvent.click(screen.getByRole("button", { name: "Drop 2, keep 1" }));
    const alert = await screen.findByRole("alert");
    expect(row("b/old").contains(alert)).toBe(true);
    expect(alert.textContent).toBe("b/old is already dropped.");
    expect(row("b/old").hasAttribute("data-refused")).toBe(true);
    expect(row("c/old").hasAttribute("data-refused")).toBe(false);
    expect(row("a/old").getAttribute("data-choice")).toBe("keep");
    expect(row("b/old").getAttribute("data-choice")).toBe("drop");
    expect(row("c/old").getAttribute("data-choice")).toBe("drop");
    expect(sent("update_feature")).toHaveLength(0);
    expect(onRefresh).not.toHaveBeenCalled();

    // Taking the refused row out clears its mark; the rest goes through.
    pick("b/old", "Drop");
    expect(screen.queryByRole("alert")).toBeNull();
    client.dropError = null;
    fireEvent.click(screen.getByRole("button", { name: "Drop 1, keep 1" }));
    await waitFor(() => expect(onRefresh).toHaveBeenCalled());
    expect(sent("drop_steps").at(-1)).toEqual(expect.objectContaining({ nodes: ["c/old"] }));
    expect(sent("update_feature")).toHaveLength(1);
  });

  it("shows an error that names no row under the list, and Cancel sends nothing", async () => {
    const { client, pick, row, sent } = setup();
    pick("b/old", "Drop");
    client.dropError = new Error("The Vault is busy.");
    fireEvent.click(screen.getByRole("button", { name: "Drop 1" }));
    const alert = await screen.findByRole("alert");
    expect(alert.closest(".tidy-foot")).not.toBeNull();
    expect(row("b/old").getAttribute("data-choice")).toBe("drop");
    const calls = sent("drop_steps").length;
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(row("b/old").querySelector(".tidy-choice")).toBeNull();
    expect(sent("drop_steps")).toHaveLength(calls);
  });
});

describe("a step the owner dropped", () => {
  const dropped = feature("a/gone", {
    progress: "dropped",
    owner_drop: { event: ulid(7), at: "2026-02-10T12:00:00.000Z" },
  });
  const node = { id: "feature:a/gone", kind: "feature" as const, label: "a/gone", feature: "a/gone", state: "dropped", assumed: false };

  it("reads Dropped by you on the plan, with the note where there is room", () => {
    expect(nodeDisplay(node, dropped).sub).toBe("Dropped by you");
    expect(droppedLabel(dropped)).toBe("Dropped by you");
    expect(droppedLabel({ ...dropped, owner_drop: { ...dropped.owner_drop!, note: "out of scope" } })).toBe(
      "Dropped by you · out of scope",
    );
  });

  it("stays Dropped when an agent dropped it", () => {
    const { owner_drop: _owner, ...byAgent } = dropped;
    expect(nodeDisplay(node, byAgent).sub).toBe("Dropped");
    expect(droppedLabel(byAgent)).toBe("Dropped");
  });

  it("has the label in the three locales", () => {
    expect(MESSAGES.en["plan.dropped_by_you"]).toBe("Dropped by you");
    expect(MESSAGES["zh-Hans"]["plan.dropped_by_you"]).toBe("由你放弃");
    expect(MESSAGES["zh-Hant"]["plan.dropped_by_you"]).toBe("由你放棄");
  });
});

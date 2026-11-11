import { useState } from "react";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { ApiError } from "../api/client";
import type {
  ApproveCheckRequest,
  DeliveryState,
  NeedsYouItem,
  ProjectState,
  ReviewDeliveryRequest,
  SetRuleRequest,
} from "../api/types";
import { ClientContext, SnapshotStore } from "../api/store";
import { argvLine } from "../state/delivery";
import { canonicalJson, sha256Hex } from "../state/hash";
import { createClient } from "../api/client";
import { createFixtureTransport } from "../dev/fixtureTransport";
import { Delivery } from "../views/Delivery";
import { Project } from "../views/Project";
import {
  FakeClient,
  makeCheckDelivery,
  makeDelivery,
  makeDeliveryProject,
  makeDeliverySnapshot,
  makeGraph,
  projectProps,
  ulid,
} from "./fixtures";

interface HarnessProps {
  client: FakeClient;
  project?: ProjectState;
  delivery?: DeliveryState;
  need?: NeedsYouItem;
  canMutate?: boolean;
  onResolved?: (message: string) => void;
  onRefresh?: () => void;
}

/** Stateful wrapper: the send-back form is owned by the shell in the app. */
function Harness({
  client,
  project: givenProject,
  delivery: givenDelivery,
  need: givenNeed,
  canMutate = true,
  onResolved = vi.fn(),
  onRefresh = vi.fn(),
}: HarnessProps) {
  const fixture = makeDeliveryProject();
  const project = givenProject ?? fixture.project;
  const delivery = givenDelivery ?? fixture.delivery;
  const need = givenNeed ?? fixture.need;
  const [sendBackOpen, setSendBackOpen] = useState(false);
  return (
    <ClientContext.Provider value={client}>
      <Delivery
        project={project}
        delivery={delivery}
        need={need}
        anchor="2026-02-10T15:00:00.000Z"
        canMutate={canMutate}
        sendBackOpen={sendBackOpen}
        onSendBackOpen={setSendBackOpen}
        onResolved={onResolved}
        onRefresh={onRefresh}
      />
    </ClientContext.Provider>
  );
}

/**
 * Commands that write; `read_report` and `get_project` are reads and do not
 * count as an action.
 */
function writes(client: FakeClient): string[] {
  return client.order.filter(
    (command) => command !== "read_report" && command !== "get_project",
  );
}

describe("delivery view", () => {
  it("shows each criterion's provenance and the three disclosure columns", () => {
    const client = new FakeClient(makeDeliverySnapshot());
    render(<Harness client={client} />);

    expect(
      screen.getByRole("heading", { name: "search-v2 is ready. Does it do what you asked?" }),
    ).toBeInTheDocument();

    // One row per receipt criterion, provenance spelled out, never inferred.
    expect(screen.getByText("An alias finds the real author")).toBeInTheDocument();
    expect(screen.getByText("Seen · agent reported passed · app observed passed")).toBeInTheDocument();
    expect(screen.getByText("Title search still works")).toBeInTheDocument();
    expect(screen.getByText("Agent says · agent reported passed")).toBeInTheDocument();
    expect(screen.getByText("Fast on a large library")).toBeInTheDocument();
    expect(screen.getByText("Not proven yet · agent reported not proven")).toBeInTheDocument();

    // Three equal columns, in the prototype's order.
    expect(screen.getByRole("heading", { name: "Proven" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Not proven yet" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Faked for now" })).toBeInTheDocument();
    expect(screen.getByText("Alias lookup returned the real author")).toBeInTheDocument();
    expect(screen.getByText("Alias list is a sample")).toBeInTheDocument();
  });

  it("reads the unblock set from the graph when one is published", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    client.graph = makeGraph({
      nodes: [
        {
          id: `review:${fixture.delivery.id}`,
          kind: "review",
          label: fixture.delivery.name,
          need_id: fixture.need.id,
          state: "pending",
          assumed: false,
        },
        {
          id: "feature:core/two",
          kind: "feature",
          label: "Core two",
          feature: "core/two",
          state: "planned",
          assumed: false,
        },
        {
          id: "feature:core/three",
          kind: "feature",
          label: "Core three",
          feature: "core/three",
          state: "planned",
          assumed: false,
        },
      ],
      edges: [
        {
          from: `review:${fixture.delivery.id}`,
          to: "feature:core/two",
          source_event: fixture.delivery.id,
        },
        { from: "feature:core/two", to: "feature:core/three", source_event: ulid(181) },
      ],
    });
    render(<Harness client={client} />);
    expect(
      await screen.findByText("Accepting it unblocks Core two, Core three"),
    ).toBeInTheDocument();
  });

  it("reads the review's dependents through its feature in the real DTO shape", async () => {
    // The backend draws feature → review and feature → dependent, never
    // review → dependent; the unblock set must still name the dependents.
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    const project = fixture.project;
    const base = project.features["core/one"]!;
    project.features["core/two"] = {
      ...base,
      node: "core/two",
      title: "Core two",
      depends_on: ["core/one"],
    };
    project.features["core/three"] = {
      ...base,
      node: "core/three",
      title: "Core three",
      depends_on: ["core/two"],
    };
    client.graph = makeGraph({
      nodes: [
        {
          id: "feature:core/one",
          kind: "feature",
          label: "CLI ask",
          feature: "core/one",
          state: "done",
          assumed: false,
        },
        {
          id: `review:${fixture.delivery.id}`,
          kind: "review",
          label: fixture.delivery.name,
          feature: "core/one",
          need_id: fixture.need.id,
          state: "pending",
          assumed: false,
        },
        {
          id: "feature:core/two",
          kind: "feature",
          label: "Core two",
          feature: "core/two",
          state: "planned",
          assumed: false,
        },
        {
          id: "feature:core/three",
          kind: "feature",
          label: "Core three",
          feature: "core/three",
          state: "planned",
          assumed: false,
        },
      ],
      edges: [
        {
          from: "feature:core/one",
          to: `review:${fixture.delivery.id}`,
          source_event: fixture.delivery.id,
        },
        { from: "feature:core/one", to: "feature:core/two", source_event: ulid(182) },
        { from: "feature:core/two", to: "feature:core/three", source_event: ulid(183) },
      ],
    });
    render(<Harness client={client} project={project} />);
    expect(
      await screen.findByText("Accepting it unblocks Core two, Core three"),
    ).toBeInTheDocument();
  });

  it("never lists the delivered step itself when the review points at it", async () => {
    // The core graph draws review → its own feature; that feature is what the
    // delivery delivers, so the unblock set names only its dependents.
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    const project = fixture.project;
    const base = project.features["core/one"]!;
    project.features["core/two"] = {
      ...base,
      node: "core/two",
      title: "Core two",
      depends_on: ["core/one"],
    };
    project.features["core/three"] = {
      ...base,
      node: "core/three",
      title: "Core three",
      depends_on: ["core/two"],
    };
    client.graph = makeGraph({
      nodes: [
        {
          id: "feature:core/one",
          kind: "feature",
          label: "CLI ask",
          feature: "core/one",
          state: "done",
          assumed: false,
        },
        {
          id: `review:${fixture.delivery.id}`,
          kind: "review",
          label: fixture.delivery.name,
          feature: "core/one",
          need_id: fixture.need.id,
          state: "pending",
          assumed: false,
        },
        {
          id: "feature:core/two",
          kind: "feature",
          label: "Core two",
          feature: "core/two",
          state: "planned",
          assumed: false,
        },
        {
          id: "feature:core/three",
          kind: "feature",
          label: "Core three",
          feature: "core/three",
          state: "planned",
          assumed: false,
        },
      ],
      edges: [
        {
          from: `review:${fixture.delivery.id}`,
          to: "feature:core/one",
          source_event: fixture.delivery.id,
        },
        { from: "feature:core/one", to: "feature:core/two", source_event: ulid(184) },
        { from: "feature:core/two", to: "feature:core/three", source_event: ulid(185) },
      ],
    });
    render(<Harness client={client} project={project} />);
    expect(
      await screen.findByText("Accepting it unblocks Core two, Core three"),
    ).toBeInTheDocument();
  });

  it("says 'None claimed' instead of inventing a success for an empty column", () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const delivery = makeDelivery({
      disclosure: { proven: [], not_proven_yet: ["Large library speed"], faked_for_now: [] },
    });
    const fixture = makeDeliveryProject();
    render(
      <Harness client={client} project={fixture.project} delivery={delivery} need={fixture.need} />,
    );
    expect(screen.getAllByText("None claimed")).toHaveLength(2);
  });

  it("shows a delivery no decision asked for without an asked line or criteria", () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const delivery = makeDelivery({ receipt: undefined, criteria: [] });
    const fixture = makeDeliveryProject();
    render(
      <Harness client={client} project={fixture.project} delivery={delivery} need={fixture.need} />,
    );
    expect(screen.queryByText(/You asked for/)).not.toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Receipt criteria" })).not.toBeInTheDocument();
    expect(screen.queryByText(/criteri/)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /report/i })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Proven" })).toBeInTheDocument();
    expect(screen.getByText("Alias lookup returned the real author")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Accept/ })).toBeEnabled();
  });

  it("accepts with the delivery revision that is on screen", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    const onResolved = vi.fn();
    const onRefresh = vi.fn();
    render(
      <Harness
        client={client}
        project={fixture.project}
        delivery={fixture.delivery}
        need={fixture.need}
        onResolved={onResolved}
        onRefresh={onRefresh}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: /Accept search-v2/ }));
    await waitFor(() => expect(onResolved).toHaveBeenCalledWith("Accepted search-v2"));

    const request = client.calls.find((call) => call.command === "review_delivery")
      ?.request as ReviewDeliveryRequest;
    expect(request.verdict).toBe("accept");
    expect(request.delivery_id).toBe(fixture.delivery.id);
    expect(request.expected_delivery_revision).toBe(fixture.delivery.revision);
    expect(request.request_id).toMatch(/^[0-9A-Z]{26}$/);
    expect(onRefresh).toHaveBeenCalled();
  });

  it("sends back with a note and saves the house rule through set_rule", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    render(
      <Harness
        client={client}
        project={fixture.project}
        delivery={fixture.delivery}
        need={fixture.need}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: "Send back…" }));
    const note = "Keep the alias list complete";
    await userEvent.type(screen.getByLabelText("What should change?"), note);
    await userEvent.click(screen.getByLabelText(/Always do this in alpha/));
    await userEvent.click(screen.getByRole("button", { name: "Send back" }));

    await waitFor(() => expect(client.order).toContain("set_rule"));

    const review = client.calls.find((call) => call.command === "review_delivery")
      ?.request as ReviewDeliveryRequest;
    expect(review.verdict).toBe("send_back");
    expect(review.reason).toBe(note);
    expect(review.expected_delivery_revision).toBe(fixture.delivery.revision);

    const rule = client.calls.find((call) => call.command === "set_rule")?.request as SetRuleRequest;
    expect(rule.project).toBe("alpha");
    expect(rule.rule.text).toBe(note);
    expect(rule.rule.enabled).toBe(true);
    expect(rule.rule.rule_id).toBe("keep-the-alias-list-complete");
    // Absent rule identity: the client computes the precondition itself.
    const absent = await sha256Hex(canonicalJson(["rule", "alpha", rule.rule.rule_id, "absent"]));
    expect(rule.expected_revision).toBe(absent);
    // The review is appended before the rule (causally after it).
    expect(client.order.indexOf("review_delivery")).toBeLessThan(client.order.indexOf("set_rule"));
  });

  it("refuses a blank send-back note without calling the API", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    render(
      <Harness
        client={client}
        project={fixture.project}
        delivery={fixture.delivery}
        need={fixture.need}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: "Send back…" }));
    await userEvent.click(screen.getByRole("button", { name: "Send back" }));

    expect(screen.getByRole("alert")).toHaveTextContent("Write what should change first.");
    expect(client.order).not.toContain("review_delivery");
  });

  it("runs a proposed check only after approving that exact profile", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    render(
      <Harness
        client={client}
        project={fixture.project}
        delivery={fixture.delivery}
        need={fixture.need}
      />,
    );

    // The exact argv and cwd are visible before anything can run; the argument
    // with a space stays one quoted argument.
    expect(
      screen.getByText('/opt/agendar/bin/agendar-check text_equals --actual "one two.txt"'),
    ).toBeInTheDocument();
    expect(screen.getByText("repos/alpha")).toBeInTheDocument();
    expect(writes(client)).toEqual([]);

    await userEvent.click(screen.getByRole("button", { name: "Approve and run" }));
    await waitFor(() => expect(client.order).toContain("run_check"));

    expect(writes(client)).toEqual(["approve_check", "run_check"]);
    const approve = client.calls.find((call) => call.command === "approve_check")
      ?.request as ApproveCheckRequest;
    expect(approve.consent).toBe(true);
    expect(approve.profile).toMatchObject({
      criterion: "title",
      cwd: "repos/alpha",
      timeout_s: 30,
      argv: ["/opt/agendar/bin/agendar-check", "text_equals", "--actual", "one two.txt"],
    });
    // The first approval sends the real absence revision of this exact profile;
    // the same golden is asserted by the Rust test of `absent_revision`.
    expect(approve.expected_approval_revision).toBe(
      "21a2a12fd04f464086beafad0b8fa952bed2ad8cc2f7fa8749b58d08055d42fe",
    );
    expect(approve.expected_approval_revision).toMatch(/^[0-9a-f]{64}$/);
    expect(screen.getByText("Seen · passed")).toBeInTheDocument();
  });

  it("keeps argument boundaries visible instead of joining argv with spaces", () => {
    // A filename with a space and two separate arguments must never look alike.
    expect(argvLine(["agendar", "--actual", "one two.txt"])).not.toBe(
      argvLine(["agendar", "--actual", "one", "two.txt"]),
    );
    expect(argvLine(["agendar", "--actual", "one two.txt"])).toBe(
      'agendar --actual "one two.txt"',
    );
    expect(argvLine(["agendar", "it's", ""])).toBe('agendar "it\'s" ""');
  });

  it("shows one panel for a check that arrives through both declared carriers", () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    const base = makeDelivery();
    const delivery: DeliveryState = makeDelivery({
      check_proposals: [
        {
          criterion: "title",
          argv: ["/opt/agendar/bin/agendar-check", "text_equals", "--actual", "one two.txt"],
          cwd: "repos/alpha",
          timeout_s: 30,
        },
      ],
    });
    expect(base.demos[0]?.body.check).toBeDefined();
    render(
      <Harness
        client={client}
        project={fixture.project}
        delivery={delivery}
        need={fixture.need}
      />,
    );
    // Same profile in the event and in the local projection: one panel, once.
    expect(screen.getAllByRole("button", { name: "Approve and run" })).toHaveLength(1);
    expect(
      screen.getAllByText('/opt/agendar/bin/agendar-check text_equals --actual "one two.txt"'),
    ).toHaveLength(1);
  });

  it("shows a proposal that only the local journal projects", () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    const demos = makeDelivery().demos.map((demo) => {
      const body = { ...demo.body };
      delete body.check;
      return { kind: demo.kind, body };
    });
    const delivery: DeliveryState = makeDelivery({
      demos,
      check_proposals: [
        {
          criterion: "title",
          argv: ["agendar", "check-probe", "file_digest", "--path", "bench/speed.out"],
          cwd: "repos/alpha",
          timeout_s: 60,
        },
      ],
    });
    render(
      <Harness
        client={client}
        project={fixture.project}
        delivery={delivery}
        need={fixture.need}
      />,
    );
    expect(
      screen.getByText("agendar check-probe file_digest --path bench/speed.out"),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve and run" })).toBeInTheDocument();
  });

  it("asks for a new approval when the displayed profile changes", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    const { rerender } = render(
      <Harness
        client={client}
        project={fixture.project}
        delivery={fixture.delivery}
        need={fixture.need}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Approve and run" }));
    await waitFor(() =>
      expect(client.order.filter((command) => command === "approve_check")).toHaveLength(1),
    );

    // Same delivery id, different cwd and timeout: the cached approval is gone.
    rerender(
      <Harness
        client={client}
        project={fixture.project}
        delivery={makeCheckDelivery({ cwd: "repos/beta", timeout_s: 45 })}
        need={fixture.need}
      />,
    );
    expect(screen.getByText("repos/beta")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Approve and run" }));
    await waitFor(() =>
      expect(client.order.filter((command) => command === "approve_check")).toHaveLength(2),
    );
    const approvals = client.calls
      .filter((call) => call.command === "approve_check")
      .map((call) => call.request as ApproveCheckRequest);
    expect(approvals[1]!.profile).toMatchObject({ cwd: "repos/beta", timeout_s: 45 });
    expect(approvals[1]!.expected_approval_revision).not.toBe(
      approvals[0]!.expected_approval_revision,
    );
  });

  it("does not run a check when approval is refused", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    client.approveError = new ApiError({
      code: "E_UNSUPPORTED",
      message: "That executable is not supported.",
      hint: "Only the bundled check helper can run.",
      retryable: false,
    });
    const fixture = makeDeliveryProject();
    render(
      <Harness
        client={client}
        project={fixture.project}
        delivery={fixture.delivery}
        need={fixture.need}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: "Approve and run" }));
    await waitFor(() => expect(client.order).toContain("approve_check"));

    expect(client.order).not.toContain("run_check");
    expect(screen.getByRole("alert")).toHaveTextContent("That executable is not supported.");
  });

  it("keeps a stale contradiction visible instead of accepting it", async () => {    const client = new FakeClient(makeDeliverySnapshot());
    client.reviewError = new ApiError({
      code: "E_STALE_ANSWER",
      message: "The delivery changed since it was shown.",
      hint: "Review the refreshed evidence.",
      retryable: false,
    });
    const fixture = makeDeliveryProject();
    const onRefresh = vi.fn();
    render(
      <Harness
        client={client}
        project={fixture.project}
        delivery={fixture.delivery}
        need={fixture.need}
        onRefresh={onRefresh}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: /Accept search-v2/ }));
    await waitFor(() => expect(screen.getByRole("alert")).toBeInTheDocument());
    expect(screen.getByRole("alert")).toHaveTextContent("The delivery changed since it was shown.");
    expect(onRefresh).toHaveBeenCalled();
  });
});

describe("delivery navigation", () => {
  it("opens the review row in the delivery view and accepts with Cmd/Ctrl-Enter", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });

    const row = document.querySelector<HTMLElement>('[data-row="need:review-one"]');
    expect(row).not.toBeNull();
    await userEvent.click(row as HTMLElement);
    await screen.findByRole("heading", {
      name: "search-v2 is ready. Does it do what you asked?",
    });

    await userEvent.keyboard("{Control>}{Enter}{/Control}");
    await waitFor(() => expect(client.order).toContain("review_delivery"));
    const request = client.calls.find((call) => call.command === "review_delivery")
      ?.request as ReviewDeliveryRequest;
    expect(request.verdict).toBe("accept");
  });
});

describe("an accept that replies before its snapshot", () => {
  it("leaves Accepting… on the reply and moves on when the later snapshot lands", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    // The host replies once the review is durable; its snapshot is published
    // afterwards, so until then reads still show the review.
    client.reviewDelivery = async (request: ReviewDeliveryRequest) => {
      client.calls.push({ command: "review_delivery", request });
      client.order.push("review_delivery");
      return { event_id: ulid(700), released: [] };
    };
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });
    await userEvent.click(
      document.querySelector<HTMLElement>('[data-row="need:review-one"]') as HTMLElement,
    );
    const question = "search-v2 is ready. Does it do what you asked?";
    await screen.findByRole("heading", { name: question });

    await userEvent.click(screen.getByRole("button", { name: /Accept search-v2/ }));
    await waitFor(() => expect(client.order).toContain("review_delivery"));
    // Not stuck on Accepting…, and the reviewed revision cannot be sent twice.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /Accept search-v2/ })).toBeDisabled(),
    );
    expect(screen.queryByRole("button", { name: /Accepting…/ })).toBeNull();
    expect(screen.getByRole("button", { name: "Send back…" })).toBeDisabled();
    expect(client.order.filter((command) => command === "review_delivery")).toHaveLength(1);

    // The publication arrives later: the review leaves and the app moves on.
    const later = structuredClone(client.snapshot);
    later.needs = later.needs.filter((item) => item.id !== fixture.need.id);
    const delivery = later.state.projects.alpha!.deliveries[fixture.delivery.id]!;
    delivery.review = "accepted";
    delivery.revision = "e".repeat(64);
    later.generation += 1;
    client.setSnapshot(later);
    client.emit("agendar://changed", {
      v: 1,
      epoch: later.epoch,
      generation: later.generation,
      event_set_digest: later.state.event_set_digest,
      as_of: later.state.evaluated_at,
      projects: ["alpha"],
      pulses: [],
      batch: "local",
    });
    await waitFor(() =>
      expect(screen.queryByRole("heading", { name: question })).not.toBeInTheDocument(),
    );
  });
});

describe("project evidence cards", () => {
  it("lists a delivery with its thumbnail provenance and the house rules", () => {
    const client = new FakeClient(makeDeliverySnapshot());
    const fixture = makeDeliveryProject();
    // A delivery the app saw in every criterion is labelled Seen, not Agent says.
    const allSeen: DeliveryState = makeDelivery({
      criteria: makeDelivery().criteria.map((entry) => ({
        ...entry,
        seen_result: "passed" as const,
        provenance: "seen" as const,
      })),
    });
    render(
      <ClientContext.Provider value={client}>
        <Project
          {...projectProps(
            { ...fixture.project, deliveries: { [allSeen.id]: allSeen } },
            client,
          )}
        />
      </ClientContext.Provider>,
    );

    expect(screen.getByText("search-v2")).toBeInTheDocument();
    expect(screen.getByText("Seen · Waiting for you")).toBeInTheDocument();
    expect(screen.getByText("Use explicit argv")).toBeInTheDocument();
  });
});

describe("dev fixture wiring", () => {
  it("renders the fixture review and runs its check through the fixture transport", async () => {
    const client = createClient(createFixtureTransport(), "fixture");
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });

    const row = document.querySelector<HTMLElement>('[data-row="need:review:shelf-alias"]');
    expect(row).not.toBeNull();
    await userEvent.click(row as HTMLElement);
    await screen.findByRole("heading", {
      name: "search-v2 is ready. Does it do what you asked?",
    });
    expect(screen.getByText("Not proven yet · agent reported not proven")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Approve and run" }));
    await waitFor(() =>
      expect(screen.getByText("Seen · agent reported not proven · app observed passed")).toBeInTheDocument(),
    );
  });
});

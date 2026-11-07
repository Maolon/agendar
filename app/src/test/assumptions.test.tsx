import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { DecisionCard, PrepareAnswerRequest, ProjectState, Receipt } from "../api/types";
import { createClient } from "../api/client";
import { ClientContext, SnapshotStore } from "../api/store";
import { Assumptions } from "../components/Assumptions";
import { createFixtureTransport } from "../dev/fixtureTransport";
import { App } from "../App";
import { AnswerAction } from "../state/answer";
import { Project } from "../views/Project";
import {
  EVALUATED_AT,
  FakeClient,
  easyCard,
  makeDecision,
  makeProject,
  projectProps,
  sha,
  ulid,
} from "./fixtures";

const ASK = ulid(170);
const RECEIPT = ulid(171);
const ASSUMED_AT = "2026-02-10T13:00:00.000Z";

const CARD: DecisionCard = easyCard({
  question: "Keep WebM as the capture format?",
  options: [
    {
      id: "webm",
      label: "Keep WebM",
      consequence: "Nothing to redo",
      reversibility: "easy",
      external: false,
    },
    {
      id: "mp4",
      label: "Switch to MP4",
      consequence: "The encoder is rewritten",
      reversibility: "costly",
      external: false,
    },
  ],
  recommended: { option: "webm", confidence: "high", because: "It plays in Safari." },
  reversibility: "costly",
});

/** A project whose `core/encoder` and `core/export` are built on one assumption. */
function assumptionProject(lifecycle: "open" | "answered" = "open"): ProjectState {
  const base = makeProject("alpha");
  const feature = base.features["core/one"]!;
  const decision = makeDecision(ASK, CARD, {
    node: "core/encoder",
    mode: "assume",
    lifecycle,
    assumed_at: ASSUMED_AT,
    built_on: ["core/encoder", "core/export"],
    ...(lifecycle === "answered" ? { effective_receipt: ulid(172) } : {}),
  });
  const receipt: Receipt = {
    id: RECEIPT,
    ask: ASK,
    node: "core/encoder",
    source: "assumed",
    chose: CARD.options[0]!,
    ruled_out: [CARD.options[1]!],
    done_when: CARD.done_when,
    created_at: ASSUMED_AT,
  };
  return {
    ...base,
    features: {
      "core/encoder": {
        ...feature,
        node: "core/encoder",
        title: "WebM encoder",
        progress: "doing",
        gate: "open",
        basis: "assumed",
        liveness: "live",
        assumptions: [RECEIPT],
      },
      "core/export": {
        ...feature,
        node: "core/export",
        title: "Export & share",
        progress: "planned",
        gate: "awaiting_upstream",
        basis: "assumed",
        assumptions: [RECEIPT],
        depends_on: ["core/encoder"],
      },
    },
    decisions: { [ASK]: decision },
    receipts: { [RECEIPT]: receipt },
    needs_you: [],
  };
}

function renderAssumptions(project = assumptionProject()) {
  const client = new FakeClient();
  render(
    <ClientContext.Provider value={client}>
      <Project {...projectProps(project, client)} />
    </ClientContext.Provider>,
  );
  return client;
}

function prepared(client: FakeClient): PrepareAnswerRequest | undefined {
  return client.calls.find((call) => call.command === "prepare_answer")
    ?.request as PrepareAnswerRequest | undefined;
}

describe("assumptions", () => {
  it("shows the age, what is built on it, and the assumed option", () => {
    renderAssumptions();
    expect(screen.getByText("Assumed 2h ago · 2 features built on it")).toBeInTheDocument();
    expect(screen.getByText(/WebM encoder, Export & share/)).toBeInTheDocument();
    expect(screen.getByText("Keep WebM")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Confirm Keep WebM" }),
    ).toBeInTheDocument();
  });

  it("confirms through the existing answer path with the assumed option", async () => {
    const client = renderAssumptions();
    await userEvent.click(screen.getByRole("button", { name: "Confirm Keep WebM" }));
    expect(prepared(client)?.ask_id).toBe(ASK);
    expect(prepared(client)?.option_id).toBe("webm");
    expect(prepared(client)?.expected_decision_revision).toBe(sha(0x31));
  });

  it("previews the rework set before an override and then answers with the other option", async () => {
    const client = renderAssumptions();
    await userEvent.selectOptions(
      screen.getByLabelText("Override Keep WebM as the capture format?"),
      "mp4",
    );
    const rework = document.querySelector(".rework");
    expect(rework?.textContent).toContain("Overriding with Switch to MP4 marks");
    expect(rework?.textContent).toContain("2 features for rework");
    expect(rework?.textContent).toContain("WebM encoder, Export & share");
    // Nothing is answered before the deliberate override action.
    expect(prepared(client)).toBeUndefined();

    await userEvent.click(screen.getByRole("button", { name: "Override with Switch to MP4" }));
    expect(prepared(client)?.option_id).toBe("mp4");
  });

  it("shows only the decision's own assumption in the decision view", () => {
    const client = new FakeClient();
    const project = assumptionProject();
    const action = new AnswerAction(client, {
      current: { onAnswered: () => undefined, onRefresh: () => undefined },
    });
    const view = render(
      <ClientContext.Provider value={client}>
        <Assumptions
          project={project}
          anchor={EVALUATED_AT}
          canMutate
          action={action}
          only={ASK}
        />
      </ClientContext.Provider>,
    );
    expect(screen.getByText("Assumed 2h ago · 2 features built on it")).toBeInTheDocument();
    view.rerender(
      <ClientContext.Provider value={client}>
        <Assumptions
          project={project}
          anchor={EVALUATED_AT}
          canMutate
          action={action}
          only={ulid(199)}
        />
      </ClientContext.Provider>,
    );
    expect(screen.queryByText("Assumed 2h ago · 2 features built on it")).not.toBeInTheDocument();
  });

  it("shows the dev fixture assumption, so the demo needs no Vault", async () => {
    const client = createClient(createFixtureTransport(), "fixture");
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });
    await userEvent.click(
      document.querySelector<HTMLElement>('[data-row="project:screen-clips"]') as HTMLElement,
    );
    expect(await screen.findByText("Assumed 4h ago · 2 features built on it")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Confirm Keep WebM" }),
    ).toBeInTheDocument();
    await userEvent.selectOptions(
      screen.getByLabelText("Override Keep WebM as the capture format?"),
      "mp4",
    );
    expect(document.querySelector(".rework")?.textContent).toContain(
      "2 features for rework",
    );
  });

  it("confirms the dev fixture assumption through the fixture answer path", async () => {
    const client = createClient(createFixtureTransport(), "fixture");
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });
    await userEvent.click(
      document.querySelector<HTMLElement>('[data-row="project:screen-clips"]') as HTMLElement,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Confirm Keep WebM" }));
    // The assumed option is confirmed through prepare_answer → answer_decision,
    // and the confirmed assumption leaves the page: with nothing to confirm the
    // Assumptions section is not shown at all.
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Confirm Keep WebM" })).toBeNull(),
    );
    expect(screen.queryByRole("heading", { name: "Assumptions" })).toBeNull();
  });

  it("drops an assumption once the decision was answered", () => {
    renderAssumptions(assumptionProject("answered"));
    expect(screen.queryByRole("heading", { name: "Assumptions" })).toBeNull();
  });
});

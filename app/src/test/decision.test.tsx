import { useReducer } from "react";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../api/client";
import { ClientContext } from "../api/store";
import { ICON_NAMES, Icon, PROJECT_GLYPHS, isIconName, isProjectGlyph } from "../components/Icon";
import { initialUiState, uiReducer } from "../state/ui";
import { useAnswerAction } from "../state/answer";
import type { AnswerDecisionData, BlobRef, DecisionCard as DecisionCardDto, ReportState } from "../api/types";
import { Decision } from "../views/Decision";
import {
  ASK_ONE,
  easyCard,
  FakeClient,
  irreversibleCard,
  makeDecision,
  makeNeed,
  makeProject,
  makeSnapshot,
  sha,
} from "./fixtures";

function Harness({
  client,
  card,
  canMutate = true,
  onAnswered = vi.fn(),
  onRefresh = vi.fn(),
  reports,
}: {
  client: FakeClient;
  card: DecisionCardDto;
  reports?: ReportState[];
  canMutate?: boolean;
  onAnswered?: (data: AnswerDecisionData) => void;
  onRefresh?: () => void;
}) {
  const [ui, dispatch] = useReducer(uiReducer, initialUiState);
  const decision = makeDecision(ASK_ONE, card);
  const project = makeProject("alpha");
  project.decisions = { [ASK_ONE]: decision };
  if (reports) project.reports = reports;
  const need = makeNeed({ id: "need-one" });
  const { action } = useAnswerAction(client, { onAnswered, onRefresh });
  return (
    <ClientContext.Provider value={client}>
      <Decision
        project={project}
        decision={decision}
        need={need}
        anchor="2026-02-10T15:00:00.000Z"
        canMutate={canMutate}
        ui={ui}
        onSelectOption={(optionId) => {
          // A changed selection is a new action and disarms the prepared token.
          if (optionId !== ui.option_by_ask[decision.id]) action.invalidate(decision.id);
          dispatch({ type: "choose_option", ask: decision.id, option: optionId, explicit: true });
        }}
        onSeeGraph={vi.fn()}
        action={action}
      />
    </ClientContext.Provider>
  );
}

const LINKED: BlobRef = { host: "h-01h0000000000000000000000", sha256: sha(0x91), bytes: 900, ext: "html" };

describe("decision view", () => {
  it("shows a linked report before the choices, with its title and summary, and opens it", async () => {
    const client = new FakeClient();
    const report: ReportState = {
      id: "01H00000000000000000000091",
      title: "Waking idle agents compared",
      summary: "Hooks wake within a second; an inbox waits for the next run.",
      report: LINKED,
      kind: "proposal",
      domain: "arch",
      ts: "2026-02-10T14:00:00.000Z",
      session: "01H00000000000000000000092",
      harness: "claude_code",
    };
    render(<Harness client={client} card={{ ...easyCard(), report: LINKED }} reports={[report]} />);

    const linked = screen.getByRole("region", { name: "Waking idle agents compared" });
    expect(linked).toHaveTextContent("Hooks wake within a second; an inbox waits for the next run.");
    expect(linked.querySelector(".ra-kicker")?.textContent).toBe("Proposal · Architecture");
    const choices = screen.getByRole("group", { name: "Options" });
    expect(linked.compareDocumentPosition(choices) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    await userEvent.click(within(linked).getByRole("button", { name: /Open report/ }));
    const opened = client.calls.find((call) => call.command === "open_report")?.request as {
      project: string;
      blob: BlobRef;
    };
    expect(opened.project).toBe("alpha");
    expect(opened.blob.sha256).toBe(LINKED.sha256);
  });

  it("names a linked report the project does not list", () => {
    render(<Harness client={new FakeClient()} card={{ ...easyCard(), report: LINKED }} />);
    const linked = screen.getByRole("region", { name: "Attached report" });
    expect(linked.querySelector(".ra-kicker")?.textContent).toBe("Report");
  });

  it("shows the chosen option as the primary label and previews the receipt", () => {
    const client = new FakeClient();
    const { container } = render(<Harness client={client} card={easyCard()} />);

    expect(screen.getByRole("heading", { name: "How should answers reach idle agents?" })).toBeInTheDocument();
    expect(screen.getByText("Your receipt")).toBeInTheDocument();
    expect(screen.getByText("Preview · not saved")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Use inbox on next start/ })).toBeInTheDocument();
    const primary = container.querySelector<HTMLButtonElement>("[data-primary-action]");
    expect(primary).not.toBeNull();
    expect(primary?.textContent).toContain("Use harness adapters");
    expect(primary?.disabled).toBe(false);
    expect(screen.getByText("Already checked")).toBeInTheDocument();
    // The answer bar stays pinned in reach, and the confidence level is copy, not an enum.
    expect(primary?.closest(".acts")?.classList.contains("dc-acts")).toBe(true);
    expect(container.querySelector(".recommendation")?.textContent).toMatch(/confidence (low|medium|high) —/);
  });

  it("sends an answer in your own words when no option fits", async () => {
    const client = new FakeClient();
    const onAnswered = vi.fn();
    const user = userEvent.setup();
    const { container } = render(
      <Harness client={client} card={easyCard()} onAnswered={onAnswered} />,
    );
    await user.click(screen.getByRole("button", { name: /Write your own answer/ }));
    const primary = () => container.querySelector<HTMLButtonElement>("[data-primary-action]")!;
    expect(primary().textContent).toContain("Send your answer");
    expect(primary().disabled).toBe(true);
    await user.type(screen.getByRole("textbox", { name: "Your own answer" }), "Poll every minute");
    // The receipt shows your words; every option is ruled out.
    expect(screen.getAllByText("Poll every minute").length).toBeGreaterThan(0);
    expect(primary().disabled).toBe(false);
    primary().click();
    await waitFor(() => expect(onAnswered).toHaveBeenCalledTimes(1));
    const prepared = client.calls.find((call) => call.command === "prepare_answer")?.request as {
      option_id: string;
      own_answer?: string;
    };
    expect(prepared).toMatchObject({ option_id: "own", own_answer: "Poll every minute" });
  });

  it("commits an easy recommendation through prepare then answer", async () => {
    const client = new FakeClient();
    const onAnswered = vi.fn();
    const { container } = render(
      <Harness client={client} card={easyCard()} onAnswered={onAnswered} />,
    );
    container.querySelector<HTMLButtonElement>("[data-primary-action]")?.click();
    await waitFor(() => expect(onAnswered).toHaveBeenCalledTimes(1));
    // `get_project` is a projection read the unblock text needs; the writes
    // themselves still happen in exactly this order.
    expect(client.order.filter((command) => command !== "get_project")).toEqual([
      "prepare_answer",
      "answer_decision",
    ]);
    const answer = client.calls.find((call) => call.command === "answer_decision")?.request as {
      echo_label: string;
      done_when?: unknown;
    };
    expect(answer.echo_label).toBe("Use harness adapters");
    expect(answer.done_when).toBeUndefined();
    expect(onAnswered.mock.calls[0]?.[0]).toMatchObject({ awaiting_ack: true });
  });

  it("sends edited done_when criteria and keeps the unedited original otherwise", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const { container } = render(<Harness client={client} card={easyCard()} />);
    await user.click(screen.getByRole("button", { name: "Edit done when" }));
    const input = screen.getByLabelText("Criterion 1");
    await user.clear(input);
    await user.type(input, "New observable criterion");
    await user.click(screen.getByRole("button", { name: "Save criteria" }));
    expect(screen.getByText("New observable criterion")).toBeInTheDocument();

    container.querySelector<HTMLButtonElement>("[data-primary-action]")?.click();
    await waitFor(() => expect(client.order).toContain("answer_decision"));
    const answer = client.calls.find((call) => call.command === "answer_decision")?.request as {
      done_when: { id: string; text: string }[];
    };
    expect(answer.done_when).toEqual([{ id: "wakes", text: "New observable criterion" }]);
  });

  it("arms an irreversible choice only after an explicit selection and echoed confirmation", async () => {
    const user = userEvent.setup();
    const client = new FakeClient(makeSnapshot({ firstCard: irreversibleCard() }));
    const { container } = render(<Harness client={client} card={irreversibleCard()} />);
    const primary = () => container.querySelector<HTMLButtonElement>("[data-primary-action]");
    expect(primary()?.disabled).toBe(true);
    expect(primary()?.textContent).toContain("Pick an option");
    expect(screen.getByText("Pick an option to see the receipt.")).toBeInTheDocument();

    const [deployOption] = screen.getAllByRole("button", { name: /Deploy now/ });
    await user.click(deployOption!);
    expect(primary()?.textContent).toContain("Deploy now");
    await user.click(primary()!);

    await waitFor(() => expect(client.order).toContain("prepare_answer"));
    expect(client.order).not.toContain("answer_decision");
    const confirm = await screen.findByRole("button", { name: "Confirm: Deploy now" });
    expect(confirm).toBeInTheDocument();

    // Changing the choice disarms the token.
    const [waitOption] = screen.getAllByRole("button", { name: /Wait until Friday/ });
    await user.click(waitOption!);
    expect(screen.queryByRole("button", { name: /Confirm:/ })).toBeNull();

    const [deployAgain] = screen.getAllByRole("button", { name: /Deploy now/ });
    await user.click(deployAgain!);
    await user.click(primary()!);
    await screen.findByRole("button", { name: "Confirm: Deploy now" });
    await user.click(screen.getByRole("button", { name: "Confirm: Deploy now" }));
    await waitFor(() => expect(client.order).toContain("answer_decision"));
    const answer = client.calls.filter((call) => call.command === "answer_decision").at(-1)?.request as {
      echo_label: string;
    };
    expect(answer.echo_label).toBe("Deploy now");
  });

  it("refuses a backend confirm escalation until an explicit choice and the echoed confirmation", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.guardOverride = "confirm";
    const { container } = render(<Harness client={client} card={easyCard()} />);
    const primary = () => container.querySelector<HTMLButtonElement>("[data-primary-action]");

    // The implicit recommendation is not a choice: the escalation asks for one.
    await user.click(primary()!);
    expect(await screen.findByRole("alert")).toHaveTextContent("explicit choice");
    expect(screen.queryByRole("button", { name: /Confirm:/ })).toBeNull();
    expect(client.order).not.toContain("answer_decision");
    expect(primary()?.textContent).toContain("Pick an option");
    expect(primary()?.disabled).toBe(true);

    await user.click(screen.getByRole("button", { name: /Use harness adapters/ }));
    await user.click(primary()!);
    const confirm = await screen.findByRole("button", { name: "Confirm: Use harness adapters" });
    expect(confirm).toBeInTheDocument();
    expect(client.order).not.toContain("answer_decision");

    await user.click(confirm);
    await waitFor(() => expect(client.order).toContain("answer_decision"));
  });

  it("labels each checked fact with its provenance in readable text", () => {
    const client = new FakeClient();
    render(<Harness client={client} card={easyCard()} />);
    const seen = screen.getByText(/Stop hook can hold a session/).closest(".fact");
    expect(seen).toHaveTextContent("Seen");
    const said = screen.getByText(/Queue can message it/).closest(".fact");
    expect(said).toHaveTextContent("Agent says");
  });

  it("retries the identical immutable request after an uncertain answer outcome", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.answerError = new ApiError({
      code: "E_IO",
      message: "The commit may not have been recorded.",
      hint: "Retry the same answer.",
      retryable: true,
    });
    const { container } = render(<Harness client={client} card={easyCard()} />);
    await user.click(container.querySelector<HTMLButtonElement>("[data-primary-action]")!);
    expect(await screen.findByRole("alert")).toHaveTextContent("may not have been recorded");
    const answered = () =>
      client.calls.filter((call) => call.command === "answer_decision").map((call) => call.request);
    expect(answered()).toHaveLength(1);
    const first = answered()[0];

    // The retry re-sends the retained payload, token included, with no new prepare.
    client.answerError = null;
    await user.click(container.querySelector<HTMLButtonElement>("[data-primary-action]")!);
    await waitFor(() => expect(answered()).toHaveLength(2));
    expect(answered()[1]).toEqual(first);
    expect(client.calls.filter((call) => call.command === "prepare_answer")).toHaveLength(1);
  });

  it("shows a stale-answer error and refreshes instead of pretending success", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.prepareError = new ApiError({
      code: "E_STALE_ANSWER",
      message: "Answer is no longer current.",
      hint: "Refresh the card before answering.",
      retryable: false,
    });
    const onRefresh = vi.fn();
    const { container } = render(
      <Harness client={client} card={easyCard()} onRefresh={onRefresh} />,
    );
    await user.click(container.querySelector<HTMLButtonElement>("[data-primary-action]")!);
    expect(await screen.findByRole("alert")).toHaveTextContent("Answer is no longer current.");
    expect(screen.queryByText(/Receipt saved/)).toBeNull();
    expect(onRefresh).toHaveBeenCalled();
    expect(client.order).not.toContain("answer_decision");
  });

  it("disables every mutation while the state is stale", () => {
    const client = new FakeClient();
    const { container } = render(<Harness client={client} card={easyCard()} canMutate={false} />);
    expect(container.querySelector<HTMLButtonElement>("[data-primary-action]")?.disabled).toBe(true);
    expect(
      screen.getByText("Actions are disabled until the app refreshes the current state."),
    ).toBeInTheDocument();
  });
});

describe("icon map", () => {
  it("maps every vocabulary name to a rendered glyph and rejects unknown strings", () => {
    const { container } = render(
      <div>
        {ICON_NAMES.map((name) => (
          <Icon key={name} name={name} />
        ))}
      </div>,
    );
    expect(container.querySelectorAll("svg.ic")).toHaveLength(ICON_NAMES.length);
    expect(isIconName("star")).toBe(true);
    expect(isIconName("emoji-face")).toBe(false);
    for (const glyph of PROJECT_GLYPHS) expect(isProjectGlyph(glyph)).toBe(true);
    expect(isProjectGlyph("star")).toBe(false);
  });
});

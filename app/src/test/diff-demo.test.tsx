import { useState } from "react";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { DeliveryState, Demo } from "../api/types";
import { ClientContext } from "../api/store";
import { DemoView } from "../components/Demo";
import { Delivery } from "../views/Delivery";
import { parseHunks } from "../state/diff";
import { FakeClient, makeDelivery, makeDeliveryProject, makeDeliverySnapshot, sha, textBlob } from "./fixtures";

const COMMIT = "233cf80bebc689ffa7ca698deff2d95d16437dad";
const BLOB = textBlob(sha(0xd1), "txt");

function longHunk(): string {
  const lines = Array.from({ length: 50 }, (_, index) => `+added line ${index + 1}`);
  return ["@@ -0,0 +1,50 @@", ...lines].join("\n");
}

const EXCERPT = [
  "diff --git a/crates/agendar-core/src/model/validate.rs b/crates/agendar-core/src/model/validate.rs",
  "index 96fdba3..ce8a1ad 100644",
  "--- a/crates/agendar-core/src/model/validate.rs",
  "+++ b/crates/agendar-core/src/model/validate.rs",
  "@@ -1265,4 +1266,5 @@ fn validate_decision_ack(body: &DecisionAck) -> Result<()> {",
  " }",
  "-fn validate_delivery(body: &Delivery) -> Result<()> {",
  "+fn validate_delivery(body: &Delivery, event: &Event) -> Result<()> {",
  "+    // Criteria are a receipt's done_when.",
  "     validate_text(\"/name\", &body.name, 1, 80, false)?;",
  "diff --git a/crates/agendar-cli/src/diff.rs b/crates/agendar-cli/src/diff.rs",
  "new file mode 100644",
  "--- /dev/null",
  "+++ b/crates/agendar-cli/src/diff.rs",
  longHunk(),
  "",
].join("\n");

const DIFF_DEMO: Demo = {
  kind: "diff",
  body: {
    blob: BLOB,
    commit: COMMIT,
    files_total: 5,
    hunks_total: 9,
    added_total: 120,
    removed_total: 30,
    shown: [
      {
        path: "crates/agendar-core/src/model/validate.rs",
        hunk: 4,
        why: "A delivery without a receipt lists no criteria.",
      },
      { path: "crates/agendar-cli/src/diff.rs", hunk: 1, why: "The cutter itself." },
    ],
    omitted: [
      { path: "app/src/views/Delivery.tsx", added: 42, removed: 38 },
      { path: "README.md", added: 1, removed: 0 },
      { path: "skills/agendar/SKILL.md", added: 3, removed: 2 },
    ],
  },
};

function clientWithExcerpt(): FakeClient {
  const client = new FakeClient(makeDeliverySnapshot());
  client.reports.set(BLOB.sha256, { mime: "text/plain", text: EXCERPT, sha256: BLOB.sha256 });
  return client;
}

describe("diff demo", () => {
  it("numbers lines from the @@ header", () => {
    const [first] = parseHunks(EXCERPT);
    expect(first?.lines.map((line) => [line.kind, line.old, line.new])).toEqual([
      ["ctx", 1265, 1266],
      ["del", 1266, undefined],
      ["add", undefined, 1267],
      ["add", undefined, 1268],
      ["ctx", 1267, 1269],
    ]);
  });

  it("shows counts, provenance, why lines, numbered rows, a collapsed long hunk and the omitted files", async () => {
    const client = clientWithExcerpt();
    render(
      <ClientContext.Provider value={client}>
        <DemoView project="agendar" demo={DIFF_DEMO} />
      </ClientContext.Provider>,
    );
    expect(screen.getByText("Core diff · 233cf80")).toBeInTheDocument();
    expect(screen.getByText("2 of 5 files · 2 of 9 hunks")).toBeInTheDocument();
    expect(screen.getByText("Cut verbatim from the commit")).toBeInTheDocument();
    await screen.findByText("The cutter itself.");
    expect(screen.getByText("A delivery without a receipt lists no criteria.")).toBeInTheDocument();
    expect(screen.getByText("The cutter itself.")).toBeInTheDocument();
    expect(screen.getByText("validate.rs")).toBeInTheDocument();
    expect(screen.getByText("crates/agendar-core/src/model/")).toBeInTheDocument();

    const added = await screen.findByText(
      "+fn validate_delivery(body: &Delivery, event: &Event) -> Result<()> {",
    );
    const addedRow = added.closest(".diff-row") as HTMLElement;
    expect(addedRow).toHaveClass("add");
    expect(within(addedRow).getByText("1267")).toBeInTheDocument();
    const removedRow = screen
      .getByText("-fn validate_delivery(body: &Delivery) -> Result<()> {")
      .closest(".diff-row") as HTMLElement;
    expect(removedRow).toHaveClass("del");
    expect(within(removedRow).getByText("1266")).toBeInTheDocument();
    expect(
      screen.getByText("@@ -1265,4 +1266,5 @@ fn validate_decision_ack(body: &DecisionAck) -> Result<()> {"),
    ).toBeInTheDocument();

    // 50 lines: 20 + 12 visible, 18 behind the button.
    expect(screen.getByText("+added line 20")).toBeInTheDocument();
    expect(screen.queryByText("+added line 21")).not.toBeInTheDocument();
    expect(screen.getByText("+added line 39")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Show 18 more lines" }));
    expect(screen.getByText("+added line 21")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /more lines/ })).not.toBeInTheDocument();

    const toggle = screen.getByRole("button", { name: "Not shown: 3 files, +46 −40" });
    expect(screen.queryByText("Delivery.tsx")).not.toBeInTheDocument();
    await userEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("Delivery.tsx")).toBeInTheDocument();
    expect(screen.getByText("README.md")).toBeInTheDocument();
    expect(screen.getByText("+42")).toBeInTheDocument();
    expect(screen.getByText("−38")).toBeInTheDocument();
  });
});

function DeliveryHarness({ delivery }: { delivery: DeliveryState }) {
  const fixture = makeDeliveryProject();
  const [open, setOpen] = useState(false);
  return (
    <ClientContext.Provider value={clientWithExcerpt()}>
      <Delivery
        project={fixture.project}
        delivery={delivery}
        need={fixture.need}
        anchor="2026-02-10T15:00:00.000Z"
        canMutate
        sendBackOpen={open}
        onSendBackOpen={setOpen}
        onResolved={vi.fn()}
        onRefresh={vi.fn()}
      />
    </ClientContext.Provider>
  );
}

describe("delivery title", () => {
  it("does not claim anyone asked when there is no receipt", () => {
    render(
      <DeliveryHarness
        delivery={makeDelivery({ receipt: undefined, criteria: [], demos: [DIFF_DEMO] })}
      />,
    );
    expect(screen.getByRole("heading", { name: "search-v2 is ready for review." })).toBeInTheDocument();
    expect(screen.queryByText(/Does it do what you asked/)).not.toBeInTheDocument();
  });

  it("keeps the asked title for a delivery with a receipt", () => {
    render(<DeliveryHarness delivery={makeDelivery()} />);
    expect(
      screen.getByRole("heading", { name: "search-v2 is ready. Does it do what you asked?" }),
    ).toBeInTheDocument();
  });
});

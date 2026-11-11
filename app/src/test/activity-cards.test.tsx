import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { DeliveryState, Demo } from "../api/types";
import { ClientContext } from "../api/store";
import { EVIDENCE_SHOWN, EvidenceCards, thumbLines } from "../components/EvidenceCard";
import { translator } from "../i18n";
import { parseHunks } from "../state/diff";
import { thumbnailDemo } from "../state/delivery";
import { FakeClient, imageBlob, makeDelivery, sha, textBlob, ulid } from "./fixtures";

const DIFF_BLOB = textBlob(sha(0xd7));
const EXCERPT = [
  "diff --git a/src/lib.rs b/src/lib.rs",
  "--- a/src/lib.rs",
  "+++ b/src/lib.rs",
  "@@ -10,4 +10,4 @@ fn main() {",
  " fn keep() {}",
  "-fn old() {}",
  "+fn new() {}",
  " fn tail() {}",
  "",
].join("\n");

const DIFF_DEMO: Demo = {
  kind: "diff",
  body: {
    blob: DIFF_BLOB,
    commit: "233cf80bebc689ffa7ca698deff2d95d16437dad",
    files_total: 1,
    hunks_total: 1,
    added_total: 1,
    removed_total: 1,
    shown: [{ path: "src/lib.rs", hunk: 1, why: "The rename." }],
    omitted: [],
  },
};

function reviewed(n: number, overrides: Partial<DeliveryState> = {}): DeliveryState {
  return makeDelivery({
    id: ulid(700 + n),
    name: `delivery ${n}`,
    review: "accepted",
    demos: [{ kind: "cli_transcript", body: { blob: textBlob() } }],
    ...overrides,
  });
}

function client(): FakeClient {
  const fake = new FakeClient();
  fake.reports.set(DIFF_BLOB.sha256, { mime: "text/plain", text: EXCERPT, sha256: DIFF_BLOB.sha256 });
  return fake;
}

function renderCards(deliveries: DeliveryState[], fake = client()) {
  return render(
    <ClientContext.Provider value={fake}>
      <EvidenceCards project="alpha" deliveries={deliveries} needs={[]} />
    </ClientContext.Provider>,
  );
}

const names = () =>
  [...document.querySelectorAll(".ev .cap-name")].map((element) => element.textContent);

describe("project activity cards", () => {
  it("shows the newest deliveries first, the latest few, with Show all / Show less", async () => {
    const user = userEvent.setup();
    // Oldest first, as the projection hands them over.
    const deliveries = Array.from({ length: 9 }, (_, index) => reviewed(index + 1));
    renderCards(deliveries);

    expect(names()).toEqual(["delivery 9", "delivery 8", "delivery 7", "delivery 6", "delivery 5", "delivery 4"]);
    expect(names()).toHaveLength(EVIDENCE_SHOWN);

    await user.click(screen.getByRole("button", { name: "Show all 9" }));
    expect(names()).toHaveLength(9);
    expect(names().at(-1)).toBe("delivery 1");

    await user.click(screen.getByRole("button", { name: "Show less" }));
    expect(names()).toHaveLength(EVIDENCE_SHOWN);
  });

  it("has no toggle when everything fits, and never folds away a delivery awaiting review", () => {
    const { unmount } = renderCards([reviewed(1), reviewed(2)]);
    expect(screen.queryByRole("button", { name: /Show all/ })).toBeNull();
    unmount();

    // An old pending delivery stays visible beside the six newest.
    const pending = reviewed(0, { name: "still waiting", review: "pending" });
    renderCards([pending, ...Array.from({ length: 8 }, (_, index) => reviewed(index + 1))]);
    expect(names()).toHaveLength(EVIDENCE_SHOWN + 1);
    expect(names().at(-1)).toBe("still waiting");
  });

  it("leads with the human verdict once a delivery is reviewed", () => {
    renderCards([
      reviewed(1, { name: "seen one", criteria: makeDelivery().criteria.map((c) => ({ ...c, provenance: "seen" as const })) }),
      reviewed(2, { name: "agent one" }),
      reviewed(3, { name: "unproven one", criteria: [] }),
      reviewed(4, { name: "sent one", review: "sent_back" }),
      reviewed(5, { name: "waiting one", review: "pending", criteria: [] }),
    ]);
    const meta = (name: string) =>
      screen.getByText(name).closest(".ev")!.querySelector(".cap-meta")!.textContent;
    expect(meta("seen one")).toBe("Accepted · seen");
    expect(meta("agent one")).toBe("Accepted · agent's word");
    expect(meta("unproven one")).toBe("Accepted");
    expect(meta("sent one")).toBe("Sent back");
    // Waiting: how proven it is still leads.
    expect(meta("waiting one")).toBe("Not proven yet · Waiting for you");
  });

  it("words the verdicts in both Chinese locales", () => {
    for (const language of ["zh-Hans", "zh-Hant"] as const) {
      const t = translator(language);
      expect(t("dl.status.accepted_seen")).toMatch(/^已接受 · /);
      expect(t("dl.status.accepted_agent")).toMatch(/^已接受 · /);
      expect(t("ev.all", { n: 9 })).toContain("9");
      expect(t("ev.less")).not.toBe("Show less");
    }
  });

  it("gives a diff delivery a +/- thumbnail that opens the diff full-window", async () => {
    const user = userEvent.setup();
    renderCards([reviewed(1, { name: "diff one", demos: [DIFF_DEMO] })]);
    const thumb = await screen.findByRole("button", { name: "Open the diff: lib.rs" });
    await waitFor(() => expect(thumb.querySelector(".th-diff-row.del")).not.toBeNull());
    expect(thumb.querySelector(".th-diff-file")?.textContent).toBe("lib.rs");
    expect(thumb.querySelector(".th-diff-row.del")?.textContent).toBe("-fn old() {}");
    expect(thumb.querySelector(".th-diff-row.add")?.textContent).toBe("+fn new() {}");

    await user.click(thumb);
    const dialog = screen.getByRole("dialog", { name: "diff one · Diff" });
    expect(await within(dialog).findByText("Core diff · 233cf80")).toBeInTheDocument();
    expect(await within(dialog).findByText("The rename.")).toBeInTheDocument();
    expect(dialog.querySelector(".diff-row.add")).not.toBeNull();
  });

  it("picks a screenshot, then a diff, then a transcript for the thumbnail", () => {
    const transcript: Demo = { kind: "cli_transcript", body: { blob: textBlob() } };
    const shot: Demo = { kind: "screenshot", body: { blob: imageBlob(), alt: "x" } };
    expect(thumbnailDemo(makeDelivery({ demos: [transcript, DIFF_DEMO, shot] }))).toBe(shot);
    expect(thumbnailDemo(makeDelivery({ demos: [transcript, DIFF_DEMO] }))).toBe(DIFF_DEMO);
    expect(thumbnailDemo(makeDelivery({ demos: [transcript] }))).toBe(transcript);
  });

  it("starts the diff thumbnail at the first change", () => {
    const lines = parseHunks(EXCERPT)[0]!.lines;
    expect(thumbLines(lines).map((line) => line.kind)).toEqual(["ctx", "del", "add", "ctx"]);
  });
});

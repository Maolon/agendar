import { useState } from "react";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DeliveryState, Demo, ProjectState } from "../api/types";
import { ClientContext } from "../api/store";
import {
  anchorQuote,
  compareDemos,
  compareDisclosure,
  demoKey,
  placeLabel,
  redeliveryOf,
  sinceRows,
} from "../state/redelivery";
import { Delivery } from "../views/Delivery";
import {
  FakeClient,
  imageBlob,
  makeDelivery,
  makeDeliveryProject,
  makeDeliverySnapshot,
  sha,
  textBlob,
  ulid,
} from "./fixtures";

const transcript = (n: number): Demo => ({ kind: "cli_transcript", body: { blob: textBlob(sha(n)) } });
const shot = (n: number): Demo => ({ kind: "screenshot", body: { blob: imageBlob(sha(n)), alt: "x" } });
const diff = (n: number, commit: string): Demo => ({
  kind: "diff",
  body: { blob: textBlob(sha(n)), commit },
});

describe("redelivery comparisons", () => {
  it("shows a quoted line without its code indentation", () => {
    expect(anchorQuote({ kind: "transcript_line", demo: 0, line: 3, quote: "    let x = 1;  " })).toBe("let x = 1;");
    expect(anchorQuote({ kind: "region", demo: 0, x: 1, y: 1, w: 10, h: 10 })).toBeUndefined();
  });

  it("matches demos by kind, bytes and, for a diff, commit", () => {
    expect(demoKey(transcript(1))).toBe(demoKey(transcript(1)));
    expect(demoKey(transcript(1))).not.toBe(demoKey(transcript(2)));
    expect(demoKey(transcript(1))).not.toBe(demoKey({ ...shot(1), kind: "screenshot" }));
    expect(demoKey(diff(1, "abc"))).not.toBe(demoKey(diff(1, "def")));

    const delta = compareDemos(
      [diff(1, "abc"), transcript(3), shot(4)],
      [diff(1, "abc"), transcript(2), shot(4)],
    );
    expect(delta.marks).toEqual(["same", "new", "same"]);
    expect(delta.dropped).toBe(1);
    // One previous copy matches one current copy.
    expect(compareDemos([shot(4), shot(4)], [shot(4)])).toEqual({ marks: ["same", "new"], dropped: 0 });
  });

  it("compares disclosure columns line by line", () => {
    const delta = compareDisclosure(
      { proven: ["a", "b"], not_proven_yet: [], faked_for_now: ["f"] },
      { proven: ["a"], not_proven_yet: ["c"], faked_for_now: ["f"] },
    );
    expect(delta.proven).toEqual({ added: [1], removed: [] });
    expect(delta.not_proven_yet).toEqual({ added: [], removed: ["c"] });
    expect(delta.faked_for_now).toEqual({ added: [], removed: [] });
  });

  it("pairs the reason and each comment with its reply", () => {
    const note = {
      review: ulid(1),
      at: "2026-02-10T14:00:00.000Z",
      reason: "Too slow",
      comments: [
        { anchor: { kind: "region" as const, demo: 0, x: 0, y: 0, w: 10, h: 10 }, text: "Here" },
        { anchor: { kind: "region" as const, demo: 0, x: 5, y: 5, w: 10, h: 10 }, text: "There" },
      ],
    };
    const rows = sinceRows(note, [{ text: "Cached" }, { comment: 2, text: "Moved" }]);
    expect(rows.map((row) => [row.kind, row.n, row.text, row.reply])).toEqual([
      ["reason", undefined, "Too slow", "Cached"],
      ["comment", 1, "Here", undefined],
      ["comment", 2, "There", "Moved"],
    ]);
    // Without a reason, an overall reply is its own row.
    const bare = sinceRows({ ...note, reason: undefined }, [{ text: "Note" }]);
    expect(bare.at(-1)).toMatchObject({ kind: "note", reply: "Note" });
  });

  it("names places by file line, transcript and screenshot", () => {
    const demos = [shot(1), transcript(2), transcript(3)];
    expect(
      placeLabel({ kind: "diff_line", demo: 0, file: "a/b.rs", side: "new", line: 9, quote: "x" }, demos),
    ).toBe("b.rs:9");
    expect(placeLabel({ kind: "transcript_line", demo: 2, line: 12, quote: "x" }, demos)).toBe(
      "Transcript 2, line 12",
    );
    expect(placeLabel({ kind: "region", demo: 0, x: 0, y: 0, w: 1, h: 1 }, demos)).toBe("Screenshot 1, area");
  });
});

function redeliveryProject(sentBack: boolean): { project: ProjectState; delivery: DeliveryState } {
  const fixture = makeDeliveryProject();
  const previousId = ulid(650);
  const previous = makeDelivery({
    id: previousId,
    name: "search-v1",
    demos: [transcript(0x70), shot(0x82)],
    disclosure: { proven: [], not_proven_yet: ["Old claim"], faked_for_now: ["Alias list is a sample"] },
    superseded_by: fixture.delivery.id,
    ...(sentBack
      ? {
          sent_back: {
            review: ulid(651),
            at: "2026-02-10T14:00:00.000Z",
            reason: "Results are slow",
            comments: [
              {
                anchor: { kind: "transcript_line" as const, demo: 0, line: 2, quote: "2 The Prince" },
                text: "Wrong order",
              },
              {
                anchor: { kind: "region" as const, demo: 1, x: 0, y: 0, w: 100, h: 100 },
                text: "Cut off",
              },
            ],
          },
        }
      : {}),
  });
  const delivery: DeliveryState = {
    ...fixture.delivery,
    supersedes: previousId,
    replies: [{ comment: 1, text: "Sorted by year now" }],
  };
  const project: ProjectState = {
    ...fixture.project,
    deliveries: { [previousId]: previous, [delivery.id]: delivery },
  };
  return { project, delivery };
}

function Harness({
  project,
  delivery,
  client = new FakeClient(makeDeliverySnapshot()),
}: {
  project: ProjectState;
  delivery: DeliveryState;
  client?: FakeClient;
}) {
  const [open, setOpen] = useState(false);
  return (
    <ClientContext.Provider value={client}>
      <Delivery
        project={project}
        delivery={delivery}
        need={makeDeliveryProject().need}
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

describe("redelivery page", () => {
  it("opens on the send-back with replies, marks demos and disclosure, and shows the previous version", async () => {
    const { project, delivery } = redeliveryProject(true);
    render(<Harness project={project} delivery={delivery} />);
    const section = screen.getByRole("region", { name: "Since your send-back" });
    expect(within(section).getByText("Replaces the version you sent back 1h ago")).toBeInTheDocument();
    expect(within(section).getByText("Results are slow")).toBeInTheDocument();
    expect(within(section).getByText("Transcript 1, line 2")).toBeInTheDocument();
    expect(within(section).getByText("2 The Prince")).toBeInTheDocument();
    expect(within(section).getByText("Sorted by year now")).toBeInTheDocument();
    // The reason and comment 2 have no reply.
    expect(within(section).getAllByText("No reply")).toHaveLength(2);

    // The transcript is new; the screenshot is the same bytes and folds.
    expect(screen.getAllByText("New").length).toBeGreaterThan(0);
    const same = screen.getByRole("button", { name: /Same as before/ });
    expect(same).toHaveAttribute("aria-expanded", "false");
    await userEvent.click(same);
    expect(same).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("1 earlier demo dropped")).toBeInTheDocument();
    // Disclosure: "Old claim" was removed, struck through.
    expect(screen.getByText("Old claim").tagName).toBe("DEL");

    await userEvent.click(within(section).getByRole("button", { name: "Previous version" }));
    const dialog = screen.getByRole("dialog", { name: "Previous version: search-v1" });
    expect(within(dialog).queryByRole("button", { name: /Accept/ })).toBeNull();
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("renders as before without a send-back or a superseded delivery", () => {
    const { project, delivery } = redeliveryProject(false);
    expect(redeliveryOf(project, delivery)).toBeNull();
    const { unmount } = render(<Harness project={project} delivery={delivery} />);
    expect(screen.queryByRole("region", { name: "Since your send-back" })).toBeNull();
    expect(screen.queryByText("New")).toBeNull();
    expect(screen.queryByRole("button", { name: /Same as before/ })).toBeNull();
    unmount();
    const plain = makeDeliveryProject();
    render(<Harness project={plain.project} delivery={plain.delivery} />);
    expect(screen.queryByRole("region", { name: "Since your send-back" })).toBeNull();
  });
});

describe("previous version with its send-back comments", () => {
  function setup() {
    const client = new FakeClient(makeDeliverySnapshot());
    client.reports.set(sha(0x70), { mime: "text/plain", text: "1 Dune\n2 The Prince\n3 Emma\n", sha256: sha(0x70) });
    client.reports.set(sha(0x82), { mime: "image/png", media_token: "token-shot", sha256: sha(0x82) });
    const { project, delivery } = redeliveryProject(true);
    render(<Harness project={project} delivery={delivery} client={client} />);
    return within(screen.getByRole("region", { name: "Since your send-back" }));
  }

  afterEach(() => {
    delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView;
  });

  it("shows each comment at its place, read-only", async () => {
    const section = setup();
    await userEvent.click(section.getByRole("button", { name: "Previous version" }));
    const dialog = within(screen.getByRole("dialog", { name: "Previous version: search-v1" }));
    // The transcript line carries its comment under it.
    const note = await dialog.findByRole("note", { name: "Comment 1" });
    expect(note).toHaveTextContent("Wrong order");
    expect(note).not.toHaveClass("hot");
    expect(note.closest(".term-line")).toHaveTextContent("2 The Prince");
    // The screenshot shows its area, and lists the comment under it.
    await waitFor(() => expect(document.querySelector(".rd-prev .cm-area")).not.toBeNull());
    expect(dialog.getByRole("button", { name: /Cut off/ })).toBeInTheDocument();
    // Nothing can be added, edited or removed.
    expect(dialog.queryByRole("button", { name: /^Comment on/ })).toBeNull();
    expect(dialog.queryByRole("button", { name: /^Remove comment/ })).toBeNull();
    expect(dialog.getByRole("button", { name: "Close the previous version" })).toHaveFocus();
  });

  it("opens a line comment scrolled to its line, focused and highlighted", async () => {
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll;
    const section = setup();
    const row = section.getByRole("button", { name: /Wrong order/ });
    expect(row).toHaveAccessibleDescription("Show on the previous version");
    await userEvent.click(row);
    const dialog = within(screen.getByRole("dialog", { name: "Previous version: search-v1" }));
    const note = await dialog.findByRole("note", { name: "Comment 1" });
    expect(note).toHaveClass("hot");
    expect(note).toHaveFocus();
    expect(scroll).toHaveBeenCalledWith({ block: "center" });
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(row).toHaveFocus();
  });

  it("opens an area comment in the viewer with its pin selected", async () => {
    const section = setup();
    section.getByRole("button", { name: /Cut off/ }).focus();
    await userEvent.keyboard("{Enter}");
    const viewer = within(await screen.findByRole("dialog", { name: "Previous version: x" }));
    expect(viewer.getByRole("button", { name: "Comment 2", pressed: true })).toBeInTheDocument();
    expect(viewer.getByRole("group", { name: "Comment 2" })).toHaveTextContent("Cut off");
    // Read-only: no Mark tool, no Edit or Delete.
    expect(viewer.queryByRole("button", { name: "Mark" })).toBeNull();
    expect(viewer.queryByRole("button", { name: "Delete" })).toBeNull();
    // Escape deselects, then closes the viewer; the previous version stays.
    await userEvent.keyboard("{Escape}{Escape}");
    expect(screen.queryByRole("dialog", { name: "Previous version: x" })).toBeNull();
    const dialog = within(screen.getByRole("dialog", { name: "Previous version: search-v1" }));
    // From there the comment under the screenshot opens the viewer again.
    await userEvent.click(dialog.getByRole("button", { name: /Cut off/ }));
    expect(await screen.findByRole("dialog", { name: "Previous version: x" })).toBeInTheDocument();
  });
});

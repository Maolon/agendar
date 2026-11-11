import { useState } from "react";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DeliveryState, Demo, ReviewDeliveryRequest } from "../api/types";
import { ClientContext } from "../api/store";
import { clipQuote, regionFrom } from "../state/comments";
import { Delivery } from "../views/Delivery";
import { FakeClient, imageBlob, makeDeliveryProject, makeDeliverySnapshot, sha, textBlob } from "./fixtures";

const DIFF_BLOB = textBlob(sha(0xd3), "txt");
const LOG_BLOB = textBlob(sha(0xd4), "txt");
const SHOT_BLOB = imageBlob(sha(0xd5));

const EXCERPT = [
  "diff --git a/crates/core/src/validate.rs b/crates/core/src/validate.rs",
  "--- a/crates/core/src/validate.rs",
  "+++ b/crates/core/src/validate.rs",
  "@@ -838,2 +838,2 @@ fn check()",
  "     let a = 1;",
  "-    let b = 2;",
  "+    let b = 3;",
  "",
].join("\n");

const DEMOS: Demo[] = [
  {
    kind: "diff",
    body: {
      blob: DIFF_BLOB,
      commit: "233cf80bebc689ffa7ca698deff2d95d16437dad",
      files_total: 1,
      hunks_total: 1,
      added_total: 1,
      removed_total: 1,
      shown: [{ path: "crates/core/src/validate.rs", hunk: 1, why: "The bound." }],
      omitted: [],
    },
  },
  { kind: "cli_transcript", body: { blob: LOG_BLOB } },
  { kind: "screenshot", body: { blob: SHOT_BLOB, alt: "Search results" } },
];

function setup() {
  const client = new FakeClient(makeDeliverySnapshot());
  client.reports.set(DIFF_BLOB.sha256, { mime: "text/plain", text: EXCERPT, sha256: DIFF_BLOB.sha256 });
  client.reports.set(LOG_BLOB.sha256, {
    mime: "text/plain",
    text: "$ shelf search\n4 books\nok\n",
    sha256: LOG_BLOB.sha256,
  });
  client.reports.set(SHOT_BLOB.sha256, { mime: "image/png", media_token: "token-shot", sha256: SHOT_BLOB.sha256 });
  const fixture = makeDeliveryProject();
  const delivery: DeliveryState = { ...fixture.delivery, demos: DEMOS };
  fixture.project.deliveries[delivery.id] = delivery;
  function Harness() {
    const [open, setOpen] = useState(false);
    return (
      <ClientContext.Provider value={client}>
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
  render(<Harness />);
  return { client, delivery };
}

afterEach(() => {
  vi.restoreAllMocks();
});

async function addLineComment(name: string, text: string): Promise<void> {
  await userEvent.click(await screen.findByRole("button", { name }));
  const box = screen.getByRole("textbox", { name: new RegExp(`^Comment on`) });
  expect(box).toHaveFocus();
  await userEvent.type(box, `${text}{Enter}`);
}

describe("anchored send-back comments", () => {
  it("pins a diff line, a transcript line and a screenshot area, then sends them back without a reason", async () => {
    const { client, delivery } = setup();

    // A diff line: the "+" on the new side of line 839.
    await addLineComment("Comment on validate.rs:839", "Why 3?");
    // Esc cancels an open box without saving.
    await userEvent.click(screen.getByRole("button", { name: "Comment on validate.rs:838" }));
    await userEvent.type(screen.getByRole("textbox", { name: /^Comment on/ }), "draft{Escape}");
    expect(screen.queryByRole("textbox", { name: /^Comment on/ })).not.toBeInTheDocument();

    // A transcript line, rendered line by line.
    await addLineComment("Comment on Transcript line 2", "Show the titles");

    // A screenshot area: open it full screen, then drag a rectangle there.
    await userEvent.click(await screen.findByRole("button", { name: "Open Search results full screen" }));
    const viewer = screen.getByRole("dialog", { name: "Search results" });
    const image = await within(viewer).findByRole("img", { name: "Search results" });
    const canvas = image.parentElement as HTMLElement;
    vi.spyOn(image, "getBoundingClientRect").mockReturnValue({
      left: 0,
      top: 0,
      width: 200,
      height: 100,
      right: 200,
      bottom: 100,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    fireEvent.pointerDown(canvas, { button: 0, clientX: 20, clientY: 10, pointerId: 1 });
    fireEvent.pointerMove(canvas, { clientX: 80, clientY: 30, pointerId: 1 });
    fireEvent.pointerUp(canvas, { clientX: 80, clientY: 30, pointerId: 1 });
    fireEvent.click(canvas);
    const areaBox = within(viewer).getByRole("textbox", { name: "Comment on an area of Search results" });
    await userEvent.type(areaBox, "Clipped{Enter}");
    // The pin shows full screen at once; then the viewer closes.
    expect(within(viewer).getByText("3", { selector: ".cm-area .cm-pin" })).toBeInTheDocument();
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    // Numbered pins at each place, the area one on the inline preview too.
    expect(screen.getAllByText("1", { selector: ".cm-pins .cm-pin" })).toHaveLength(1);
    expect(screen.getByText("2", { selector: ".cm-pins .cm-pin" })).toHaveAttribute("title", "Show the titles");
    expect(screen.getByText("3", { selector: ".cm-area .cm-pin" })).toBeInTheDocument();

    // Accept is held back while comments exist.
    expect(screen.getByRole("button", { name: /^Accept/ })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: /Send back with 3 comments/ }));

    // The send-back panel lists each comment with its place.
    const list = screen.getByRole("list", { name: "Comments" });
    const items = within(list).getAllByRole("listitem");
    expect(items.map((item) => item.querySelector(".cm-loc")?.textContent)).toEqual([
      "validate.rs:839",
      "Transcript line 2",
      "Screenshot 1, area",
    ]);

    // The reason is optional with comments.
    await userEvent.click(screen.getByRole("button", { name: "Send back with 3 comments" }));
    await waitFor(() => expect(client.order).toContain("review_delivery"));
    const review = client.calls.find((call) => call.command === "review_delivery")
      ?.request as ReviewDeliveryRequest;
    expect(review.verdict).toBe("send_back");
    expect(review.reason).toBeUndefined();
    expect(review.delivery_id).toBe(delivery.id);
    expect(review.comments).toEqual([
      {
        anchor: {
          kind: "diff_line",
          demo: 0,
          file: "crates/core/src/validate.rs",
          side: "new",
          line: 839,
          quote: "    let b = 3;",
        },
        text: "Why 3?",
      },
      { anchor: { kind: "transcript_line", demo: 1, line: 2, quote: "4 books" }, text: "Show the titles" },
      { anchor: { kind: "region", demo: 2, x: 100, y: 100, w: 300, h: 200 }, text: "Clipped" },
    ]);
  });

  it("removes a comment from the list and needs a reason again without any", async () => {
    const { client } = setup();
    await addLineComment("Comment on validate.rs:839", "Why 3?");
    // A deleted line comments the old side.
    await addLineComment("Comment on validate.rs:839 (old)", "Keep 2");
    await userEvent.click(screen.getByRole("button", { name: /^Send back with 2 comments/ }));
    await userEvent.click(screen.getByRole("button", { name: "Remove comment 1" }));
    expect(screen.getByRole("button", { name: "Send back with 1 comment" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Remove comment 1" }));
    expect(screen.queryByRole("list", { name: "Comments" })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Send back" }));
    expect(screen.getByRole("alert")).toBeInTheDocument();
    expect(client.order).not.toContain("review_delivery");
  });

  it("edits and deletes an area comment in the viewer, and the page's list follows", async () => {
    setup();
    await addLineComment("Comment on validate.rs:839", "Why 3?");
    await userEvent.click(await screen.findByRole("button", { name: "Open Search results full screen" }));
    const viewer = screen.getByRole("dialog", { name: "Search results" });
    const image = await within(viewer).findByRole("img", { name: "Search results" });
    vi.spyOn(image, "getBoundingClientRect").mockReturnValue({
      left: 0,
      top: 0,
      width: 200,
      height: 100,
      right: 200,
      bottom: 100,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    const stage = viewer.querySelector<HTMLElement>(".mv-port")!;
    for (const [text, from, to] of [
      ["First", [20, 10], [80, 30]],
      ["Second", [100, 50], [150, 90]],
    ] as const) {
      fireEvent.pointerDown(stage, { button: 0, clientX: from[0], clientY: from[1], pointerId: 1 });
      fireEvent.pointerMove(stage, { clientX: to[0], clientY: to[1], pointerId: 1 });
      fireEvent.pointerUp(stage, { clientX: to[0], clientY: to[1], pointerId: 1 });
      await userEvent.type(within(viewer).getByRole("textbox"), `${text}{Enter}`);
    }

    // Pin 2 → popover → Edit.
    await userEvent.click(within(viewer).getByRole("button", { name: "Comment 2", pressed: false }));
    const popover = within(viewer).getByRole("group", { name: "Comment 2" });
    expect(within(popover).getByText("First")).toBeInTheDocument();
    await userEvent.click(within(popover).getByRole("button", { name: "Edit" }));
    const field = within(viewer).getByRole("textbox", { name: "Edit comment 2" });
    expect(field).toHaveValue("First");
    await userEvent.clear(field);
    await userEvent.type(field, "Cut off{Enter}");
    expect(within(viewer).getByRole("group", { name: "Comment 2" })).toHaveTextContent("Cut off");

    // Delete it: the next one becomes 2, on the page too.
    await userEvent.click(within(viewer).getByRole("button", { name: "Delete" }));
    expect(within(viewer).queryByRole("group", { name: "Comment 2" })).toBeNull();
    expect(within(viewer).getAllByText("2", { selector: ".cm-area .cm-pin" })).toHaveLength(1);
    expect(within(viewer).queryByText("3", { selector: ".cm-area .cm-pin" })).toBeNull();
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await userEvent.click(screen.getByRole("button", { name: /Send back with 2 comments/ }));
    const list = screen.getByRole("list", { name: "Comments" });
    expect(within(list).getAllByRole("listitem").map((item) => item.querySelector(".cm-text")?.textContent)).toEqual([
      "Why 3?",
      "Second",
    ]);
  });

  it("makes per-mille regions and short quotes", () => {
    expect(regionFrom({ x: 0.5, y: 0.5 }, { x: 0.1, y: 0.2 })).toEqual({ x: 100, y: 200, w: 400, h: 300 });
    expect(regionFrom({ x: 1.2, y: -1 }, { x: 1.2, y: -1 })).toEqual({ x: 999, y: 0, w: 1, h: 1 });
    const long = clipQuote("x".repeat(250));
    expect(Array.from(long)).toHaveLength(200);
    expect(long.endsWith("…")).toBe(true);
  });
});

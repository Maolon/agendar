import { useState } from "react";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Demo, ReviewAnchor, ReviewComment } from "../api/types";
import { ClientContext } from "../api/store";
import { DemoView } from "../components/Demo";
import type { DemoComments } from "../state/comments";
import { FakeClient, imageBlob, sha } from "./fixtures";

const SHOT = sha(0xe1);
const DEMO: Demo = { kind: "screenshot", body: { blob: imageBlob(SHOT), alt: "Search results" } };
const OLD_AREA = { kind: "region", demo: 2, x: 500, y: 500, w: 100, h: 100 } as const;
const EXISTING: ReviewComment = { anchor: OLD_AREA, text: "Old" };

function setup({
  enabled = true,
  editable,
  existing = [],
}: { enabled?: boolean; editable?: boolean; existing?: ReviewComment[] } = {}) {
  const client = new FakeClient();
  client.reports.set(SHOT, { mime: "image/png", media_token: "token-shot", sha256: SHOT });
  const onAdd = vi.fn<(anchor: ReviewAnchor, text: string) => void>();
  const page: { list: ReviewComment[] } = { list: existing };
  function Harness() {
    const [list, setList] = useState<ReviewComment[]>(existing);
    page.list = list;
    // Page-wide numbers: two comments elsewhere come first (indexes 0 and 1).
    const comments: DemoComments = {
      demo: 2,
      enabled,
      editable: editable ?? enabled,
      pins: list.map((comment, at) => ({ n: at + 3, comment })),
      onAdd: (anchor, text) => {
        onAdd(anchor, text);
        setList((value) => [...value, { anchor, text }]);
      },
      onRemove: (index) => setList((value) => value.filter((_, at) => at + 2 !== index)),
      onEdit: (index, text) =>
        setList((value) => value.map((comment, at) => (at + 2 === index ? { ...comment, text } : comment))),
    };
    return (
      <ClientContext.Provider value={client}>
        <DemoView project="alpha" demo={DEMO} comments={comments} />
      </ClientContext.Provider>
    );
  }
  render(<Harness />);
  return { onAdd, page };
}

const rect = (left: number, top: number, width: number, height: number) => ({
  left,
  top,
  width,
  height,
  right: left + width,
  bottom: top + height,
  x: left,
  y: top,
  toJSON: () => ({}),
});

const BOX = rect(0, 0, 200, 100);

async function openViewer() {
  const user = userEvent.setup();
  const inline = await screen.findByRole("button", { name: "Open Search results full screen" });
  await user.click(inline);
  const viewer = screen.getByRole("dialog", { name: "Search results" });
  const image = await within(viewer).findByRole("img", { name: "Search results" });
  vi.spyOn(image, "getBoundingClientRect").mockReturnValue(BOX);
  const stage = viewer.querySelector<HTMLElement>(".mv-port")!;
  return { user, inline, viewer, image, stage };
}

/** Gives the stage and the image a size, as a real layout and load would. */
function measure(stage: HTMLElement, image: HTMLElement, natural = { w: 1600, h: 800 }, box = { w: 800, h: 400 }) {
  vi.spyOn(stage, "getBoundingClientRect").mockReturnValue(rect(0, 0, box.w, box.h));
  Object.defineProperty(image, "naturalWidth", { value: natural.w, configurable: true });
  Object.defineProperty(image, "naturalHeight", { value: natural.h, configurable: true });
  fireEvent.load(image);
}

function canvasOf(viewer: HTMLElement): HTMLElement {
  return viewer.querySelector<HTMLElement>(".mv-canvas")!;
}

function drag(target: HTMLElement, from: [number, number], to: [number, number]): void {
  fireEvent.pointerDown(target, { button: 0, clientX: from[0], clientY: from[1], pointerId: 1 });
  fireEvent.pointerMove(target, { clientX: to[0], clientY: to[1], pointerId: 1 });
  fireEvent.pointerUp(target, { clientX: to[0], clientY: to[1], pointerId: 1 });
  fireEvent.click(target);
}

function tap(target: HTMLElement, at: [number, number]): void {
  fireEvent.pointerDown(target, { button: 0, clientX: at[0], clientY: at[1], pointerId: 1 });
  fireEvent.pointerUp(target, { button: 0, clientX: at[0], clientY: at[1], pointerId: 1 });
  fireEvent.click(target);
}

function tool(viewer: HTMLElement, name: string): HTMLElement {
  return within(within(viewer).getByRole("toolbar", { name: "Image tools" })).getByRole("button", { name });
}

function zoomText(viewer: HTMLElement): string | null {
  return within(viewer).getByRole("status", { name: "Zoom" }).textContent;
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("screenshot viewer: marks", () => {
  it("opens full screen on click; the inline image does not draw", async () => {
    setup();
    const inline = await screen.findByRole("button", { name: "Open Search results full screen" });
    expect(screen.getByText("Click to view full screen and mark areas.")).toBeInTheDocument();
    const preview = within(inline).getByRole("img");
    vi.spyOn(preview, "getBoundingClientRect").mockReturnValue(BOX);
    fireEvent.pointerDown(preview, { button: 0, clientX: 20, clientY: 10, pointerId: 1 });
    fireEvent.pointerMove(preview, { clientX: 80, clientY: 30, pointerId: 1 });
    fireEvent.pointerUp(preview, { clientX: 80, clientY: 30, pointerId: 1 });
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(document.querySelector(".cm-area")).toBeNull();

    fireEvent.click(inline);
    const viewer = screen.getByRole("dialog", { name: "Search results" });
    const image = await within(viewer).findByRole("img", { name: "Search results" });
    expect(image.getAttribute("src")).toContain("token-shot");
    // The stage has focus, so its keys work at once; Mark is the tool.
    expect(viewer.querySelector(".mv-port")).toHaveFocus();
    expect(tool(viewer, "Mark")).toHaveAttribute("aria-pressed", "true");
    expect(tool(viewer, "Mark")).toHaveAttribute("title", expect.stringMatching(/^Mark \((M)\)$/));
  });

  it("draws a region and saves it as a per-mille anchor; pins update live", async () => {
    const { onAdd } = setup({ existing: [EXISTING] });
    const { user, viewer, stage } = await openViewer();
    expect(within(viewer).getByText("3", { selector: ".cm-area .cm-pin" })).toBeInTheDocument();

    drag(stage, [20, 10], [80, 30]);
    const box = within(viewer).getByRole("textbox", { name: "Comment on an area of Search results" });
    expect(box).toHaveFocus();
    // Keys act on the box: arrows, tool letters and Enter do not reach the viewer.
    fireEvent.keyDown(box, { key: "ArrowLeft" });
    await user.type(box, "hm Clipped{Enter}");

    expect(onAdd).toHaveBeenCalledWith({ kind: "region", demo: 2, x: 100, y: 100, w: 300, h: 200 }, "hm Clipped");
    expect(tool(viewer, "Mark")).toHaveAttribute("aria-pressed", "true");
    expect(within(viewer).queryByRole("textbox")).toBeNull();
    expect(within(viewer).getByText("4", { selector: ".cm-area .cm-pin" })).toBeInTheDocument();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("gives a region drawn at 300% the same anchor as at Fit", async () => {
    const { onAdd } = setup();
    const { user, viewer, image, stage } = await openViewer();
    measure(stage, image, { w: 1200, h: 600 }, { w: 400, h: 200 });
    expect(zoomText(viewer)).toBe("33%");
    drag(stage, [20, 10], [80, 30]);
    await user.type(within(viewer).getByRole("textbox"), "Fit{Enter}");

    // ⌘1: actual size, three times the fit.
    fireEvent.keyDown(stage, { key: "1", metaKey: true });
    expect(zoomText(viewer)).toBe("100%");
    const canvas = canvasOf(viewer);
    expect(canvas.style.width).toBe("1200px");
    vi.spyOn(image, "getBoundingClientRect").mockReturnValue(
      rect(parseFloat(canvas.style.left), parseFloat(canvas.style.top), 1200, 600),
    );
    const left = parseFloat(canvas.style.left);
    const top = parseFloat(canvas.style.top);
    drag(stage, [left + 120, top + 60], [left + 480, top + 180]);
    await user.type(within(viewer).getByRole("textbox"), "Zoomed{Enter}");
    expect(onAdd.mock.calls.map(([anchor]) => anchor)).toEqual([
      { kind: "region", demo: 2, x: 100, y: 100, w: 300, h: 200 },
      { kind: "region", demo: 2, x: 100, y: 100, w: 300, h: 200 },
    ]);
  });

  it("zooms 2× at a double-clicked point, back to Fit on the next; a single click never zooms", async () => {
    setup();
    const { viewer, image, stage } = await openViewer();
    measure(stage, image);
    const canvas = canvasOf(viewer);
    expect(zoomText(viewer)).toBe("50%");
    tap(stage, [200, 100]);
    expect(zoomText(viewer)).toBe("50%");

    fireEvent.doubleClick(stage, { clientX: 200, clientY: 100 });
    expect(zoomText(viewer)).toBe("100%");
    // The image point under the pointer stays there.
    expect(canvas.style).toMatchObject({ left: "-200px", top: "-100px", width: "1600px" });
    fireEvent.doubleClick(stage, { clientX: 600, clientY: 300 });
    expect(zoomText(viewer)).toBe("50%");
    expect(canvas.style).toMatchObject({ left: "0px", top: "0px", width: "800px" });

    // Ctrl + wheel zooms at the pointer; the toolbar buttons step and reset.
    fireEvent.wheel(stage, { deltaY: -50, ctrlKey: true, clientX: 0, clientY: 0 });
    expect(zoomText(viewer)).toBe("82%");
    expect(canvas.style.left).toBe("0px");
    await userEvent.click(tool(viewer, "Fit to window"));
    expect(zoomText(viewer)).toBe("50%");
    await userEvent.click(tool(viewer, "Zoom in"));
    expect(zoomText(viewer)).toBe("63%");
    await userEvent.click(tool(viewer, "Actual size"));
    expect(zoomText(viewer)).toBe("100%");
    // Plain wheel pans when zoomed; Shift+wheel pans across.
    fireEvent.wheel(stage, { deltaY: 40 });
    fireEvent.wheel(stage, { deltaY: 30, shiftKey: true });
    expect(canvas.style).toMatchObject({ left: "-430px", top: "-240px" });
  });

  it("switches tools with M and H, pans with Hand or a held Space, and ignores the letters while typing", async () => {
    setup();
    const { user, viewer, image, stage } = await openViewer();
    measure(stage, image);
    fireEvent.keyDown(stage, { key: "1", ctrlKey: true });
    const canvas = canvasOf(viewer);
    expect(canvas.style.left).toBe("-400px");

    fireEvent.keyDown(stage, { key: "h" });
    expect(tool(viewer, "Hand")).toHaveAttribute("aria-pressed", "true");
    drag(stage, [300, 200], [350, 230]);
    expect(within(viewer).queryByRole("textbox")).toBeNull();
    expect(canvas.style).toMatchObject({ left: "-350px", top: "-170px" });
    fireEvent.keyDown(stage, { key: "m" });
    expect(tool(viewer, "Mark")).toHaveAttribute("aria-pressed", "true");

    // Space held: Hand until released, then Mark again.
    fireEvent.keyDown(stage, { key: " " });
    expect(tool(viewer, "Hand")).toHaveAttribute("aria-pressed", "true");
    drag(stage, [300, 200], [250, 200]);
    expect(canvas.style.left).toBe("-400px");
    expect(within(viewer).queryByRole("textbox")).toBeNull();
    fireEvent.keyUp(stage, { key: " " });
    expect(tool(viewer, "Mark")).toHaveAttribute("aria-pressed", "true");

    // The toolbar: a click picks the tool; ↓ moves between its buttons.
    await user.click(tool(viewer, "Hand"));
    expect(tool(viewer, "Hand")).toHaveAttribute("aria-pressed", "true");
    tool(viewer, "Hand").focus();
    fireEvent.keyDown(tool(viewer, "Hand"), { key: "ArrowDown" });
    expect(tool(viewer, "Zoom in")).toHaveFocus();
  });

  it("selects a pin: popover, Backspace deletes, Escape deselects then closes", async () => {
    const { page } = setup({ existing: [EXISTING, { anchor: { ...OLD_AREA, x: 0, y: 0 }, text: "Top" }] });
    const { viewer, stage, inline } = await openViewer();
    // A click inside an area selects it.
    tap(stage, [110, 55]);
    expect(within(viewer).getByRole("group", { name: "Comment 3" })).toHaveTextContent("Old");
    fireEvent.keyDown(stage, { key: "Escape" });
    expect(within(viewer).queryByRole("group")).toBeNull();
    expect(screen.getByRole("dialog")).toBeInTheDocument();

    await userEvent.click(within(viewer).getByRole("button", { name: "Comment 4", pressed: false }));
    expect(within(viewer).getByRole("group", { name: "Comment 4" })).toHaveTextContent("Top");
    fireEvent.keyDown(stage, { key: "Backspace" });
    expect(page.list.map((comment) => comment.text)).toEqual(["Old"]);
    expect(within(viewer).queryByRole("group")).toBeNull();

    fireEvent.keyDown(stage, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(inline).toHaveFocus();
  });

  it("lists this image's comments: a row selects, × removes", async () => {
    const { page } = setup({ existing: [EXISTING, { ...EXISTING, text: "Twin" }] });
    const { user, viewer } = await openViewer();
    const list = within(viewer).getByRole("complementary", { name: "Comments" });
    expect(within(list).getAllByRole("listitem").map((row) => row.textContent)).toEqual(["3Old", "4Twin"]);

    await user.click(within(list).getByRole("button", { name: "Comment 4" }));
    expect(within(viewer).getByRole("group", { name: "Comment 4" })).toBeInTheDocument();
    await user.click(within(list).getByRole("button", { name: "Remove comment 3" }));
    expect(page.list.map((comment) => comment.text)).toEqual(["Twin"]);
    // Numbers re-flow as on the page.
    expect(within(list).getAllByRole("listitem").map((row) => row.textContent)).toEqual(["3Twin"]);

    await user.click(within(list).getByRole("button", { name: "Hide comments" }));
    expect(within(list).queryByRole("listitem")).toBeNull();
  });

  it("undoes and redoes the marks added in this session", async () => {
    const { page } = setup({ existing: [EXISTING] });
    const { user, viewer, stage } = await openViewer();
    expect(tool(viewer, "Undo")).toBeDisabled();
    drag(stage, [20, 10], [80, 30]);
    await user.type(within(viewer).getByRole("textbox"), "One{Enter}");
    drag(stage, [100, 50], [150, 90]);
    await user.type(within(viewer).getByRole("textbox"), "Two{Enter}");
    expect(page.list.map((comment) => comment.text)).toEqual(["Old", "One", "Two"]);

    // ⌘Z in a comment box undoes typing, not a mark.
    drag(stage, [10, 10], [40, 40]);
    const box = within(viewer).getByRole("textbox");
    fireEvent.keyDown(box, { key: "z", metaKey: true });
    expect(page.list).toHaveLength(3);
    fireEvent.keyDown(box, { key: "Escape" });

    fireEvent.keyDown(stage, { key: "z", metaKey: true });
    expect(page.list.map((comment) => comment.text)).toEqual(["Old", "One"]);
    fireEvent.keyDown(stage, { key: "z", ctrlKey: true, shiftKey: true });
    expect(page.list.map((comment) => comment.text)).toEqual(["Old", "One", "Two"]);
    await user.click(tool(viewer, "Undo"));
    await user.click(tool(viewer, "Undo"));
    expect(page.list.map((comment) => comment.text)).toEqual(["Old"]);
    // Marks from before the session stay.
    expect(tool(viewer, "Undo")).toBeDisabled();
  });

  it("comments on the whole image from the toolbar and from the page", async () => {
    const { onAdd } = setup();
    const { user, viewer, inline } = await openViewer();
    await user.click(tool(viewer, "Comment on image"));
    await user.type(within(viewer).getByRole("textbox"), "Too dark{Enter}");
    expect(onAdd).toHaveBeenLastCalledWith({ kind: "region", demo: 2, x: 0, y: 0, w: 1000, h: 1000 }, "Too dark");

    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(inline).toHaveFocus();

    // The page's "Comment on image" opens the viewer with the box focused.
    await user.click(screen.getByRole("button", { name: "Comment on image" }));
    const box = within(screen.getByRole("dialog")).getByRole("textbox");
    expect(box).toHaveFocus();
    await user.type(box, "Crop it{Enter}");
    expect(onAdd).toHaveBeenLastCalledWith({ kind: "region", demo: 2, x: 0, y: 0, w: 1000, h: 1000 }, "Crop it");
  });

  it("closes the box first on Escape or a click outside, then the viewer, and returns focus", async () => {
    const { onAdd } = setup();
    const { user, viewer, stage, inline } = await openViewer();
    drag(stage, [20, 10], [80, 30]);
    await user.type(within(viewer).getByRole("textbox"), "draft");
    await user.keyboard("{Escape}");
    expect(within(viewer).queryByRole("textbox")).toBeNull();
    expect(screen.getByRole("dialog")).toBeInTheDocument();

    drag(stage, [20, 10], [80, 30]);
    await user.click(document.querySelector(".mv-backdrop")!);
    expect(within(viewer).queryByRole("textbox")).toBeNull();
    expect(screen.getByRole("dialog")).toBeInTheDocument();

    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(onAdd).not.toHaveBeenCalled();
    expect(inline).toHaveFocus();
  });

  it("disables Mark with the reason at the comment limit, and hides it when the delivery is not live", async () => {
    setup({ enabled: false, editable: true, existing: [EXISTING] });
    const { viewer, stage } = await openViewer();
    expect(tool(viewer, "Mark")).toHaveAttribute("aria-disabled", "true");
    await userEvent.click(tool(viewer, "Mark"));
    fireEvent.keyDown(stage, { key: "m" });
    expect(tool(viewer, "Mark")).toHaveAttribute("title", "20 comments at most");
    expect(tool(viewer, "Hand")).toHaveAttribute("aria-pressed", "true");
    drag(stage, [20, 10], [80, 30]);
    expect(within(viewer).queryByRole("textbox")).toBeNull();
    // Removing still works at the limit.
    expect(within(viewer).getByRole("button", { name: "Remove comment 3" })).toBeInTheDocument();
  });

  it("does not draw or edit when the delivery is not live; pins still show", async () => {
    setup({ enabled: false, existing: [EXISTING] });
    expect(await screen.findByText("Click to view full screen.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Comment on image" })).toBeNull();
    const { viewer, stage } = await openViewer();
    const tools = within(viewer).getByRole("toolbar", { name: "Image tools" });
    expect(within(tools).queryByRole("button", { name: "Mark" })).toBeNull();
    expect(within(tools).queryByRole("button", { name: "Undo" })).toBeNull();
    expect(within(tools).queryByRole("button", { name: "Comment on image" })).toBeNull();
    expect(within(viewer).getByText("3", { selector: ".cm-area .cm-pin" })).toBeInTheDocument();
    drag(stage, [20, 10], [80, 30]);
    expect(within(viewer).queryByRole("textbox")).toBeNull();
    expect(within(viewer).queryByRole("button", { name: "Remove comment 3" })).toBeNull();
    tap(stage, [110, 55]);
    const popover = within(viewer).getByRole("group", { name: "Comment 3" });
    expect(within(popover).queryByRole("button", { name: "Delete" })).toBeNull();
  });
});

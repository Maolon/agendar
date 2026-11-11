import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { DeliveryState } from "../api/types";
import { ClientContext } from "../api/store";
import { EvidenceCard } from "../components/EvidenceCard";
import { ReportCards } from "../components/Reports";
import { FakeClient, imageBlob, makeDelivery, sha, textBlob } from "./fixtures";

const SHOT_A = sha(0xb1);
const SHOT_B = sha(0xb2);
const LOG = sha(0xb3);

function shots(): DeliveryState {
  return makeDelivery({
    demos: [
      { kind: "screenshot", body: { blob: imageBlob(SHOT_A), alt: "Search results" } },
      { kind: "cli_transcript", body: { blob: textBlob(LOG) } },
      { kind: "screenshot", body: { blob: imageBlob(SHOT_B), alt: "Empty state" } },
    ],
  });
}

function renderCard(client: FakeClient, delivery: DeliveryState) {
  render(
    <ClientContext.Provider value={client}>
      <EvidenceCard project="alpha" delivery={delivery} />
    </ClientContext.Provider>,
  );
}

function imageClient(): FakeClient {
  const client = new FakeClient();
  client.reports.set(SHOT_A, { mime: "image/png", media_token: "token-a", sha256: SHOT_A });
  client.reports.set(SHOT_B, { mime: "image/png", media_token: "token-b", sha256: SHOT_B });
  return client;
}

describe("evidence card viewer", () => {
  it("opens a screenshot full-window, toggles actual size, and returns focus on Escape", async () => {
    const user = userEvent.setup();
    renderCard(imageClient(), shots());
    const thumb = await screen.findByRole("button", { name: "Open screenshot full size: Search results" });
    await user.click(thumb);

    const dialog = screen.getByRole("dialog", { name: "Search results" });
    // Full window: rendered outside the card.
    expect(dialog.closest(".ev")).toBeNull();
    const image = await within(dialog).findByRole("img", { name: "Search results" });
    expect(image.getAttribute("src")).toContain("token-a");
    // The image stage has focus; Activity offers Hand and zoom, no Mark, Undo or list.
    const stage = dialog.querySelector<HTMLElement>(".mv-port")!;
    expect(stage).toHaveFocus();
    const tools = within(dialog).getByRole("toolbar", { name: "Image tools" });
    expect(within(tools).getByRole("button", { name: "Hand" })).toHaveAttribute("aria-pressed", "true");
    expect(within(tools).queryByRole("button", { name: "Mark" })).toBeNull();
    expect(within(tools).queryByRole("button", { name: "Undo" })).toBeNull();
    expect(within(dialog).queryByRole("complementary")).toBeNull();

    // Same zoom model: measured, then Actual size and Fit.
    vi.spyOn(stage, "getBoundingClientRect").mockReturnValue({
      left: 0, top: 0, width: 800, height: 400, right: 800, bottom: 400, x: 0, y: 0, toJSON: () => ({}),
    });
    Object.defineProperty(image, "naturalWidth", { value: 1600, configurable: true });
    Object.defineProperty(image, "naturalHeight", { value: 800, configurable: true });
    fireEvent.load(image);
    const zoom = within(tools).getByRole("status", { name: "Zoom" });
    expect(zoom).toHaveTextContent("50%");
    await user.click(within(tools).getByRole("button", { name: "Actual size" }));
    expect(zoom).toHaveTextContent("100%");
    fireEvent.keyDown(stage, { key: "0", metaKey: true });
    expect(zoom).toHaveTextContent("50%");
    // M does nothing without marks.
    fireEvent.keyDown(stage, { key: "m" });
    expect(within(tools).getByRole("button", { name: "Hand" })).toHaveAttribute("aria-pressed", "true");

    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.getByRole("button", { name: "Open screenshot full size: Search results" })).toHaveFocus();
  });

  it("steps through every screenshot with the arrow keys and closes on a backdrop click", async () => {
    const user = userEvent.setup();
    renderCard(imageClient(), shots());
    await user.click(await screen.findByRole("button", { name: /Open screenshot full size/ }));
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByText("1 / 2")).toBeInTheDocument();

    fireEvent.keyDown(document.activeElement!, { key: "ArrowRight" });
    expect(screen.getByRole("dialog")).toHaveAccessibleName("Empty state");
    const second = await within(screen.getByRole("dialog")).findByRole("img", { name: "Empty state" });
    expect(second.getAttribute("src")).toContain("token-b");
    expect(within(screen.getByRole("dialog")).getByText("2 / 2")).toBeInTheDocument();

    fireEvent.keyDown(document.activeElement!, { key: "ArrowLeft" });
    expect(screen.getByRole("dialog")).toHaveAccessibleName("Search results");

    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Next" }));
    expect(screen.getByRole("dialog")).toHaveAccessibleName("Empty state");

    await user.click(document.querySelector(".mv-backdrop")!);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("reads a whole transcript in the reader, not inside the card", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const text = ["$ agendar check", "one", "two", "three", "four", "five: done"].join("\n");
    client.reports.set(LOG, { mime: "text/plain", text, sha256: LOG });
    renderCard(
      client,
      makeDelivery({ demos: [{ kind: "cli_transcript", body: { blob: textBlob(LOG) } }] }),
    );
    const thumb = await screen.findByRole("button", { name: "Open the full transcript" });
    await waitFor(() => expect(thumb).toHaveTextContent("one"));
    // The card keeps its three-line preview.
    expect(thumb).not.toHaveTextContent("five: done");

    await user.click(thumb);
    const dialog = screen.getByRole("dialog", { name: "search-v2 · Transcript" });
    const pre = await waitFor(() => {
      const node = dialog.querySelector("pre.mv-text");
      expect(node).not.toBeNull();
      return node!;
    });
    expect(pre.textContent).toBe(text);
    expect(thumb).not.toHaveTextContent("five: done");

    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(thumb).toHaveFocus();
  });
});

describe("report full text", () => {
  it("opens the full report in the in-app panel from the activity line", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const digest = sha(0xb4);
    const body = "# Wake options\n\nHooks wake fastest.\n\nFull comparison table follows.";
    client.reports.set(digest, { mime: "text/markdown", text: body, sha256: digest });
    render(
      <ClientContext.Provider value={client}>
        <ReportCards
          project="alpha"
          anchor="2026-02-10T15:00:00.000Z"
          reports={[
            {
              id: "01J00000000000000000000999",
              title: "Wake options compared",
              summary: "Hooks wake fastest.",
              report: { host: "h-fixture", sha256: digest, bytes: 80, ext: "md" },
              ts: "2026-02-10T14:00:00.000Z",
              session: "01J00000000000000000000101",
              harness: "claude_code",
            },
          ]}
        />
      </ClientContext.Provider>,
    );
    const read = screen.getByRole("button", { name: "Read the full report: Wake options compared" });
    await user.click(read);
    const dialog = await screen.findByRole("dialog", { name: /Wake options compared/ });
    await waitFor(() => expect(dialog.querySelector("pre")?.textContent).toBe(body));
    // Opening in the browser stays a separate action.
    expect(client.calls.some((call) => call.command === "open_report")).toBe(false);

    fireEvent.keyDown(screen.getByRole("button", { name: "Close the report" }), { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(read).toHaveFocus();
  });

  it("reads an HTML report as plain prose, not source", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const digest = sha(0xb5);
    const html =
      "<html><head><title>Audit</title><style>p{color:red}</style></head><body>" +
      "<h1>Audit</h1><p>Two issues &amp; one fix.</p><script>alert(1)</script>" +
      "<ul><li>Leak</li><li>Race</li></ul></body></html>";
    client.reports.set(digest, { mime: "text/html", text: html, sha256: digest });
    render(
      <ClientContext.Provider value={client}>
        <ReportCards
          project="alpha"
          anchor="2026-02-10T15:00:00.000Z"
          reports={[
            {
              id: "01J00000000000000000000998",
              title: "Audit",
              summary: "Two issues.",
              report: { host: "h-fixture", sha256: digest, bytes: 200, ext: "html" },
              ts: "2026-02-10T14:00:00.000Z",
              session: "01J00000000000000000000101",
              harness: "claude_code",
            },
          ]}
        />
      </ClientContext.Provider>,
    );
    await user.click(screen.getByRole("button", { name: "Read the full report: Audit" }));
    const dialog = await screen.findByRole("dialog", { name: /Audit/ });
    const prose = await waitFor(() => {
      const node = dialog.querySelector(".report-prose");
      expect(node).not.toBeNull();
      return node!;
    });
    expect(prose.textContent).toBe("Audit\n\nTwo issues & one fix.\n\n• Leak\n• Race");
    expect(prose.children).toHaveLength(0);
    expect(dialog.querySelector("script, ul, h1")).toBeNull();
  });
});

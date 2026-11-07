import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "../api/client";
import { ClientContext } from "../api/store";
import type { DecisionCard as DecisionCardDto } from "../api/types";
import { DecisionCard } from "../components/DecisionCard";
import { FakeClient, easyCard, sha } from "./fixtures";

const ANCHOR = "2026-02-10T15:00:00.000Z";

function image(n: number) {
  return { host: "h-00000000000000000000000001", sha256: sha(n), ext: "png", bytes: 1200 };
}

function picturedCard(): DecisionCardDto {
  const card = easyCard();
  return {
    ...card,
    options: card.options.map((option, index) =>
      index < 2 ? { ...option, image: image(0x10 + index) } : option,
    ),
  };
}

function renderCard(client: FakeClient, onSelect = vi.fn()) {
  const card = picturedCard();
  const windowKeys = vi.fn();
  window.addEventListener("keydown", windowKeys);
  render(
    <ClientContext.Provider value={client}>
      <DecisionCard
        project="alpha"
        glyph="radar"
        slice="next-1"
        card={card}
        mode="ask"
        anchor={ANCHOR}
        since={ANCHOR}
        defaultAt={undefined}
        stalledAgents={1}
        selected={undefined}
        explicit={false}
        onSelect={onSelect}
        doneWhen={card.done_when}
        onDoneWhen={vi.fn()}
        receiptTag="Preview · not saved"
        receiptSaved={false}
        unblockLabels={[]}
        primary={{ label: "Choose", onClick: vi.fn(), disabled: false, kbd: null }}
        confirm={null}
        pending={false}
        canMutate
      />
    </ClientContext.Provider>,
  );
  return { onSelect, windowKeys };
}

function withImages(client: FakeClient) {
  for (const n of [0x10, 0x11]) {
    client.reports.set(sha(n), { mime: "image/png", media_token: `token-${n}`, sha256: sha(n) });
  }
  return client;
}

afterEach(() => {
  vi.useRealTimers();
});

describe("option images", () => {
  it("shows each option's image above its card at full colour, and keeps rows aligned", async () => {
    const client = withImages(new FakeClient());
    renderCard(client);
    const tiles = await screen.findAllByRole("button", { name: /full size$/ });
    expect(tiles).toHaveLength(2);
    await waitFor(() =>
      expect(within(tiles[0]!).getByRole("img", { name: "Use harness adapters" })).toBeInTheDocument(),
    );
    // The option without an image keeps an empty slot so the cards line up.
    expect(document.querySelectorAll(".opt-pic.none")).toHaveLength(1);
    expect(
      client.calls.filter((call) => call.command === "read_report").length,
    ).toBeGreaterThanOrEqual(2);
  });

  it("opens a viewer that flips with the arrow keys and chooses with Enter", async () => {
    const client = withImages(new FakeClient());
    const { onSelect, windowKeys } = renderCard(client);
    await userEvent.click(await screen.findByRole("button", { name: "View Use harness adapters full size" }));
    const dialog = await screen.findByRole("dialog", { name: "Compare option images" });
    expect(within(dialog).getByRole("heading", { name: "Use harness adapters" })).toBeInTheDocument();
    expect(within(dialog).getByText("1 / 2")).toBeInTheDocument();

    fireEvent.keyDown(dialog, { key: "ArrowRight" });
    expect(within(dialog).getByRole("heading", { name: "Use inbox on next start" })).toBeInTheDocument();
    const frames = dialog.querySelectorAll(".compare-frame");
    expect(frames[1]?.getAttribute("data-current")).toBe("true");
    expect(frames[0]?.getAttribute("data-current")).toBe("false");

    // Option 3 has no image, so its number does not move the viewer.
    fireEvent.keyDown(dialog, { key: "3" });
    expect(within(dialog).getByText("2 / 2")).toBeInTheDocument();
    fireEvent.keyDown(dialog, { key: "1" });
    expect(within(dialog).getByText("1 / 2")).toBeInTheDocument();

    fireEvent.keyDown(dialog, { key: "Enter" });
    expect(onSelect).toHaveBeenCalledWith("one");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    // None of the viewer's keys reached the window-level shortcuts.
    expect(windowKeys).not.toHaveBeenCalled();
    window.removeEventListener("keydown", windowKeys);
  });

  it("closes on Escape without leaving the decision", async () => {
    const client = withImages(new FakeClient());
    const { windowKeys } = renderCard(client);
    await userEvent.click(await screen.findByRole("button", { name: "View Use inbox on next start full size" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(windowKeys).not.toHaveBeenCalled();
    expect(screen.getByRole("heading", { name: "How should answers reach idle agents?" })).toBeInTheDocument();
    window.removeEventListener("keydown", windowKeys);
  });

  it("keeps retrying an image that has not synced from the asking machine yet", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const client = new FakeClient();
    client.reportError = new ApiError({
      code: "E_IO",
      message: "The blob is not available.",
      hint: "Wait for sync.",
      retryable: true,
    });
    renderCard(client);
    await waitFor(() => expect(screen.getAllByText("Waiting for the image to sync…")).toHaveLength(2));
    withImages(client);
    client.reportError = null;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_100);
    });
    await waitFor(() =>
      expect(screen.getByRole("img", { name: "Use harness adapters" })).toBeInTheDocument(),
    );
  });
});

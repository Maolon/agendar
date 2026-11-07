import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../api/client";
import { ClientContext } from "../api/store";
import { Delivery } from "../views/Delivery";
import { FakeClient, makeDeliveryProject, makeDeliverySnapshot, sha } from "./fixtures";

const REPORT_DIGEST = sha(0x84);
const HOSTILE = '<img src=x onerror="alert(1)"><b>bold?</b>';

function renderDelivery(client: FakeClient) {
  const fixture = makeDeliveryProject();
  const onRefresh = vi.fn();
  render(
    <ClientContext.Provider value={client}>
      <Delivery
        project={fixture.project}
        delivery={fixture.delivery}
        need={fixture.need}
        anchor="2026-02-10T15:00:00.000Z"
        canMutate
        sendBackOpen={false}
        onSendBackOpen={() => undefined}
        onResolved={() => undefined}
        onRefresh={onRefresh}
      />
    </ClientContext.Provider>,
  );
  return { fixture, onRefresh };
}

describe("report panel", () => {
  it("reads the report blob and renders it as escaped text, never HTML", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    client.reports.set(REPORT_DIGEST, { mime: "text/html", text: HOSTILE, sha256: REPORT_DIGEST });
    renderDelivery(client);

    await userEvent.click(screen.getByRole("button", { name: /Read the report/ }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveAccessibleName(/search-v2 report/);

    // The bytes reach the DOM as text children of a <pre>.
    expect(dialog.querySelector("pre")?.textContent).toBe(HOSTILE);
    expect(dialog.querySelector("img")).toBeNull();
    expect(dialog.querySelector("b")).toBeNull();
    expect(screen.getByText("HTML source — active rendering unsupported.")).toBeInTheDocument();

    const request = client.calls.find(
      (call) =>
        call.command === "read_report" &&
        (call.request as { blob?: { sha256?: string } }).blob?.sha256 === REPORT_DIGEST,
    )?.request as { project: string; blob: { sha256: string } };
    expect(request.project).toBe("alpha");
    expect(request.blob.sha256).toBe(REPORT_DIGEST);
  });

  it("closes on Escape without clearing the review", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    client.reports.set(REPORT_DIGEST, { mime: "text/plain", text: "plain report", sha256: REPORT_DIGEST });
    renderDelivery(client);

    await userEvent.click(screen.getByRole("button", { name: /Read the report/ }));
    const close = await screen.findByRole("button", { name: "Close the report" });
    fireEvent.keyDown(close, { key: "Escape" });

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(
      screen.getByRole("heading", { name: "search-v2 is ready. Does it do what you asked?" }),
    ).toBeInTheDocument();
    expect(client.order).not.toContain("review_delivery");
  });

  it("reports an unreadable report instead of showing a body", async () => {
    const client = new FakeClient(makeDeliverySnapshot());
    client.reportError = new ApiError({
      code: "E_IO",
      message: "The report blob is not available.",
      hint: "Refresh the delivery.",
      retryable: true,
    });
    renderDelivery(client);

    await userEvent.click(screen.getByRole("button", { name: /Read the report/ }));
    const dialog = await screen.findByRole("dialog");
    await waitFor(() =>
      expect(dialog.querySelector("[role='alert']")).toHaveTextContent("The report blob is not available."),
    );
    expect(dialog.querySelector("pre")).toBeNull();
  });
});

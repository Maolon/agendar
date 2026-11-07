import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { PresenceRow } from "../api/types";
import { NowCards } from "../components/NowCards";
import { PresenceRows } from "../components/PresenceRows";
import { makeProject, makeSnapshot } from "./fixtures";

const ANCHOR = "2026-02-10T15:00:00.000Z";

const rows: PresenceRow[] = [
  { key: "claude_code:a", harness: "claude_code", state: "working", since: "2026-02-10T14:30:00.000Z", folder: "app", project: "alpha" },
  { key: "claude_code:b", harness: "claude_code", state: "waiting", since: "2026-02-10T14:55:00.000Z", folder: "scratch", project: null },
];

describe("agents seen without the skill", () => {
  it("lists them as status rows, the waiting ones first, never as questions", () => {
    const snapshot = makeSnapshot();
    render(<PresenceRows rows={rows} snapshot={snapshot} anchor={ANCHOR} />);
    const items = screen.getAllByRole("listitem");
    expect(items[0]).toHaveTextContent("Claude Code in scratch · waiting at its prompt");
    expect(items[0]).toHaveTextContent("5m ago");
    expect(items[1]).toHaveTextContent(`in ${snapshot.state.projects.alpha!.summary.title} · working`);
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("shows an observed agent as a Now card of its project", () => {
    const project = makeProject("alpha");
    render(
      <NowCards
        project={project}
        anchor={ANCHOR}
        needs={[]}
        selected={null}
        onSelect={() => undefined}
        observed={[rows[1]!]}
      />,
    );
    expect(screen.getByText("Waiting at its prompt")).toBeInTheDocument();
    expect(screen.getByText("no skill")).toBeInTheDocument();
  });
});

describe("project icons", () => {
  it("shows the project's own icon in full colour once it loads, the glyph until then", async () => {
    const { ClientContext } = await import("../api/store");
    const { FakeClient } = await import("./fixtures");
    const { ProjectAvatar } = await import("../components/ProjectAvatar");
    const client = new FakeClient();
    const sha = "c".repeat(64);
    client.reports.set(sha, { mime: "image/png", sha256: sha, media_token: "t".repeat(32) });
    const project = makeProject("alpha");
    project.summary.icon = { host: "h-01J00000000000000000000001", sha256: sha, bytes: 67, ext: "png" };
    const { container } = render(
      <ClientContext.Provider value={client}>
        <ProjectAvatar glyph="radar" project={project.summary} />
      </ClientContext.Provider>,
    );
    expect(container.querySelector("svg")).not.toBeNull();
    const { waitFor } = await import("@testing-library/react");
    await waitFor(() => expect(container.querySelector("img.av-img")).not.toBeNull());
    const img = container.querySelector("img.av-img");
    expect(img!.getAttribute("src")).toContain("t".repeat(32));
  });
});

describe("reports", () => {
  it("shows the summary and opens the document through open_report", async () => {
    const { ClientContext } = await import("../api/store");
    const { FakeClient } = await import("./fixtures");
    const { ReportCards } = await import("../components/Reports");
    const userEvent = (await import("@testing-library/user-event")).default;
    const user = userEvent.setup();
    const client = new FakeClient();
    const blob = { host: "h-01J00000000000000000000001", sha256: "d".repeat(64), bytes: 120, ext: "html" };
    render(
      <ClientContext.Provider value={client}>
        <ReportCards
          project="alpha"
          anchor={ANCHOR}
          reports={[
            {
              id: "01J00000000000000000000999",
              title: "Wake options compared",
              summary: "Hooks wake fastest.\nDetails inside.",
              report: blob,
              ts: "2026-02-10T14:00:00.000Z",
              session: "01J00000000000000000000101",
              harness: "claude_code",
            },
          ]}
        />
      </ClientContext.Provider>,
    );
    expect(screen.getByText("Wake options compared")).toBeInTheDocument();
    // Collapsed: one line with the summary's first line only.
    expect(screen.getByText(/Hooks wake fastest\./)).toBeInTheDocument();
    expect(screen.queryByText(/Details inside/)).toBeNull();
    await user.click(screen.getByRole("button", { name: "Open report" }));
    const call = client.calls.find((entry) => entry.command === "open_report");
    expect(call?.request).toEqual({ project: "alpha", blob });
    await user.click(screen.getByRole("button", { name: "Show summary" }));
    expect(screen.getByText(/Details inside/)).toBeInTheDocument();
  });

  it("shows only the newest report on one line until expanded", async () => {
    const { ClientContext } = await import("../api/store");
    const { FakeClient } = await import("./fixtures");
    const { ReportCards } = await import("../components/Reports");
    const userEvent = (await import("@testing-library/user-event")).default;
    const user = userEvent.setup();
    const blob = { host: "h-01J00000000000000000000001", sha256: "d".repeat(64), bytes: 120, ext: "html" };
    const report = (id: string, title: string, ts: string) => ({
      id,
      title,
      summary: `${title} summary.`,
      report: blob,
      ts,
      session: "01J00000000000000000000101",
      harness: "claude_code" as const,
    });
    render(
      <ClientContext.Provider value={new FakeClient()}>
        <ReportCards
          project="alpha"
          anchor={ANCHOR}
          reports={[
            report("01J00000000000000000000997", "Older report", "2026-02-10T12:00:00.000Z"),
            report("01J00000000000000000000998", "Newest report", "2026-02-10T14:30:00.000Z"),
            report("01J00000000000000000000999", "Middle report", "2026-02-10T13:00:00.000Z"),
          ]}
        />
      </ClientContext.Provider>,
    );
    expect(screen.getByText("Newest report")).toBeInTheDocument();
    expect(screen.queryByText("Older report")).toBeNull();
    expect(screen.queryByText("Middle report")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Show all 3" }));
    const titles = [...document.querySelectorAll(".rcard .rt")].map((node) => node.textContent);
    expect(titles).toEqual(["Newest report", "Middle report", "Older report"]);
    await user.click(screen.getByRole("button", { name: "Show less" }));
    expect(screen.queryByText("Older report")).toBeNull();
  });
});

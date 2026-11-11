import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { createClient } from "../api/client";
import { SnapshotStore } from "../api/store";
import { Sidebar } from "../components/Sidebar";
import { createFixtureTransport } from "../dev/fixtureTransport";
import { buildFixtureSnapshot } from "../dev/fixtures";
import { App } from "../App";

describe("twelve-project fixture", () => {
  it("projects twelve projects across every variance", () => {
    const snapshot = buildFixtureSnapshot();
    const projects = Object.values(snapshot.state.projects);
    expect(projects).toHaveLength(12);
    expect(new Set(projects.map((project) => project.summary.variance))).toEqual(
      new Set(["rework", "stopped", "no_demo", "active", "quiet"]),
    );
    expect(projects.filter((project) => project.summary.variance === "quiet")).toHaveLength(4);
  });

  it("renders twelve project rows with four quiet projects collapsed", async () => {
    const snapshot = buildFixtureSnapshot();
    render(
      <Sidebar
        snapshot={snapshot}
        selection={null}
        quietOpen={false}
        onSelect={vi.fn()}
        onToggleQuiet={vi.fn()}
      />,
    );
    expect(document.querySelectorAll("[data-row^='project:']")).toHaveLength(8);
    expect(screen.getByRole("button", { name: "4 more quiet" })).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "4 more quiet" }));
  });

  it("opens the fixture app with its twelve projects and every home section", async () => {
    const client = createClient(createFixtureTransport(), "fixture");
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);

    await screen.findByRole("heading", { name: /Since you last looked/ });
    expect(document.querySelectorAll("[data-row^='project:']")).toHaveLength(8);
    for (const id of ["needs", "moving", "stalled", "done", "next"]) {
      expect(document.querySelector(`[data-dash="${id}"]`)).not.toBeNull();
    }

    const collapse = screen.getByRole("button", { name: "4 more quiet" });
    await userEvent.click(collapse);
    expect(document.querySelectorAll("[data-row^='project:']")).toHaveLength(12);
  });
});

describe("fixture policy and agenda mutations", () => {
  it("saves a policy with the shown revision and locks irreversible to ask", async () => {
    const client = createClient(createFixtureTransport(), "fixture");
    const snapshot = await client.getSnapshot();
    const project = snapshot.state.projects.agendar!;
    const shownRevision = project.policy.revision;
    await client.setPolicy({
      request_id: "01J000000000000000000000AA",
      project: "agendar",
      expected_revision: shownRevision,
      policy: {
        ...project.policy.value,
        preset: "custom",
        cells: { ...project.policy.value.cells, easy_local: "ask" },
      },
    });
    const after = await client.getSnapshot();
    const updated = after.state.projects.agendar!;
    expect(updated.policy.revision).not.toBe(shownRevision);
    expect(updated.policy.value.preset).toBe("custom");
    expect(updated.policy.value.cells.easy_local).toBe("ask");
    expect(updated.policy.value.cells.irreversible_local).toBe("ask");
  });

  it("updates the project goal and a feature agenda through expected fields", async () => {
    const client = createClient(createFixtureTransport(), "fixture");
    const snapshot = await client.getSnapshot();
    const project = snapshot.state.projects.agendar!;

    await client.updateProject({
      request_id: "01J000000000000000000000AB",
      project: "agendar",
      expected_fields: { goal: project.field_revisions.goal! },
      patch: { goal: "Answer one question end to end on two Macs" },
    });
    const afterGoal = await client.getSnapshot();
    expect(afterGoal.state.projects.agendar!.summary.goal).toBe(
      "Answer one question end to end on two Macs",
    );

    await client.updateFeature({
      request_id: "01J000000000000000000000AC",
      project: "agendar",
      node: "core/menubar",
      expected_fields: {},
      patch: {
        title: "Menu bar triage v2",
        deadline: { op: "set", value: "2026-03-01T12:00:00.000Z" },
        priority: 0,
      },
    });
    const afterFeature = await client.getSnapshot();
    const feature = afterFeature.state.projects.agendar!.features["core/menubar"]!;
    expect(feature.title).toBe("Menu bar triage v2");
    expect(feature.deadline).toBe("2026-03-01T12:00:00.000Z");
    expect(feature.priority).toBe(0);
  });
});

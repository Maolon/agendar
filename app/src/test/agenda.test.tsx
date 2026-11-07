import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { ClientContext } from "../api/store";
import type { UpdateFeatureRequest, UpdateProjectRequest } from "../api/types";
import { Project } from "../views/Project";
import { FakeClient, makeGraph, makeProject, projectProps, sha } from "./fixtures";

function renderProject(client: FakeClient) {
  const base = makeProject("alpha");
  const project = {
    ...base,
    summary: { ...base.summary, goal: "Ship the fixture" },
    field_revisions: { goal: sha(0x71) },
    features: {
      "core/one": {
        ...base.features["core/one"]!,
        deadline: "2026-02-12T17:00:00.000Z",
        field_revisions: {
          title: sha(0x72),
          deadline: sha(0x73),
          priority: sha(0x74),
        },
      },
    },
  };
  // One published feature node: selecting it reveals its editor, exactly as a
  // human reaches it on the graph.
  client.graph = makeGraph({
    nodes: [
      {
        id: "feature:core/one",
        kind: "feature",
        label: "CLI ask",
        feature: "core/one",
        state: "doing",
        assumed: false,
      },
    ],
  });
  function Harness() {
    const [node, setNode] = useState<string | null>(null);
    return (
      <ClientContext.Provider value={client}>
        <Project
          {...projectProps(project, client, {
            selectedNode: node ?? undefined,
            onSelectNode: setNode,
          })}
        />
      </ClientContext.Provider>
    );
  }
  render(<Harness />);
  return project;
}

async function selectCliAsk(): Promise<void> {
  await userEvent.click(
    await screen.findByRole("button", { name: /CLI ask\. .*Trace what it waits on/ }),
  );
}

describe("agenda edits", () => {
  it("saves the project goal with its field revision and marks it human-edited", async () => {
    const client = new FakeClient();
    renderProject(client);

    await userEvent.click(screen.getByRole("button", { name: "Edit goal" }));
    const input = screen.getByLabelText("Project goal");
    fireEvent.change(input, { target: { value: "Ship the agenda safely" } });
    await userEvent.click(screen.getByRole("button", { name: "Save goal" }));

    await waitFor(() => expect(client.order).toContain("update_project"));
    const request = client.calls.find((call) => call.command === "update_project")
      ?.request as UpdateProjectRequest;
    expect(request.project).toBe("alpha");
    expect(request.patch).toEqual({ goal: "Ship the agenda safely" });
    expect(request.expected_fields).toEqual({ goal: sha(0x71) });
    expect(request.request_id).toMatch(/^[0-9A-Z]{26}$/);
    expect(await screen.findByText("Edited by you")).toBeInTheDocument();
  });

  it("patches only changed feature fields and marks them", async () => {
    const client = new FakeClient();
    renderProject(client);
    await selectCliAsk();

    await userEvent.click(screen.getByRole("button", { name: "Edit CLI ask" }));
    fireEvent.change(screen.getByLabelText("Title"), { target: { value: "CLI ask v2" } });
    const deadline = screen.getByLabelText("Deadline");
    fireEvent.change(deadline, { target: { value: "2026-03-01T12:00" } });
    fireEvent.change(screen.getByLabelText("Priority (0 highest)"), {
      target: { value: "0" },
    });
    await userEvent.click(screen.getByRole("button", { name: "Save step" }));

    await waitFor(() => expect(client.order).toContain("update_feature"));
    const request = client.calls.find((call) => call.command === "update_feature")
      ?.request as UpdateFeatureRequest;
    expect(request.project).toBe("alpha");
    expect(request.node).toBe("core/one");
    expect(request.patch).toEqual({
      title: "CLI ask v2",
      deadline: {
        op: "set",
        value: new Date("2026-03-01T12:00").toISOString(),
      },
      priority: 0,
    });
    expect(request.expected_fields).toEqual({
      title: sha(0x72),
      deadline: sha(0x73),
      priority: sha(0x74),
    });
    expect(
      await screen.findByText("Edited by you: title, deadline, priority"),
    ).toBeInTheDocument();
  });

  it("sends a tagged clear when the deadline is emptied", async () => {
    const client = new FakeClient();
    renderProject(client);
    await selectCliAsk();

    await userEvent.click(screen.getByRole("button", { name: "Edit CLI ask" }));
    fireEvent.change(screen.getByLabelText("Deadline"), { target: { value: "" } });
    await userEvent.click(screen.getByRole("button", { name: "Save step" }));

    await waitFor(() => expect(client.order).toContain("update_feature"));
    const request = client.calls.find((call) => call.command === "update_feature")
      ?.request as UpdateFeatureRequest;
    expect(request.patch).toEqual({ deadline: { op: "clear" } });
    expect(request.expected_fields).toEqual({ deadline: sha(0x73) });
  });

  it("refuses an out-of-range priority without calling update_feature", async () => {
    const client = new FakeClient();
    renderProject(client);
    await selectCliAsk();

    await userEvent.click(screen.getByRole("button", { name: "Edit CLI ask" }));
    fireEvent.change(screen.getByLabelText("Priority (0 highest)"), {
      target: { value: "4" },
    });
    await userEvent.click(screen.getByRole("button", { name: "Save step" }));

    expect(screen.getByRole("alert")).toHaveTextContent("Priority is a whole number from 0");
    expect(client.order).not.toContain("update_feature");
  });
});

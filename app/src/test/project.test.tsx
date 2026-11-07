import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ClientContext } from "../api/store";
import { Project } from "../views/Project";
import { FakeClient, makeProject, projectProps } from "./fixtures";

describe("project view repository links", () => {
  it("records a local repo link for the shown project without a Vault event", async () => {
    const client = new FakeClient();
    const project = makeProject("alpha");
    render(
      <ClientContext.Provider value={client}>
        <Project {...projectProps(project, client)} />
      </ClientContext.Provider>,
    );

    // Repositories live in the collapsed "Decisions · House rules · …" fold.
    fireEvent.click(screen.getByRole("button", { name: /Repositories/ }));
    fireEvent.change(screen.getByLabelText("Repository path"), {
      target: { value: "/tmp/example-repo" },
    });
    fireEvent.click(screen.getByRole("button", { name: /Link repository/i }));

    await waitFor(() => expect(client.order).toContain("link_repo"));
    const request = client.calls.find((call) => call.command === "link_repo")?.request as {
      repo: string;
      project: string;
    };
    expect(request.repo).toBe("/tmp/example-repo");
    expect(request.project).toBe("alpha");
    await screen.findByText("Linked /tmp/example-repo to alpha.");
  });
});

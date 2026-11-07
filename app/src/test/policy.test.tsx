import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { ClientContext } from "../api/store";
import type { PolicySet, SetPolicyRequest } from "../api/types";
import { PolicyPanel } from "../components/PolicyPanel";
import { FakeClient, makeProject, sha } from "./fixtures";

function renderPanel() {
  const client = new FakeClient();
  const project = makeProject("alpha");
  render(
    <ClientContext.Provider value={client}>
      <PolicyPanel project={project} canMutate />
    </ClientContext.Provider>,
  );
  return { client, project };
}

function balancedPolicy(): PolicySet {
  return makeProject("alpha").policy.value;
}

describe("policy panel", () => {
  it("applies a preset table and locks irreversible cells to ask", async () => {
    const { client } = renderPanel();
    expect(screen.getAllByRole("combobox")).toHaveLength(4);
    expect(screen.queryByLabelText("Irreversible, local")).toBeNull();
    expect(screen.getAllByTitle("Irreversible actions always ask")).toHaveLength(2);

    await userEvent.click(screen.getByRole("button", { name: "Strict" }));
    expect(screen.getByTestId("policy-preset")).toHaveTextContent("Strict");
    for (const select of screen.getAllByRole("combobox")) {
      expect(select).toHaveValue("ask");
    }
    expect(screen.getByText(/Default timeouts: hold only/)).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Save policy" }));
    await waitFor(() => expect(client.order).toContain("set_policy"));
    const request = client.calls.find((call) => call.command === "set_policy")
      ?.request as SetPolicyRequest;
    expect(request.project).toBe("alpha");
    // The current projected revision is the precondition, not a re-read value.
    expect(request.expected_revision).toBe(sha(0x61));
    expect(request.request_id).toMatch(/^[0-9A-Z]{26}$/);
    expect(request.policy.preset).toBe("strict");
    expect(request.policy.allow_timeouts).toBe(false);
    expect(request.policy.cells.irreversible_local).toBe("ask");
    expect(request.policy.cells.irreversible_wide).toBe("ask");
    expect(request.policy.cells.easy_local).toBe("ask");
  });

  it("marks an edited cell as custom and sends the budget numbers", async () => {
    const { client } = renderPanel();
    fireEvent.change(screen.getByLabelText("Easy to undo, local"), {
      target: { value: "assume" },
    });
    expect(screen.getByTestId("policy-preset")).toHaveTextContent("Custom");

    fireEvent.change(screen.getByLabelText("Features on unconfirmed choices"), {
      target: { value: "12" },
    });
    fireEvent.change(screen.getByLabelText("Slices awaiting review"), {
      target: { value: "3" },
    });
    fireEvent.change(screen.getByLabelText("Hours without a demo"), {
      target: { value: "6.5" },
    });

    await userEvent.click(screen.getByRole("button", { name: "Save policy" }));
    await waitFor(() => expect(client.order).toContain("set_policy"));
    const request = client.calls.find((call) => call.command === "set_policy")
      ?.request as SetPolicyRequest;
    expect(request.policy.preset).toBe("custom");
    expect(request.policy.cells.easy_local).toBe("assume");
    expect(request.policy.budget).toEqual({
      max_speculative_features: 12,
      max_pending_slices: 3,
      max_demo_gap_s: 23_400,
    });
  });

  it("never weakens an irreversible cell even if the draft says otherwise", async () => {
    const client = new FakeClient();
    const project = makeProject("alpha");
    const tampered: PolicySet = {
      ...balancedPolicy(),
      cells: { ...balancedPolicy().cells, irreversible_wide: "auto" },
    };
    render(
      <ClientContext.Provider value={client}>
        <PolicyPanel
          project={{ ...project, policy: { ...project.policy, value: tampered } }}
          canMutate
        />
      </ClientContext.Provider>,
    );
    await userEvent.click(screen.getByRole("button", { name: "Save policy" }));
    await waitFor(() => expect(client.order).toContain("set_policy"));
    const request = client.calls.find((call) => call.command === "set_policy")
      ?.request as SetPolicyRequest;
    expect(request.policy.cells.irreversible_wide).toBe("ask");
  });

  it("refuses to save a non-numeric budget without calling set_policy", async () => {
    const { client } = renderPanel();
    fireEvent.change(screen.getByLabelText("Features on unconfirmed choices"), {
      target: { value: "" },
    });
    await userEvent.click(screen.getByRole("button", { name: "Save policy" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Use a whole number of features.");
    expect(client.order).not.toContain("set_policy");
  });
});

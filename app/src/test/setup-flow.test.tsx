import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { ApiError } from "../api/client";
import { ClientContext, SnapshotStore } from "../api/store";
import type {
  ConfigureVaultRequest,
  GetSetupLocationsData,
  InstallLocalRequest,
  SetPreferencesRequest,
} from "../api/types";
import { Setup } from "../components/Setup";
import {
  consentDetection,
  installDetection,
  openedMessage,
  SKILL_INSTALL_COMMAND,
  systemPermissionNote,
  vaultPathProblem,
} from "../state/setup";
import {
  FakeClient,
  ICLOUD_LOCATION,
  LOCAL_LOCATION,
  PATH_LINK_PLAN,
  sha,
} from "./fixtures";

// Each test mounts its own tree: unmount it here so no earlier render is queried.
afterEach(() => {
  cleanup();
});

const VAULT = "/srv/agendar-vault";
const STEP_ONE = "Where should Agendar keep its shared data?";
const STEP_TWO = "Connect your tools";
const CREATE = "Create folder and continue";

/** The native shell's not-configured answer, with the revisions Setup needs. */
function notConfigured(): ApiError {
  return new ApiError({
    code: "E_CONTEXT",
    message: "Agendar is not configured for a Vault yet.",
    hint: "Choose a Vault folder in setup.",
    retryable: false,
    details: [
      { field: "expected_config_revision", message: sha(0x11) },
      { field: "expected_preferences_revision", message: sha(0x12) },
    ],
  });
}

function renderSetup(client: FakeClient, overrides: Partial<ComponentProps<typeof Setup>> = {}) {
  const handlers = {
    onConfigured: vi.fn(),
    onPreferences: vi.fn(),
    onRefresh: vi.fn(),
    onOpen: vi.fn(),
  };
  const tree = (extra: Partial<ComponentProps<typeof Setup>> = {}) => (
    <ClientContext.Provider value={client}>
      <Setup
        configRevision={sha(0x11)}
        preferencesRevision={sha(0x12)}
        notificationsConsent={false}
        {...handlers}
        {...overrides}
        {...extra}
      />
    </ClientContext.Provider>
  );
  const view = render(tree());
  return {
    ...handlers,
    rerender: (extra: Partial<ComponentProps<typeof Setup>> = {}) => view.rerender(tree(extra)),
  };
}

/** Fill step 1 with the explicit button and wait for the optional step. */
async function reachConnectStep(
  client: FakeClient,
  overrides: Partial<ComponentProps<typeof Setup>> = {},
) {
  const user = userEvent.setup();
  const handlers = renderSetup(client, overrides);
  await user.type(screen.getByLabelText("Shared data folder"), VAULT);
  await user.click(screen.getByRole("button", { name: CREATE }));
  await screen.findByRole("heading", { name: STEP_TWO });
  return { ...handlers, user };
}

function installRequests(client: FakeClient): InstallLocalRequest[] {
  return client.calls
    .filter((call) => call.command === "install_local")
    .map((call) => call.request as InstallLocalRequest);
}

function region(name: string): HTMLElement {
  return screen.getByRole("region", { name });
}

describe("setup vault step", () => {
  it("asks for the shared data folder without tools or writes", () => {
    const client = new FakeClient();
    renderSetup(client);

    expect(screen.getByRole("heading", { name: STEP_ONE })).toBeInTheDocument();
    expect(screen.getByLabelText("Shared data folder")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: CREATE })).toBeEnabled();
    // One checkbox only: whether to create a custom folder. No acknowledgement,
    // no tools and no notification controls on the required screen.
    expect(screen.getAllByRole("checkbox")).toHaveLength(1);
    expect(screen.queryByText(/agent skill/)).toBeNull();
    expect(screen.queryByText(/desktop notifications/i)).toBeNull();
    // Location detection is read-only; no install or configuration is sent.
    expect(client.order).toEqual(["get_setup_locations"]);
  });

  it("selects a detected iCloud folder by default and uses its Agendar child", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.setupLocations = [ICLOUD_LOCATION, LOCAL_LOCATION];
    renderSetup(client);

    const icloud = await screen.findByRole("radio", { name: /iCloud Drive/ });
    const local = screen.getByRole("radio", { name: /This computer/ });
    const custom = screen.getByRole("radio", { name: /Use a different folder/ });
    expect(icloud).toBeChecked();
    expect(local).not.toBeChecked();
    expect(screen.getByText(ICLOUD_LOCATION.path)).toBeVisible();
    expect(screen.queryByLabelText("Shared data folder", { selector: "input" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: /Create this folder/ })).toBeNull();
    // The manual choice is the final location option.
    expect(screen.getAllByRole("radio").at(-1)).toBe(custom);

    await user.click(screen.getByRole("button", { name: CREATE }));
    await screen.findByRole("heading", { name: STEP_TWO });
    const request = client.calls.find((call) => call.command === "configure_vault")
      ?.request as ConfigureVaultRequest;
    expect(request).toMatchObject({
      path: ICLOUD_LOCATION.path,
      create: true,
      consent: true,
    });
  });

  it("does not replace a manual path when detection finishes later", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    let resolveLocations!: (data: GetSetupLocationsData) => void;
    const pending = new Promise<GetSetupLocationsData>((resolve) => {
      resolveLocations = resolve;
    });
    vi.spyOn(client, "getSetupLocations").mockReturnValue(pending);
    renderSetup(client);

    const field = screen.getByLabelText("Shared data folder");
    await user.type(field, VAULT);
    resolveLocations({ locations: [ICLOUD_LOCATION] });
    expect(await screen.findByText(ICLOUD_LOCATION.path)).toBeVisible();
    expect(screen.getByRole("radio", { name: /Use a different folder/ })).toBeChecked();
    expect(field).toHaveValue(VAULT);
  });

  it("keeps manual entry available when detection fails", async () => {
    const client = new FakeClient();
    client.setupLocationsError = new Error("Detection unavailable");
    renderSetup(client);

    expect(await screen.findByText(/Could not detect suggested folders/)).toBeVisible();
    expect(screen.getByLabelText("Shared data folder")).toBeVisible();
    expect(screen.getByRole("radio", { name: /Use a different folder/ })).toBeChecked();
    expect(client.order).toEqual(["get_setup_locations"]);
    expect(client.order).not.toContain("configure_vault");
  });

  it("keeps the storage and sync details in a collapsed, labelled disclosure", async () => {
    const user = userEvent.setup();
    renderSetup(new FakeClient());

    const toggle = screen.getByRole("button", { name: "How storage and sync work" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    const detail = screen.getByText(/never in the Vault/);
    expect(detail).not.toBeVisible();

    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(detail).toBeVisible();
    expect(document.getElementById(toggle.getAttribute("aria-controls") ?? "")).toBe(
      detail.closest(".fold-panel"),
    );
  });

  it("explains a relative path next to the field and sends nothing", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const handlers = renderSetup(client);

    const field = screen.getByLabelText("Shared data folder");
    await user.type(field, "Documents/Agendar");
    await user.click(screen.getByRole("button", { name: CREATE }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Enter the full path, starting from the top of your disk.");
    expect(field).toHaveAttribute("aria-invalid", "true");
    expect(field).toHaveAccessibleDescription(/Enter the full path/);
    expect(field).toHaveValue("Documents/Agendar");
    expect(field).toHaveFocus();
    expect(client.order).not.toContain("configure_vault");
    expect(handlers.onConfigured).not.toHaveBeenCalled();
  });

  it("keeps the typed path and the create choice when the backend refuses", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.vaultError = new ApiError({
      code: "E_VALIDATION",
      message: "The Vault folder does not exist; enable Create to create it.",
      field: "/path",
      hint: "Pick an existing folder or keep Create ticked.",
      retryable: false,
    });
    const handlers = renderSetup(client);

    const field = screen.getByLabelText("Shared data folder");
    const create = screen.getByRole("checkbox", { name: "Create this folder if it does not exist" });
    await user.type(field, VAULT);
    await user.click(create);
    await user.click(screen.getByRole("button", { name: "Open folder and continue" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("The Vault folder does not exist");
    expect(alert).toHaveTextContent("Pick an existing folder or keep Create ticked.");
    expect(field).toHaveValue(VAULT);
    expect(create).not.toBeChecked();
    expect(field).toHaveAttribute("aria-describedby", expect.stringContaining("setup-vault-error"));
    expect(handlers.onConfigured).not.toHaveBeenCalled();
    expect(screen.queryByRole("heading", { name: STEP_TWO })).toBeNull();
  });

  it("sends explicit consent with the create button and moves focus to the optional step", async () => {
    const client = new FakeClient();
    const { onConfigured } = await reachConnectStep(client);

    expect(screen.getByRole("heading", { name: STEP_TWO })).toHaveFocus();
    expect(onConfigured).toHaveBeenCalledTimes(1);
    const request = client.calls.find((call) => call.command === "configure_vault")
      ?.request as ConfigureVaultRequest;
    expect(request).toMatchObject({
      path: VAULT,
      create: true,
      consent: true,
      expected_config_revision: sha(0x11),
    });
  });

  it("submits from the keyboard with Enter in the folder field", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    renderSetup(client);

    await user.type(screen.getByLabelText("Shared data folder"), `${VAULT}{Enter}`);
    await screen.findByRole("heading", { name: STEP_TWO });
    expect(client.order).toContain("configure_vault");
  });

  it("locks the field and shows the pending state while the folder is prepared", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const configure = client.configureVault.bind(client);
    vi.spyOn(client, "configureVault").mockImplementation(async (request) => {
      await gate;
      return configure(request);
    });
    renderSetup(client);

    await user.type(screen.getByLabelText("Shared data folder"), VAULT);
    await user.click(screen.getByRole("button", { name: CREATE }));

    expect(await screen.findByRole("button", { name: "Creating folder…" })).toBeDisabled();
    expect(screen.getByLabelText("Shared data folder")).toHaveAttribute("readonly");
    release();
    await screen.findByRole("heading", { name: STEP_TWO });
  });

  it("treats a stale setup revision as a refresh request, not a decision failure", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.vaultError = new ApiError({
      code: "E_STALE_ANSWER",
      message: "The decision changed after it was prepared.",
      hint: "Review the refreshed card and prepare the answer again.",
      retryable: false,
    });
    const handlers = renderSetup(client);

    await user.type(screen.getByLabelText("Shared data folder"), VAULT);
    await user.click(screen.getByRole("button", { name: CREATE }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Agendar's setup changed while this screen was open.",
    );
    expect(screen.queryByText(/decision changed/)).toBeNull();
    expect(handlers.onRefresh).toHaveBeenCalledTimes(1);
    expect(screen.getByLabelText("Shared data folder")).toHaveValue(VAULT);
    expect(screen.getByRole("button", { name: CREATE })).toBeDisabled();
    expect(screen.getByText(/Refreshing setup/)).toBeInTheDocument();

    // Enter must not send the rejected revision again.
    await user.keyboard("{Enter}");
    expect(client.calls.filter((call) => call.command === "configure_vault")).toHaveLength(1);

    const next = sha(0x22);
    handlers.rerender({ configRevision: next });
    await user.click(screen.getByRole("button", { name: CREATE }));
    await screen.findByRole("heading", { name: STEP_TWO });
    const requests = client.calls
      .filter((call) => call.command === "configure_vault")
      .map((call) => call.request as ConfigureVaultRequest);
    expect(requests).toHaveLength(2);
    expect(requests[1]).toMatchObject({
      path: VAULT,
      create: true,
      consent: true,
      expected_config_revision: next,
    });
  });

  it("disables the Vault action with a visible reason until the shell supplies a revision", async () => {
    const user = userEvent.setup();
    const handlers = renderSetup(new FakeClient(), { configRevision: undefined });

    expect(screen.getByRole("button", { name: CREATE })).toBeDisabled();
    expect(
      screen.getByText("Agendar is still starting. Check again in a moment."),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Check again" }));
    expect(handlers.onRefresh).toHaveBeenCalledTimes(1);
  });
});

describe("setup optional step", () => {
  it("offers two independent, unchecked connections and the skill install command", async () => {
    const client = new FakeClient();
    await reachConnectStep(client);
    expect(await screen.findByText(PATH_LINK_PLAN.destination ?? "")).toBeVisible();

    const command = screen.getByRole("checkbox", { name: "Connect the agendar command" });
    const notify = screen.getByRole("checkbox", { name: "Allow desktop notifications" });
    expect(command).not.toBeChecked();
    expect(notify).not.toBeChecked();
    expect(command).toBeEnabled();
    expect(notify).toBeEnabled();
    expect(screen.getAllByRole("checkbox")).toHaveLength(2);
    expect(within(region("Agent skill")).getByText(SKILL_INSTALL_COMMAND)).toBeVisible();
    // The PATH check is the only dry run; nothing is installed, consented or saved yet.
    const requests = installRequests(client);
    expect(requests).toHaveLength(1);
    expect(requests.every((request) => request.dry_run && request.consent === false)).toBe(true);
    expect(client.order).not.toContain("set_preferences");
  });

  it("installs the command line only after its review, with the reviewed plan digest", async () => {
    const client = new FakeClient();
    const { user } = await reachConnectStep(client);
    const cli = region("Command line");

    await user.click(await within(cli).findByRole("checkbox", { name: "Connect the agendar command" }));
    expect(within(cli).getByText(/Review: places the bundled command/)).toBeInTheDocument();
    expect(installRequests(client).some((request) => !request.dry_run)).toBe(false);

    await user.click(within(cli).getByRole("button", { name: "Connect command line" }));
    expect(await within(cli).findByText(/Connected in this run at/)).toBeInTheDocument();
    expect(within(cli).queryByText(/Already installed/)).toBeNull();
    expect(screen.getByText(/Connected in this run: command line/)).toBeInTheDocument();
    expect(installRequests(client).filter((request) => !request.dry_run)).toEqual([
      expect.objectContaining({
        kind: "path_link",
        dry_run: false,
        consent: true,
        expected_plan_digest: PATH_LINK_PLAN.plan_digest,
      }),
    ]);
  });

  it("keeps the manual command collapsed and copyable", async () => {
    // userEvent.setup() installs its own clipboard stub, so the mock goes in after it.
    const { user } = await reachConnectStep(new FakeClient());
    const original = Object.getOwnPropertyDescriptor(navigator, "clipboard");
    const writeText = vi.fn(async (_text: string) => undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    try {
      const cli = region("Command line");
      const toggle = within(cli).getByRole("button", { name: "Connect it yourself instead" });
      expect(toggle).toHaveAttribute("aria-expanded", "false");
      const command = within(cli).getByText(PATH_LINK_PLAN.command ?? "");
      expect(command).not.toBeVisible();

      await user.click(toggle);
      expect(command).toBeVisible();
      await user.click(within(cli).getByRole("button", { name: "Copy command" }));
      expect(writeText).toHaveBeenCalledWith(PATH_LINK_PLAN.command);
      expect(await within(cli).findByText("Copied.")).toBeInTheDocument();
    } finally {
      if (original) Object.defineProperty(navigator, "clipboard", original);
      else Reflect.deleteProperty(navigator, "clipboard");
    }
  });

  it("keeps the two connections independent when one destination check fails", async () => {
    const client = new FakeClient();
    client.pathLinkPlanError = new ApiError({
      code: "E_CONTEXT",
      message: "The bundled agendar CLI was not found next to the app: missing.",
      hint: "Rebuild with `pnpm -C app run bundle:cli`, or set AGENDAR_CLI_PATH for development.",
      retryable: true,
    });
    const { user } = await reachConnectStep(client);
    const cli = region("Command line");

    const alert = await within(cli).findByRole("alert");
    expect(alert).toHaveTextContent(
      "Could not check this option. The bundled agendar CLI was not found next to the app: missing.",
    );
    expect(alert).not.toHaveTextContent("bundle:cli");
    expect(alert).not.toHaveTextContent("AGENDAR_CLI_PATH");
    expect(alert).not.toHaveFocus();
    expect(within(cli).getByRole("checkbox", { name: "Command line is not verified" })).toBeDisabled();
    // The skill command does not depend on the failed check.
    expect(within(region("Agent skill")).getByText(SKILL_INSTALL_COMMAND)).toBeVisible();

    // Retrying reloads only the failed check.
    await user.click(within(cli).getByRole("button", { name: "Check again" }));
    expect(await within(cli).findByText(PATH_LINK_PLAN.destination ?? "")).toBeVisible();
  });

  it("keeps the checked choice and shows an install refusal inline with a retry", async () => {
    const client = new FakeClient();
    client.pathLinkInstallError = new ApiError({
      code: "E_UNSUPPORTED",
      message: "Refusing to overwrite an unrelated file in the PATH directory.",
      hint: "Fix the input and retry without deleting logs.",
      retryable: false,
    });
    const { user } = await reachConnectStep(client);
    const cli = region("Command line");

    await user.click(await within(cli).findByRole("checkbox", { name: "Connect the agendar command" }));
    await user.click(within(cli).getByRole("button", { name: "Connect command line" }));

    const alert = await within(cli).findByRole("alert");
    expect(alert).toHaveTextContent("Refusing to overwrite an unrelated file in the PATH directory.");
    expect(alert).not.toHaveTextContent("Fix the input and retry without deleting logs.");
    expect(alert).toHaveFocus();
    expect(within(cli).getByRole("checkbox", { name: "Connect the agendar command" })).toBeChecked();
    expect(within(cli).getByRole("button", { name: "Connect command line" })).toBeEnabled();
  });

  it("refreshes a stale command-line plan before another write", async () => {
    const client = new FakeClient();
    const { user } = await reachConnectStep(client);
    const nextDigest = sha(0xb1);
    const nextDestination = "/opt/agendar-fixture/local-bin/agendar-next";
    client.pathLinkPlan = {
      ...client.pathLinkPlan,
      plan_digest: nextDigest,
      destination: nextDestination,
    };
    const cli = region("Command line");

    await user.click(await within(cli).findByRole("checkbox", { name: "Connect the agendar command" }));
    await user.click(within(cli).getByRole("button", { name: "Connect command line" }));

    const stale = await within(cli).findByRole("alert");
    expect(stale).toHaveTextContent("The plan changed after it was shown.");
    expect(stale).toHaveTextContent("Review the new plan and consent again.");
    expect(await within(cli).findByText(nextDestination)).toBeInTheDocument();
    expect(within(cli).getByText(/Review: places the bundled command/)).toBeInTheDocument();
    const pathCalls = () => installRequests(client).filter((request) => request.kind === "path_link");
    expect(pathCalls().map((request) => request.dry_run)).toEqual([true, false, true]);
    expect(pathCalls()[1]).toMatchObject({
      consent: true,
      expected_plan_digest: PATH_LINK_PLAN.plan_digest,
    });
    expect(pathCalls()[2]).toMatchObject({ dry_run: true, consent: false });
    expect(within(cli).getByRole("checkbox", { name: "Connect the agendar command" })).toBeChecked();
    expect(within(cli).getByRole("checkbox", { name: "Connect the agendar command" })).toBeEnabled();

    await user.click(within(cli).getByRole("button", { name: "Connect command line" }));
    expect(await within(cli).findByText(/Connected in this run at/)).toBeInTheDocument();
    expect(within(cli).queryByText(/Already installed/)).toBeNull();
    expect(pathCalls().filter((request) => !request.dry_run)).toEqual([
      expect.objectContaining({ expected_plan_digest: PATH_LINK_PLAN.plan_digest, consent: true }),
      expect.objectContaining({ expected_plan_digest: nextDigest, consent: true, dry_run: false }),
    ]);
    expect(installRequests(client).every((request) => request.kind === "path_link")).toBe(true);
  });

  it("shows the standard skill install command and never writes or detects a skill", async () => {
    // userEvent.setup() installs its own clipboard stub, so the mock goes in after it.
    const client = new FakeClient();
    const { user } = await reachConnectStep(client);
    const original = Object.getOwnPropertyDescriptor(navigator, "clipboard");
    const writeText = vi.fn(async (_text: string) => undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    try {
      const skill = region("Agent skill");
      expect(within(skill).getByText(SKILL_INSTALL_COMMAND)).toBeVisible();
      expect(within(skill).queryByRole("checkbox")).toBeNull();
      expect(within(skill).queryByText("Detected")).toBeNull();
      expect(within(skill).queryByText(/SessionStart|PostToolUse|Stop hook/)).toBeNull();

      await user.click(within(skill).getByRole("button", { name: "Copy command" }));
      expect(writeText).toHaveBeenCalledWith("npx skills add Maolon/agendar -g");
      expect(await within(skill).findByText("Copied.")).toBeInTheDocument();
      expect(installRequests(client).every((request) => request.kind === "path_link")).toBe(true);
      expect(installRequests(client).some((request) => !request.dry_run)).toBe(false);
    } finally {
      if (original) Object.defineProperty(navigator, "clipboard", original);
      else Reflect.deleteProperty(navigator, "clipboard");
    }
  });

  it("shows an already-linked command as checked and connected", async () => {
    const client = new FakeClient();
    client.pathLinkPlan = { ...PATH_LINK_PLAN, already_installed: true };
    await reachConnectStep(client);
    const cli = region("Command line");
    const box = await within(cli).findByRole("checkbox", {
      name: "Command line is already installed",
    });

    expect(within(cli).getByText(/Already installed/)).toBeInTheDocument();
    expect(box).toBeChecked();
    expect(box).toBeDisabled();
    expect(box).toHaveAccessibleDescription(/Already installed/);
    expect(within(cli).getByText("Detected")).toBeInTheDocument();
    expect(within(cli).queryByRole("button", { name: "Connect command line" })).toBeNull();
  });

  it("saves notification consent only from its own button and keeps the choice on failure", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.setPreferencesError = new ApiError({
      code: "E_IO",
      message: "Could not save notification settings.",
      hint: "Try again in a moment.",
      retryable: true,
    });
    const { onPreferences } = await reachConnectStep(client, {
      notificationPermission: "denied",
    });
    const notify = region("Desktop notifications");

    await user.click(within(notify).getByRole("checkbox", { name: "Allow desktop notifications" }));
    expect(client.order).not.toContain("set_preferences");
    await user.click(within(notify).getByRole("button", { name: "Allow notifications" }));
    expect(await within(notify).findByRole("alert")).toHaveTextContent(
      "Could not save notification settings.",
    );
    expect(within(notify).getByRole("alert")).toHaveTextContent("Try again in a moment.");
    expect(within(notify).getByRole("checkbox", { name: "Allow desktop notifications" })).toBeChecked();

    await user.click(within(notify).getByRole("button", { name: "Try again" }));
    expect(
      await within(notify).findByText(
        "Agendar consent is stored. The system may still ask for permission.",
      ),
    ).toBeInTheDocument();
    expect(onPreferences).toHaveBeenCalledWith(expect.objectContaining({ notifications: true }));
    const request = client.calls
      .filter((call) => call.command === "set_preferences")
      .at(-1)?.request as SetPreferencesRequest;
    expect(request).toMatchObject({ notifications: true, expected_revision: sha(0x12) });
    expect(within(notify).getByText(/permission as denied/)).toBeInTheDocument();
    expect(screen.queryByText(/notifications are set up/i)).toBeNull();
  });

  it("refreshes a stale notification revision before another write", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.setPreferencesError = new ApiError({
      code: "E_STALE_ANSWER",
      message: "The decision changed after it was prepared.",
      hint: "Review the refreshed card and prepare the answer again.",
      retryable: false,
    });
    const handlers = await reachConnectStep(client);
    const notify = region("Desktop notifications");

    await user.click(within(notify).getByRole("checkbox", { name: "Allow desktop notifications" }));
    await user.click(within(notify).getByRole("button", { name: "Allow notifications" }));
    const alert = await within(notify).findByRole("alert");
    expect(alert).toHaveTextContent(
      "Notification settings changed while this screen was open. Agendar is refreshing them.",
    );
    expect(alert).toHaveTextContent("Try again after the refresh.");
    expect(screen.queryByText("The decision changed after it was prepared.")).toBeNull();
    expect(screen.queryByText("Review the refreshed card and prepare the answer again.")).toBeNull();
    expect(handlers.onRefresh).toHaveBeenCalledTimes(1);
    expect(client.calls.filter((call) => call.command === "set_preferences")).toHaveLength(1);
    expect(within(notify).getByRole("checkbox", { name: "Allow desktop notifications" })).toBeChecked();
    expect(screen.getByRole("button", { name: "Open Agendar" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: CREATE })).toBeNull();

    await user.click(within(notify).getByRole("button", { name: "Check again" }));
    expect(handlers.onRefresh).toHaveBeenCalledTimes(2);
    expect(client.calls.filter((call) => call.command === "set_preferences")).toHaveLength(1);

    const next = sha(0x13);
    handlers.rerender({ preferencesRevision: next });
    await user.click(within(notify).getByRole("button", { name: "Try again" }));
    expect(
      await within(notify).findByText(
        "Agendar consent is stored. The system may still ask for permission.",
      ),
    ).toBeInTheDocument();
    const request = client.calls
      .filter((call) => call.command === "set_preferences")
      .at(-1)?.request as SetPreferencesRequest;
    expect(request).toMatchObject({ notifications: true, expected_revision: next });
  });

  it("shows notifications that are already allowed as checked", async () => {
    const client = new FakeClient();
    await reachConnectStep(client, {
      notificationsConsent: true,
      notificationPermission: "unknown",
    });
    const notify = region("Desktop notifications");
    const box = within(notify).getByRole("checkbox", {
      name: "Agendar notification consent is stored",
    });

    expect(box).toBeChecked();
    expect(box).toBeDisabled();
    expect(box).toHaveAccessibleDescription(/already stored/);
    expect(within(notify).getByText("Detected")).toBeInTheDocument();
    expect(within(notify).getByText(/cannot say the system allowed notifications/)).toBeInTheDocument();
    expect(within(notify).queryByRole("button", { name: "Allow notifications" })).toBeNull();
    expect(client.order).not.toContain("set_preferences");
  });

  it("never lets a notification problem block the Vault step", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.setPreferencesError = new ApiError({
      code: "E_IO",
      message: "Preferences are unavailable.",
      hint: "",
      retryable: true,
    });
    renderSetup(client);

    await user.type(screen.getByLabelText("Shared data folder"), VAULT);
    await user.click(screen.getByRole("button", { name: CREATE }));
    await screen.findByRole("heading", { name: STEP_TWO });

    expect(client.order).toContain("configure_vault");
    expect(client.order).not.toContain("set_preferences");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("Skip for now says what is not connected and installs nothing", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const { onOpen } = await reachConnectStep(client);

    await user.click(screen.getByRole("button", { name: "Skip for now" }));
    expect(await screen.findByRole("heading", { name: "Agendar is ready to open" })).toHaveFocus();
    expect(screen.getByText("Command line: not connected. Nothing was installed.")).toBeInTheDocument();
    expect(screen.getByText(/Agent skill: install it yourself with/)).toHaveTextContent(
      SKILL_INSTALL_COMMAND,
    );
    expect(screen.getByText("Desktop notifications: Agendar consent is not stored.")).toBeInTheDocument();
    expect(installRequests(client).some((request) => !request.dry_run)).toBe(false);
    expect(onOpen).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Back to connections" }));
    expect(await screen.findByRole("heading", { name: STEP_TWO })).toHaveFocus();
    await user.click(screen.getByRole("button", { name: "Skip for now" }));
    await user.click(await screen.findByRole("button", { name: "Open Agendar" }));
    expect(onOpen).toHaveBeenCalledWith({
      commandLine: "absent",
      notifications: "absent",
    });
  });

  it("opens from the optional step with only the connections that finished", async () => {
    const client = new FakeClient();
    const { user, onOpen } = await reachConnectStep(client);
    const cli = region("Command line");

    await user.click(await within(cli).findByRole("checkbox", { name: "Connect the agendar command" }));
    await user.click(within(cli).getByRole("button", { name: "Connect command line" }));
    expect(await within(cli).findByText(/Connected in this run at/)).toBeInTheDocument();
    expect(within(cli).queryByText(/Already installed/)).toBeNull();
    expect(screen.getByText(/Connected in this run: command line/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Skip for now" }));
    expect(screen.getByText("Command line: connected in this run.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Open Agendar" }));
    expect(onOpen).toHaveBeenCalledWith({
      commandLine: "connected",
      notifications: "absent",
    });
  });

  it("holds Open Agendar until the snapshot has loaded", async () => {
    await reachConnectStep(new FakeClient(), { ready: false });

    expect(screen.getByRole("button", { name: "Open Agendar" })).toBeDisabled();
    expect(screen.getByText("Finishing setup…")).toBeInTheDocument();
  });
});

describe("setup detection", () => {
  it("shows a detected command without writing", async () => {
    const client = new FakeClient();
    client.pathLinkPlan = { ...PATH_LINK_PLAN, already_installed: true };
    await reachConnectStep(client);
    const cli = region("Command line");

    const command = await within(cli).findByRole("checkbox", {
      name: "Command line is already installed",
    });
    expect(command).toBeChecked();
    expect(command).toBeDisabled();
    expect(command).toHaveAccessibleDescription(/Already installed/);
    expect(within(cli).getByText("Detected")).toBeInTheDocument();
    expect(within(cli).getByText(/Already installed/)).toBeInTheDocument();
    expect(within(cli).getByText(PATH_LINK_PLAN.destination ?? "")).toBeInTheDocument();
    expect(within(cli).queryByRole("button", { name: "Connect command line" })).toBeNull();
    expect(within(cli).queryByRole("button", { name: "Connect it yourself instead" })).toBeNull();

    expect(installRequests(client).every((request) => request.dry_run && request.consent === false)).toBe(
      true,
    );
    expect(client.order).not.toContain("set_preferences");
  });

  it("keeps stored consent and system permission apart", async () => {
    const granted = new FakeClient();
    await reachConnectStep(granted, {
      notificationsConsent: false,
      notificationPermission: "granted",
    });
    const openConsent = region("Desktop notifications");
    expect(within(openConsent).getByRole("checkbox", { name: "Allow desktop notifications" })).not.toBeChecked();
    expect(within(openConsent).queryByText("Detected")).toBeNull();
    expect(within(openConsent).getByText(/permission as granted/)).toBeInTheDocument();
    expect(within(openConsent).queryByRole("button", { name: "Allow notifications" })).toBeNull();
    expect(granted.order).not.toContain("set_preferences");
    cleanup();

    const denied = new FakeClient();
    await reachConnectStep(denied, {
      notificationsConsent: true,
      notificationPermission: "denied",
    });
    const stored = region("Desktop notifications");
    const storedConsent = within(stored).getByRole("checkbox", {
      name: "Agendar notification consent is stored",
    });
    expect(storedConsent).toBeChecked();
    expect(storedConsent).toBeDisabled();
    expect(storedConsent).toHaveAccessibleDescription(/already stored/);
    expect(within(stored).queryByText(/notifications are set up/i)).toBeNull();
    expect(within(stored).getByText("Detected")).toBeInTheDocument();
    expect(within(stored).getByText(/permission as denied/)).toBeInTheDocument();
    expect(within(stored).queryByText(/permission as granted/)).toBeNull();
    expect(within(stored).queryByRole("button", { name: "Allow notifications" })).toBeNull();
    expect(denied.order).not.toContain("set_preferences");
  });

  it("says an unreported link and unread consent are not verified, and writes nothing", async () => {
    const client = new FakeClient();
    const unreported = { ...PATH_LINK_PLAN };
    delete unreported.already_installed;
    client.pathLinkPlan = unreported;
    await reachConnectStep(client, { notificationsConsent: undefined });
    const cli = region("Command line");
    const notify = region("Desktop notifications");

    const command = await within(cli).findByRole("checkbox", {
      name: "Command line is not verified",
    });
    expect(command).toBeDisabled();
    expect(command).not.toBeChecked();
    expect(command).toHaveAccessibleDescription(/The check did not say/);
    expect(within(cli).queryByRole("button", { name: "Connect command line" })).toBeNull();
    const consent = within(notify).getByRole("checkbox", {
      name: "Agendar notification consent is not verified",
    });
    expect(consent).toBeDisabled();
    expect(consent).not.toBeChecked();
    expect(consent).toHaveAccessibleDescription(/Agendar consent was not verified/);
    expect(within(notify).getByText(/System notification permission was not verified/)).toBeInTheDocument();
    expect(client.order).not.toContain("set_preferences");
    expect(installRequests(client).filter((request) => !request.dry_run)).toHaveLength(0);
  });
});

describe("setup inside the app", () => {
  it("does not answer or mark a need while setup is showing the loaded snapshot", async () => {
    const user = userEvent.setup();
    const hasFocus = vi.spyOn(document, "hasFocus").mockReturnValue(true);
    try {
      const client = new FakeClient();
      client.snapshotError = notConfigured();
      render(<App client={client} store={new SnapshotStore(client)} surface="main" />);

      await user.type(await screen.findByLabelText("Shared data folder"), VAULT);
      await user.click(screen.getByRole("button", { name: CREATE }));
      await screen.findByRole("heading", { name: STEP_TWO });
      await screen.findByText(PATH_LINK_PLAN.destination ?? "");
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "Open Agendar" })).toBeEnabled(),
      );

      await user.keyboard("{ArrowDown}{ArrowUp}{ArrowDown}{Enter}");
      await user.keyboard("{Meta>}{Enter}{/Meta}");
      await user.keyboard("{Control>}{Enter}{/Control}");
      await user.keyboard("{Meta>}{Enter}{/Meta}");

      expect(client.order).not.toContain("answer_decision");
      expect(client.order).not.toContain("prepare_answer");
      expect(client.calls.filter((call) => call.command === "set_preferences")).toEqual([]);
      expect(screen.getByRole("heading", { name: STEP_TWO })).toBeInTheDocument();

      await user.click(screen.getByRole("button", { name: "Open Agendar" }));
      expect(await screen.findByRole("combobox", { name: "Search everything" })).toHaveFocus();
      expect(client.order).not.toContain("answer_decision");
      expect(
        client.calls.some(
          (call) =>
            call.command === "set_preferences" &&
            (call.request as { mark_looked?: boolean }).mark_looked === true,
        ),
      ).toBe(false);
    } finally {
      hasFocus.mockRestore();
    }
  });

  it("keeps the optional step after the snapshot loads, then opens Agendar with a truthful message", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.snapshotError = notConfigured();
    render(<App client={client} store={new SnapshotStore(client)} surface="main" />);

    await user.type(await screen.findByLabelText("Shared data folder"), VAULT);
    await user.click(screen.getByRole("button", { name: CREATE }));
    await screen.findByRole("heading", { name: STEP_TWO });

    // The snapshot refresh after configuration must not unmount the step.
    await waitFor(() => expect(client.snapshotCalls).toBeGreaterThanOrEqual(2));
    await screen.findByText(PATH_LINK_PLAN.destination ?? "");
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Open Agendar" })).toBeEnabled(),
    );
    expect(screen.getByRole("heading", { name: STEP_TWO })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Open Agendar" }));
    expect(await screen.findByRole("heading", { name: /Since you last looked/ })).toBeInTheDocument();
    expect(
      await screen.findByText(
        "Agendar is open. Not set up: command line, notification consent.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: STEP_TWO })).toBeNull();
  });
});

describe("setup resume", () => {
  it("resumes at the optional step after a failed refresh instead of asking for the Vault again", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    client.snapshotError = notConfigured();
    render(<App client={client} store={new SnapshotStore(client)} surface="main" />);

    await user.type(await screen.findByLabelText("Shared data folder"), VAULT);
    // The snapshot refresh that follows the Vault step fails once.
    client.snapshotError = new ApiError({
      code: "E_IO",
      message: "Cannot reach the Agendar service.",
      hint: "Try again.",
      retryable: true,
    });
    await user.click(screen.getByRole("button", { name: CREATE }));
    await user.click(await screen.findByRole("button", { name: "Try again" }));

    expect(await screen.findByRole("heading", { name: STEP_TWO })).toHaveFocus();
    expect(screen.getByText(VAULT)).toBeVisible();
    expect(screen.queryByLabelText("Shared data folder")).toBeNull();
  });
});

describe("setup helpers", () => {
  it("accepts only full paths and refuses shortcuts, relative paths and a disk root", () => {
    expect(vaultPathProblem("")).toBe("Enter the full path to a folder.");
    expect(vaultPathProblem("~/Agendar")).toMatch(/Shortcuts such as ~/);
    expect(vaultPathProblem("Documents/Agendar")).toMatch(/full path/);
    expect(vaultPathProblem("/")).toMatch(/top of a disk/);
    expect(vaultPathProblem("C:\\")).toMatch(/top of a disk/);
    expect(vaultPathProblem(VAULT)).toBeNull();
    expect(vaultPathProblem("C:\\Work\\Agendar")).toBeNull();
    expect(vaultPathProblem("\\\\server\\share\\agendar")).toBeNull();
  });

  it("names only the connections that are not set up, and keeps unread checks unverified", () => {
    expect(openedMessage({ commandLine: "detected", notifications: "detected" })).toBe(
      "Agendar is open. The command line is connected. Notification consent is stored.",
    );
    expect(openedMessage({ commandLine: "connected", notifications: "absent" })).toBe(
      "Agendar is open. Connected in this run: command line. Not set up: notification consent.",
    );
    expect(openedMessage({ commandLine: "detected", notifications: "absent" })).toBe(
      "Agendar is open. Not set up: notification consent.",
    );
    expect(openedMessage({ commandLine: "connected", notifications: "connected" })).toBe(
      "Agendar is open. Connected in this run: command line, notification consent. Notification consent is stored.",
    );
    expect(openedMessage({ commandLine: "unverified", notifications: "unverified" })).toBe(
      "Agendar is open. Not verified: command line, notification consent.",
    );
  });

  it("treats a dry-run flag as detection and leaves OS permission out of consent", () => {
    expect(installDetection({ status: "loading" }, false)).toBe("unverified");
    expect(installDetection({ status: "error", message: "Cannot check." }, false)).toBe("unverified");
    expect(
      installDetection({ status: "ready", value: { ...PATH_LINK_PLAN, already_installed: true } }, false),
    ).toBe("detected");
    expect(
      installDetection(
        { status: "ready", value: { ...PATH_LINK_PLAN, already_installed: false } },
        false,
      ),
    ).toBe("absent");
    const unreported = { ...PATH_LINK_PLAN };
    delete unreported.already_installed;
    expect(installDetection({ status: "ready", value: unreported }, false)).toBe("unverified");
    expect(consentDetection(undefined, false)).toBe("unverified");
    expect(consentDetection(false, false)).toBe("absent");
    expect(consentDetection(true, false)).toBe("detected");
    expect(consentDetection(false, true)).toBe("connected");
    expect(consentDetection(true, true)).toBe("connected");
    expect(
      installDetection({ status: "ready", value: { ...PATH_LINK_PLAN, already_installed: false } }, true),
    ).toBe("connected");
    expect(systemPermissionNote("unknown")).toMatch(/cannot say the system allowed/);
    expect(systemPermissionNote("granted")).toMatch(/granted/);
    expect(systemPermissionNote("unverified")).toMatch(/not verified/);
  });
});

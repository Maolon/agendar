import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../api/client";
import { ClientContext } from "../api/store";
import type {
  ConfigureVaultRequest,
  InstallLocalRequest,
  LinkRepoRequest,
  SetPreferencesRequest,
  UnlinkRepoRequest,
} from "../api/types";
import { handleKeyDown, type KeyboardHandlers } from "../hooks/useKeyboard";
import { Settings } from "../views/Settings";
import { I18nProvider, detectLanguage, languageOfTag } from "../i18n";
import { FakeClient, makeSnapshot } from "./fixtures";

function renderSettings(client = new FakeClient()) {
  const snapshot = makeSnapshot();
  snapshot.preferences.motion = true;
  snapshot.preferences.theme = "system";
  snapshot.state.projects.alpha!.summary.title = "Alpha project";
  const onPreferences = vi.fn();
  render(
    <ClientContext.Provider value={client}>
      <Settings snapshot={snapshot} onPreferences={onPreferences} onRefresh={vi.fn()} />
    </ClientContext.Provider>,
  );
  return { client, snapshot, onPreferences };
}

function prefCalls(client: FakeClient): SetPreferencesRequest[] {
  return client.calls
    .filter((call) => call.command === "set_preferences")
    .map((call) => call.request as SetPreferencesRequest);
}

async function openTab(name: string) {
  const user = userEvent.setup();
  await user.click(screen.getByRole("tab", { name }));
  return user;
}

describe("settings page", () => {
  it("saves the theme and the motion setting through set_preferences", async () => {
    const user = userEvent.setup();
    const { client, snapshot, onPreferences } = renderSettings();

    await user.click(screen.getByRole("radio", { name: "Dark" }));
    await waitFor(() => expect(prefCalls(client)).toHaveLength(1));
    expect(prefCalls(client)[0]).toMatchObject({
      theme: "dark",
      expected_revision: snapshot.preferences.revision,
    });
    expect(onPreferences).toHaveBeenCalledWith(expect.objectContaining({ theme: "dark" }));

    const motion = screen.getByRole("switch", { name: "Animate live changes" });
    expect(motion).toHaveAttribute("aria-checked", "true");
    await user.click(motion);
    await waitFor(() => expect(prefCalls(client)).toHaveLength(2));
    expect(prefCalls(client)[1]).toMatchObject({ motion: false });
  });

  it("keeps each area in its own tab and moves with the arrow keys", async () => {
    renderSettings();
    const tabs = screen.getAllByRole("tab");
    expect(tabs.map((tab) => tab.textContent)).toEqual(["General", "Agents", "Notifications", "Storage", "About"]);
    expect(tabs[0]).toHaveAttribute("aria-selected", "true");
    const user = userEvent.setup();
    tabs[0]!.focus();
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("tab", { name: "Agents" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tabpanel")).toHaveAttribute("aria-labelledby", "stab-agents");
    // About lists the version, the bundled CLI and the protocol; the Codex override is not a setting here.
    await user.click(screen.getByRole("tab", { name: "About" }));
    expect(screen.getByRole("tabpanel").textContent).not.toMatch(/Codex/);
  });

  it("chooses the display language", async () => {
    const user = userEvent.setup();
    const { client } = renderSettings();
    await user.selectOptions(screen.getByRole("combobox", { name: "Language" }), "zh-Hant");
    await waitFor(() => expect(prefCalls(client)).toHaveLength(1));
    expect(prefCalls(client)[0]).toMatchObject({ language: "zh-Hant" });
  });

  it("shows where data lives and the linked repositories, without writing", async () => {
    const { client } = renderSettings();
    await openTab("Storage");
    const data = screen.getByRole("region", { name: "Locations" });
    expect(await within(data).findByText("/srv/agendar-vault")).toBeInTheDocument();
    expect(within(data).getByText("/srv/agendar-data")).toBeInTheDocument();
    const repos = screen.getByRole("region", { name: "Repositories" });
    expect(within(repos).getByText("/srv/code/alpha")).toBeInTheDocument();
    // The project title, not the slug.
    expect(within(repos).getByText("Alpha project", { selector: ".srepo-project" })).toBeInTheDocument();
    expect(client.order).toContain("get_local_config");
    expect(client.order).not.toContain("set_preferences");
  });

  it("lists each agent's connection and connects one on an explicit click", async () => {
    const { client } = renderSettings();
    const user = await openTab("Agents");
    const connections = screen.getByRole("region", { name: "Connections" });
    expect(await within(connections).findByText("Claude Code")).toBeInTheDocument();
    expect(within(connections).getByText("Outdated")).toBeInTheDocument();
    expect(within(connections).getByText("Not installed")).toBeInTheDocument();
    expect(client.order).not.toContain("set_binding");
    await user.click(within(connections).getByRole("button", { name: "Connect Cursor" }));
    await waitFor(() => expect(client.order).toContain("set_binding"));
    expect(client.calls.find((call) => call.command === "set_binding")?.request).toEqual({
      harness: "cursor",
      action: "bind",
    });
    expect(await within(connections).findByText("Restart Cursor to load it.")).toBeInTheDocument();
    // Not installed on this computer: nothing to connect.
    expect(within(connections).getByRole("button", { name: "Connect Gemini CLI" })).toBeDisabled();
  });

  it("calls an agent connected when it reports through the skill, without hooks", async () => {
    const client = new FakeClient();
    const cursor = client.bindings.find((binding) => binding.harness === "cursor")!;
    cursor.last_seen = new Date(Date.now() - 12 * 60 * 1000).toISOString();
    renderSettings(client);
    await openTab("Agents");
    const connections = screen.getByRole("region", { name: "Connections" });
    const row = (await within(connections).findByText("Cursor")).closest(".srow2") as HTMLElement;
    expect(within(row).getByText("Connected")).toBeInTheDocument();
    expect(within(row).getByText("Through the Agendar skill · 12m ago")).toBeInTheDocument();
    expect(within(row).getByRole("button", { name: "Add MCP Cursor" })).toBeInTheDocument();
    expect(within(row).queryByText("Not connected")).toBeNull();
  });

  it("only dry-runs the command line and observe hooks until a confirmed install", async () => {
    const { client } = renderSettings();
    await openTab("Agents");
    expect(await screen.findByText("npx skills add Maolon/agendar -g")).toBeInTheDocument();
    await waitFor(() =>
      expect(client.calls.filter((call) => call.command === "install_local").length).toBeGreaterThan(0),
    );
    const installs = client.calls
      .filter((call) => call.command === "install_local")
      .map((call) => call.request as InstallLocalRequest);
    expect(installs.some((request) => request.kind === "observe_hooks")).toBe(true);
    expect(installs.every((request) => request.dry_run)).toBe(true);
  });

  it("unlinks a repository, archives its project once no folder links it, and links another", async () => {
    const { client } = renderSettings();
    const user = await openTab("Storage");
    const repos = screen.getByRole("region", { name: "Repositories" });
    await user.click(await within(repos).findByRole("button", { name: "Unlink /srv/code/alpha" }));
    await waitFor(() => expect(within(repos).queryByText("/srv/code/alpha")).toBeNull());
    const unlink = client.calls.find((call) => call.command === "unlink_repo")?.request as UnlinkRepoRequest;
    expect(unlink).toMatchObject({ repo: "/srv/code/alpha", expected_project: "alpha" });
    expect(client.order.indexOf("unlink_repo")).toBeLessThan(client.order.indexOf("update_project"));
    expect(client.calls.find((call) => call.command === "update_project")?.request).toMatchObject({
      project: "alpha",
      patch: { archived: true },
    });
    expect(
      await within(repos).findByText("Unlinked /srv/code/alpha and archived Alpha project. The folder itself was not touched."),
    ).toBeInTheDocument();

    await user.type(within(repos).getByLabelText("Repository folder"), "/srv/code/other");
    await user.click(within(repos).getByRole("button", { name: "Link repository" }));
    await waitFor(() => expect(client.order).toContain("link_repo"));
    const link = client.calls.find((call) => call.command === "link_repo")?.request as LinkRepoRequest;
    expect(link).toMatchObject({ repo: "/srv/code/other", project: "alpha" });
  });

  it("keeps the project when another folder on this computer still links it", async () => {
    const client = new FakeClient();
    client.localConfig.repos["/srv/code/alpha-docs"] = "alpha";
    renderSettings(client);
    const user = await openTab("Storage");
    const repos = screen.getByRole("region", { name: "Repositories" });
    await user.click(await within(repos).findByRole("button", { name: "Unlink /srv/code/alpha" }));
    expect(
      await within(repos).findByText("Unlinked /srv/code/alpha. The folder itself was not touched."),
    ).toBeInTheDocument();
    expect(client.order).not.toContain("update_project");
  });

  it("says the settings changed elsewhere and re-reads them when an unlink is refused", async () => {
    const client = new FakeClient();
    client.unlinkError = new ApiError({
      code: "E_STALE_ANSWER",
      message: "Settings on this computer changed after this page read them.",
      hint: "Review the refreshed values and try again.",
      retryable: true,
    });
    renderSettings(client);
    const user = await openTab("Storage");
    const repos = screen.getByRole("region", { name: "Repositories" });
    const reads = () => client.order.filter((command) => command === "get_local_config").length;
    await within(repos).findByRole("button", { name: "Unlink /srv/code/alpha" });
    const before = reads();
    await user.click(within(repos).getByRole("button", { name: "Unlink /srv/code/alpha" }));
    expect(
      await within(repos).findByText("Settings on this computer changed elsewhere and were reloaded. Try again."),
    ).toBeInTheDocument();
    await waitFor(() => expect(reads()).toBeGreaterThan(before));
    expect(client.order).not.toContain("update_project");
  });

  it("re-reads this computer's settings when the window regains focus", async () => {
    const { client } = renderSettings();
    await openTab("Storage");
    const reads = () => client.order.filter((command) => command === "get_local_config").length;
    await waitFor(() => expect(reads()).toBeGreaterThan(0));
    const before = reads();
    window.dispatchEvent(new Event("focus"));
    await waitFor(() => expect(reads()).toBeGreaterThan(before));
  });

  it("switches the shared data folder only after an explicit choice", async () => {
    const { client } = renderSettings();
    const user = await openTab("Storage");
    const data = screen.getByRole("region", { name: "Locations" });
    await user.click(await within(data).findByRole("button", { name: "Change…" }));
    await user.type(within(data).getByLabelText("Shared data folder path"), "/srv/other-vault");
    await user.click(within(data).getByRole("checkbox", { name: "Create this folder if it does not exist" }));
    expect(client.order).not.toContain("configure_vault");
    await user.click(within(data).getByRole("button", { name: "Switch to this folder" }));
    await waitFor(() => expect(client.order).toContain("configure_vault"));
    const request = client.calls.find((call) => call.command === "configure_vault")?.request as ConfigureVaultRequest;
    expect(request).toMatchObject({ path: "/srv/other-vault", create: true, consent: true });
    expect(request.expected_config_revision).toBe(client.localConfig.config_revision);
  });

  it("says whether the system allows notifications and sends a test", async () => {
    const client = new FakeClient();
    const snapshot = makeSnapshot();
    snapshot.local.notifications = "denied";
    snapshot.local.notification_error = "Notifications are not allowed for this application";
    render(
      <ClientContext.Provider value={client}>
        <Settings snapshot={snapshot} onPreferences={vi.fn()} onRefresh={vi.fn()} initialTab="notifications" />
      </ClientContext.Provider>,
    );
    expect(screen.getByText(/^Blocked — turn Agendar on in/)).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("Notifications are not allowed");
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Send a test notification" }));
    expect(client.order).toContain("test_notice");
    expect(await screen.findByText(/^Sent\. Nothing appeared\?/)).toBeInTheDocument();
  });

  it("opens with Cmd or Ctrl and comma", () => {
    const openSettings = vi.fn();
    const handlers = { openSettings, focusSearch: vi.fn() } as unknown as KeyboardHandlers;
    handleKeyDown(new KeyboardEvent("keydown", { key: ",", metaKey: true }), handlers);
    expect(openSettings).toHaveBeenCalledTimes(1);
  });
});

describe("language", () => {
  it("detects Chinese variants by script and region, and falls back to English", () => {
    expect(languageOfTag("zh-CN")).toBe("zh-Hans");
    expect(languageOfTag("zh-SG")).toBe("zh-Hans");
    expect(languageOfTag("zh-Hans-SG")).toBe("zh-Hans");
    expect(languageOfTag("zh-TW")).toBe("zh-Hant");
    expect(languageOfTag("zh-HK")).toBe("zh-Hant");
    expect(languageOfTag("zh-Hant-HK")).toBe("zh-Hant");
    expect(languageOfTag("zh")).toBe("zh-Hans");
    expect(languageOfTag("en-GB")).toBe("en");
    expect(languageOfTag("fr-FR")).toBeNull();
    expect(detectLanguage(["fr-FR", "zh-MO"])).toBe("zh-Hant");
    expect(detectLanguage(["fr-FR", "de"])).toBe("en");
    expect(detectLanguage([])).toBe("en");
  });

  it("renders Settings in the chosen language", () => {
    const client = new FakeClient();
    const snapshot = makeSnapshot();
    render(
      <I18nProvider preference="zh-Hans">
        <ClientContext.Provider value={client}>
          <Settings snapshot={snapshot} onPreferences={vi.fn()} onRefresh={vi.fn()} />
        </ClientContext.Provider>
      </I18nProvider>,
    );
    expect(screen.getByRole("heading", { name: "设置" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "通用" })).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "动态显示实时变化" })).toBeInTheDocument();
  });
});

import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ClientContext } from "../api/store";
import { formatBytes } from "../api/format";
import type { ProjectUsage, UsageNumbers } from "../api/types";
import { I18nProvider } from "../i18n";
import { Settings } from "../views/Settings";
import { FakeClient, emptyUsage, makeSnapshot } from "./fixtures";

function numbers(patch: Partial<UsageNumbers>): UsageNumbers {
  return { ...emptyUsage(), ...patch };
}

const tally = (count: number, bytes: number, local = bytes) => ({ count, bytes, local_bytes: local });

/** An archived project that could free its delivery attachments, and a live one that frees nothing. */
function twoProjects(): ProjectUsage[] {
  return [
    {
      project: "old",
      title: "Old launch",
      archived: true,
      ...numbers({
        total_bytes: 6_600_000,
        total_local_bytes: 6_600_000,
        event_bytes: 700_000,
        event_local_bytes: 700_000,
        attachments: tally(30, 5_900_000),
        reclaimable: tally(21, 3_100_000),
      }),
    },
    {
      project: "alpha",
      title: "Alpha project",
      archived: false,
      ...numbers({
        total_bytes: 2_400_000,
        total_local_bytes: 1_900_000,
        event_bytes: 400_000,
        event_local_bytes: 400_000,
        attachments: tally(12, 2_000_000, 1_500_000),
        superseded: tally(4, 800_000, 300_000),
        unreferenced: tally(1, 48_000),
      }),
    },
  ];
}

async function openStorage(client: FakeClient, language?: "zh-Hans" | "zh-Hant") {
  const settings = (
    <ClientContext.Provider value={client}>
      <Settings snapshot={makeSnapshot()} onPreferences={vi.fn()} onRefresh={vi.fn()} initialTab="storage" />
    </ClientContext.Provider>
  );
  render(language ? <I18nProvider preference={language}>{settings}</I18nProvider> : settings);
  return userEvent.setup();
}

function cellsOf(row: HTMLElement): string[] {
  return within(row)
    .getAllByRole("cell")
    .map((cell) => cell.textContent ?? "");
}

describe("storage usage", () => {
  it("lists each project's total, attachments and what a cleanup could free", async () => {
    const client = new FakeClient();
    const projects = twoProjects();
    client.storageUsage = {
      evaluated_at: "2026-11-10T00:00:00.000Z",
      superseded_after_days: 30,
      total: numbers({
        total_bytes: 9_000_000,
        total_local_bytes: 8_500_000,
        attachments: tally(42, 7_900_000, 7_400_000),
        reclaimable: tally(21, 3_100_000),
        superseded: tally(4, 800_000, 300_000),
        unreferenced: tally(1, 48_000),
      }),
      projects,
    };
    await openStorage(client);
    const usage = screen.getByRole("region", { name: "Usage" });
    const table = await within(usage).findByRole("table");
    expect(within(table).getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual([
      "Project",
      "Total",
      "Attachments",
      "Could free",
    ]);

    const old = within(table).getByRole("row", { name: /Old launch/ });
    expect(cellsOf(old)).toEqual(["6.6 MB", "5.9 MB", "3.1 MB"]);
    expect(within(old).getByText("Archived")).toBeInTheDocument();
    // Nothing else to say about it: no secondary line.
    expect(within(old).getByRole("rowheader").textContent).toBe("Old launchArchived");

    // A live project frees nothing; the other numbers sit quietly under its name.
    const alpha = within(table).getByRole("row", { name: /Alpha project/ });
    expect(cellsOf(alpha)).toEqual(["2.4 MB", "2.0 MB", "—"]);
    expect(within(alpha).getByText("Superseded 800 KB · Unreferenced 48 KB · On this computer 1.9 MB")).toBeInTheDocument();

    const all = within(table).getByRole("row", { name: /All projects/ });
    expect(cellsOf(all)).toEqual(["9.0 MB", "7.9 MB", "3.1 MB"]);

    // It measures and lists; there is nothing to press.
    expect(within(usage).queryByRole("button")).toBeNull();
    expect(client.order).toContain("get_storage_usage");
  });

  it("says so when there are no projects", async () => {
    const empty = new FakeClient();
    await openStorage(empty);
    const usage = screen.getByRole("region", { name: "Usage" });
    expect(await within(usage).findByText("No projects.")).toBeInTheDocument();
    expect(within(usage).queryByRole("table")).toBeNull();
  });

  it("reports a failed reading in place of the table", async () => {
    const client = new FakeClient();
    client.storageUsage = null;
    await openStorage(client);
    const usage = screen.getByRole("region", { name: "Usage" });
    expect(await within(usage).findByRole("alert")).toHaveTextContent("Could not measure the shared data folder.");
  });

  it("is translated, and a single project has no summary row", async () => {
    const client = new FakeClient();
    const [, alpha] = twoProjects();
    client.storageUsage = {
      evaluated_at: "2026-11-10T00:00:00.000Z",
      superseded_after_days: 30,
      total: alpha!,
      projects: [alpha!],
    };
    await openStorage(client, "zh-Hans");
    const usage = screen.getByRole("region", { name: "用量" });
    const table = await within(usage).findByRole("table");
    expect(within(table).getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual([
      "项目",
      "总计",
      "附件",
      "可回收",
    ]);
    expect(within(table).getByText("已被取代 800 KB · 未被引用 48 KB · 本机 1.9 MB")).toBeInTheDocument();
    expect(within(table).getAllByRole("row")).toHaveLength(2);
  });

  it("formats sizes in decimal units", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(999)).toBe("999 B");
    expect(formatBytes(1000)).toBe("1.0 KB");
    expect(formatBytes(48_000)).toBe("48 KB");
    expect(formatBytes(800_000)).toBe("800 KB");
    expect(formatBytes(6_640_000)).toBe("6.6 MB");
    expect(formatBytes(41_800_000_000)).toBe("41.8 GB");
  });
});

/**
 * Settings: everything Agendar keeps on this computer, in five tabs.
 * General (language, theme, motion), Agents (how each harness is connected,
 * the command line, the skill, observe-only hooks), Notifications, Storage
 * (shared data folder, usage per project, linked repositories) and About. Nothing here writes to
 * the shared Vault.
 */

import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import {
  Bell,
  BookOpen,
  Bot,
  Check,
  Clock,
  Copy,
  Database,
  Eye,
  FolderGit2,
  HardDrive,
  Info,
  Languages,
  Monitor,
  Package,
  Plug,
  RefreshCw,
  Send,
  ShieldCheck,
  SlidersHorizontal,
  Sparkles,
  SunMoon,
  Terminal,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";
import { errorCode } from "../api/client";
import { formatAgo, formatBytes, newRequestId } from "../api/format";
import { expectedFields, projectFields, projectPatch } from "../state/agenda";
import { useClient } from "../api/store";
import type {
  Binding,
  LocalConfig,
  Preferences,
  SetupLocation,
  Snapshot,
  StorageUsage,
  Theme,
  UnlinkRepoData,
  UsageNumbers,
  UpdateStatus,
} from "../api/types";
import { harnessLabel } from "../components/NowCards";
import { LANGUAGE_NAMES, LANGUAGE_OPTIONS, detectLanguage, systemLanguages, useT, type Translate } from "../i18n";
import type { MessageKey } from "../i18n/messages";
import { failureOf, useInstallTool, vaultPathProblem, type ToolConnection } from "../state/setup";

export type SettingsTab = "general" | "agents" | "notifications" | "storage" | "about";

export interface SettingsProps {
  snapshot: Snapshot;
  onPreferences: (preferences: Preferences) => void;
  onRefresh: () => void;
  initialTab?: SettingsTab;
}

const TABS: { id: SettingsTab; icon: LucideIcon; label: MessageKey }[] = [
  { id: "general", icon: SlidersHorizontal, label: "tab.general" },
  { id: "agents", icon: Bot, label: "tab.agents" },
  { id: "notifications", icon: Bell, label: "tab.notifications" },
  { id: "storage", icon: HardDrive, label: "tab.storage" },
  { id: "about", icon: Info, label: "tab.about" },
];

const THEMES: { value: Theme; label: MessageKey }[] = [
  { value: "system", label: "theme.system" },
  { value: "light", label: "theme.light" },
  { value: "dark", label: "theme.dark" },
];

type PreferencePatch = {
  theme?: Theme;
  motion?: boolean;
  notifications?: boolean;
  language?: NonNullable<Preferences["language"]>;
  update_check?: boolean;
};

function useReducedMotion(): boolean {
  const query = "(prefers-reduced-motion: reduce)";
  const [reduced, setReduced] = useState(
    () => typeof window !== "undefined" && window.matchMedia?.(query).matches === true,
  );
  useEffect(() => {
    const list = window.matchMedia?.(query);
    if (!list) return;
    const update = () => setReduced(list.matches);
    list.addEventListener?.("change", update);
    return () => list.removeEventListener?.("change", update);
  }, []);
  return reduced;
}

const ICON_SIZE = 16;

/** A titled card of rows. */
function Group({ id, title, aside, children }: { id: string; title: string; aside?: ReactNode; children: ReactNode }) {
  return (
    <section className="sgroup" aria-labelledby={id}>
      <div className="sgroup-h">
        <h3 id={id}>{title}</h3>
        {aside}
      </div>
      <div className="sgroup-card">{children}</div>
    </section>
  );
}

/** One setting: icon, label and hint on the left, the control on the right. */
function Row({
  icon: Glyph,
  label,
  hint,
  children,
  below,
}: {
  icon: LucideIcon;
  label: ReactNode;
  hint?: ReactNode;
  children?: ReactNode;
  below?: ReactNode;
}) {
  return (
    <div className="srow2">
      <div className="srow-line">
        <span className="srow-ic" aria-hidden="true">
          <Glyph size={ICON_SIZE} strokeWidth={1.9} />
        </span>
        <div className="srow-main">
          <div className="srow-label">{label}</div>
          {hint ? <div className="srow-hint">{hint}</div> : null}
        </div>
        {children ? <div className="srow-ctl">{children}</div> : null}
      </div>
      {below ? <div className="srow-below">{below}</div> : null}
    </div>
  );
}

function Badge({ tone, children }: { tone: "ok" | "warn" | "err" | "muted"; children: ReactNode }) {
  const Glyph = tone === "ok" ? Check : tone === "warn" ? RefreshCw : tone === "err" ? TriangleAlert : null;
  return (
    <span className={`sbadge ${tone}`}>
      {Glyph ? <Glyph size={12} strokeWidth={2.2} aria-hidden="true" /> : null}
      {children}
    </span>
  );
}

function Switch({
  checked,
  label,
  disabled,
  onChange,
}: {
  checked: boolean;
  label: string;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      className="sswitch"
      disabled={disabled}
      onClick={() => onChange(!checked)}
    >
      <span className="sswitch-knob" aria-hidden="true" />
    </button>
  );
}

function Path({ value, empty }: { value?: string | null | undefined; empty: string }) {
  return value ? <code className="spath">{value}</code> : <span className="muted">{empty}</span>;
}

function CopyButton({ text }: { text: string }) {
  const t = useT();
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="btn sm"
      onClick={() => {
        void navigator.clipboard?.writeText(text).then(() => setCopied(true));
      }}
    >
      {copied ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
      {copied ? t("action.copied") : t("action.copy")}
    </button>
  );
}

export function Settings({ snapshot, onPreferences, onRefresh, initialTab = "general" }: SettingsProps) {
  const client = useClient();
  const t = useT();
  const preferences = snapshot.preferences;
  const [tab, setTab] = useState<SettingsTab>(initialTab);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const tabRefs = useRef<Record<string, HTMLButtonElement | null>>({});

  async function save(patch: PreferencePatch): Promise<void> {
    if (saving) return;
    setSaving(true);
    setProblem(null);
    try {
      const data = await client.setPreferences({
        request_id: newRequestId(),
        expected_revision: preferences.revision,
        ...patch,
      });
      onPreferences(data.preferences);
    } catch (cause) {
      setProblem(failureOf(cause).message);
      onRefresh();
    } finally {
      setSaving(false);
    }
  }

  function onTabKey(event: KeyboardEvent<HTMLDivElement>): void {
    const index = TABS.findIndex((item) => item.id === tab);
    const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    if (!step) return;
    event.preventDefault();
    const next = TABS[(index + step + TABS.length) % TABS.length]!;
    setTab(next.id);
    tabRefs.current[next.id]?.focus();
  }

  return (
    <div className="view settings">
      <div className="phead">
        <h2>{t("settings.title")}</h2>
      </div>

      <div className="stabs" role="tablist" aria-label={t("settings.title")} onKeyDown={onTabKey}>
        {TABS.map(({ id, icon: Glyph, label }) => (
          <button
            key={id}
            type="button"
            role="tab"
            id={`stab-${id}`}
            aria-selected={tab === id}
            aria-controls={`spanel-${id}`}
            tabIndex={tab === id ? 0 : -1}
            className="stab"
            ref={(node) => {
              tabRefs.current[id] = node;
            }}
            onClick={() => setTab(id)}
          >
            <Glyph size={17} strokeWidth={1.9} aria-hidden="true" />
            <span>{t(label)}</span>
          </button>
        ))}
      </div>

      {problem ? (
        <div className="alert" role="alert">
          <TriangleAlert size={ICON_SIZE} aria-hidden="true" />
          <div className="body">
            <span>{problem}</span>
          </div>
        </div>
      ) : null}

      <div role="tabpanel" id={`spanel-${tab}`} aria-labelledby={`stab-${tab}`} className="spanel">
        {tab === "general" ? <GeneralTab preferences={preferences} saving={saving} save={save} /> : null}
        {tab === "agents" ? <AgentsTab /> : null}
        {tab === "notifications" ? (
          <NotificationsTab snapshot={snapshot} saving={saving} save={save} />
        ) : null}
        {tab === "storage" ? <StorageTab snapshot={snapshot} onRefresh={onRefresh} /> : null}
        {tab === "about" ? (
          <AboutTab snapshot={snapshot} saving={saving} save={save} onRefresh={onRefresh} />
        ) : null}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// General

function GeneralTab({
  preferences,
  saving,
  save,
}: {
  preferences: Preferences;
  saving: boolean;
  save: (patch: PreferencePatch) => Promise<void>;
}) {
  const t = useT();
  const reduced = useReducedMotion();
  const detected = LANGUAGE_NAMES[detectLanguage(systemLanguages())];
  const language = preferences.language ?? "auto";
  return (
    <>
      <Group id="settings-language" title={t("general.language")}>
        <Row
          icon={Languages}
          label={t("general.language.display")}
          hint={t("general.language.detected", { name: detected })}
        >
          <select
            className="sselect"
            aria-label={t("general.language")}
            value={language}
            disabled={saving}
            onChange={(event) => void save({ language: event.target.value as PreferencePatch["language"] })}
          >
            {LANGUAGE_OPTIONS.map((option) => (
              <option key={option} value={option}>
                {option === "auto" ? t("general.language.auto") : LANGUAGE_NAMES[option]}
              </option>
            ))}
          </select>
        </Row>
      </Group>
      <Group id="settings-appearance" title={t("general.appearance")}>
        <Row icon={SunMoon} label={t("general.theme")}>
          <div className="seg" role="radiogroup" aria-label={t("general.theme")}>
            {THEMES.map((theme) => (
              <label key={theme.value} className="seg-opt" data-checked={preferences.theme === theme.value}>
                <input
                  type="radio"
                  name="settings-theme"
                  value={theme.value}
                  checked={preferences.theme === theme.value}
                  disabled={saving}
                  onChange={() => void save({ theme: theme.value })}
                />
                {t(theme.label)}
              </label>
            ))}
          </div>
        </Row>
        <Row
          icon={Sparkles}
          label={t("general.motion")}
          hint={reduced ? t("general.motion.reduced") : t("general.motion.hint")}
        >
          <Switch
            checked={preferences.motion}
            label={t("general.motion")}
            disabled={saving}
            onChange={(motion) => void save({ motion })}
          />
        </Row>
      </Group>
    </>
  );
}

// ---------------------------------------------------------------------------
// Agents

const SKILL_COMMAND = "npx skills add Maolon/agendar -g";

/** How long a reporting agent counts as connected without hooks or MCP. */
const SEEN_DAYS = 7;

/** Reported through the skill or CLI lately, with no hooks or MCP entry. */
function connectedBySkill(binding: Binding, now: number): boolean {
  if (binding.state !== "not_bound" || !binding.last_seen) return false;
  const seen = Date.parse(binding.last_seen);
  return !Number.isNaN(seen) && now - seen <= SEEN_DAYS * 24 * 60 * 60 * 1000;
}

function bindingTone(state: Binding["state"]): "ok" | "warn" | "err" | "muted" {
  return state === "bound" ? "ok" : state === "stale" ? "warn" : state === "unreadable" ? "err" : "muted";
}

function AgentsTab() {
  const client = useClient();
  const t = useT();
  const [bindings, setBindings] = useState<Binding[] | null | undefined>(undefined);
  const [busy, setBusy] = useState<string | null>(null);
  const [notes, setNotes] = useState<Record<string, { text: string; error: boolean }>>({});
  const cli = useInstallTool(client, "path_link", true);
  const observe = useInstallTool(client, "observe_hooks", true);

  useEffect(() => {
    let cancelled = false;
    client
      .getBindings()
      .then((data) => {
        if (!cancelled) setBindings(data.bindings);
      })
      .catch(() => {
        if (!cancelled) setBindings(null);
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  async function act(binding: Binding, action: "bind" | "unbind"): Promise<void> {
    setBusy(binding.harness);
    try {
      const data = await client.setBinding({ harness: binding.harness, action });
      setBindings((current) =>
        current?.map((item) => (item.harness === binding.harness ? data.binding : item)) ?? current,
      );
      const name = harnessLabel(binding.harness);
      setNotes((current) => ({
        ...current,
        [binding.harness]: {
          text: binding.harness === "codex" && action === "bind" ? t("agents.next.codex") : t("agents.next.restart", { name }),
          error: false,
        },
      }));
    } catch (cause) {
      setNotes((current) => ({ ...current, [binding.harness]: { text: failureOf(cause).message, error: true } }));
    } finally {
      setBusy(null);
    }
  }

  return (
    <>
      <Group
        id="settings-connections"
        title={t("agents.connections")}
        aside={<span className="ghint">{t("agents.connections.hint")}</span>}
      >
        {bindings === undefined ? (
          <p className="srow-empty" role="status">
            {t("agents.loading")}
          </p>
        ) : bindings === null ? (
          <p className="srow-empty" role="alert">
            {t("agents.load_error")}
          </p>
        ) : (
          bindings.map((binding) => {
            const name = harnessLabel(binding.harness);
            const note = notes[binding.harness];
            const kind = t(`kind.${binding.kind}` as MessageKey);
            const now = Date.now();
            const bySkill = connectedBySkill(binding, now);
            const connectLabel = bySkill
              ? t(binding.kind === "mcp" ? "action.add_mcp" : "action.add_hooks")
              : binding.state === "stale"
                ? t("action.reconnect")
                : t("action.connect");
            return (
              <Row
                key={binding.harness}
                icon={binding.kind === "hooks" ? Plug : binding.kind === "mcp" ? Package : Terminal}
                label={
                  <>
                    {name}
                    <span className="skind">{kind}</span>
                  </>
                }
                hint={binding.found ? <Path value={binding.path} empty="" /> : t("harness.not_found")}
                below={
                  note ? (
                    <p className={`srow-note${note.error ? " err" : ""}`} role={note.error ? "alert" : "status"}>
                      {note.text}
                    </p>
                  ) : binding.state === "manual" && binding.detail ? (
                    <div className="srow-copy">
                      <code className="spath">{binding.detail}</code>
                      <CopyButton text={binding.detail} />
                    </div>
                  ) : bySkill ? (
                    <p className="srow-note" role="status">
                      {t("agents.via_skill", { ago: formatAgo(binding.last_seen, new Date(now).toISOString()) })}
                    </p>
                  ) : null
                }
              >
                <Badge tone={bySkill ? "ok" : bindingTone(binding.state)}>
                  {bySkill ? t("state.bound") : t(`state.${binding.state}` as MessageKey)}
                </Badge>
                {binding.state === "bound" ? (
                  <button
                    type="button"
                    className="btn sm ghost"
                    aria-label={`${t("action.disconnect")} ${name}`}
                    disabled={busy !== null}
                    onClick={() => void act(binding, "unbind")}
                  >
                    {t("action.disconnect")}
                  </button>
                ) : binding.state === "not_bound" || binding.state === "stale" ? (
                  <button
                    type="button"
                    className={bySkill ? "btn sm ghost" : "btn sm"}
                    aria-label={`${connectLabel} ${name}`}
                    disabled={busy !== null || !binding.found}
                    onClick={() => void act(binding, "bind")}
                  >
                    {connectLabel}
                  </button>
                ) : null}
              </Row>
            );
          })
        )}
      </Group>

      <Group id="settings-tools" title={t("agents.tools")}>
        <InstallRow
          icon={Terminal}
          label={t("agents.cli")}
          hint={t("agents.cli.hint")}
          tool={cli}
          on={t("agents.cli.installed")}
          off={t("agents.cli.missing")}
          action={t("action.install")}
          confirm={(path) => t("agents.cli.confirm", { path })}
        />
        <Row icon={BookOpen} label={t("agents.skill")} hint={t("agents.skill.hint")} below={
          <div className="srow-copy">
            <code className="spath">{SKILL_COMMAND}</code>
            <CopyButton text={SKILL_COMMAND} />
          </div>
        } />
        <InstallRow
          icon={Eye}
          label={t("agents.observe")}
          hint={t("agents.observe.hint")}
          tool={observe}
          on={t("agents.observe.on")}
          off={t("agents.observe.off")}
          action={t("action.turn_on")}
          confirm={(path) => t("agents.observe.confirm", { path })}
        />
      </Group>
    </>
  );
}

/** A reviewed local install (PATH link, observe hooks): status, then an
 *  explicit confirm that shows where it writes. A dry run only, until then. */
function InstallRow({
  icon,
  label,
  hint,
  tool,
  on,
  off,
  action,
  confirm,
}: {
  icon: LucideIcon;
  label: string;
  hint: string;
  tool: ToolConnection;
  on: string;
  off: string;
  action: string;
  confirm: (path: string) => string;
}) {
  const t = useT();
  const plan = tool.plan.status === "ready" ? tool.plan.value : undefined;
  const installed = tool.detection === "detected" || tool.detection === "connected";
  const unknown = tool.detection === "unverified";
  const target = plan?.destination ?? plan?.settings_path ?? "";
  return (
    <Row
      icon={icon}
      label={label}
      hint={hint}
      below={
        tool.checked && !installed && plan ? (
          <div className="srow-confirm">
            <span>{confirm(target)}</span>
            <button type="button" className="btn sm ghost" onClick={() => tool.setChecked(false)}>
              {t("action.cancel")}
            </button>
            <button
              type="button"
              className="btn sm primary"
              disabled={!tool.canInstall}
              onClick={() => void tool.install()}
            >
              {action}
            </button>
          </div>
        ) : tool.action.status === "error" ? (
          <p className="srow-note err" role="alert">
            {tool.action.message}
          </p>
        ) : null
      }
    >
      <Badge tone={installed ? "ok" : unknown ? "err" : "muted"}>
        {installed ? on : unknown ? t("agents.cli.unknown") : off}
      </Badge>
      {!installed && !unknown && !tool.checked ? (
        <button type="button" className="btn sm" aria-label={`${action} ${label}`} onClick={() => tool.setChecked(true)}>
          {action}
        </button>
      ) : null}
    </Row>
  );
}

// ---------------------------------------------------------------------------
// Notifications

const ON_MAC = typeof navigator !== "undefined" && /Mac/.test(navigator.userAgent);

function NotificationsTab({
  snapshot,
  saving,
  save,
}: {
  snapshot: Snapshot;
  saving: boolean;
  save: (patch: PreferencePatch) => Promise<void>;
}) {
  const client = useClient();
  const t = useT();
  const local = snapshot.local;
  const where = ON_MAC ? t("notifications.where.mac") : t("notifications.where.other");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  async function sendTest(): Promise<void> {
    setBusy(true);
    try {
      const sent = await client.testNotice();
      setResult(
        sent.sent
          ? t("notifications.test.sent", { where })
          : t("notifications.test.failed", { reason: sent.error ?? "—" }),
      );
    } catch {
      setResult(t("notifications.test.unreachable"));
    } finally {
      setBusy(false);
    }
  }
  const permission =
    local.notifications === "granted"
      ? t("notifications.system.granted")
      : local.notifications === "denied"
        ? t("notifications.system.denied", { where })
        : t("notifications.system.unknown");
  return (
    <Group id="settings-notifications" title={t("tab.notifications")}>
      <Row icon={Bell} label={t("notifications.desktop")} hint={t("notifications.desktop.hint")}>
        <Switch
          checked={snapshot.preferences.notifications === true}
          label={t("notifications.desktop")}
          disabled={saving}
          onChange={(notifications) => void save({ notifications })}
        />
      </Row>
      <Row
        icon={ShieldCheck}
        label={t("notifications.system")}
        below={
          local.notification_error ? (
            <p className="srow-note err" role="alert">
              {t("notifications.last_error", { error: local.notification_error })}
            </p>
          ) : null
        }
      >
        <Badge tone={local.notifications === "granted" ? "ok" : local.notifications === "denied" ? "err" : "muted"}>
          {permission}
        </Badge>
      </Row>
      <Row
        icon={Send}
        label={t("notifications.test")}
        below={
          result ? (
            <p className="srow-note" role="status">
              {result}
            </p>
          ) : null
        }
      >
        <button type="button" className="btn sm" disabled={busy} onClick={() => void sendTest()}>
          {t("notifications.test")}
        </button>
      </Row>
    </Group>
  );
}

// ---------------------------------------------------------------------------
// Storage

function StorageTab({ snapshot, onRefresh }: { snapshot: Snapshot; onRefresh: () => void }) {
  const client = useClient();
  const t = useT();
  const [config, setConfig] = useState<LocalConfig | null | undefined>(undefined);
  const [usage, setUsage] = useState<StorageUsage | null | undefined>(undefined);
  const [tick, setTick] = useState(0);
  const reload = () => setTick((value) => value + 1);
  const [vaultOpen, setVaultOpen] = useState(false);
  const [locations, setLocations] = useState<SetupLocation[]>([]);
  const [vaultChoice, setVaultChoice] = useState("custom");
  const [vaultPath, setVaultPath] = useState("");
  const [vaultCreate, setVaultCreate] = useState(false);
  const [vaultBusy, setVaultBusy] = useState(false);
  const [vaultNote, setVaultNote] = useState<{ text: string; error: boolean } | null>(null);
  const [repoPath, setRepoPath] = useState("");
  const [repoProject, setRepoProject] = useState("");
  const [repoNote, setRepoNote] = useState<{ text: string; error: boolean } | null>(null);

  /** The config changed elsewhere (an agent start, the CLI): say so in the user's language. */
  const storageFailure = (cause: unknown): string =>
    errorCode(cause) === "E_STALE_ANSWER" ? t("storage.changed") : failureOf(cause).message;

  // Agents and the CLI write this config too; re-read it whenever the user
  // comes back to the window, so the list is current before a click.
  useEffect(() => {
    const refresh = () => {
      if (document.visibilityState === "visible") reload();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    client
      .getLocalConfig()
      .then((data) => {
        if (!cancelled) setConfig(data);
      })
      .catch(() => {
        if (!cancelled) setConfig(null);
      });
    return () => {
      cancelled = true;
    };
  }, [client, tick]);

  // The numbers `agendar prune` prints. The last reading stays up while the
  // next one is taken.
  useEffect(() => {
    let cancelled = false;
    client
      .getStorageUsage()
      .then((data) => {
        if (!cancelled) setUsage(data);
      })
      .catch(() => {
        if (!cancelled) setUsage(null);
      });
    return () => {
      cancelled = true;
    };
  }, [client, tick]);

  useEffect(() => {
    if (!vaultOpen) return;
    let cancelled = false;
    client
      .getSetupLocations()
      .then((data) => {
        if (!cancelled) setLocations(data.locations);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [client, vaultOpen]);

  async function switchVault(): Promise<void> {
    if (!config || vaultBusy) return;
    const location = locations.find((item) => item.id === vaultChoice);
    const path = (location ? location.path : vaultPath).trim();
    const invalid = vaultPathProblem(path);
    if (invalid) {
      setVaultNote({ text: invalid, error: true });
      return;
    }
    setVaultBusy(true);
    setVaultNote(null);
    try {
      await client.configureVault({
        request_id: newRequestId(),
        expected_config_revision: config.config_revision,
        path,
        create: location ? true : vaultCreate,
        consent: true,
      });
      setVaultNote({ text: t("storage.switched", { path }), error: false });
      setVaultOpen(false);
      reload();
      onRefresh();
    } catch (cause) {
      setVaultNote({ text: storageFailure(cause), error: true });
      reload();
    } finally {
      setVaultBusy(false);
    }
  }

  /**
   * Unlink a folder; when no other folder here links its project, archive the
   * project too, as a human edit (decided by the user over an Archive offer).
   */
  async function unlink(path: string, slug: string): Promise<void> {
    setRepoNote(null);
    let data: UnlinkRepoData;
    try {
      data = await client.unlinkRepo({ request_id: newRequestId(), repo: path, expected_project: slug });
    } catch (cause) {
      setRepoNote({ text: storageFailure(cause), error: true });
      reload();
      return;
    }
    const project = snapshot.state.projects[slug];
    if (!project || project.summary.archived || data.project_still_linked) {
      setRepoNote({ text: t("repos.unlinked", { path }), error: false });
      reload();
      return;
    }
    const title = project.summary.title;
    const edit = { archived: true };
    try {
      await client.updateProject({
        request_id: newRequestId(),
        project: slug,
        expected_fields: expectedFields(project.field_revisions, projectFields(edit)),
        patch: projectPatch(edit),
      });
      setRepoNote({ text: t("repos.unlinked_archived", { path, project: title }), error: false });
      onRefresh();
    } catch (cause) {
      setRepoNote({
        text: t("repos.unlinked_not_archived", { path, project: title, error: failureOf(cause).message }),
        error: true,
      });
    }
    reload();
  }

  async function link(): Promise<void> {
    const path = repoPath.trim();
    const project = repoProject || Object.keys(snapshot.state.projects)[0] || "";
    if (!path || !project) {
      setRepoNote({ text: t("repos.missing"), error: true });
      return;
    }
    setRepoNote(null);
    try {
      const data = await client.linkRepo({
        request_id: newRequestId(),
        repo: path,
        project,
      });
      setRepoNote({
        text: t("repos.linked", {
          repo: data.repo,
          project: snapshot.state.projects[data.project]?.summary.title ?? data.project,
        }),
        error: false,
      });
      setRepoPath("");
    } catch (cause) {
      setRepoNote({ text: failureOf(cause).message, error: true });
    }
    reload();
  }

  const repos = Object.entries(config?.repos ?? {}).sort(([a], [b]) => a.localeCompare(b));

  return (
    <>
      <Group id="settings-locations" title={t("storage.locations")}>
        {config === undefined ? (
          <p className="srow-empty" role="status">
            {t("storage.reading")}
          </p>
        ) : config === null ? (
          <p className="srow-empty" role="alert">
            {t("storage.read_error")}
          </p>
        ) : (
          <>
            <Row
              icon={Database}
              label={t("storage.vault")}
              hint={<Path value={config.vault} empty={t("storage.not_set")} />}
              below={
                <>
                  {vaultNote ? (
                    <p className={`srow-note${vaultNote.error ? " err" : ""}`} role={vaultNote.error ? "alert" : "status"}>
                      {vaultNote.text}
                    </p>
                  ) : null}
                  {vaultOpen ? (
                    <fieldset className="sform" disabled={vaultBusy}>
                      <legend className="srow-label">{t("storage.change.title")}</legend>
                      {locations.map((location) => (
                        <label key={location.id} className="setup-check">
                          <input
                            type="radio"
                            name="settings-vault"
                            checked={vaultChoice === location.id}
                            onChange={() => setVaultChoice(location.id)}
                          />
                          {location.label} <code className="spath">{location.path}</code>
                        </label>
                      ))}
                      <label className="setup-check">
                        <input
                          type="radio"
                          name="settings-vault"
                          checked={vaultChoice === "custom"}
                          onChange={() => setVaultChoice("custom")}
                        />
                        {t("storage.another")}
                      </label>
                      {vaultChoice === "custom" ? (
                        <>
                          <input
                            type="text"
                            className="setup-input"
                            aria-label={t("storage.path")}
                            placeholder={t("storage.path.placeholder")}
                            value={vaultPath}
                            onChange={(event) => setVaultPath(event.target.value)}
                          />
                          <label className="setup-check">
                            <input
                              type="checkbox"
                              checked={vaultCreate}
                              onChange={(event) => setVaultCreate(event.target.checked)}
                            />
                            {t("storage.create")}
                          </label>
                        </>
                      ) : null}
                      <p className="srow-hint">{t("storage.keep")}</p>
                      <div className="acts">
                        <button type="button" className="btn sm ghost" onClick={() => setVaultOpen(false)}>
                          {t("action.cancel")}
                        </button>
                        <button type="button" className="btn sm primary" onClick={() => void switchVault()}>
                          {vaultBusy ? t("storage.switching") : t("storage.switch")}
                        </button>
                      </div>
                    </fieldset>
                  ) : null}
                </>
              }
            >
              {vaultOpen ? null : (
                <button type="button" className="btn sm" onClick={() => setVaultOpen(true)}>
                  {t("storage.change")}
                </button>
              )}
            </Row>
            <Row icon={HardDrive} label={t("storage.data")} hint={<Path value={config.data_dir} empty={t("storage.unknown")} />} />
            <Row icon={Monitor} label={t("storage.host")} hint={<Path value={snapshot.local.host} empty={t("storage.unknown")} />} />
          </>
        )}
      </Group>

      {usage === null && !config?.vault ? null : (
        <Group id="settings-usage" title={t("usage.title")}>
          {usage === undefined ? (
            <p className="srow-empty" role="status">
              {t("usage.reading")}
            </p>
          ) : usage === null ? (
            <p className="srow-empty" role="alert">
              {t("usage.read_error")}
            </p>
          ) : usage.projects.length === 0 ? (
            <p className="srow-empty">{t("usage.none")}</p>
          ) : (
            <UsageTable usage={usage} />
          )}
        </Group>
      )}

      <Group
        id="settings-repos"
        title={t("repos.title")}
        aside={repos.length ? <span className="ghint">{t("repos.count", { n: repos.length })}</span> : null}
      >
        {repos.length ? (
          repos.map(([path, slug]) => (
            <Row
              key={path}
              icon={FolderGit2}
              label={<span className="srepo-project">{snapshot.state.projects[slug]?.summary.title ?? slug}</span>}
              hint={<code className="spath">{path}</code>}
            >
              <button
                type="button"
                className="btn sm ghost"
                aria-label={t("repos.unlink.label", { path })}
                onClick={() => void unlink(path, slug)}
              >
                {t("repos.unlink")}
              </button>
            </Row>
          ))
        ) : (
          <p className="srow-empty">{t("repos.none")}</p>
        )}
        <div className="srow2">
          <div className="slink">
            <input
              type="text"
              className="setup-input"
              aria-label={t("repos.folder")}
              placeholder={t("repos.folder.placeholder")}
              value={repoPath}
              onChange={(event) => setRepoPath(event.target.value)}
            />
            <select
              aria-label={t("repos.project")}
              className="sselect"
              value={repoProject || Object.keys(snapshot.state.projects)[0] || ""}
              onChange={(event) => setRepoProject(event.target.value)}
            >
              {Object.values(snapshot.state.projects)
                .filter((project) => !project.summary.archived)
                .map((project) => (
                  <option key={project.summary.slug} value={project.summary.slug}>
                    {project.summary.title}
                  </option>
                ))}
            </select>
            <button type="button" className="btn sm" onClick={() => void link()}>
              {t("repos.link")}
            </button>
          </div>
          {repoNote ? (
            <p className={`srow-note${repoNote.error ? " err" : ""}`} role={repoNote.error ? "alert" : "status"}>
              {repoNote.text}
            </p>
          ) : null}
        </div>
      </Group>
    </>
  );
}

/** The quiet line under a row: only the numbers that are not zero. */
function usageNotes(numbers: UsageNumbers, t: Translate): string {
  const notes: string[] = [];
  if (numbers.superseded.bytes > 0) notes.push(t("usage.superseded", { size: formatBytes(numbers.superseded.bytes) }));
  if (numbers.unreferenced.bytes > 0)
    notes.push(t("usage.unreferenced", { size: formatBytes(numbers.unreferenced.bytes) }));
  if (numbers.total_local_bytes !== numbers.total_bytes)
    notes.push(t("usage.local", { size: formatBytes(numbers.total_local_bytes) }));
  return notes.join(" · ");
}

/** One row per project, largest first: total, attachments, what a cleanup could free. */
function UsageTable({ usage }: { usage: StorageUsage }) {
  const t = useT();
  const cells = (numbers: UsageNumbers) => (
    <>
      <td>{formatBytes(numbers.total_bytes)}</td>
      <td>{formatBytes(numbers.attachments.bytes)}</td>
      <td className={numbers.reclaimable.bytes > 0 ? "susage-free" : "susage-none"}>
        {numbers.reclaimable.bytes > 0 ? formatBytes(numbers.reclaimable.bytes) : "—"}
      </td>
    </>
  );
  const name = (label: ReactNode, numbers: UsageNumbers) => {
    const notes = usageNotes(numbers, t);
    return (
      <th scope="row">
        <span className="susage-name">{label}</span>
        {notes ? <span className="susage-notes">{notes}</span> : null}
      </th>
    );
  };
  return (
    <table className="susage">
      <thead>
        <tr>
          <th scope="col">{t("usage.project")}</th>
          <th scope="col">{t("usage.total")}</th>
          <th scope="col">{t("usage.attachments")}</th>
          <th scope="col">{t("usage.free")}</th>
        </tr>
      </thead>
      <tbody>
        {usage.projects.map((project) => (
          <tr key={project.project}>
            {name(
              <>
                {project.title ?? project.project}
                {project.archived ? <span className="archived-tag">{t("search.archived")}</span> : null}
              </>,
              project,
            )}
            {cells(project)}
          </tr>
        ))}
      </tbody>
      {usage.projects.length > 1 ? (
        <tfoot>
          <tr>
            {name(t("usage.all"), usage.total)}
            {cells(usage.total)}
          </tr>
        </tfoot>
      ) : null}
    </table>
  );
}

// ---------------------------------------------------------------------------
// About

function AboutTab({
  snapshot,
  saving,
  save,
  onRefresh,
}: {
  snapshot: Snapshot;
  saving: boolean;
  save: (patch: PreferencePatch) => Promise<void>;
  onRefresh: () => void;
}) {
  const client = useClient();
  const t = useT();
  const [config, setConfig] = useState<LocalConfig | null>(null);
  useEffect(() => {
    let cancelled = false;
    client
      .getLocalConfig()
      .then((data) => {
        if (!cancelled) setConfig(data);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [client]);
  return (
    <>
      <Group id="settings-about" title={t("tab.about")}>
        <Row
          icon={Info}
          label={t("about.version")}
          hint={<Path value={config?.app_version ?? snapshot.update?.current} empty={t("storage.unknown")} />}
        />
        <Row icon={Terminal} label={t("about.cli")} hint={<Path value={config?.cli_path} empty={t("about.cli.missing")} />} />
        <Row icon={ShieldCheck} label={t("about.protocol")} hint={t("about.protocol.value")} />
      </Group>
      {snapshot.update ? (
        <UpdatesGroup
          status={snapshot.update}
          enabled={snapshot.preferences.update_check !== false}
          saving={saving}
          save={save}
          onRefresh={onRefresh}
        />
      ) : null}
    </>
  );
}

/** Release channel, latest public release, last check and the switch. */
function UpdatesGroup({
  status: cached,
  enabled,
  saving,
  save,
  onRefresh,
}: {
  status: UpdateStatus;
  enabled: boolean;
  saving: boolean;
  save: (patch: PreferencePatch) => Promise<void>;
  onRefresh: () => void;
}) {
  const client = useClient();
  const t = useT();
  const [fresh, setFresh] = useState<UpdateStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  // A newer cached check (from the snapshot) wins over the last manual result.
  // A running check (pushed live by the host) always shows.
  const status = {
    ...(fresh && (fresh.checked_at ?? "") >= (cached.checked_at ?? "") ? fresh : cached),
    checking: cached.checking,
  };
  const failure = problem ?? (status.error ? t("update.failed", { error: status.error }) : null);
  async function checkNow(): Promise<void> {
    setBusy(true);
    setProblem(null);
    try {
      const data = await client.checkUpdates();
      setFresh(data.update);
      onRefresh();
    } catch (cause) {
      setProblem(t("update.failed", { error: failureOf(cause).message }));
    } finally {
      setBusy(false);
    }
  }
  const release = status.channel === "release";
  return (
    <Group id="settings-updates" title={t("update.title")}>
      <Row
        icon={Package}
        label={t("update.channel")}
        hint={release ? t("update.channel.release") : t("update.channel.dev")}
      />
      <Row
        icon={Sparkles}
        label={t("update.latest")}
        hint={
          status.latest
            ? t("update.latest.value", {
                version: status.latest,
                date: (status.published_at ?? "").slice(0, 10) || "—",
              })
            : t("update.latest.unknown")
        }
      >
        {release && status.latest ? (
          status.available ? (
            <Badge tone="warn">{t("update.available", { version: status.latest })}</Badge>
          ) : (
            <Badge tone="ok">{t("update.up_to_date")}</Badge>
          )
        ) : null}
        {status.url ? (
          <button type="button" className="btn sm" onClick={() => void client.openRelease().catch(() => undefined)}>
            {t("update.whats_new")}
          </button>
        ) : null}
      </Row>
      <Row
        icon={Clock}
        label={t("update.checked")}
        hint={
          status.checked_at
            ? formatAgo(status.checked_at, new Date().toISOString())
            : t("update.checked.never")
        }
        below={
          failure ? (
            <p className="srow-note err" role="alert">
              {failure}
            </p>
          ) : null
        }
      >
        <button
          type="button"
          className="btn sm"
          disabled={!enabled || busy || status.checking === true}
          onClick={() => void checkNow()}
        >
          {busy ? t("update.checking") : t("update.check_now")}
        </button>
      </Row>
      <Row icon={RefreshCw} label={t("update.auto")}>
        <Switch
          checked={enabled}
          label={t("update.auto")}
          disabled={saving}
          onChange={(update_check) => void save({ update_check })}
        />
      </Row>
    </Group>
  );
}

// Typed helper kept for callers that want the translate function shape.
export type { Translate };

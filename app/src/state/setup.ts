/**
 * First-run Setup state. The Vault step keeps its form in `components/Setup.tsx`.
 * Each optional connection here owns its own dry-run plan, explicit selection
 * and action, so one failure never hides, blocks or re-consents another. Nothing
 * in this module writes unless a user presses the matching button.
 */

import { tx } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { isApiError, type AgendarClient } from "../api/client";
import { newRequestId } from "../api/format";
import type {
  InstallLocalRequest,
  InstallPlan,
  NotificationPermission,
  Preferences,
} from "../api/types";

/** A dry-run plan: loading, ready, or failed with a message for its own item. */
export type LoadState<T> =
  | { status: "loading" }
  | { status: "ready"; value: T }
  | { status: "error"; message: string; hint?: string };

/** One explicit action: idle, running, finished (with what changed), or failed. */
export type ActionState =
  | { status: "idle" }
  | { status: "pending" }
  | { status: "done"; detail?: string }
  | { status: "error"; message: string; hint?: string };

/** The message and hint a user needs from a failed call. */
export interface Failure {
  message: string;
  hint?: string;
}

/** Generic operator hints that describe the backend, not the setup step. */
const INTERNAL_HINT =
  /fix the input and retry without deleting logs|rebuild|agendar_cli_path|bundle:cli/i;

/** Drop internal hints. The message itself still names what failed. */
export function setupHint(hint: string | undefined): string | undefined {
  const text = hint?.trim();
  if (!text || INTERNAL_HINT.test(text)) return undefined;
  return text;
}

export function failureOf(cause: unknown): Failure {
  if (isApiError(cause)) {
    const hint = setupHint(cause.hint);
    return hint ? { message: cause.message, hint } : { message: cause.message };
  }
  return { message: cause instanceof Error ? cause.message : tx("setup.failed") };
}

const ABSOLUTE_PATH = /^(\/|[A-Za-z]:[\\/]|\\\\)/;
const TOP_OF_DISK = /^(\/+|[A-Za-z]:[\\/]*)$/;

/**
 * Why a typed Vault path cannot be used, or null. A relative path would resolve
 * against the app's working directory and `~` is not expanded, so both are
 * refused here, before `configure_vault` is called.
 */
export function vaultPathProblem(value: string): string | null {
  const path = value.trim();
  if (path === "") return tx("setup.path.empty");
  if (path.startsWith("~")) return tx("setup.path.tilde");
  if (!ABSOLUTE_PATH.test(path)) return tx("setup.path.relative");
  if (TOP_OF_DISK.test(path)) return tx("setup.path.root");
  return null;
}

/** `hooks` is the advanced Claude Code integration, offered only in Settings. */
export type InstallTool = "path_link" | "hooks" | "observe_hooks";

function toolFields(tool: InstallTool): Pick<InstallLocalRequest, "kind" | "harness"> {
  return tool === "path_link" ? { kind: tool } : { kind: tool, harness: "claude_code" };
}

/**
 * What a read-only check could see. `detected` was already present.
 * `connected` was completed in this run. `unverified` must not be claimed
 * present or absent. None of these values is consent to write.
 */
export type Detection = "detected" | "connected" | "absent" | "unverified";

/**
 * Dry-run result, or a placement this run just finished. `connected` is this
 * run. `detected` was already present. Never reads the disk itself.
 */
export function installDetection(plan: LoadState<InstallPlan>, installedNow: boolean): Detection {
  if (installedNow) return "connected";
  if (plan.status !== "ready") return "unverified";
  if (plan.value.already_installed === true) return "detected";
  if (plan.value.already_installed === false) return "absent";
  return "unverified";
}

/**
 * Stored Agendar consent. `undefined` means preferences were not read.
 * `connected` was saved in this run. `detected` was already stored.
 * This is not OS permission.
 */
export function consentDetection(consent: boolean | undefined, savedNow: boolean): Detection {
  if (savedNow) return "connected";
  if (consent === true) return "detected";
  if (consent === false) return "absent";
  return "unverified";
}

/** OS permission from the snapshot, or unverified when that field was not read. */
export function systemPermissionNote(permission: NotificationPermission | "unverified"): string {
  switch (permission) {
    case "granted":
      return tx("setup.perm.granted");
    case "denied":
      return tx("setup.perm.denied");
    case "unknown":
      return tx("setup.perm.unknown");
    case "unverified":
      return tx("setup.perm.unverified");
  }
}

export interface ToolConnection {
  plan: LoadState<InstallPlan>;
  /** Explicit selection: it only reveals the review, it never installs. */
  checked: boolean;
  setChecked: (checked: boolean) => void;
  action: ActionState;
  /** Read-only detection. A dry run does not check this box or install. */
  detection: Detection;
  /** Installed by this run, or already present according to the plan. */
  connected: boolean;
  /** Reviewed plan, confirmed absent, nothing running: the button may install. */
  canInstall: boolean;
  install: () => Promise<void>;
  retry: () => void;
}

/**
 * The optional CLI connection. The dry run loads only while `enabled`
 * (after the Vault step) and writes nothing. The install sends the plan digest
 * it reviewed, so a changed destination is refused by the backend.
 */
export function useInstallTool(
  client: AgendarClient,
  tool: InstallTool,
  enabled: boolean,
): ToolConnection {
  const [plan, setPlan] = useState<LoadState<InstallPlan>>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);
  /** Attempt whose digest was rejected. A write waits for a newer dry run. */
  const [rejectedAttempt, setRejectedAttempt] = useState<number | null>(null);
  const [checked, setChecked] = useState(false);
  const [action, setAction] = useState<ActionState>({ status: "idle" });

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    client
      .installLocal({ request_id: newRequestId(), ...toolFields(tool), dry_run: true, consent: false })
      .then((data) => {
        if (!cancelled) setPlan({ status: "ready", value: data.plan });
      })
      .catch((cause: unknown) => {
        if (!cancelled) setPlan({ status: "error", ...failureOf(cause) });
      });
    return () => {
      cancelled = true;
    };
  }, [client, tool, enabled, attempt]);

  const digest = plan.status === "ready" ? plan.value.plan_digest : undefined;
  const detection = installDetection(plan, action.status === "done");
  const connected = detection === "detected" || detection === "connected";
  const planCurrent = rejectedAttempt === null || attempt !== rejectedAttempt;
  // Detection must not select the install. Only an absent item the user checked can write.
  const canInstall =
    checked &&
    detection === "absent" &&
    digest !== undefined &&
    planCurrent &&
    action.status !== "pending";
  // A second press before React re-renders must not start a second install.
  const inFlight = useRef(false);

  function reloadPlan(): void {
    setPlan({ status: "loading" });
    setAttempt((value) => value + 1);
  }

  async function install(): Promise<void> {
    const reviewed = digest;
    if (!canInstall || reviewed === undefined || inFlight.current) return;
    inFlight.current = true;
    setAction({ status: "pending" });
    try {
      const data = await client.installLocal({
        request_id: newRequestId(),
        ...toolFields(tool),
        dry_run: false,
        consent: true,
        expected_plan_digest: reviewed,
      });
      if (!data.installed) throw new Error(tx("setup.not_confirmed"));
      setRejectedAttempt(null);
      setAction({
        status: "done",
        detail: data.plan.destination ?? data.plan.settings_path,
      });
    } catch (cause) {
      const failure = failureOf(cause);
      if (isApiError(cause) && cause.code === "E_STALE_ANSWER") {
        // The reviewed digest must not be written again. Reload the dry run
        // and keep the refusal on screen until the user consents to the new plan.
        setAction({ status: "error", ...failure });
        setRejectedAttempt(attempt);
        reloadPlan();
      } else {
        setAction({ status: "error", ...failure });
      }
    } finally {
      inFlight.current = false;
    }
  }

  function retry(): void {
    setRejectedAttempt(null);
    setAction({ status: "idle" });
    reloadPlan();
  }

  return { plan, checked, setChecked, action, detection, connected, canInstall, install, retry };
}

export interface NotificationChoice {
  checked: boolean;
  setChecked: (checked: boolean) => void;
  action: ActionState;
  /** Stored Agendar consent, or saved by this run. Not OS permission. */
  detection: Detection;
  connected: boolean;
  /** What the snapshot said about the system, without treating it as consent. */
  permissionNote: string;
  canSave: boolean;
  save: () => Promise<void>;
  /** Ask the shell to reload preferences after a stale revision. */
  refresh: () => void;
}

export interface NotificationOptions {
  /** Preferences revision the save must echo; undefined until it is known. */
  revision: string | undefined;
  /** Stored consent. Omit when preferences have not been read. */
  consent: boolean | undefined;
  /** OS permission from the snapshot. Omit when the snapshot has not been read. */
  permission: NotificationPermission | "unverified";
  onSaved?: (preferences: Preferences) => void;
  /** Reload preferences when the revision we sent was rejected. */
  onStale?: () => void;
}

/**
 * Local notification consent, saved only by its own button. It never gates the
 * Vault, and a failed save keeps the checkbox so the user can retry.
 */
export function useNotificationChoice(
  client: AgendarClient,
  options: NotificationOptions,
): NotificationChoice {
  const [checked, setChecked] = useState(false);
  const [action, setAction] = useState<ActionState>({ status: "idle" });
  /** Revision the backend rejected. Another write waits for a different one. */
  const [rejectedRevision, setRejectedRevision] = useState<string | null>(null);
  const revision = options.revision;
  const detection = consentDetection(options.consent, action.status === "done");
  const connected = detection === "detected" || detection === "connected";
  const revisionCurrent = revision !== undefined && revision !== rejectedRevision;
  const canSave =
    checked && detection === "absent" && revisionCurrent && action.status !== "pending";
  const inFlight = useRef(false);

  function refresh(): void {
    options.onStale?.();
  }

  async function save(): Promise<void> {
    if (!canSave || revision === undefined || inFlight.current) return;
    inFlight.current = true;
    setAction({ status: "pending" });
    try {
      const data = await client.setPreferences({
        request_id: newRequestId(),
        expected_revision: revision,
        notifications: true,
      });
      setRejectedRevision(null);
      options.onSaved?.(data.preferences);
      setAction({ status: "done" });
    } catch (cause) {
      const failure = failureOf(cause);
      if (isApiError(cause) && cause.code === "E_STALE_ANSWER") {
        setRejectedRevision(revision);
        setAction({
          status: "error",
          message: tx("setup.notif.stale"),
          hint: tx("setup.notif.stale_hint"),
        });
        options.onStale?.();
      } else {
        setAction({ status: "error", ...failure });
      }
    } finally {
      inFlight.current = false;
    }
  }

  return {
    checked,
    setChecked,
    action,
    detection,
    connected,
    permissionNote: systemPermissionNote(options.permission),
    canSave,
    save,
    refresh,
  };
}

/** What a Setup run could see. Every completion line derives from it. */
export interface SetupSummary {
  commandLine: Detection;
  notifications: Detection;
}

/**
 * The standard Agent Skills installer command. Agendar never writes, detects or
 * claims skill files; the person runs this and picks their agents.
 */
export const SKILL_INSTALL_COMMAND = "npx skills add Maolon/agendar -g";

const SUMMARY_LABELS = [
  ["commandLine", "setup.label.cli"],
  ["notifications", "setup.label.notif"],
] as const;

/** Labels whose detection matches `status`, in display order. */
export function labelsWhere(summary: SetupSummary, status: Detection): string[] {
  return SUMMARY_LABELS.filter(([key]) => summary[key] === status).map(([, label]) => tx(label));
}

export function commandLineStatus(detection: Detection): string {
  if (detection === "detected") return tx("setup.cli.detected");
  if (detection === "connected") return tx("setup.cli.connected");
  if (detection === "unverified") return tx("setup.cli.unverified");
  return tx("setup.cli.absent");
}

export function notificationStatus(detection: Detection): string {
  if (detection === "detected") {
    return tx("setup.notif.detected");
  }
  if (detection === "connected") {
    return tx("setup.notif.connected");
  }
  if (detection === "unverified") return tx("setup.notif.unverified");
  return tx("setup.notif.absent");
}

/** The completion message when Agendar opens. It never claims a skipped or unread item. */
export function openedMessage(summary: SetupSummary): string {
  const missing = labelsWhere(summary, "absent");
  const unverified = labelsWhere(summary, "unverified");
  const connected = labelsWhere(summary, "connected");
  if (missing.length === 0 && unverified.length === 0 && connected.length === 0) {
    return tx("setup.opened.all");
  }
  const parts = [tx("setup.opened")];
  if (connected.length > 0) parts.push(tx("setup.connected_list", { list: connected.join(", ") }));
  if (summary.notifications === "detected" || summary.notifications === "connected") {
    parts.push(tx("setup.consent_stored"));
  }
  if (missing.length > 0) parts.push(tx("setup.not_set_list", { list: missing.join(", ") }));
  if (unverified.length > 0) parts.push(tx("setup.unverified_list", { list: unverified.join(", ") }));
  return parts.join(" ");
}

import { tx } from "../i18n";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { isApiError } from "../api/client";
import { newRequestId } from "../api/format";
import { useClient } from "../api/store";
import type {
  ConfigureVaultData,
  NotificationPermission,
  Preferences,
  SetupLocation,
} from "../api/types";
import {
  failureOf,
  useInstallTool,
  useNotificationChoice,
  vaultPathProblem,
  type Failure,
  type SetupSummary,
} from "../state/setup";
import { AppLogo } from "./BrandMark";
import { Fold } from "./Fold";
import { Icon } from "./Icon";
import { ConnectFooter, ConnectStep, ReadyFooter, ReadyStep } from "./SetupSteps";

export interface SetupProps {
  /** Config revision from the native shell; undefined disables the Vault step. */
  configRevision: string | undefined;
  /** Local preferences revision for the optional notification choice. */
  preferencesRevision: string | undefined;
  /**
   * Stored Agendar consent from preferences. Omit when preferences have not
   * been read; that is not verified, and it is not treated as off.
   */
  notificationsConsent?: boolean;
  /** OS permission from the snapshot. Omit when the snapshot has not been read. */
  notificationPermission?: NotificationPermission;
  /** Whether the app snapshot has loaded; Open Agendar waits for it. */
  ready?: boolean;
  /** Vault folder already configured in this session: resume at the optional step. */
  resumeVaultPath?: string;
  onConfigured?: (data: ConfigureVaultData, folder: string) => void;
  onPreferences?: (preferences: Preferences) => void;
  /** Ask the native shell for fresh state, e.g. after a stale revision. */
  onRefresh?: () => void;
  /** Leave setup and open Agendar, with the truthful connection summary. */
  onOpen?: (summary: SetupSummary) => void;
}

type Stage = "vault" | "connect" | "ready";

const PATH_ID = "setup-vault-path";
const HINT_ID = "setup-vault-hint";
const ERROR_ID = "setup-vault-error";

/**
 * First-run setup in two steps. Step 1 is required: choose the shared data
 * folder (the Vault) and create or open it with its own explicit button, which
 * is the consent for `configure_vault`. Step 2 is optional: each connection is
 * reviewed and installed separately, and notifications never gate step 1.
 */
export function Setup({
  configRevision,
  preferencesRevision,
  notificationsConsent,
  notificationPermission,
  ready = true,
  resumeVaultPath,
  onConfigured,
  onPreferences,
  onRefresh,
  onOpen,
}: SetupProps) {
  const client = useClient();
  const [stage, setStage] = useState<Stage>(resumeVaultPath === undefined ? "vault" : "connect");
  const [customPath, setCustomPath] = useState("");
  const [locations, setLocations] = useState<SetupLocation[]>([]);
  const [locationsLoading, setLocationsLoading] = useState(true);
  const [locationsError, setLocationsError] = useState(false);
  const [selectedLocation, setSelectedLocation] = useState("custom");
  const [create, setCreate] = useState(true);
  const [submitting, setSubmitting] = useState(false);
  const [problem, setProblem] = useState<Failure | null>(null);
  const [vaultPath, setVaultPath] = useState(resumeVaultPath ?? "");
  /** Config revision the backend rejected. The button waits for a newer one. */
  const [rejectedRevision, setRejectedRevision] = useState<string | null>(null);
  const pathField = useRef<HTMLInputElement>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const inFlight = useRef(false);
  const locationChoiceTouched = useRef(false);

  const connecting = stage !== "vault";
  const cli = useInstallTool(client, "path_link", connecting);
  const notifications = useNotificationChoice(client, {
    revision: preferencesRevision,
    consent: notificationsConsent,
    permission: notificationPermission ?? "unverified",
    onSaved: onPreferences,
    onStale: onRefresh,
  });
  const summary: SetupSummary = {
    commandLine: cli.detection,
    notifications: notifications.detection,
  };
  const busy = [cli.action, notifications.action].some(
    (action) => action.status === "pending",
  );

  // A new step moves focus to its heading so the change is announced.
  useEffect(() => {
    heading.current?.focus();
  }, [stage]);

  // Read-only, native allowlisted suggestions. Manual entry remains available
  // while detection runs and when no provider can be detected.
  useEffect(() => {
    if (stage !== "vault") return;
    let cancelled = false;
    client
      .getSetupLocations()
      .then((data) => {
        if (cancelled) return;
        setLocations(data.locations);
        setLocationsLoading(false);
        setLocationsError(false);
        if (data.locations[0] && !locationChoiceTouched.current) {
          setSelectedLocation(data.locations[0].id);
        }
      })
      .catch(() => {
        if (cancelled) return;
        setLocationsLoading(false);
        setLocationsError(true);
        setSelectedLocation("custom");
      });
    return () => {
      cancelled = true;
    };
  }, [client, stage]);

  const custom = selectedLocation === "custom";
  const selectedFolder = custom
    ? customPath
    : (locations.find((location) => location.id === selectedLocation)?.path ?? "");
  const createFolder = custom ? create : true;
  const revisionBlocked =
    configRevision !== undefined && configRevision === rejectedRevision;

  async function submitVault(): Promise<void> {
    if (inFlight.current || configRevision === undefined || configRevision === rejectedRevision) {
      return;
    }
    const message = vaultPathProblem(selectedFolder);
    if (message) {
      setProblem({ message });
      if (custom) pathField.current?.focus();
      else heading.current?.focus();
      return;
    }
    const folder = selectedFolder.trim();
    inFlight.current = true;
    setSubmitting(true);
    setProblem(null);
    try {
      const data = await client.configureVault({
        request_id: newRequestId(),
        expected_config_revision: configRevision,
        path: folder,
        create: createFolder,
        consent: true,
      });
      setVaultPath(folder);
      inFlight.current = false;
      setSubmitting(false);
      setStage("connect");
      onConfigured?.(data, folder);
    } catch (cause) {
      inFlight.current = false;
      setSubmitting(false);
      if (isApiError(cause) && cause.code === "E_STALE_ANSWER") {
        setRejectedRevision(configRevision ?? null);
        setProblem({
          message: tx("setup.vault.stale"),
        });
        onRefresh?.();
      } else {
        setProblem(failureOf(cause));
      }
      if (custom) pathField.current?.focus();
      else heading.current?.focus();
    }
  }

  function onFormSubmit(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    if (stage === "vault") void submitVault();
  }

  const vaultButton = submitting
    ? createFolder
      ? tx("setup.creating")
      : tx("setup.opening")
    : createFolder
      ? tx("setup.create_continue")
      : tx("setup.open_continue");

  return (
    <form className="setup-screen" data-tauri-drag-region noValidate aria-busy={submitting || busy} onSubmit={onFormSubmit}>
      <div className="setup-scroll">
        <main className="setup-card">
          <AppLogo size={40} />
          {stage === "vault" ? (
            <>
              <p className="setup-step">{tx("setup.step1")}</p>
              <h1 ref={heading} tabIndex={-1} className="setup-title">
                {tx("setup.vault_title")}
              </h1>
              <p className="setup-lead">
                {tx("setup.vault_lead")}
              </p>
              <fieldset
                className="setup-locations"
                aria-describedby={!custom && problem ? ERROR_ID : undefined}
              >
                <legend className="setup-label">{tx("storage.vault")}</legend>
                {locationsLoading ? (
                  <p className="setup-note" role="status">
                    {tx("setup.looking")}
                  </p>
                ) : null}
                {locationsError ? (
                  <p className="setup-note" role="alert">
                    {tx("setup.detect_failed")}
                  </p>
                ) : null}
                {locations.map((location) => (
                  <label
                    className="setup-location"
                    data-selected={selectedLocation === location.id}
                    key={location.id}
                  >
                    <input
                      type="radio"
                      name="setup-vault-location"
                      value={location.id}
                      checked={selectedLocation === location.id}
                      disabled={submitting}
                      onChange={() => {
                        locationChoiceTouched.current = true;
                        setSelectedLocation(location.id);
                        setProblem(null);
                      }}
                    />
                    <Icon name={location.kind === "local" ? "server" : "cloud"} />
                    <span className="setup-location-body">
                      <span className="setup-location-title">
                        {location.label} <b>/ {location.folder_name}</b>
                      </span>
                      <span className="setup-location-detail">
                        {location.kind === "icloud_drive"
                          ? tx("setup.loc.icloud")
                          : location.kind === "onedrive"
                            ? tx("setup.loc.onedrive")
                            : tx("setup.loc.local")}
                      </span>
                      <code className="setup-location-path">{location.path}</code>
                    </span>
                  </label>
                ))}
                <label className="setup-location" data-selected={custom}>
                  <input
                    type="radio"
                    name="setup-vault-location"
                    value="custom"
                    checked={custom}
                    disabled={submitting}
                    onChange={() => {
                      locationChoiceTouched.current = true;
                      setSelectedLocation("custom");
                      setProblem(null);
                    }}
                  />
                  <Icon name="server" />
                  <span className="setup-location-body">
                    <span className="setup-location-title">{tx("setup.loc.other")}</span>
                    <span className="setup-location-detail">{tx("setup.loc.other_hint")}</span>
                  </span>
                </label>
              </fieldset>
              {custom ? (
                <div className="setup-field">
                  <label className="setup-label" htmlFor={PATH_ID}>
                    {tx("storage.vault")}
                  </label>
                  <span id={HINT_ID} className="setup-hint">
                    {tx("setup.path_hint")}
                  </span>
                  <input
                    ref={pathField}
                    id={PATH_ID}
                    type="text"
                    className="setup-input"
                    value={customPath}
                    readOnly={submitting}
                    autoComplete="off"
                    spellCheck={false}
                    aria-invalid={problem ? true : undefined}
                    aria-describedby={problem ? `${HINT_ID} ${ERROR_ID}` : HINT_ID}
                    onChange={(event) => {
                      locationChoiceTouched.current = true;
                      setCustomPath(event.target.value);
                      setProblem(null);
                    }}
                  />
                </div>
              ) : null}
              {custom ? (
                <label className="setup-check">
                  <input
                    type="checkbox"
                    checked={create}
                    disabled={submitting}
                    onChange={(event) => setCreate(event.target.checked)}
                  />
                  {tx("storage.create")}
                </label>
              ) : null}
              {problem ? (
                <div id={ERROR_ID} className="alert setup-alert" role="alert">
                  <Icon name="alert-triangle" />
                  <div className="body">
                    <span>{problem.message}</span>
                    {problem.hint ? <span className="hint">{problem.hint}</span> : null}
                  </div>
                </div>
              ) : null}
              <Fold title={tx("setup.how")}>
                <ul className="setup-points">
                  <li>
                    {tx("setup.how.1")}
                  </li>
                  <li>
                    {tx("setup.how.2")}
                  </li>
                  <li>
                    {tx("setup.how.3")}
                  </li>
                  <li>
                    {tx("setup.how.4")}
                  </li>
                  <li>{tx("setup.how.5")}</li>
                </ul>
              </Fold>
            </>
          ) : null}
          {stage === "connect" ? (
            <ConnectStep
              vaultPath={vaultPath}
              headingRef={heading}
              cli={cli}
              notifications={notifications}
            />
          ) : null}
          {stage === "ready" ? (
            <ReadyStep vaultPath={vaultPath} headingRef={heading} summary={summary} />
          ) : null}
        </main>
      </div>
      <footer className="setup-foot">
        {stage === "vault" ? (
          <>
            <p className="setup-foot-text" role="status" aria-live="polite">
              {configRevision === undefined
                ? tx("setup.starting")
                : revisionBlocked
                  ? tx("setup.refreshing")
                  : submitting
                    ? tx("setup.working_folder")
                    : tx("setup.nothing_written")}
            </p>
            <div className="acts">
              {configRevision === undefined || revisionBlocked ? (
                <button type="button" className="btn" onClick={() => onRefresh?.()}>
                  {tx("setup.check_again")}
                </button>
              ) : null}
              <button
                type="submit"
                className="btn primary"
                disabled={configRevision === undefined || submitting || revisionBlocked}
              >
                {vaultButton}
              </button>
            </div>
          </>
        ) : null}
        {stage === "connect" ? (
          <ConnectFooter
            summary={summary}
            busy={busy}
            ready={ready}
            onSkip={() => setStage("ready")}
            onOpen={() => onOpen?.(summary)}
          />
        ) : null}
        {stage === "ready" ? (
          <ReadyFooter
            ready={ready}
            onBack={() => setStage("connect")}
            onOpen={() => onOpen?.(summary)}
          />
        ) : null}
      </footer>
    </form>
  );
}

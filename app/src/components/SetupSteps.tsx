import { tx } from "../i18n";
import type { RefObject } from "react";
import {
  commandLineStatus,
  labelsWhere,
  notificationStatus,
  SKILL_INSTALL_COMMAND,
  type NotificationChoice,
  type SetupSummary,
  type ToolConnection,
} from "../state/setup";
import { Icon } from "./Icon";
import { CommandLineItem, NotificationItem, SkillItem } from "./SetupItems";

/** Step 2 of 2: optional connections. Each item is independent of the others. */
interface ConnectStepProps {
  vaultPath: string;
  headingRef: RefObject<HTMLHeadingElement | null>;
  cli: ToolConnection;
  notifications: NotificationChoice;
}

export function ConnectStep({
  vaultPath,
  headingRef,
  cli,
  notifications,
}: ConnectStepProps) {
  return (
    <>
      <p className="setup-step">{tx("setup.step2_optional")}</p>
      <h1 ref={headingRef} tabIndex={-1} className="setup-title">
        {tx("setup.connect_title")}
      </h1>
      <p className="setup-success" role="status">
        <Icon name="badge-check" />
        <span>
          {tx("setup.ready_at")}<code>{vaultPath}</code>{tx("setup.period")}
        </span>
      </p>
      <p className="setup-lead">
        {tx("setup.connect_lead")}
      </p>
      <CommandLineItem tool={cli} />
      <SkillItem />
      <NotificationItem choice={notifications} />
    </>
  );
}

/** Step 2 summary after "Skip for now": says what is and is not connected. */
interface ReadyStepProps {
  vaultPath: string;
  headingRef: RefObject<HTMLHeadingElement | null>;
  summary: SetupSummary;
}

export function ReadyStep({ vaultPath, headingRef, summary }: ReadyStepProps) {
  return (
    <>
      <p className="setup-step">{tx("setup.step2_summary")}</p>
      <h1 ref={headingRef} tabIndex={-1} className="setup-title">
        {tx("setup.ready_title")}
      </h1>
      <p className="setup-lead">
        {tx("setup.set_up_at")}<code>{vaultPath}</code>{tx("setup.period")}
      </p>
      <ul className="setup-points">
        <li>{commandLineStatus(summary.commandLine)}</li>
        <li>
          {tx("setup.skill_pre")}<code>{SKILL_INSTALL_COMMAND}</code>{tx("setup.skill_post")}
        </li>
        <li>{notificationStatus(summary.notifications)}</li>
      </ul>
    </>
  );
}

interface ConnectFooterProps {
  summary: SetupSummary;
  busy: boolean;
  ready: boolean;
  onSkip: () => void;
  onOpen: () => void;
}

export function ConnectFooter({ summary, busy, ready, onSkip, onOpen }: ConnectFooterProps) {
  const missing = labelsWhere(summary, "absent");
  const unverified = labelsWhere(summary, "unverified");
  const connected = labelsWhere(summary, "connected");
  const connectedLine =
    connected.length > 0 ? tx("setup.connected_list", { list: connected.join(", ") }) : "";
  const consentStored =
    (summary.notifications === "detected" || summary.notifications === "connected") &&
    (missing.length > 0 || unverified.length > 0);
  const line = busy
    ? tx("setup.finishing_change")
    : !ready
      ? tx("setup.finishing")
      : missing.length === 0 && unverified.length === 0
        ? [
            connectedLine,
            tx("setup.all_connected"),
          ]
            .filter((part) => part !== "")
            .join(" ")
        : missing.length === 2
          ? tx("setup.nothing_yet")
          : [
              connectedLine,
              consentStored ? tx("setup.consent_stored") : "",
              missing.length > 0 ? tx("setup.not_connected_list", { list: missing.join(", ") }) : "",
              unverified.length > 0 ? tx("setup.unverified_list", { list: unverified.join(", ") }) : "",
            ]
              .filter((part) => part !== "")
              .join(" ");
  return (
    <>
      <p className="setup-foot-text" role="status" aria-live="polite">
        {line}
      </p>
      <div className="acts">
        <button type="button" className="btn" disabled={busy} onClick={onSkip}>
          {tx("setup.skip")}
        </button>
        <button type="button" className="btn primary" disabled={busy || !ready} onClick={onOpen}>
          {tx("tray.open_app")}
        </button>
      </div>
    </>
  );
}

interface ReadyFooterProps {
  ready: boolean;
  onBack: () => void;
  onOpen: () => void;
}

export function ReadyFooter({ ready, onBack, onOpen }: ReadyFooterProps) {
  return (
    <>
      <p className="setup-foot-text" role="status" aria-live="polite">
        {ready ? tx("setup.go_back_hint") : tx("setup.finishing")}
      </p>
      <div className="acts">
        <button type="button" className="btn" onClick={onBack}>
          {tx("setup.back")}
        </button>
        <button type="button" className="btn primary" disabled={!ready} onClick={onOpen}>
          {tx("tray.open_app")}
        </button>
      </div>
    </>
  );
}

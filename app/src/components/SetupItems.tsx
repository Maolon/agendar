import { tx } from "../i18n";
import { useEffect, useRef, useState, type ReactNode } from "react";
import type { InstallPlan } from "../api/types";
import {
  SKILL_INSTALL_COMMAND,
  type ActionState,
  type LoadState,
  type NotificationChoice,
  type ToolConnection,
} from "../state/setup";
import { Fold } from "./Fold";
import { Icon } from "./Icon";

/** The destination an action reported, or a plain fallback when it reported none. */
function detailOf(action: ActionState, fallback: string): string {
  return action.status === "done" && action.detail ? action.detail : fallback;
}

function FailureLine({
  message,
  hint,
  focus = false,
  children,
}: {
  message: string;
  hint?: string;
  /** User actions may move focus here. A plan that loads on its own must not. */
  focus?: boolean;
  children?: ReactNode;
}) {
  const alertRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (focus) alertRef.current?.focus();
  }, [focus]);
  return (
    <div ref={alertRef} tabIndex={-1} className="alert setup-alert" role="alert">
      <Icon name="alert-triangle" />
      <div className="body">
        <span>{message}</span>
        {hint ? <span className="hint">{hint}</span> : null}
        {children}
      </div>
    </div>
  );
}

function PlanNote({
  plan,
  label,
  value,
  onRetry,
  errorLabel = tx("si.err_label"),
}: {
  plan: LoadState<InstallPlan>;
  label: string;
  value: string | undefined;
  onRetry: () => void;
  errorLabel?: string;
}) {
  if (plan.status === "loading") {
    return (
      <p className="setup-note" role="status">
        {tx("si.checking")}
      </p>
    );
  }
  if (plan.status === "error") {
    return (
      <FailureLine message={`${errorLabel} ${plan.message}`} hint={plan.hint}>
        <button type="button" className="btn" onClick={onRetry}>
          {tx("setup.check_again")}
        </button>
      </FailureLine>
    );
  }
  return (
    <p className="setup-item-desc">
      {label}: <code>{value ?? tx("si.not_reported")}</code>
    </p>
  );
}

/**
 * Result of one explicit action. A success moves focus to its message so a
 * screen reader hears what changed; a failure keeps the item's own retry.
 */
function ActionNote({
  action,
  done,
  onRetry,
  retryLabel,
}: {
  action: ActionState;
  done: string;
  onRetry: () => void;
  retryLabel: string;
}) {
  const doneRef = useRef<HTMLParagraphElement>(null);
  useEffect(() => {
    if (action.status === "done") doneRef.current?.focus();
  }, [action.status]);
  if (action.status === "pending") {
    return (
      <p className="setup-note" role="status">
        {tx("si.working")}
      </p>
    );
  }
  if (action.status === "done") {
    return (
      <p ref={doneRef} tabIndex={-1} className="setup-success" role="status">
        <Icon name="check" />
        <span>{done}</span>
      </p>
    );
  }
  if (action.status === "error") {
    return (
      <FailureLine message={action.message} hint={action.hint} focus>
        <button type="button" className="btn" onClick={onRetry}>
          {retryLabel}
        </button>
      </FailureLine>
    );
  }
  return null;
}

function CopyCommand({ command }: { command: string }) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");
  async function copy(): Promise<void> {
    try {
      await navigator.clipboard.writeText(command);
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
  }
  return (
    <div className="setup-copy">
      <code className="setup-command">{command}</code>
      <div className="acts">
        <button type="button" className="btn" onClick={() => void copy()}>
          {tx("si.copy_cmd")}
        </button>
        <span className="setup-note" role="status">
          {copyState === "copied"
            ? tx("si.copied")
            : copyState === "failed"
              ? tx("si.copy_failed")
              : ""}
        </span>
      </div>
    </div>
  );
}

type ItemMode = "choice" | "detected" | "connected" | "unverified";

/** One optional connection: its own checkbox, review and explicit action. */
function OptionalItem({
  id,
  title,
  description,
  checkLabel,
  checked,
  onCheck,
  mode,
  explanation,
  busy = false,
  children,
}: {
  id: string;
  title: string;
  description: string;
  checkLabel: string;
  checked: boolean;
  onCheck: (checked: boolean) => void;
  mode: ItemMode;
  /** Why a detected, just-connected, or unverified control cannot be toggled. */
  explanation?: string;
  busy?: boolean;
  children: ReactNode;
}) {
  const statusId = `${id}-status`;
  const locked = mode !== "choice";
  const shown = mode === "choice" ? checked : mode === "detected" || mode === "connected";
  const badge = mode === "detected" ? tx("si.detected") : mode === "connected" ? tx("si.connected") : null;
  return (
    <section
      className="setup-item"
      aria-labelledby={`${id}-title`}
      aria-busy={busy ? true : undefined}
    >
      <div className="setup-item-head">
        <h2 id={`${id}-title`}>{title}</h2>
        {badge ? (
          <span className="setup-badge">
            <Icon name="check" />
            {badge}
          </span>
        ) : null}
      </div>
      <p className="setup-item-desc">{description}</p>
      <label className="setup-check">
        <input
          type="checkbox"
          checked={shown}
          disabled={locked}
          aria-describedby={explanation ? statusId : undefined}
          onChange={(event) => {
            if (locked) return;
            onCheck(event.target.checked);
          }}
        />
        {checkLabel}
      </label>
      {explanation ? (
        <p id={statusId} className="setup-note">
          {explanation}
        </p>
      ) : null}
      {children}
    </section>
  );
}

function itemMode(detection: "detected" | "connected" | "absent" | "unverified"): ItemMode {
  return detection === "absent" ? "choice" : detection;
}

export function CommandLineItem({ tool }: { tool: ToolConnection }) {
  const plan = tool.plan.status === "ready" ? tool.plan.value : undefined;
  const reviewable = tool.checked && plan !== undefined && tool.detection === "absent";
  const manual = tool.detection === "absent" || tool.detection === "unverified";
  const checkLabel =
    tool.detection === "detected"
      ? tx("si.cli.installed")
      : tool.detection === "connected"
        ? tx("si.cli.connected")
        : tool.detection === "unverified"
          ? tx("si.cli.unverified")
          : tx("si.cli.connect");
  const explanation =
    tool.detection === "detected"
      ? tx("si.cli.x_detected")
      : tool.detection === "connected"
        ? tx("si.cli.x_connected")
        : tool.detection === "unverified"
          ? tool.plan.status === "ready"
            ? tx("si.cli.x_unv_ready")
            : tx("si.cli.x_unv")
          : undefined;
  return (
    <OptionalItem
      id="setup-command-line"
      title={tx("agents.cli")}
      description={tx("si.cli.desc")}
      checkLabel={checkLabel}
      checked={tool.checked}
      onCheck={tool.setChecked}
      mode={itemMode(tool.detection)}
      {...(explanation ? { explanation } : {})}
      busy={tool.plan.status === "loading" || tool.action.status === "pending"}
    >
      <PlanNote plan={tool.plan} label={tx("si.destination")} value={plan?.destination} onRetry={tool.retry} />
      {plan ? (
        <p className="setup-note">
          {tx("si.cli.scope")}
        </p>
      ) : null}
      {reviewable ? (
        <p className="setup-review">
          {tx("si.cli.review")}
        </p>
      ) : null}
      {reviewable ? (
        <div className="acts">
          <button
            type="button"
            className="btn"
            disabled={!tool.canInstall}
            onClick={() => void tool.install()}
          >
            {tx("si.cli.button")}
          </button>
        </div>
      ) : null}
      <ActionNote
        action={tool.action}
        done={tx("si.cli.done", { where: detailOf(tool.action, tx("si.the_destination")) })}
        onRetry={tool.retry}
        retryLabel={tx("setup.check_again")}
      />
      {manual ? (
        <Fold title={tx("si.manual")}>
          {plan?.command ? (
            <>
              <p className="setup-note">
                {tx("si.manual.run")}
              </p>
              <CopyCommand command={plan.command} />
            </>
          ) : (
            <p className="setup-note">{tx("si.manual.wait")}</p>
          )}
        </Fold>
      ) : null}
    </OptionalItem>
  );
}

/**
 * Advanced: Claude Code hooks. The Skill already covers Claude Code; hooks add
 * automatic inbox delivery at session start, after tools and on stop. Same
 * review-then-consent contract as the command line.
 */
interface HooksCopy {
  id: string;
  title: string;
  description: string;
  noun: string;
  install: string;
}

function skillHooks(): HooksCopy {
  return {
    id: "setup-hooks",
    title: tx("si.hooks.title"),
    description: tx("si.hooks.desc"),
    noun: "Claude Code hooks",
    install: tx("si.hooks.install"),
  };
}

function observeHooks(): HooksCopy {
  return {
    id: "setup-observe-hooks",
    title: tx("agents.observe"),
    description: tx("si.observe.desc"),
    noun: tx("si.observe.noun"),
    install: tx("agents.observe"),
  };
}

export function HooksItem({ tool }: { tool: ToolConnection }) {
  return <HookSetItem tool={tool} copy={skillHooks()} />;
}

export function ObserveHooksItem({ tool }: { tool: ToolConnection }) {
  return <HookSetItem tool={tool} copy={observeHooks()} />;
}

function HookSetItem({ tool, copy }: { tool: ToolConnection; copy: HooksCopy }) {
  const plan = tool.plan.status === "ready" ? tool.plan.value : undefined;
  const reviewable = tool.checked && plan !== undefined && tool.detection === "absent";
  const checkLabel =
    tool.detection === "detected"
      ? tx("si.hooks.installed", { noun: copy.noun })
      : tool.detection === "connected"
        ? tx("si.hooks.installed_now", { noun: copy.noun })
        : tool.detection === "unverified"
          ? tx("si.hooks.unverified", { noun: copy.noun })
          : copy.install;
  const explanation =
    tool.detection === "detected"
      ? tx("si.hooks.x_detected")
      : tool.detection === "connected"
        ? tx("si.hooks.x_connected")
        : tool.detection === "unverified"
          ? tx("si.hooks.x_unv")
          : undefined;
  return (
    <OptionalItem
      id={copy.id}
      title={copy.title}
      description={copy.description}
      checkLabel={checkLabel}
      checked={tool.checked}
      onCheck={tool.setChecked}
      mode={itemMode(tool.detection)}
      {...(explanation ? { explanation } : {})}
      busy={tool.plan.status === "loading" || tool.action.status === "pending"}
    >
      <PlanNote plan={tool.plan} label={tx("si.settings_file")} value={plan?.settings_path} onRetry={tool.retry} />
      {reviewable && plan ? (
        <div className="setup-review">
          <p>{tx("si.hooks.review")}</p>
          <ul className="setup-points">
            {plan.changes.map((change) => (
              <li key={`${change.event}:${change.command}`}>
                <b>{change.event}</b> · <code>{change.command}</code>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
      {reviewable ? (
        <div className="acts">
          <button
            type="button"
            className="btn"
            disabled={!tool.canInstall}
            onClick={() => void tool.install()}
          >
            {tx("si.hooks.button")}
          </button>
        </div>
      ) : null}
      <ActionNote
        action={tool.action}
        done={tx("si.hooks.done", { where: detailOf(tool.action, tx("si.cc_settings")) })}
        onRetry={tool.retry}
        retryLabel={tx("setup.check_again")}
      />
    </OptionalItem>
  );
}

/**
 * The agent skill comes from the standard Agent Skills installer, not from
 * Agendar. This item only shows that command: it never writes, detects or
 * claims skill files.
 */
export function SkillItem() {
  return (
    <section className="setup-item" aria-labelledby="setup-agent-skill-title">
      <div className="setup-item-head">
        <h2 id="setup-agent-skill-title">{tx("agents.skill")}</h2>
      </div>
      <p className="setup-item-desc">
        {tx("si.skill.desc")}
      </p>
      <CopyCommand command={SKILL_INSTALL_COMMAND} />
      <p className="setup-note">
        {tx("si.skill.node")}
      </p>
    </section>
  );
}

export function NotificationItem({ choice }: { choice: NotificationChoice }) {
  const checkLabel =
    choice.detection === "detected"
      ? tx("si.n.stored")
      : choice.detection === "connected"
        ? tx("si.n.stored_now")
        : choice.detection === "unverified"
          ? tx("si.n.unverified")
          : tx("si.n.allow");
  const explanation =
    choice.detection === "detected"
      ? tx("si.n.x_detected")
      : choice.detection === "connected"
        ? tx("si.n.x_connected")
        : choice.detection === "unverified"
          ? tx("si.n.x_unv")
          : undefined;
  return (
    <OptionalItem
      id="setup-notifications"
      title={tx("notifications.desktop")}
      description={tx("si.n.desc")}
      checkLabel={checkLabel}
      checked={choice.checked}
      onCheck={choice.setChecked}
      mode={itemMode(choice.detection)}
      {...(explanation ? { explanation } : {})}
      busy={choice.action.status === "pending"}
    >
      <p className="setup-note">{choice.permissionNote}</p>
      {choice.detection === "absent" && choice.checked ? (
        <div className="acts">
          <button
            type="button"
            className="btn"
            disabled={!choice.canSave}
            onClick={() => void choice.save()}
          >
            {tx("si.n.button")}
          </button>
        </div>
      ) : null}
      <ActionNote
        action={choice.action}
        done={tx("si.n.done")}
        onRetry={() => {
          if (choice.canSave) void choice.save();
          else choice.refresh();
        }}
        retryLabel={choice.canSave ? tx("si.try_again") : tx("setup.check_again")}
      />
    </OptionalItem>
  );
}

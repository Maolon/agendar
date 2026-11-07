import { useState } from "react";
import { useT } from "../i18n";
import type { MessageKey } from "../i18n/messages";
import { SNOOZE_CHOICES, useSnooze, type SnoozeChoice } from "../state/snooze";
import { Icon } from "./Icon";

const CHOICE_LABEL: Record<SnoozeChoice, MessageKey> = {
  hour: "snooze.hour",
  tomorrow: "snooze.tomorrow",
  week: "snooze.week",
};

/**
 * "Not now" for one Needs-you item: hide it for a while, or bring it back.
 * `compact` shows only the clock (tray rows); its name stays "Not now".
 */
export function NeedSnooze({ id, compact = false }: { id: string; compact?: boolean }) {
  const api = useSnooze();
  const t = useT();
  const [busy, setBusy] = useState(false);
  if (!api) return null;

  async function run(action: () => Promise<void>): Promise<void> {
    setBusy(true);
    try {
      await action();
    } finally {
      setBusy(false);
    }
  }

  if (api.until(id)) {
    return (
      <span className="snooze">
        <span className="snooze-note">{t("snooze.on")}</span>
        <button type="button" className="btn sm ghost" disabled={busy} onClick={() => void run(() => api.restore(id))}>
          {t("snooze.restore")}
        </button>
      </span>
    );
  }
  if (api.open === id) {
    return (
      <span className="snooze" role="group" aria-label={t("snooze.button")}>
        {SNOOZE_CHOICES.map((choice) => (
          <button
            key={choice}
            type="button"
            className="btn sm ghost"
            disabled={busy}
            onClick={() => void run(() => api.snooze(id, choice))}
          >
            {t(CHOICE_LABEL[choice])}
          </button>
        ))}
      </span>
    );
  }
  return (
    <span className="snooze">
      <button
        type="button"
        className={compact ? "btn sm ghost icon-only" : "btn sm ghost"}
        aria-label={t("snooze.button")}
        title={compact ? t("snooze.button") : undefined}
        onClick={() => api.setOpen(id)}
      >
        <Icon name="clock" />
        {compact ? null : t("snooze.button")}
      </button>
    </span>
  );
}

/**
 * Cancel for a hand-off item: it closes for good on this computer. The step is
 * untouched and stays stopped on its project page, where it can be handed off.
 */
export function NeedDismiss({ id, compact = false }: { id: string; compact?: boolean }) {
  const api = useSnooze();
  const t = useT();
  const [busy, setBusy] = useState(false);
  if (!api) return null;
  if (api.dismissed(id)) {
    return compact ? null : <span className="snooze-note">{t("dismiss.on")}</span>;
  }
  return (
    <button
      type="button"
      className={compact ? "btn sm ghost icon-only" : "btn"}
      aria-label={t("dismiss.button")}
      title={compact ? t("dismiss.button") : undefined}
      disabled={busy}
      onClick={() => {
        setBusy(true);
        void api.dismiss(id).finally(() => setBusy(false));
      }}
    >
      <Icon name="x" />
      {compact ? null : t("dismiss.button")}
    </button>
  );
}

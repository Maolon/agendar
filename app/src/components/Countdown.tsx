import { tx } from "../i18n";
import { formatRemaining } from "../api/format";
import type { OnTimeout, UtcTime } from "../api/types";
import { Icon } from "./Icon";

interface CountdownProps {
  since: UtcTime;
  defaultAt: UtcTime | undefined;
  now: UtcTime;
  onTimeout: OnTimeout;
  chosenLabel: string | undefined;
}

/**
 * Waiting/remaining ring for the default. It shows time left before the
 * declared timeout, never a percentage of work done. Without a deadline it
 * renders nothing rather than a fabricated fraction.
 */
export function Countdown({ since, defaultAt, now, onTimeout, chosenLabel }: CountdownProps) {
  if (!defaultAt) return null;
  const start = Date.parse(since);
  const deadline = Date.parse(defaultAt);
  const current = Date.parse(now);
  const total = deadline - start;
  const remaining = Math.max(0, deadline - current);
  const fraction = total > 0 ? Math.min(1, Math.max(0, remaining / total)) : 0;
  const circumference = 2 * Math.PI * 10;
  const then =
    onTimeout.kind === "proceed_as_assumption"
      ? tx("cd.goes_with", { label: chosenLabel ?? tx("cd.the_rec") })
      : tx("cd.holds");
  return (
    <span className="r" aria-label={tx("cd.label", { d: formatRemaining(defaultAt, now), then })}>
      <Icon name="timer" className="c2" />
      <svg className="ring" viewBox="0 0 26 26" aria-hidden="true">
        <circle className="bg" cx="13" cy="13" r="10" />
        <circle
          className="fg"
          cx="13"
          cy="13"
          r="10"
          strokeDasharray={`${(circumference * fraction).toFixed(1)} ${circumference.toFixed(1)}`}
        />
      </svg>
      <span>
        <b>{formatRemaining(defaultAt, now)}</b> {then}
      </span>
    </span>
  );
}

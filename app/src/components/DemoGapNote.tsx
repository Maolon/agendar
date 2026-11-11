import type { ProjectState } from "../api/types";
import { DELIVER_COMMAND, demoGap, demoGapLine, demoGapWhat } from "../state/demoGap";
import { Icon } from "./Icon";

/**
 * Under the project header while the demo budget is spent: how long, which
 * step, and what clears it.
 */
export function DemoGapNote({ project }: { project: ProjectState }) {
  const gap = demoGap(project);
  if (!gap?.over) return null;
  const what = demoGapWhat(gap);
  const line = demoGapLine(gap);
  // Every locale's line opens with the condition; the command reads as code.
  const rest = line.startsWith(what) ? line.slice(what.length) : ` ${line}`;
  const at = rest.indexOf(DELIVER_COMMAND);
  return (
    <p className="unblocks gapnote" role="note">
      <Icon name="eye-off" className="c2" />
      <span>
        {line.startsWith(what) ? <b>{what}</b> : null}
        {at < 0 ? (
          rest
        ) : (
          <>
            {rest.slice(0, at)}
            <code>{DELIVER_COMMAND}</code>
            {rest.slice(at + DELIVER_COMMAND.length)}
          </>
        )}
      </span>
    </p>
  );
}

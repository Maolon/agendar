/**
 * Activity: what happened on this project, newest first, in sentences.
 * Items come from the host's activity projection; this view only formats them.
 */

import { renderMsg } from "../i18n/msg";
import { tx } from "../i18n";
import { ReportLink } from "./Reports";
import { useState } from "react";
import type { ActivityItem } from "../api/types";
import { Icon, type IconName } from "./Icon";

const SHOWN = 6;

function clock(ts: string): string {
  const date = new Date(ts);
  return Number.isNaN(date.getTime())
    ? ""
    : date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
}

function iconOf(name: string): IconName {
  const known: readonly string[] = [
    "check",
    "clock",
    "git-branch",
    "bot",
    "help-circle",
    "badge-check",
    "pause-circle",
    "radio",
    "target",
    "refresh-cw",
    "x",
    "user",
  ];
  return (known.includes(name) ? name : "circle-dot") as IconName;
}

export function ActivityFeed({
  items,
  onSelectNode,
  project,
}: {
  items: ActivityItem[] | undefined;
  onSelectNode?: (node: string) => void;
  /** The project slug, so a report line can open its document. */
  project?: string;
}) {
  const [all, setAll] = useState(false);
  if (!items?.length) return <div className="muted-note">{tx("act.none")}</div>;
  const shown = all ? items : items.slice(0, SHOWN);
  return (
    <>
      <ol className="afeed">
        {shown.map((item, index) => (
          <li key={`${item.ts}-${index}`} className={item.actor_kind === "human" ? "you" : ""}>
            <span className="adot" aria-hidden="true">
              <Icon name={iconOf(item.icon)} />
            </span>
            <span className="atext">
              {item.node && onSelectNode ? (
                <button type="button" className="alink" onClick={() => onSelectNode(item.node!)}>
                  {renderMsg(item.msg, item.text)}
                </button>
              ) : (
                <span>{renderMsg(item.msg, item.text)}</span>
              )}
              {item.detail ? <span className="adetail">{item.detail}</span> : null}
              {item.report && project ? <ReportLink project={project} blob={item.report} /> : null}
            </span>
            <time dateTime={item.ts}>{clock(item.ts)}</time>
          </li>
        ))}
      </ol>
      {items.length > SHOWN ? (
        <button type="button" className="link amore" onClick={() => setAll((value) => !value)}>
          {all ? tx("act.fewer") : tx("act.all", { n: items.length })}
        </button>
      ) : null}
    </>
  );
}

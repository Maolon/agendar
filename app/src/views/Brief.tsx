import { renderMsg } from "../i18n/msg";
import { tx } from "../i18n";
import { formatAge } from "../api/format";
import { useClient } from "../api/store";
import type { BriefSection, Snapshot } from "../api/types";
import { indexGraph, ifYouWaitText } from "../graph/traversal";
import { useProjectGraphs } from "../state/graph";
import { workingAgents } from "../state/project";
import { BriefVisual } from "../components/BriefVisual";
import { Icon, type IconName } from "../components/Icon";
import { PresenceRows } from "../components/PresenceRows";
import { activeNeeds, briefWithoutSnoozed } from "../state/snooze";
import type { Selection } from "../state/ui";

const SECTIONS: { key: BriefSection; icon: IconName }[] = [
  { key: "delivered", icon: "package" },
  { key: "coming_up", icon: "clock" },
  { key: "needs_you", icon: "hand" },
  { key: "if_you_wait", icon: "pause-circle" },
  { key: "by_default", icon: "timer" },
  { key: "at_risk", icon: "gauge" },
];

interface BriefProps {
  snapshot: Snapshot;
  onSelect: (selection: Selection) => void;
}

function targetSelection(
  snapshot: Snapshot,
  target: { kind: "need" | "project"; id: string },
): Selection | null {
  if (target.kind === "need") {
    const need = snapshot.needs.find((item) => item.id === target.id);
    return need ? { kind: "need", id: need.id } : null;
  }
  return snapshot.state.projects[target.id] ? { kind: "project", id: target.id } : null;
}

/**
 * Default view: the six sections in fixed order, each with its projected rows.
 * A section with no row still shows its label and an honest empty line, and
 * "All clear" appears only when every one of the six is empty.
 */
export function Brief({ snapshot, onSelect }: BriefProps) {
  const { preferences, state, local } = snapshot;
  const brief = briefWithoutSnoozed(snapshot);
  const client = useClient();
  const rowCount = SECTIONS.reduce((total, section) => total + brief[section.key].length, 0);
  const lookedAge = preferences.last_looked_at
    ? formatAge(preferences.last_looked_at, state.evaluated_at)
    : "";
  const running = Object.values(state.projects).reduce(
    (total, project) => total + workingAgents(project),
    0,
  );
  const firstNeed = activeNeeds(snapshot)[0];

  // “If you wait” counts the graph's reachable blocked steps, not only the
  // children the row was projected with.
  const waitProjects = [
    ...new Set(
      brief.if_you_wait
        .map((row) =>
          row.target.kind === "project"
            ? row.target.id
            : snapshot.needs.find((item) => item.id === row.target.id)?.project,
        )
        .filter((slug): slug is string => Boolean(slug) && Boolean(state.projects[slug ?? ""])),
    ),
  ];
  const graphs = useProjectGraphs(client, waitProjects, state.evaluated_at);

  function waitText(row: (typeof brief.if_you_wait)[number]): string {
    const slug =
      row.target.kind === "project"
        ? row.target.id
        : snapshot.needs.find((item) => item.id === row.target.id)?.project;
    if (!slug) return renderMsg(row.msg, row.text);
    const project = state.projects[slug];
    const graph = graphs[slug];
    // A need-targeted row names its own need; "first need in the project" would
    // swap in another item's graph reach.
    const need =
      row.target.kind === "need"
        ? snapshot.needs.find((item) => item.id === row.target.id)
        : snapshot.needs.find((item) => item.project === slug);
    if (!project || !graph || !need) return renderMsg(row.msg, row.text);
    return ifYouWaitText(indexGraph(graph), project, need) ?? renderMsg(row.msg, row.text);
  }

  if (!rowCount) {
    return (
      <div className="view">
        <div className="allclear">
          <Icon name="radar" />
          <h2>{tx("brief.all_clear")}</h2>
          <div>
            {running > 0
              ? running === 1
                ? tx("brief.working.one")
                : tx("brief.working.other", { n: running })
              : tx("brief.no_agents")}
          </div>
          <div style={{ marginTop: 14, fontSize: 12.5 }}>
            {tx("brief.all_empty")}
            {local.health !== "healthy" ? tx("brief.local", { health: local.health }) : ""}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="view">
      <h2 className="h">
        {tx("brief.since")}{" "}
        {lookedAge ? <span className="sub">{lookedAge}</span> : null}
      </h2>
      <PresenceRows rows={snapshot.presence ?? []} snapshot={snapshot} anchor={state.evaluated_at} />
      <div className="brief">
        {SECTIONS.map((section) => {
          const rows = brief[section.key];
          if (!rows.length) {
            return (
              <div key={section.key} className="br br-none" data-brief-empty={section.key}>
                <span className="bi">
                  <Icon name={section.icon} />
                </span>
                <span className="bl">{tx(`brief.${section.key}`)}</span>
                <span className="bt muted">{tx(`brief.empty.${section.key}`)}</span>
                <span className="bx" />
              </div>
            );
          }
          return rows.map((row) => {
            const target = targetSelection(snapshot, row.target);
            const text = section.key === "if_you_wait" ? waitText(row) : renderMsg(row.msg, row.text);
            const content = (
              <>
                <span className="bi">
                  <Icon name={section.icon} />
                </span>
                <span className="bl">{tx(`brief.${section.key}`)}</span>
                <span className="bt">
                  {text}
                  {row.detail ? <small>{renderMsg(row.detail_msg, row.detail)}</small> : null}
                </span>
                <span className="bx">
                  <BriefVisual section={section.key} row={row} snapshot={snapshot} />
                </span>
              </>
            );
            return target ? (
              <button
                key={row.id}
                type="button"
                className="br"
                data-brief-section={section.key}
                data-brief-row={row.id}
                onClick={() => onSelect(target)}
              >
                {content}
              </button>
            ) : (
              <div
                key={row.id}
                className="br"
                data-brief-section={section.key}
                data-brief-row={row.id}
              >
                {content}
              </div>
            );
          });
        })}
      </div>
      {firstNeed ? (
        <div className="acts" style={{ justifyContent: "flex-start" }}>
          <button
            type="button"
            className="btn primary"
            onClick={() => onSelect({ kind: "need", id: firstNeed.id })}
          >
            {tx("brief.start")} <kbd>⌘↵</kbd>
          </button>
        </div>
      ) : null}
    </div>
  );
}

import { tx } from "../i18n";
import { useMemo, useState } from "react";
import { formatAgo, newRequestId } from "../api/format";
import { useClient } from "../api/store";
import type { PresenceRow, ProjectState } from "../api/types";
import type { MotionPulse } from "../graph/motion";
import { playableUnblock } from "../graph/motion";
import type { AnswerAction } from "../state/answer";
import { liveAssumptions } from "../state/assumptions";
import { useProjectGraphs } from "../state/graph";
import type { UnblockPlay } from "../state/ui";
import { Assumptions } from "../components/Assumptions";
import { EvidenceCards } from "../components/EvidenceCard";
import { ArchiveToggle, FeatureEditor, GoalEditor } from "../components/Agenda";
import { graphNodeElementId } from "../components/Graph";
import { ActivityFeed } from "../components/ActivityFeed";
import { ReportCards } from "../components/Reports";
import { Fold } from "../components/Fold";
import { NowCards } from "../components/NowCards";
import { useSnooze } from "../state/snooze";
import { isActiveNeed, isInfoNeed } from "../state/claim";
import { PlanMap } from "../components/PlanMap";
import { planOutline } from "../graph/lanes";
import { DemoGapNote } from "../components/DemoGapNote";
import { ProjectOverview } from "../components/ProjectOverview";
import { Corrections } from "../components/Corrections";
import { ProjectStatsRow } from "../components/ProjectStatsRow";
import { Icon } from "../components/Icon";
import { PolicyPanel } from "../components/PolicyPanel";
import { ProjectAvatar } from "../components/ProjectAvatar";
import { calibrationLine, etaClaims, gateLabel, planSteps, PROGRESS_LABEL, reviewWaiting, waitsForReview } from "../state/project";
import { formatDeadline } from "../state/agenda";
import { droppedLabel } from "../state/drop";
import { StepDrop } from "../components/StepDrop";

export interface ProjectProps {
  project: ProjectState;
  /** Agents seen without the skill in this project's folders. */
  presence?: PresenceRow[];
  anchor: string;
  canMutate: boolean;
  agentsOpen: boolean;
  onToggleAgents: () => void;
  /** Open the review item that still waits on a delivery. */
  onOpenNeed?: (needId: string) => void;
  onRefresh?: () => void;
  /** Graph selection is UI state, owned by the shell so Escape can clear it. */
  selectedNode?: string | undefined;
  onSelectNode?: (node: string | null) => void;
  /** Selected node opens a pending decision/review card. */
  onOpenCard?: (needId: string) => void;
  /** Shared answer action, used by the assumption strip. */
  action: AnswerAction;
  /** Motion is allowed right now (preference, reduced-motion, visibility, freshness). */
  motion: boolean;
  pulses: MotionPulse[];
  pulseToken: number;
  /** Latest pulse time per `${project}/${node}`, for the In-flight age label. */
  pulseAges: Readonly<Record<string, string>>;
  /** The last persisted answer, for the one orchestrated unblock moment. */
  unblock: UnblockPlay | null;
  onUnblockPlayed: (answerId: string) => void;
}

/**
 * Project page: header with an editable goal, What changed,
 * the dependency graph with In flight, assumptions, calibration, decisions,
 * rules, the policy panel and collapsed agents.
 */
export function Project({
  project,
  presence,
  anchor,
  canMutate,
  agentsOpen,
  onToggleAgents,
  onOpenNeed,
  onRefresh,
  selectedNode,
  onSelectNode,
  onOpenCard,
  action,
  motion,
  pulses,
  pulseToken,
  pulseAges,
  unblock,
  onUnblockPlayed,
}: ProjectProps) {
  const snooze = useSnooze();
  const client = useClient();
  const [repo, setRepo] = useState("");
  const [linkState, setLinkState] = useState<{ text: string; error: boolean } | null>(null);
  // Human-edited fields stay marked while this project page is mounted; the
  // projection does not publish whether a field's source event is human.
  const [edited, setEdited] = useState<
    Record<string, { fields: string[]; event: string }>
  >({});
  const summary = project.summary;
  const features = Object.values(project.features);
  const steps = planSteps(project.features);
  const waiting = reviewWaiting(project);
  const review = features
    .filter((feature) => waitsForReview(feature, waiting))
    .sort((a, b) => a.title.localeCompare(b.title));
  const decisions = Object.values(project.decisions);
  const deliveries = Object.values(project.deliveries).filter(
    (delivery) => !delivery.superseded_by,
  );
  const sessions = Object.values(project.sessions);
  const graphs = useProjectGraphs(client, [summary.slug], anchor);
  const graph = graphs[summary.slug];
  // Until the graph answers, the plan shows its lanes from the snapshot.
  const planGraph = useMemo(() => graph ?? planOutline(project.features), [graph, project.features]);
  const assumptions = liveAssumptions(project, anchor);
  const selectedFeature =
    selectedNode && selectedNode.startsWith("feature:")
      ? project.features[selectedNode.slice("feature:".length)]
      : undefined;
  const graphPlay = useMemo(() => {
    // `played` is the one-shot guard: after the animation starts (or motion is
    // suppressed) this returns null and the moment is never replayed.
    if (!unblock || unblock.played) return null;
    const playable = playableUnblock(unblock, summary.slug, Date.now());
    if (!playable) return null;
    return { ...playable, played: false };
  }, [summary.slug, unblock]);

  function selectNode(node: string | null): void {
    onSelectNode?.(node);
    if (!node) return;
    window.requestAnimationFrame?.(() => {
      document.getElementById(graphNodeElementId(node))?.scrollIntoView?.({ block: "nearest" });
    });
  }

  async function linkRepository(): Promise<void> {
    const path = repo.trim();
    if (!path) {
      setLinkState({ text: tx("proj.repo_choose"), error: true });
      return;
    }
    try {
      const data = await client.linkRepo({
        request_id: newRequestId(),
        repo: path,
        project: summary.slug,
      });
      setLinkState({ text: tx("proj.linked", { repo: data.repo, project: data.project }), error: false });
      setRepo("");
    } catch (cause) {
      setLinkState({
        text: cause instanceof Error ? cause.message : tx("proj.repo_failed"),
        error: true,
      });
    }
  }

  return (
    <div className="view">
      <div className="phead">
        <ProjectAvatar glyph={summary.glyph} size="lg" project={summary} />
        <div>
          <h2>{summary.title}</h2>
          <p>{summary.brief || tx("proj.no_brief")}</p>
          <GoalEditor
            key={summary.slug}
            project={project}
            canMutate={canMutate}
            {...(edited.goal ? { editedEvent: edited.goal.event } : {})}
            onEdited={(event) =>
              setEdited((current) => ({ ...current, goal: { fields: ["goal"], event } }))
            }
            {...(onRefresh ? { onRefresh } : {})}
          />
        </div>
        <div className="phead-acts">
          {summary.archived ? <span className="archived-tag">Archived</span> : null}
          <ArchiveToggle
            project={project}
            canMutate={canMutate}
            {...(onRefresh ? { onRefresh } : {})}
          />
        </div>
      </div>

      <ProjectStatsRow stats={project.stats} />
      <ProjectOverview project={project} motion={motion} />
      <DemoGapNote project={project} />
      <Corrections
        project={project}
        anchor={anchor}
        canMutate={canMutate}
        onOpenNeed={onOpenNeed}
        onRefresh={onRefresh}
      />

      {assumptions.length ? (
        <div className="sec">
          <div className="sec-h">
            <h3>{tx("proj.assumptions")}</h3>
            <span className="ghint">{`${assumptions.length} to confirm`}</span>
          </div>
          <Assumptions project={project} anchor={anchor} canMutate={canMutate} action={action} />
        </div>
      ) : null}

      <div className="sec">
        <div className="sec-h">
          <h3>{tx("proj.now")}</h3>
        </div>
        <NowCards
          project={project}
          {...(presence?.length ? { observed: presence } : {})}
          anchor={anchor}
          needs={project.needs_you.filter((need) => isActiveNeed(need) && !snooze?.dismissed(need.id))}
          stoppedRows={Object.fromEntries(
            project.needs_you
              .filter((need) => need.kind === "rescue" && need.node && (isInfoNeed(need) || snooze?.dismissed(need.id)))
              .map((need) => [need.node as string, need.id]),
          )}
          selected={selectedNode ?? null}
          onSelect={selectNode}
          pulseAges={pulseAges}
          motion={motion}
          {...(onOpenNeed ? { onOpenNeed } : {})}
        />
      </div>

      {project.reports?.length ? (
        <div className="sec">
          <div className="sec-h">
            <h3>{tx("proj.reports")}</h3>
          </div>
          <ReportCards project={summary.slug} reports={project.reports} anchor={anchor} />
        </div>
      ) : null}

      <div className="sec">
        <div className="sec-h">
          <h3>{tx("proj.activity")}</h3>
        </div>
        <EvidenceCards
          project={summary.slug}
          deliveries={deliveries}
          needs={project.needs_you}
          {...(onOpenNeed ? { onOpenNeed } : {})}
        />
        <ActivityFeed
          project={summary.slug}
          items={project.activity}
          onSelectNode={(node) => selectNode(`feature:${node}`)}
        />
      </div>

      <div className="sec">
        <div className="sec-h">
          <h3>{tx("proj.plan")}</h3>
          <span className="ghint">
            {selectedNode ? (
              <button type="button" className="link" onClick={() => selectNode(null)}>
                {tx("proj.show_all")}
              </button>
            ) : null}
          </span>
        </div>
        {graph === null ? (
          <>
            <div className="muted-note">The dependency graph is not available right now.</div>
            <ol className="steps">
              {steps.map(({ feature, depth }, index) => {
                const claims = etaClaims(feature);
                return (
                  <li
                    key={feature.node}
                    className="step"
                    data-step-node={feature.node}
                    data-depth={depth}
                    style={{ marginLeft: `${Math.min(depth, 5) * 14}px` }}
                  >
                    <span className="sn" aria-hidden="true">
                      {index + 1}
                    </span>
                    <span className="sbody">
                      <span className="stitle">
                        {feature.title}
                        <span className="node">{feature.node}</span>
                      </span>
                      <span className="smeta" title={claims.title}>
                        {feature.paused ? tx("proj.paused_pre") : ""}
                        {feature.progress === "dropped" ? droppedLabel(feature) : PROGRESS_LABEL[feature.progress]} ·{" "}
                        {gateLabel(feature.gate)}
                        {feature.deadline ? tx("proj.due", { d: formatDeadline(feature.deadline) }) : ""}
                        {claims.text ? ` · ${claims.text}` : ""} · priority {feature.priority}
                      </span>
                    </span>
                    <span className="step-acts">
                      <FeatureEditor
                        key={`${summary.slug}:${feature.node}:${feature.field_revisions.title ?? ""}:${
                          feature.field_revisions.deadline ?? ""
                        }:${feature.field_revisions.priority ?? ""}`}
                        project={project}
                        feature={feature}
                        canMutate={canMutate}
                        {...(edited[`feature:${feature.node}`]
                          ? { edited: edited[`feature:${feature.node}`]! }
                          : {})}
                        onEdited={(fields, event) =>
                          setEdited((current) => ({
                            ...current,
                            [`feature:${feature.node}`]: { fields, event },
                          }))
                        }
                        {...(onRefresh ? { onRefresh } : {})}
                      />
                      <StepDrop
                        key={`drop:${summary.slug}:${feature.node}`}
                        project={project}
                        feature={feature}
                        canMutate={canMutate}
                        onRefresh={onRefresh}
                      />
                    </span>
                  </li>
                );
              })}
            </ol>
          </>
        ) : (
          <PlanMap
            graph={planGraph}
            outline={!graph}
            project={project}
            selected={selectedNode ?? null}
            onSelect={selectNode}
            onOpenNeed={(needId) => (onOpenCard ?? onOpenNeed)?.(needId)}
            motion={motion}
            pulses={pulses}
            pulseToken={pulseToken}
            play={graphPlay}
            onPlayed={onUnblockPlayed}
            canMutate={canMutate}
            onRefresh={onRefresh}
          />
        )}
        {graph && selectedFeature ? (
          <div className="graph-detail" data-graph-detail={selectedFeature.node}>
            <span className="gd-title">
              {selectedFeature.title}
              <span className="node">{selectedFeature.node}</span>
            </span>
            {selectedFeature.progress === "dropped" ? (
              <span className="gd-dropped">{droppedLabel(selectedFeature)}</span>
            ) : null}
            <FeatureEditor
              key={`${summary.slug}:${selectedFeature.node}:${selectedFeature.field_revisions.title ?? ""}:${
                selectedFeature.field_revisions.deadline ?? ""
              }:${selectedFeature.field_revisions.priority ?? ""}`}
              project={project}
              feature={selectedFeature}
              canMutate={canMutate}
              {...(edited[`feature:${selectedFeature.node}`]
                ? { edited: edited[`feature:${selectedFeature.node}`]! }
                : {})}
              onEdited={(fields, event) =>
                setEdited((current) => ({
                  ...current,
                  [`feature:${selectedFeature.node}`]: { fields, event },
                }))
              }
              {...(onRefresh ? { onRefresh } : {})}
            />
            <StepDrop
              key={`drop:${summary.slug}:${selectedFeature.node}`}
              project={project}
              feature={selectedFeature}
              canMutate={canMutate}
              onRefresh={onRefresh}
            />
          </div>
        ) : null}
        {unblock && unblock.project === summary.slug ? (
          <div className="unblock-state" role="status">
            <Icon name={unblock.phase === "acked" ? "check" : "clock"} className="c2" />
            <span>
              {unblock.phase === "acked"
                ? tx("proj.acked")
                : tx("proj.awaiting_ack")}
            </span>
          </div>
        ) : null}
        <div className="calib">
          <Icon name="clock" />
          <span>{calibrationLine(summary.calibration)}</span>
        </div>
      </div>

      {review.length ? (
        <Fold title={tx("proj.ready_review", { n: review.length })}>
          <ul className="rlist">
            {review.map((feature) => (
              <li key={feature.node}>
                <button
                  type="button"
                  className="link"
                  onClick={() => selectNode(`feature:${feature.node}`)}
                >
                  {feature.title}
                </button>
              </li>
            ))}
          </ul>
        </Fold>
      ) : null}

      <Fold
        title={tx("proj.more", { d: decisions.length, r: project.rules.length, a: sessions.length })}
      >
      <div className="sec">
          <div className="sec-h">
            <h3>{tx("proj.decisions")}</h3>
          </div>
          {decisions.length ? (
            <div className="stubs">
              {decisions.map((decision) => {
                const receipt = decision.effective_receipt
                  ? project.receipts[decision.effective_receipt]
                  : undefined;
                return (
                  <span className="stub" key={decision.id}>
                    <span className="who">
                      <Icon name={receipt?.source === "human" ? "user" : "bot"} />
                    </span>
                    <span className="perf" />
                    <span className="what">{receipt?.chose.label ?? decision.card.question}</span>
                  </span>
                );
              })}
            </div>
          ) : (
            <div className="muted-note">No decisions yet.</div>
          )}
        </div>
  
        <div className="sec">
          <div className="sec-h">
            <h3>{tx("proj.rules")}</h3>
          </div>
          {project.rules.length ? (
            <div className="rules">
              {project.rules.map((rule) => (
                <div key={rule.value.rule_id}>
                  <Icon name="shield" />
                  <span>{rule.value.text}</span>
                  {rule.value.enabled ? null : <span className="muted">off</span>}
                </div>
              ))}
            </div>
          ) : (
            <div className="muted-note">No house rules yet.</div>
          )}
        </div>
  
        <div className="sec">
          <div className="sec-h">
            <h3>{tx("proj.policy")}</h3>
            <span className="ghint">
              Revision <span className="mono">{project.policy.revision.slice(0, 8)}</span>
            </span>
          </div>
          <PolicyPanel
            project={project}
            canMutate={canMutate}
            {...(onRefresh ? { onRefresh } : {})}
          />
        </div>
  
        <div className="sec">
          <div className="sec-h">
            <h3>{tx("proj.repos")}</h3>
          </div>
          <p className="muted-note">
            Link this project to a repository folder so CLI commands and hooks resolve it from the
            session directory instead of an environment variable.
          </p>
          <div className="form-row">
            <label htmlFor="link-repo">{tx("proj.repo_path")}</label>
            <input
              id="link-repo"
              type="text"
              placeholder={tx("proj.repo_placeholder")}
              value={repo}
              onChange={(event) => setRepo(event.target.value)}
            />
            <button type="button" className="btn" onClick={() => void linkRepository()}>
              <Icon name="link" />
              {tx("proj.link_repo")}
            </button>
          </div>
          {linkState ? (
            <p className="setup-note" role={linkState.error ? "alert" : "status"}>
              {linkState.text}
            </p>
          ) : null}
        </div>
  
        <div className="sec">
          <button type="button" className="agents-t" onClick={onToggleAgents} aria-expanded={agentsOpen}>
            <Icon name="terminal" />
            {sessions.length ? tx("proj.agents", { n: sessions.length }) : tx("proj.agents_none")}
            {sessions.length ? <Icon name="chevron-right" /> : null}
          </button>
          {agentsOpen && sessions.length ? (
            <div className="agents">
              {sessions.map((session) => (
                <div key={session.id}>
                  <Icon name={session.liveness === "live" ? "radio" : "eye-off"} className="c2" />
                  <span className="host">{session.host}</span>
                  <span>{session.harness.replace("_", " ")}</span>
                  <span>{session.current_node ?? tx("proj.no_node")}</span>
                  <span className="muted">
                    {tx("proj.last_event", { ago: formatAgo(session.last_event_ts, anchor) })}
                    {session.liveness === "lost" ? tx("proj.lost") : ""}
                  </span>
                </div>
              ))}
            </div>
          ) : null}
        </div>
      </Fold>
    </div>
  );
}

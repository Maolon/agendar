import { tx } from "../i18n";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { errorCode } from "../api/client";
import { formatAgo, newRequestId } from "../api/format";
import { useClient } from "../api/store";
import type { BlobRef, DeliveryState, NeedsYouItem, ProjectState } from "../api/types";
import { featureNodeId, indexGraph, reviewNodeId, reviewUnblockText } from "../graph/traversal";
import { CheckPanel } from "../components/Checks";
import { DemoView } from "../components/Demo";
import { DisclosureView } from "../components/Disclosure";
import { Icon } from "../components/Icon";
import { NeedSnooze } from "../components/NeedSnooze";
import { ProjectAvatar } from "../components/ProjectAvatar";
import { ReportPanel } from "../components/Report";
import { SendBack } from "../components/SendBack";
import {
  checkProposals,
  criterionFor,
  dependents,
  provenanceIcon,
  provenanceLabel,
  proposalKey,
  reasonProblem,
  reviewLabel,
  ruleIdFrom,
} from "../state/delivery";
import { useProjectGraphs } from "../state/graph";
import { ruleRevision } from "../state/hash";

interface DeliveryProps {
  project: ProjectState;
  delivery: DeliveryState;
  need: NeedsYouItem;
  anchor: string;
  canMutate: boolean;
  sendBackOpen: boolean;
  onSendBackOpen: (open: boolean) => void;
  onResolved: (message: string) => void;
  onRefresh: () => void;
  /** Cmd/Ctrl-Enter target owned by this view. */
  primaryRef?: { current: (() => void) | null };
}

interface DeliveryError {
  message: string;
  hint?: string;
}

/**
 * Delivery review. Read-only evidence first: the demo,
 * each receipt criterion with its provenance, and the three disclosure columns.
 * Accept and Send back commit exactly one `review_delivery` against the delivery
 * revision this view rendered; a stale revision shows a refreshed contradiction
 * and requires another deliberate click.
 */
export function Delivery({
  project,
  delivery,
  need,
  anchor,
  canMutate,
  sendBackOpen,
  onSendBackOpen,
  onResolved,
  onRefresh,
  primaryRef,
}: DeliveryProps) {
  const client = useClient();
  const [pending, setPending] = useState<"accept" | "send_back" | null>(null);
  const [error, setError] = useState<DeliveryError | null>(null);
  const [report, setReport] = useState<{ blob: BlobRef; title: string } | null>(null);
  const acceptRequest = useRef<string | null>(null);
  const sendBackRequest = useRef<string | null>(null);

  const slug = project.summary.slug;
  const feature = project.features[delivery.node];
  const slice = feature?.slice ?? delivery.node;
  const receipt = project.receipts[delivery.receipt];
  const cardReport = receipt
    ? project.decisions[receipt.ask]?.card.report
    : undefined;
  const proposals = checkProposals(delivery);
  const graphs = useProjectGraphs(client, [slug], anchor);
  const graph = graphs[slug];
  const graphIndex = useMemo(() => (graph ? indexGraph(graph) : null), [graph]);
  const reachable = graphIndex
    ? reviewUnblockText(
        graphIndex,
        project,
        reviewNodeId(delivery.id),
        featureNodeId(delivery.node),
      )
    : null;
  const unblocks =
    reachable?.labels ??
    (need.unblocks.length ? need.unblocks : dependents(project, delivery.node)).map(
      (node) => project.features[node]?.title ?? node,
    );

  function failure(cause: unknown, hint: string): DeliveryError {
    return {
      message: cause instanceof Error ? cause.message : tx("tray.failed"),
      hint: errorCode(cause) === "E_STALE_ANSWER" ? tx("dl.changed") : hint,
    };
  }

  const accept = useCallback((): void => {
    if (!canMutate || pending) return;
    void (async () => {
      setPending("accept");
      setError(null);
      const requestId = acceptRequest.current ?? newRequestId();
      acceptRequest.current = requestId;
      try {
        await client.reviewDelivery({
          request_id: requestId,
          delivery_id: delivery.id,
          expected_delivery_revision: delivery.revision,
          verdict: "accept",
        });
        acceptRequest.current = null;
        setPending(null);
        onResolved(tx("dl.accepted_x", { x: delivery.name }));
        onRefresh();
      } catch (cause) {
        setPending(null);
        setError(failure(cause, tx("dl.accept_failed")));
        onRefresh();
      }
    })();
  }, [canMutate, client, delivery.id, delivery.name, delivery.revision, onRefresh, onResolved, pending]);

  // Cmd/Ctrl-Enter accepts from anywhere in this view.
  useEffect(() => {
    if (!primaryRef) return;
    primaryRef.current = accept;
    return () => {
      primaryRef.current = null;
    };
  }, [accept, primaryRef]);

  function sendBack(note: string, saveRule: boolean): void {
    if (!canMutate || pending) return;
    const problem = reasonProblem(note);
    if (problem) {
      setError({ message: problem });
      return;
    }
    void (async () => {
      setPending("send_back");
      setError(null);
      const requestId = sendBackRequest.current ?? newRequestId();
      sendBackRequest.current = requestId;
      try {
        await client.reviewDelivery({
          request_id: requestId,
          delivery_id: delivery.id,
          expected_delivery_revision: delivery.revision,
          verdict: "send_back",
          reason: note,
        });
        sendBackRequest.current = null;
        // The house rule is a separate `set_rule` event after the review
        // (the brief's path); the review itself already recorded the note.
        let ruleSaved = false;
        let ruleProblem: string | null = null;
        if (saveRule) {
          const ruleId = ruleIdFrom(note, delivery.id.slice(-8).toLowerCase());
          try {
            const expected = await ruleRevision(project.rules, slug, ruleId);
            await client.setRule({
              request_id: newRequestId(),
              project: slug,
              expected_revision: expected,
              rule: { rule_id: ruleId, text: note, enabled: true },
            });
            ruleSaved = true;
          } catch (cause) {
            ruleProblem = cause instanceof Error ? cause.message : tx("dl.rule_failed");
          }
        }
        setPending(null);
        onSendBackOpen(false);
        onResolved(
          ruleSaved
            ? tx("dl.sent_back_rule", { x: slug })
            : tx("dl.sent_back", { x: slug }),
        );
        if (ruleProblem) {
          setError({
            message: ruleProblem,
            hint: tx("dl.rule_only"),
          });
        }
        onRefresh();
      } catch (cause) {
        setPending(null);
        setError(failure(cause, tx("dl.send_failed")));
        onRefresh();
      }
    })();
  }

  return (
    <div className="view">
      <div className="crumb">
        <ProjectAvatar glyph={project.summary.glyph} size="sm" project={project.summary} />
        <span>{slug}</span>
        <span className="sep">›</span>
        <Icon name="flag" className="c3" />
        <span>{slice}</span>
        <NeedSnooze id={need.id} />
      </div>
      <h1 className="h">{tx("dl.title", { x: delivery.name })}</h1>
      <div className="strip">
        <div className="l">
          <Icon name="package" className="c2" />
          <span>
            <b>{reviewLabel(delivery.review)}</b>
            {need.since ? ` · ${formatAgo(need.since, anchor)}` : ""}
          </span>
        </div>
        <div className="r">
          <span>
            {delivery.criteria.length === 1
              ? tx("dl.criterion.one")
              : tx("dl.criterion.other", { n: delivery.criteria.length })}
          </span>
        </div>
      </div>
      {unblocks.length ? (
        <div className="unblocks">
          <Icon name="git-branch" />
          <span>{tx("dl.unblocks", { list: unblocks.join(", ") })}</span>
        </div>
      ) : null}
      {receipt ? (
        <p className="lede">
          {tx("dl.asked_pre")}<b>{receipt.chose.label}</b>
          {tx("dl.asked_post")}
        </p>
      ) : null}

      {delivery.demos.map((demo, index) => (
        <DemoView key={`${delivery.id}-demo-${index}`} project={slug} demo={demo} />
      ))}

      <div className="crit" role="list" aria-label={tx("dl.criteria_label")}>
        {delivery.criteria.map((entry) => {
          const icon = provenanceIcon(entry.provenance);
          return (
            <div className="crit-row" role="listitem" key={entry.criterion.id}>
              <span className={icon === "eye" ? "crit-ic" : "crit-ic c2"}>
                <Icon name={icon} />
              </span>
              <span className="crit-text">
                <span className={entry.provenance === "not_proven_yet" ? "muted" : undefined}>
                  {entry.criterion.text}
                </span>
                <span className="crit-meta">
                  {provenanceLabel(entry.provenance)}
                  {entry.agent_result === "passed"
                    ? tx("dl.agent_passed")
                    : entry.agent_result === "failed"
                      ? tx("dl.agent_failed")
                      : tx("dl.agent_unproven")}
                  {entry.seen_result ? tx("dl.app_observed", { r: entry.seen_result }) : ""}
                </span>
                {entry.conflict ? (
                  <span className="crit-conflict">
                    <Icon name="alert-triangle" className="c2" />
                    {tx("dl.conflict")}
                  </span>
                ) : null}
              </span>
            </div>
          );
        })}
      </div>

      {proposals.map((proposal) => {
        const criterion = criterionFor(delivery, proposal.criterion);
        if (!criterion) return null;
        return (
          <CheckPanel
            key={`${delivery.id}:${proposalKey(proposal)}`}
            project={slug}
            delivery={delivery}
            criterion={criterion}
            proposal={proposal}
            canMutate={canMutate}
            onRefresh={onRefresh}
          />
        );
      })}

      <DisclosureView disclosure={delivery.disclosure} />

      {cardReport ? (
        <div className="acts" style={{ justifyContent: "flex-start" }}>
          <button
            type="button"
            className="btn"
            onClick={() => setReport({ blob: cardReport, title: tx("dl.report_title", { x: delivery.name }) })}
          >
            <Icon name="file-text" />
            {tx("dl.read_report")}
          </button>
        </div>
      ) : null}

      {error ? (
        <div className="alert" role="alert">
          <Icon name="alert-triangle" />
          <span className="body">
            <span>{error.message}</span>
            {error.hint ? <span className="hint">{error.hint}</span> : null}
          </span>
        </div>
      ) : null}

      {sendBackOpen ? (
        <SendBack
          project={slug}
          pending={pending === "send_back"}
          error={error}
          onSubmit={sendBack}
          onCancel={() => onSendBackOpen(false)}
        />
      ) : (
        <div className="acts">
          <button
            type="button"
            className="btn"
            onClick={() => onSendBackOpen(true)}
            disabled={!canMutate || pending !== null}
          >
            {tx("dl.send_back_open")}
          </button>
          <button
            type="button"
            className="btn primary"
            onClick={accept}
            disabled={!canMutate || pending !== null}
          >
            <Icon name="check" />
            {pending === "accept" ? tx("dl.accepting") : tx("dl.accept_x", { x: delivery.name })}
            <kbd>⌘↵</kbd>
          </button>
        </div>
      )}
      {!canMutate ? (
        <p className="muted-note">{tx("dl.disabled")}</p>
      ) : null}

      {report ? (
        <ReportPanel
          project={slug}
          blob={report.blob}
          title={report.title}
          onClose={() => setReport(null)}
        />
      ) : null}
    </div>
  );
}

import { tx } from "../i18n";
import { shortcut } from "../state/shortcut";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { errorCode } from "../api/client";
import { formatAgo, newRequestId } from "../api/format";
import { useClient } from "../api/store";
import type {
  BlobRef,
  DeliveryState,
  NeedsYouItem,
  ProjectState,
  ReviewAnchor,
  ReviewComment,
} from "../api/types";
import { featureNodeId, indexGraph, reviewNodeId, reviewUnblockText } from "../graph/traversal";
import { CheckPanel } from "../components/Checks";
import { DemoView } from "../components/Demo";
import { DisclosureView } from "../components/Disclosure";
import { Icon } from "../components/Icon";
import { NeedSnooze } from "../components/NeedSnooze";
import { ProjectAvatar } from "../components/ProjectAvatar";
import { ReportPanel } from "../components/Report";
import { SendBack, sendBackLabel } from "../components/SendBack";
import { NewTag, PreviousVersion, SameDemo, SinceSendBack } from "../components/SinceSendBack";
import { MAX_COMMENTS, type DemoComments } from "../state/comments";
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
import { compareDemos, compareDisclosure, redeliveryOf } from "../state/redelivery";
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
  // The previous version, open; `open` is the send-back comment it opened on.
  const [previous, setPrevious] = useState<{ open: number | null } | null>(null);
  // Comments pinned to lines and areas of the demos, in the order placed.
  const [comments, setComments] = useState<ReviewComment[]>([]);
  const acceptRequest = useRef<string | null>(null);
  // The review is durable once the command resolves; the snapshot that moves
  // this page on may arrive later. Until then the reviewed revision stays shut.
  const [reviewed, setReviewed] = useState<string | null>(null);
  const live = canMutate && reviewed !== delivery.revision;
  const sendBackRequest = useRef<string | null>(null);

  const slug = project.summary.slug;
  const feature = project.features[delivery.node];
  const slice = feature?.slice ?? delivery.node;
  const receipt = delivery.receipt ? project.receipts[delivery.receipt] : undefined;
  const cardReport = receipt
    ? project.decisions[receipt.ask]?.card.report
    : undefined;
  const proposals = checkProposals(delivery);
  // A redelivery after a send-back opens on what changed since.
  const redelivery = redeliveryOf(project, delivery);
  const demoDelta = redelivery ? compareDemos(delivery.demos, redelivery.previous.demos) : null;
  const disclosureDelta = redelivery
    ? compareDisclosure(delivery.disclosure, redelivery.previous.disclosure)
    : undefined;
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

  useEffect(() => {
    setComments([]);
    setPrevious(null);
  }, [delivery.id]);

  const addComment = useCallback((anchor: ReviewAnchor, text: string): void => {
    setComments((list) => (list.length >= MAX_COMMENTS ? list : [...list, { anchor, text }]));
  }, []);

  // A changed list is a new send-back request.
  const removeComment = useCallback((index: number): void => {
    sendBackRequest.current = null;
    setComments((list) => list.filter((_, at) => at !== index));
  }, []);

  const editComment = useCallback((index: number, text: string): void => {
    sendBackRequest.current = null;
    setComments((list) => list.map((comment, at) => (at === index ? { ...comment, text } : comment)));
  }, []);

  function demoComments(index: number): DemoComments {
    return {
      demo: index,
      enabled: live && pending === null && comments.length < MAX_COMMENTS,
      editable: live && pending === null,
      pins: comments
        .map((comment, at) => ({ n: at + 1, comment }))
        .filter(({ comment }) => comment.anchor.demo === index),
      onAdd: addComment,
      onRemove: removeComment,
      onEdit: editComment,
    };
  }

  function failure(cause: unknown, hint: string): DeliveryError {
    return {
      message: cause instanceof Error ? cause.message : tx("tray.failed"),
      hint: errorCode(cause) === "E_STALE_ANSWER" ? tx("dl.changed") : hint,
    };
  }

  const commented = comments.length > 0;
  const accept = useCallback((): void => {
    // Accepting would drop the comments: remove them first.
    if (!live || pending || commented) return;
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
        setReviewed(delivery.revision);
        setPending(null);
        onResolved(tx("dl.accepted_x", { x: delivery.name }));
        onRefresh();
      } catch (cause) {
        setPending(null);
        setError(failure(cause, tx("dl.accept_failed")));
        onRefresh();
      }
    })();
  }, [live, client, commented, delivery.id, delivery.name, delivery.revision, onRefresh, onResolved, pending]);

  // Cmd/Ctrl-Enter runs the primary action from anywhere in this view: accept,
  // or with comments open the send-back form.
  useEffect(() => {
    if (!primaryRef) return;
    primaryRef.current = commented ? () => onSendBackOpen(true) : accept;
    return () => {
      primaryRef.current = null;
    };
  }, [accept, commented, onSendBackOpen, primaryRef]);

  function sendBack(note: string, saveRule: boolean): void {
    if (!live || pending) return;
    const sent = comments;
    const problem = sent.length && !note.trim() && !saveRule ? null : reasonProblem(note);
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
          ...(note ? { reason: note } : {}),
          ...(sent.length ? { comments: sent } : {}),
        });
        sendBackRequest.current = null;
        setReviewed(delivery.revision);
        setComments([]);
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
      <h1 className="h">
        {delivery.receipt
          ? tx("dl.title", { x: delivery.name })
          : tx("dl.title_review", { x: delivery.name })}
      </h1>
      <div className="strip">
        <div className="l">
          <Icon name="package" className="c2" />
          <span>
            <b>{reviewLabel(delivery.review)}</b>
            {need.since ? ` · ${formatAgo(need.since, anchor)}` : ""}
          </span>
        </div>
        {delivery.criteria.length ? (
          <div className="r">
            <span>
              {delivery.criteria.length === 1
                ? tx("dl.criterion.one")
                : tx("dl.criterion.other", { n: delivery.criteria.length })}
            </span>
          </div>
        ) : null}
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

      {redelivery ? (
        <SinceSendBack
          delivery={delivery}
          redelivery={redelivery}
          anchor={anchor}
          onOpenPrevious={(comment) => setPrevious({ open: comment ?? null })}
        />
      ) : null}

      {delivery.demos.map((demo, index) => {
        const view = (
          <DemoView
            key={`${delivery.id}-demo-${index}`}
            project={slug}
            demo={demo}
            comments={demoComments(index)}
          />
        );
        const mark = demoDelta?.marks[index];
        if (mark === "same") {
          return (
            <SameDemo key={`${delivery.id}-same-${index}`} demo={demo}>
              {view}
            </SameDemo>
          );
        }
        if (mark === "new") {
          return (
            <div className="rd-demo-new" key={`${delivery.id}-new-${index}`}>
              <NewTag />
              {view}
            </div>
          );
        }
        return view;
      })}
      {demoDelta?.dropped ? (
        <p className="muted-note">
          {demoDelta.dropped === 1
            ? tx("rd.dropped.one")
            : tx("rd.dropped.other", { n: demoDelta.dropped })}
        </p>
      ) : null}

      {delivery.criteria.length ? (
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
      ) : null}

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

      <DisclosureView disclosure={delivery.disclosure} delta={disclosureDelta} />

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
          comments={comments}
          demos={delivery.demos}
          onRemoveComment={removeComment}
          onSubmit={sendBack}
          onCancel={() => onSendBackOpen(false)}
        />
      ) : (
        // Pinned to the bottom like a decision's answer: Accept and Send back
        // stay in reach below long demos and disclosures.
        <div className="acts dc-acts">
          {commented ? (
            <>
              {/* A disabled button shows no tooltip of its own in every engine. */}
              <span className="btn-wrap" title={tx("cm.accept_blocked")}>
                <button type="button" className="btn" disabled aria-describedby={`accept-blocked-${delivery.id}`}>
                  <Icon name="check" />
                  {tx("dl.accept_x", { x: delivery.name })}
                </button>
                <span id={`accept-blocked-${delivery.id}`} className="sr-only">
                  {tx("cm.accept_blocked")}
                </span>
              </span>
              <button
                type="button"
                className="btn primary"
                onClick={() => onSendBackOpen(true)}
                disabled={!live || pending !== null}
              >
                <Icon name="refresh-cw" />
                {sendBackLabel(comments.length)}
                <kbd>{shortcut("mod+enter")}</kbd>
              </button>
            </>
          ) : (
            <>
              <button
                type="button"
                className="btn"
                onClick={() => onSendBackOpen(true)}
                disabled={!live || pending !== null}
              >
                {tx("dl.send_back_open")}
              </button>
              <button
                type="button"
                className="btn primary"
                onClick={accept}
                disabled={!live || pending !== null}
              >
                <Icon name="check" />
                {pending === "accept" ? tx("dl.accepting") : tx("dl.accept_x", { x: delivery.name })}
                <kbd>{shortcut("mod+enter")}</kbd>
              </button>
            </>
          )}
        </div>
      )}
      {!canMutate ? (
        <p className="muted-note">{tx("dl.disabled")}</p>
      ) : null}

      {previous && redelivery ? (
        <PreviousVersion
          project={slug}
          previous={redelivery.previous}
          note={redelivery.note}
          open={previous.open}
          onClose={() => setPrevious(null)}
        />
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

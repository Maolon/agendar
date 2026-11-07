import { tx } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { errorCode } from "../api/client";
import { newRequestId } from "../api/format";
import { useClient } from "../api/store";
import type { CheckResult, DeliveryState, ProjectedCriterion } from "../api/types";
import { argvLine, profileFor, proposalKey, type CheckProposal } from "../state/delivery";
import { absentApprovalRevision } from "../state/hash";
import { Icon } from "./Icon";

interface CheckPanelProps {
  project: string;
  delivery: DeliveryState;
  criterion: ProjectedCriterion;
  proposal: CheckProposal;
  canMutate: boolean;
  onRefresh: () => void;
}

/**
 * A check the delivery proposes. Nothing is executed from
 * this view before `approve_check` resolves for the exact argv, cwd and
 * timeout shown here; the run then goes through `run_check` with that approval.
 */
export function CheckPanel({
  project,
  delivery,
  criterion,
  proposal,
  canMutate,
  onRefresh,
}: CheckPanelProps) {
  const client = useClient();
  const [phase, setPhase] = useState<"idle" | "running">("idle");
  const [result, setResult] = useState<CheckResult | null>(null);
  const [error, setError] = useState<{ message: string; hint?: string } | null>(null);
  const approval = useRef<{ id: string; revision: string } | null>(null);
  const approveRequest = useRef<string | null>(null);
  const runRequest = useRef<string | null>(null);
  // Approval is exact for one profile: a different argv, cwd or timeout (even
  // on the same delivery) drops the cached approval and its pending requests.
  const shownProfile = proposalKey(proposal);
  const lastProfile = useRef(shownProfile);
  useEffect(() => {
    if (lastProfile.current === shownProfile) return;
    lastProfile.current = shownProfile;
    approval.current = null;
    approveRequest.current = null;
    runRequest.current = null;
    setResult(null);
    setError(null);
    setPhase("idle");
  }, [shownProfile]);

  async function approveAndRun(): Promise<void> {
    if (!canMutate || phase === "running") return;
    setPhase("running");
    setError(null);
    setResult(null);
    try {
      let current = approval.current;
      if (!current) {
        const requestId = approveRequest.current ?? newRequestId();
        approveRequest.current = requestId;
        const profile = profileFor(proposal);
        // The first approval reports the exact absence revision this profile
        // hashes to, never a bare placeholder.
        const absent = await absentApprovalRevision({
          argv: proposal.argv,
          cwd: proposal.cwd,
          timeout_s: proposal.timeout_s,
        });
        const approved = await client.approveCheck({
          request_id: requestId,
          expected_approval_revision: absent,
          profile,
          consent: true,
        });
        approveRequest.current = null;
        // `approve_check` returns the profile digest, which `run_check` accepts
        // as the current approval revision (no approval projection needed).
        current = { id: approved.approval_id, revision: approved.approval_id };
        approval.current = current;
      }
      const runRequestId = runRequest.current ?? newRequestId();
      runRequest.current = runRequestId;
      const data = await client.runCheck({
        request_id: runRequestId,
        project,
        delivery_id: delivery.id,
        criterion: proposal.criterion,
        approval_id: current.id,
        expected_delivery_revision: delivery.revision,
        expected_approval_revision: current.revision,
      });
      runRequest.current = null;
      setResult(data.result);
      setPhase("idle");
      onRefresh();
    } catch (cause) {
      const code = errorCode(cause);
      if (code === "E_STALE_ANSWER" || code === "E_NOT_IMPLEMENTED") {
        // The approval no longer matches: the next click must approve again.
        approval.current = null;
        approveRequest.current = null;
        runRequest.current = null;
      }
      setError({
        message: cause instanceof Error ? cause.message : tx("ck.failed"),
        hint:
          code === "E_UNSUPPORTED"
            ? tx("ck.unsupported")
            : code === "E_STALE_ANSWER"
              ? tx("ck.stale")
              : tx("ck.not_run"),
      });
      setPhase("idle");
      onRefresh();
    }
  }

  const seen = criterion.provenance === "seen";
  const shown = result ?? criterion.seen_result ?? null;

  return (
    <div className="check">
      <div className="check-h">
        <Icon name="terminal" className="c2" />
        <span>{tx("ck.proposed", { x: criterion.criterion.text })}</span>
      </div>
      <dl className="check-grid">
        <dt>argv</dt>
        <dd>
          <code className="mono">{argvLine(proposal.argv)}</code>
        </dd>
        <dt>cwd</dt>
        <dd>
          <code className="mono">{proposal.cwd}</code>
        </dd>
        <dt>timeout</dt>
        <dd>{proposal.timeout_s}s</dd>
      </dl>
      <p className="ghint">
        {tx("ck.scope")}
      </p>
      <div className="acts" style={{ justifyContent: "flex-start" }}>
        <button
          type="button"
          className="btn"
          onClick={() => void approveAndRun()}
          disabled={!canMutate || phase === "running"}
        >
          <Icon name="check" />
          {phase === "running"
            ? tx("ck.running")
            : seen
              ? tx("ck.rerun")
              : tx("ck.run")}
        </button>
        {shown ? (
          <span
            className={shown === "passed" ? "check-result" : "check-result failed"}
            role="status"
          >
            <Icon name="eye" className="c2" />
            {tx("ck.seen", { r: shown === "passed" ? tx("ck.passed") : shown === "failed" ? tx("ck.failed_r") : tx("ck.unproven") })}
          </span>
        ) : (
          <span className="muted-note">{tx("ck.not_yet")}</span>
        )}
      </div>
      {shown ? (
        <p className="ghint">
          {tx("ck.output")}
        </p>
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
      {!canMutate ? (
        <p className="muted-note">{tx("ck.disabled")}</p>
      ) : null}
    </div>
  );
}

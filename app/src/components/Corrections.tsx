import { useState } from "react";
import { tx } from "../i18n";
import { formatAgo, newRequestId } from "../api/format";
import { useClient } from "../api/store";
import type { DeliveryState, ProjectState } from "../api/types";
import { correctedSteps, correctionReason } from "../state/claim";
import { reasonProblem } from "../state/delivery";
import { CorrectionMark } from "./CorrectionMark";

interface CorrectionsProps {
  project: ProjectState;
  anchor: string;
  canMutate: boolean;
  onOpenNeed?: ((needId: string) => void) | undefined;
  onRefresh?: (() => void) | undefined;
}

/**
 * One line per project when the board corrected any step: "3 corrected",
 * opening the list with each one's reason and the owner's actions (open an
 * abandoned step's details, send an accepted delivery back).
 */
export function Corrections({ project, anchor, canMutate, onOpenNeed, onRefresh }: CorrectionsProps) {
  const steps = correctedSteps(project, project.needs_you);
  if (!steps.length) return null;
  const count = steps.length === 1 ? tx("corr.count.one") : tx("corr.count.other", { n: steps.length });
  return (
    <details className="corr-line" data-testid="corrections">
      <summary>{count}</summary>
      <ul className="corr-list">
        {steps.map(({ feature, correction, details, sendBack }) => (
          <li key={feature.node} data-corrected={feature.node}>
            <CorrectionMark correction={correction} />
            <b>{feature.title}</b>
            <span className="corr-why">
              {correctionReason(correction)} · {formatAgo(correction.since, anchor)}
            </span>
            {details && onOpenNeed ? (
              <button type="button" className="link" onClick={() => onOpenNeed(details.id)}>
                {tx("corr.details")}
              </button>
            ) : null}
            {sendBack && canMutate ? (
              <SendBackInline delivery={sendBack} {...(onRefresh ? { onRefresh } : {})} />
            ) : null}
          </li>
        ))}
      </ul>
    </details>
  );
}

function SendBackInline({ delivery, onRefresh }: { delivery: DeliveryState; onRefresh?: () => void }) {
  const [open, setOpen] = useState(false);
  if (!open) {
    return (
      <button type="button" className="link" onClick={() => setOpen(true)}>
        {tx("corr.send_back")}
      </button>
    );
  }
  return (
    <SendBackForm
      delivery={delivery}
      onDone={() => {
        setOpen(false);
        onRefresh?.();
      }}
    />
  );
}

/** The reason and one Send back: a later send-back outranks the accept. */
function SendBackForm({ delivery, onDone }: { delivery: DeliveryState; onDone: () => void }) {
  const client = useClient();
  const [note, setNote] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  async function submit(): Promise<void> {
    const problem = reasonProblem(note);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await client.reviewDelivery({
        request_id: newRequestId(),
        delivery_id: delivery.id,
        expected_delivery_revision: delivery.revision,
        verdict: "send_back",
        reason: note.trim(),
      });
      onDone();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : tx("tray.failed"));
    } finally {
      setBusy(false);
    }
  }
  return (
    <span className="corr-send">
      <input
        type="text"
        value={note}
        aria-label={tx("corr.send_back")}
        onChange={(event) => setNote(event.target.value)}
        disabled={busy}
      />
      <button type="button" className="btn" disabled={busy} onClick={() => void submit()}>
        {tx("corr.send_back")}
      </button>
      {error ? <span className="err">{error}</span> : null}
    </span>
  );
}

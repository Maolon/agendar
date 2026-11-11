import { useRef, useState } from "react";
import { useClient } from "../api/store";
import type { FeatureState, ProjectState } from "../api/types";
import { tx } from "../i18n";
import { canDrop, DROP_NOTE_MAX, dropRequests } from "../state/drop";
import { harnessLabel } from "./NowCards";

interface StepDropProps {
  project: ProjectState;
  feature: FeatureState;
  canMutate: boolean;
  onRefresh?: (() => void) | undefined;
}

/** The agent that holds a step in progress, by its harness name. */
export function holderOf(project: ProjectState, feature: FeatureState): string | null {
  if (feature.progress !== "doing") return null;
  if (feature.claim ? feature.claim.state === "free" : feature.liveness === "lost") return null;
  return harnessLabel(feature.session ? project.sessions[feature.session]?.harness : undefined);
}

/**
 * The owner drops one step: a link, then one inline confirm with an optional
 * note. A step someone works on names its holder in the confirm.
 */
export function StepDrop({ project, feature, canMutate, onRefresh }: StepDropProps) {
  const client = useClient();
  const [open, setOpen] = useState(false);
  const [note, setNote] = useState("");
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const request = useRef(dropRequests());
  if (!canDrop(feature)) return null;
  const holder = holderOf(project, feature);

  async function drop(): Promise<void> {
    if (pending) return;
    setPending(true);
    setFailure(null);
    try {
      await client.dropSteps(request.current(project.summary.slug, [feature.node], note));
      setOpen(false);
      setNote("");
      onRefresh?.();
    } catch (cause) {
      setFailure(cause instanceof Error ? cause.message : tx("plan.drop_failed"));
    } finally {
      setPending(false);
    }
  }

  if (!open) {
    return (
      <button
        type="button"
        className="link step-drop"
        aria-label={tx("plan.drop_x", { x: feature.title })}
        disabled={!canMutate}
        onClick={() => {
          setFailure(null);
          setOpen(true);
        }}
      >
        {tx("plan.drop")}
      </button>
    );
  }

  return (
    <form
      className="drop-form"
      aria-label={tx("plan.drop_x", { x: feature.title })}
      onSubmit={(event) => {
        event.preventDefault();
        void drop();
      }}
      onKeyDown={(event) => {
        if (event.key !== "Escape" || pending) return;
        event.preventDefault();
        setOpen(false);
      }}
    >
      {holder ? <span className="drop-held">{tx("plan.drop_held", { who: holder })}</span> : null}
      <input
        type="text"
        aria-label={tx("plan.drop_note")}
        placeholder={tx("plan.drop_note")}
        value={note}
        maxLength={DROP_NOTE_MAX}
        disabled={pending}
        autoFocus
        onChange={(event) => {
          setNote(event.target.value);
          setFailure(null);
        }}
      />
      <button type="button" className="link" onClick={() => setOpen(false)} disabled={pending}>
        {tx("action.cancel")}
      </button>
      <button type="submit" className="btn sm danger" disabled={pending || !canMutate}>
        {tx("plan.drop_step")}
      </button>
      {failure ? (
        <span className="error" role="alert">
          {failure}
        </span>
      ) : null}
    </form>
  );
}

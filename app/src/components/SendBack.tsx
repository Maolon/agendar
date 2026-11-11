import { tx } from "../i18n";
import { useState } from "react";
import type { Demo, ReviewComment } from "../api/types";
import { CommentList } from "./Comments";
import { Icon } from "./Icon";

interface SendBackProps {
  project: string;
  pending: boolean;
  /** Reason problem reported by the API, shown above the buttons. */
  error?: { message: string; hint?: string } | null;
  /** Anchored comments placed on the demos; with any, the note is optional. */
  comments?: ReviewComment[];
  demos?: Demo[];
  onRemoveComment?: (index: number) => void;
  onSubmit: (note: string, saveRule: boolean) => void;
  onCancel: () => void;
}

/** "Send back with N comments", or plain "Send back" without any. */
export function sendBackLabel(count: number): string {
  if (count === 0) return tx("sb.send");
  return count === 1 ? tx("cm.send.one") : tx("cm.send.other", { n: count });
}

/**
 * Send-back form. A rejection requires a non-blank note unless comments are
 * pinned to the demos; the house-rule checkbox is opt-in, saves the note, and
 * never starts work on its own.
 */
export function SendBack({
  project,
  pending,
  error,
  comments = [],
  demos = [],
  onRemoveComment,
  onSubmit,
  onCancel,
}: SendBackProps) {
  const [note, setNote] = useState("");
  const [saveRule, setSaveRule] = useState(false);
  const [local, setLocal] = useState<string | null>(null);
  const inputId = `send-back-note-${project}`;
  const ruleId = `send-back-rule-${project}`;

  function submit(event: React.FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const trimmed = note.trim();
    if (!trimmed && (comments.length === 0 || saveRule)) {
      setLocal(tx("dl.reason_blank"));
      return;
    }
    setLocal(null);
    onSubmit(trimmed, saveRule);
  }

  const what = comments.length ? tx("cm.optional") : tx("sb.what");
  return (
    <form className="sendback" onSubmit={submit} aria-label={tx("sb.label")}>
      <CommentList
        comments={comments}
        demos={demos}
        disabled={pending}
        onRemove={(index) => onRemoveComment?.(index)}
      />
      <label htmlFor={inputId}>{what}</label>
      <textarea
        id={inputId}
        value={note}
        placeholder={what}
        onChange={(event) => setNote(event.target.value)}
        disabled={pending}
      />
      <label className="check" htmlFor={ruleId}>
        <input
          id={ruleId}
          type="checkbox"
          checked={saveRule}
          onChange={(event) => setSaveRule(event.target.checked)}
          disabled={pending}
        />
        {tx("sb.rule", { x: project })}
      </label>
      {local ? (
        <p className="setup-note" role="alert">
          {local}
        </p>
      ) : null}
      {error ? (
        <p className="setup-note" role="alert">
          {error.message}
          {error.hint ? ` ${error.hint}` : ""}
        </p>
      ) : null}
      <div className="acts">
        <button type="button" className="btn" onClick={onCancel} disabled={pending}>
          {tx("action.cancel")}
        </button>
        <button type="submit" className="btn primary" disabled={pending}>
          <Icon name="refresh-cw" />
          {pending ? tx("sb.sending") : sendBackLabel(comments.length)}
        </button>
      </div>
    </form>
  );
}

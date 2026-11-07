import { tx } from "../i18n";
import { useState } from "react";
import { Icon } from "./Icon";

interface SendBackProps {
  project: string;
  pending: boolean;
  /** Reason problem reported by the API, shown above the buttons. */
  error?: { message: string; hint?: string } | null;
  onSubmit: (note: string, saveRule: boolean) => void;
  onCancel: () => void;
}

/**
 * Send-back form. A rejection requires a non-blank note; the
 * house-rule checkbox is opt-in and never starts work on its own.
 */
export function SendBack({ project, pending, error, onSubmit, onCancel }: SendBackProps) {
  const [note, setNote] = useState("");
  const [saveRule, setSaveRule] = useState(false);
  const [local, setLocal] = useState<string | null>(null);
  const inputId = `send-back-note-${project}`;
  const ruleId = `send-back-rule-${project}`;

  function submit(event: React.FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const trimmed = note.trim();
    if (!trimmed) {
      setLocal(tx("dl.reason_blank"));
      return;
    }
    setLocal(null);
    onSubmit(trimmed, saveRule);
  }

  return (
    <form className="sendback" onSubmit={submit} aria-label={tx("sb.label")}>
      <label htmlFor={inputId}>{tx("sb.what")}</label>
      <textarea
        id={inputId}
        value={note}
        placeholder={tx("sb.what")}
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
          {pending ? tx("sb.sending") : tx("sb.send")}
        </button>
      </div>
    </form>
  );
}

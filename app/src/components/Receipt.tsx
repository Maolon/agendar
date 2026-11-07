import { tx } from "../i18n";
import { useEffect, useState } from "react";
import { formatEtaFinish, trimmedScalarLength } from "../api/format";
import type { Criterion, DecisionOption, EtaRange } from "../api/types";
import { Icon } from "./Icon";

export interface ReceiptPreviewView {
  chose: DecisionOption;
  ruled_out: DecisionOption[];
  done_when: Criterion[];
  expected_eta?: EtaRange;
}

interface ReceiptProps {
  preview: ReceiptPreviewView;
  anchor: string;
  /** "Preview" until an event exists; "Saved" only for a persisted receipt. */
  tag: string;
  editable: boolean;
  onDoneWhen: (criteria: Criterion[] | null) => void;
}

interface DraftState {
  open: boolean;
  texts: string[];
  error: string | null;
}

const CLOSED: DraftState = { open: false, texts: [], error: null };

function validate(texts: string[]): string | null {
  if (texts.length < 1 || texts.length > 3) return tx("receipt.count");
  for (const text of texts) {
    const length = trimmedScalarLength(text);
    if (length === 0) return tx("receipt.blank");
    if (length > 175) return tx("receipt.too_long");
  }
  return null;
}

/**
 * The receipt ticket. It is a preview until the backend returns a persisted
 * receipt; the done-when criteria can be edited (1–3 valid criteria) and the
 * unedited original is kept when the editor is not saved.
 */
export function Receipt({ preview, anchor, tag, editable, onDoneWhen }: ReceiptProps) {
  const [draft, setDraft] = useState<DraftState>(CLOSED);

  useEffect(() => {
    setDraft(CLOSED);
  }, [preview.chose.id]);

  const criteria = preview.done_when;
  const expected = preview.expected_eta ? formatEtaFinish(preview.expected_eta, anchor) : "";
  const ruledOut = preview.ruled_out.map((option) => option.label).join(" · ") || "—";

  function openEditor(): void {
    setDraft({ open: true, texts: preview.done_when.map((item) => item.text), error: null });
  }

  function saveEditor(): void {
    const texts = draft.texts.map((text) => text.trim()).filter((text) => text.length > 0);
    const error = validate(texts);
    if (error) {
      setDraft((state) => ({ ...state, error }));
      return;
    }
    const next: Criterion[] = texts.map((text, index) => ({
      id: preview.done_when[index]?.id ?? `c${index + 1}`,
      text,
    }));
    onDoneWhen(next);
    setDraft(CLOSED);
  }

  return (
    <div className="ticket" aria-label={tx("receipt.title")}>
      <div className="tk-h">
        <span>{tx("receipt.title")}</span>
        <span>{tag}</span>
      </div>
      <div className="tk-perf" />
      <div className="tk-row">
        <Icon name="check" />
        <span className="k">{tx("receipt.chose")}</span>
        <span className="v">{preview.chose.label}</span>
      </div>
      <div className="tk-row">
        <Icon name="x" className="c2" />
        <span className="k">{tx("receipt.ruled_out")}</span>
        <span className="v">{ruledOut}</span>
      </div>
      <div className="tk-row">
        <Icon name="flag" />
        <span className="k">{tx("receipt.done_when")}</span>
        {draft.open ? (
          <div className="done-editor">
            {draft.texts.map((text, index) => (
              <input
                key={index}
                type="text"
                value={text}
                aria-label={tx("receipt.criterion", { n: index + 1 })}
                onChange={(event) => {
                  const next = [...draft.texts];
                  next[index] = event.target.value;
                  setDraft({ ...draft, texts: next, error: null });
                }}
              />
            ))}
            {draft.texts.length < 3 ? (
              <button
                type="button"
                className="link"
                onClick={() => setDraft({ ...draft, texts: [...draft.texts, ""], error: null })}
              >
                {tx("receipt.add")}
              </button>
            ) : null}
            {draft.error ? (
              <span className="muted" role="alert">
                {draft.error}
              </span>
            ) : null}
            <div className="acts">
              <button type="button" className="link" onClick={() => setDraft(CLOSED)}>
                {tx("action.cancel")}
              </button>
              <button type="button" className="btn" onClick={saveEditor}>
                {tx("receipt.save")}
              </button>
            </div>
          </div>
        ) : (
          <span className="v">{criteria.map((item) => item.text).join(" · ")}</span>
        )}
      </div>
      {expected ? (
        <div className="tk-row">
          <Icon name="clock" />
          <span className="k">{tx("receipt.expected")}</span>
          <span className="v">{tx("receipt.agent_says", { time: expected })}</span>
        </div>
      ) : null}
      {editable && !draft.open ? (
        <div className="acts">
          <button type="button" className="link" onClick={openEditor}>
            {tx("receipt.edit")}
          </button>
        </div>
      ) : null}
    </div>
  );
}

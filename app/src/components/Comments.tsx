import { tx } from "../i18n";
import { useEffect, useRef, useState } from "react";
import type { Demo, ReviewComment } from "../api/types";
import { MAX_COMMENT_TEXT, commentLabel, type Pin } from "../state/comments";
import { Icon } from "./Icon";

/** Inline one-line comment box: Enter saves, Esc cancels. */
export function CommentBox({
  label,
  initial = "",
  ariaLabel,
  onSave,
  onCancel,
}: {
  label: string;
  /** Text to start from (editing a comment). */
  initial?: string;
  /** Replaces "Comment on {label}" as the field's name. */
  ariaLabel?: string;
  onSave: (text: string) => void;
  onCancel: () => void;
}) {
  const [text, setText] = useState(initial);
  return (
    <div className="cm-box">
      <input
        // The box opens on a deliberate click; focus follows it.
        autoFocus
        type="text"
        aria-label={ariaLabel ?? tx("cm.box", { x: label })}
        placeholder={tx("cm.placeholder")}
        maxLength={MAX_COMMENT_TEXT}
        value={text}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing) return;
          if (event.key === "Enter") {
            event.preventDefault();
            const trimmed = text.trim();
            if (trimmed) onSave(trimmed);
          } else if (event.key === "Escape") {
            // Handled here: the window shortcut skips prevented events.
            event.preventDefault();
            onCancel();
          }
        }}
      />
    </div>
  );
}

/** Numbered markers for the comments at one place. */
export function Pins({ pins }: { pins: Pin[] }) {
  if (!pins.length) return null;
  return (
    <span className="cm-pins">
      {pins.map((pin) => (
        <span className="cm-pin" key={pin.n} title={pin.comment.text}>
          {pin.n}
        </span>
      ))}
    </span>
  );
}

/**
 * The comments of a sent-back version, read under the line they sit on. The
 * one the page opened on scrolls into view and takes focus.
 */
export function PastNotes({ pins, open }: { pins: Pin[]; open: number | null }) {
  if (!pins.length) return null;
  return (
    <span className="cm-notes">
      {pins.map((pin) => (
        <PastNote key={pin.n} pin={pin} hot={pin.n === open} />
      ))}
    </span>
  );
}

function PastNote({ pin, hot }: { pin: Pin; hot: boolean }) {
  const ref = useRef<HTMLSpanElement | null>(null);
  useEffect(() => {
    if (!hot) return;
    ref.current?.focus({ preventScroll: true });
    ref.current?.scrollIntoView?.({ block: "center" });
  }, [hot]);
  return (
    <span
      ref={ref}
      className={hot ? "cm-note hot" : "cm-note"}
      role="note"
      aria-label={tx("vt.comment_n", { n: pin.n })}
      tabIndex={hot ? -1 : undefined}
    >
      <span className="cm-pin">{pin.n}</span>
      <span className="cm-note-text">{pin.comment.text}</span>
    </span>
  );
}

/** The comments of a send-back, each with its location and a remove button. */
export function CommentList({
  comments,
  demos,
  disabled,
  onRemove,
}: {
  comments: ReviewComment[];
  demos: Demo[];
  disabled: boolean;
  onRemove: (index: number) => void;
}) {
  if (!comments.length) return null;
  return (
    <ol className="cm-list" aria-label={tx("cm.list")}>
      {comments.map((comment, index) => (
        <li key={index}>
          <span className="cm-pin">{index + 1}</span>
          <span className="cm-loc">{commentLabel(comment.anchor, demos)}</span>
          <span className="cm-text">{comment.text}</span>
          <button
            type="button"
            className="cm-remove"
            aria-label={tx("cm.remove", { n: index + 1 })}
            title={tx("cm.remove", { n: index + 1 })}
            disabled={disabled}
            onClick={() => onRemove(index)}
          >
            <Icon name="x" />
          </button>
        </li>
      ))}
    </ol>
  );
}

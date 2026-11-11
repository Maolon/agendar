import { tx } from "../i18n";
import { useRef, useState } from "react";
import type { Demo } from "../api/types";
import { useBlob } from "../hooks/useBlob";
import { demoAlt, demoBlob, demoUrl, mediaUrl } from "../state/delivery";
import { clipQuote, linePins, transcriptLines, type DemoComments } from "../state/comments";
import { CommentBox, PastNotes, Pins } from "./Comments";
import { DiffDemoView } from "./DiffDemo";
import { Icon } from "./Icon";
import { RegionPins, WHOLE_IMAGE, type ImageMarks } from "./MarkStage";
import { MediaViewer } from "./MediaViewer";

interface DemoProps {
  project: string;
  demo: Demo;
  /** Present on the review page: comment on lines and areas of this demo. */
  comments?: DemoComments;
}

/**
 * One delivery demo. Screenshots, CLI transcripts and core diffs are shown; anything else
 * says so instead of inventing a renderer.
 *
 * A screenshot is a preview that opens full screen on click, where areas are
 * marked; the transcript is rendered as escaped text children inside `<pre>`, never HTML.
 */
export function DemoView({ project, demo, comments }: DemoProps) {
  return (
    <div className="demo">
      <DemoBody project={project} demo={demo} comments={comments} />
    </div>
  );
}

function DemoBody({ project, demo, comments }: DemoProps) {
  if (demo.kind === "screenshot") return <Screenshot project={project} demo={demo} comments={comments} />;
  if (demo.kind === "cli_transcript") {
    return <Transcript project={project} demo={demo} comments={comments} />;
  }
  if (demo.kind === "diff") return <DiffDemoView project={project} demo={demo} comments={comments} />;
  if (demo.kind === "preview_url") {
    const url = demoUrl(demo);
    return (
      <div className="muted-note">
        {url
          ? tx("demo.preview", { url })
          : tx("demo.no_url")}
      </div>
    );
  }
  return (
    <div className="muted-note">
      {tx("demo.recording")}
    </div>
  );
}

function Screenshot({ project, demo, comments }: DemoProps) {
  const alt = demoAlt(demo);
  const blob = demoBlob(demo);
  const view = useBlob(project, blob);
  const pins = comments?.pins.filter(({ comment }) => comment.anchor.kind === "region") ?? [];
  const past = comments?.past;
  // A sent-back version asked for at one of this image's comments opens on it.
  const first = past && pins.some((pin) => pin.n === past.open) ? past.open : null;
  // Open: plain, or with the whole-image comment box ("Comment on image").
  const [viewing, setViewing] = useState<"view" | "whole" | null>(first === null ? null : "view");
  // The comment the viewer opens on.
  const [at, setAt] = useState<number | null>(first);
  const shot = useRef<HTMLButtonElement | null>(null);

  if (view.phase === "loading") {
    return <div className="thumb thumb-empty">{tx("demo.loading")}</div>;
  }
  if (view.phase === "error" || !view.token) {
    return (
      <div className="thumb thumb-empty" role="note">
        <Icon name="file-text" className="c3" />
        <span>{view.message ?? tx("ev.shot_unreadable")}</span>
      </div>
    );
  }
  const src = mediaUrl(view.token);
  const enabled = Boolean(comments?.enabled);
  // Pins are placed on the whole image, so a commented screenshot shows it whole and in colour.
  const marked = pins.length > 0;
  const marks: ImageMarks | undefined = comments
    ? {
        pins,
        enabled,
        editable: comments.editable,
        label: tx("cm.area_of", { x: alt }),
        onAdd: (region, text) => comments.onAdd({ kind: "region", demo: comments.demo, ...region }, text),
        // Pins carry page-wide numbers; the page's list is 0-based.
        onRemove: (n) => comments.onRemove(n - 1),
        onEdit: (n, text) => comments.onEdit(n - 1, text),
      }
    : undefined;

  return (
    <div className="shot">
      <button
        type="button"
        ref={shot}
        className={marked ? "shot-open" : "shot-closed"}
        aria-label={tx("demo.open", { x: alt })}
        onClick={() => {
          setAt(null);
          setViewing("view");
        }}
      >
        {marked ? (
          <span className="shot-canvas">
            <img src={src} alt={alt} loading="lazy" className="shot-img full" draggable={false} />
            <RegionPins pins={pins} />
          </span>
        ) : (
          <img src={src} alt={alt} loading="lazy" className="shot-img" />
        )}
      </button>
      <div className="shot-foot">
        <p className="muted-note">{enabled ? tx("demo.hint_mark") : tx("demo.hint")}</p>
        {enabled ? (
          <button type="button" className="cm-whole" onClick={() => setViewing("whole")}>
            <Icon name="message-square" />
            {tx("cm.whole")}
          </button>
        ) : null}
      </div>
      {past && pins.length ? (
        <ol className="cm-shot-notes" aria-label={tx("cm.list")}>
          {pins.map(({ n, comment }) => (
            <li key={n}>
              <button
                type="button"
                className="cm-note"
                title={tx("rd.open_spot")}
                onClick={() => {
                  setAt(n);
                  setViewing("view");
                }}
              >
                <span className="cm-pin">{n}</span>
                <span className="cm-note-text">{comment.text}</span>
              </button>
            </li>
          ))}
        </ol>
      ) : null}
      {viewing ? (
        <MediaViewer
          project={project}
          items={[{ kind: "image", blob, title: past ? tx("rd.prev_title", { x: alt }) : alt, marks }]}
          initialMark={viewing === "whole" ? WHOLE_IMAGE : undefined}
          initialPin={at ?? undefined}
          returnFocus={shot}
          onClose={() => setViewing(null)}
        />
      ) : null}
    </div>
  );
}

function Transcript({ project, demo, comments }: DemoProps) {
  const blob = demoBlob(demo);
  const view = useBlob(project, blob);
  const [draft, setDraft] = useState<number | null>(null);
  if (view.phase === "loading") return <pre className="term">{tx("ev.reading")}</pre>;
  if (view.phase === "error") {
    return (
      <pre className="term" role="note">
        {view.message ?? tx("ev.read_failed")}
      </pre>
    );
  }
  if (view.text === undefined) {
    return (
      <pre className="term" role="note">
        {tx("demo.no_text")}
      </pre>
    );
  }
  if (!comments) {
    // Escaped text children: a transcript can contain markup-looking output.
    return <pre className="term">{view.text}</pre>;
  }
  // Line by line, numbered as the host numbers them, so a line can be targeted.
  const lines = transcriptLines(view.text);
  return (
    <pre className={comments.enabled ? "term lines can-comment" : "term lines"}>
      {lines.map((text, index) => {
        const n = index + 1;
        const pins = linePins(comments.pins, n);
        return (
          <span className={comments.past && pins.length ? "term-line pinned" : "term-line"} key={n}>
            {comments.enabled ? (
              <button
                type="button"
                className="cm-add"
                aria-label={tx("cm.add", { x: tx("cm.loc.line", { n }) })}
                title={tx("cm.add", { x: tx("cm.loc.line", { n }) })}
                onClick={() => setDraft(draft === n ? null : n)}
              >
                <span className="ln">{n}</span>
                <span className="plus">+</span>
              </button>
            ) : null}
            <span className="tx">{text || " "}</span>
            {comments.past ? <PastNotes pins={pins} open={comments.past.open} /> : <Pins pins={pins} />}
            {draft === n ? (
              <CommentBox
                label={tx("cm.loc.line", { n })}
                onCancel={() => setDraft(null)}
                onSave={(comment) => {
                  comments.onAdd(
                    { kind: "transcript_line", demo: comments.demo, line: n, quote: clipQuote(text) },
                    comment,
                  );
                  setDraft(null);
                }}
              />
            ) : null}
          </span>
        );
      })}
    </pre>
  );
}

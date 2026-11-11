import { tx } from "../i18n";
import { useState } from "react";
import type { Demo, DiffDemoBody } from "../api/types";
import { useBlob } from "../hooks/useBlob";
import { diffBody, parseHunks, shortSha, splitPath, type DiffHunk, type DiffLine } from "../state/diff";
import { clipQuote, diffLineLabel, diffPins, type DemoComments } from "../state/comments";
import { CommentBox, PastNotes, Pins } from "./Comments";

/** A hunk longer than this collapses its middle. */
const COLLAPSE_OVER = 40;
const HEAD = 20;
const TAIL = 12;

/**
 * The core of a commit's diff: only the hunks the agent picked, cut verbatim by
 * the CLI, each with the agent's reason. Lines are escaped text children.
 */
export function DiffDemoView({
  project,
  demo,
  comments,
}: {
  project: string;
  demo: Demo;
  comments?: DemoComments;
}) {
  const body = diffBody(demo);
  const view = useBlob(project, body?.blob);
  if (!body) return <div className="muted-note">{tx("demo.diff.failed")}</div>;
  const files = new Set(body.shown.map((item) => item.path)).size;
  return (
    <div className="diff">
      <div className="diff-head">
        <span className="diff-title">{tx("demo.diff.title", { sha: shortSha(body.commit) })}</span>
        <span className="diff-counts">
          {tx("demo.diff.counts", {
            f: files,
            ft: body.files_total,
            h: body.shown.length,
            ht: body.hunks_total,
          })}
        </span>
        <span className="diff-chip">{tx("demo.diff.verbatim")}</span>
      </div>
      {view.phase === "loading" ? (
        <p className="muted-note">{tx("demo.diff.reading")}</p>
      ) : view.phase === "error" || view.text === undefined ? (
        <p className="muted-note" role="note">
          {view.message ?? tx("demo.diff.failed")}
        </p>
      ) : (
        <Hunks body={body} hunks={parseHunks(view.text)} comments={comments} />
      )}
      <Omitted body={body} shownFiles={files} />
    </div>
  );
}

function Hunks({
  body,
  hunks,
  comments,
}: {
  body: DiffDemoBody;
  hunks: DiffHunk[];
  comments?: DemoComments;
}) {
  // One open comment box per diff: "<hunk>:<row>".
  const [draft, setDraft] = useState<string | null>(null);
  return (
    <>
      {body.shown.map((shown, index) => {
        const hunk = hunks[index];
        const [dir, base] = splitPath(shown.path);
        return (
          <section className="diff-hunk" key={`${shown.path}:${shown.hunk}`}>
            <div className="diff-path">
              <span className="dir">{dir}</span>
              <span className="base">{base}</span>
            </div>
            <p className="diff-why">{shown.why}</p>
            {hunk ? (
              <HunkLines
                hunk={hunk}
                path={shown.path}
                id={String(index)}
                comments={comments}
                draft={draft}
                onDraft={setDraft}
              />
            ) : null}
          </section>
        );
      })}
    </>
  );
}

interface LineComments {
  path: string;
  /** This hunk's key in the diff's draft id. */
  id: string;
  comments?: DemoComments;
  draft: string | null;
  onDraft: (id: string | null) => void;
}

function HunkLines({ hunk, ...line }: { hunk: DiffHunk } & LineComments) {
  const [open, setOpen] = useState(false);
  // A sent-back version never folds a line that carries a comment.
  const commented =
    line.comments?.past !== undefined &&
    hunk.lines.slice(HEAD, hunk.lines.length - TAIL).some((row) => {
      const target = rowAnchor(row);
      return target !== null && diffPins(line.comments!.pins, line.path, target.side, target.number).length > 0;
    });
  const long = hunk.lines.length > COLLAPSE_OVER && !open && !commented;
  const head = long ? hunk.lines.slice(0, HEAD) : hunk.lines;
  const tail = long ? hunk.lines.slice(hunk.lines.length - TAIL) : [];
  const hidden = hunk.lines.length - HEAD - TAIL;
  const tailStart = hunk.lines.length - tail.length;
  return (
    <div className="diff-lines">
      <div className="diff-row hdr">
        <span className="n" />
        <span className="n" />
        <span className="t">{hunk.header}</span>
      </div>
      {head.map((row, index) => (
        <Row key={`h${index}`} line={row} at={index} {...line} />
      ))}
      {long ? (
        <button type="button" className="diff-more" onClick={() => setOpen(true)}>
          {tx("demo.diff.more", { n: hidden })}
        </button>
      ) : null}
      {tail.map((row, index) => (
        <Row key={`t${index}`} line={row} at={tailStart + index} {...line} />
      ))}
    </div>
  );
}

const MARK: Record<DiffLine["kind"], string> = { add: "+", del: "-", ctx: " ", note: "" };

/** The side and number a comment on this row points at; a context line counts as new. */
function rowAnchor(line: DiffLine): { side: "old" | "new"; number: number } | null {
  if (line.kind === "del" && line.old !== undefined) return { side: "old", number: line.old };
  if ((line.kind === "add" || line.kind === "ctx") && line.new !== undefined) {
    return { side: "new", number: line.new };
  }
  return null;
}

function Row({ line, at, path, id, comments, draft, onDraft }: { line: DiffLine; at: number } & LineComments) {
  const target = rowAnchor(line);
  const key = `${id}:${at}`;
  const commentable = Boolean(comments?.enabled && target);
  const pins = comments && target ? diffPins(comments.pins, path, target.side, target.number) : [];
  const label = target ? diffLineLabel(path, target.side, target.number) : "";
  const past = comments?.past;
  return (
    <>
      <div className={`diff-row ${line.kind}${commentable ? " can-comment" : ""}${past && pins.length ? " pinned" : ""}`}>
        <span className="n">{line.old ?? ""}</span>
        <span className="n">{line.new ?? ""}</span>
        <span className="t">
          {MARK[line.kind]}
          {line.text}
          {past ? null : <Pins pins={pins} />}
        </span>
        {commentable ? (
          <button
            type="button"
            className="cm-add"
            aria-label={tx("cm.add", { x: label })}
            title={tx("cm.add", { x: label })}
            onClick={() => onDraft(draft === key ? null : key)}
          >
            +
          </button>
        ) : null}
      </div>
      {past ? <PastNotes pins={pins} open={past.open} /> : null}
      {comments && target && draft === key ? (
        <CommentBox
          label={label}
          onCancel={() => onDraft(null)}
          onSave={(text) => {
            comments.onAdd(
              {
                kind: "diff_line",
                demo: comments.demo,
                file: path,
                side: target.side,
                line: target.number,
                quote: clipQuote(line.text),
              },
              text,
            );
            onDraft(null);
          }}
        />
      ) : null}
    </>
  );
}

function Omitted({ body, shownFiles }: { body: DiffDemoBody; shownFiles: number }) {
  const [open, setOpen] = useState(false);
  const count = Math.max(body.files_total - shownFiles, body.omitted.length);
  if (count <= 0) return null;
  const added = body.omitted.reduce((total, file) => total + file.added, 0);
  const removed = body.omitted.reduce((total, file) => total + file.removed, 0);
  const label =
    count === 1
      ? tx("demo.diff.omitted.one", { a: added, r: removed })
      : tx("demo.diff.omitted.other", { n: count, a: added, r: removed });
  return (
    <div className="diff-omitted">
      <button
        type="button"
        className="diff-omitted-toggle"
        aria-expanded={open}
        disabled={body.omitted.length === 0}
        onClick={() => setOpen((value) => !value)}
      >
        {label}
      </button>
      {open ? (
        <ul className="diff-omitted-list">
          {body.omitted.map((file) => {
            const [dir, base] = splitPath(file.path);
            return (
              <li key={file.path}>
                <span className="diff-path">
                  <span className="dir">{dir}</span>
                  <span className="base">{base}</span>
                </span>
                <span className="diff-stat">
                  <span className="add">+{file.added}</span> <span className="del">−{file.removed}</span>
                </span>
              </li>
            );
          })}
        </ul>
      ) : null}
    </div>
  );
}

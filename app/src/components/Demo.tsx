import { tx } from "../i18n";
import { useState } from "react";
import type { Demo } from "../api/types";
import { useBlob } from "../hooks/useBlob";
import { demoAlt, demoBlob, demoUrl, mediaUrl } from "../state/delivery";
import { Icon } from "./Icon";

interface DemoProps {
  project: string;
  demo: Demo;
}

/**
 * One delivery demo. Screenshots and CLI transcripts are shown; anything else
 * says so instead of inventing a renderer.
 *
 * A screenshot is desaturated in place and opens at full colour on click; the
 * transcript is rendered as escaped text children inside `<pre>`, never HTML.
 */
export function DemoView({ project, demo }: DemoProps) {
  return (
    <div className="demo">
      <DemoBody project={project} demo={demo} />
    </div>
  );
}

function DemoBody({ project, demo }: DemoProps) {
  if (demo.kind === "screenshot") return <Screenshot project={project} demo={demo} />;
  if (demo.kind === "cli_transcript") return <Transcript project={project} demo={demo} />;
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

function Screenshot({ project, demo }: DemoProps) {
  const alt = demoAlt(demo);
  const blob = demoBlob(demo);
  const view = useBlob(project, blob);
  const [open, setOpen] = useState(false);

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
  return (
    <div className="shot">
      <button
        type="button"
        className={open ? "shot-open" : "shot-closed"}
        aria-pressed={open}
        aria-label={open ? tx("demo.desat", { x: alt }) : tx("demo.colour", { x: alt })}
        onClick={() => setOpen((value) => !value)}
      >
        <img
          src={src}
          alt={alt}
          loading="lazy"
          className={open ? "shot-img full" : "shot-img"}
        />
      </button>
      <p className="muted-note">
        {open
          ? tx("demo.full")
          : tx("demo.grey")}
      </p>
    </div>
  );
}

function Transcript({ project, demo }: DemoProps) {
  const blob = demoBlob(demo);
  const view = useBlob(project, blob);
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
  // Escaped text children: a transcript can contain markup-looking output.
  return <pre className="term">{view.text}</pre>;
}

import { useId, useState, type ReactNode } from "react";
import { Icon } from "./Icon";

interface FoldProps {
  title: string;
  children: ReactNode;
}

/**
 * Collapsed-by-default disclosure. The toggle is a real button that reports
 * `aria-expanded` and controls its panel; the panel is `hidden` until opened so
 * screen readers and keyboard focus skip it while it is closed.
 */
export function Fold({ title, children }: FoldProps) {
  const [open, setOpen] = useState(false);
  const id = useId();
  const panelId = `${id}-panel`;
  return (
    <div className="fold">
      <button
        type="button"
        className="fold-toggle"
        aria-expanded={open}
        aria-controls={panelId}
        onClick={() => setOpen((value) => !value)}
      >
        <Icon name="chevron-right" className="fold-chevron" />
        <span>{title}</span>
      </button>
      <div id={panelId} className="fold-panel" hidden={!open}>
        {children}
      </div>
    </div>
  );
}

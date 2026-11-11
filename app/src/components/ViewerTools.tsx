import { tx } from "../i18n";
import { Fragment, useState, type KeyboardEvent, type ReactNode, type SyntheticEvent } from "react";
import type { Pin } from "../state/comments";
import { shortcut, type Shortcut } from "../state/shortcut";
import { Icon, type IconName } from "./Icon";
import type { Tool } from "./ImageViewport";

/** Toolbar and list buttons keep focus on the stage when clicked, so its keys keep working. */
const keepFocus = (event: SyntheticEvent) => event.preventDefault();

/** "Zoom in (⌘+)": the tool's name and its shortcut on this platform. */
export function toolTip(label: string, key?: Shortcut): string {
  return key ? `${label} (${shortcut(key)})` : label;
}

interface ToolButton {
  key: string;
  label: string;
  shortcut?: Shortcut;
  icon?: IconName;
  text?: string;
  pressed?: boolean;
  disabled?: boolean;
  /** Tooltip instead of the label and shortcut (a disabled tool's reason). */
  tip?: string;
  onClick: () => void;
}

export interface ToolbarProps {
  tool: Tool;
  onTool: (tool: Tool) => void;
  /** Mark: absent, available, or disabled with a reason. */
  mark: { disabled: boolean; reason?: string } | null;
  percent: number | null;
  fitted: boolean;
  onZoomIn: () => void;
  onZoomOut: () => void;
  onActual: () => void;
  onFit: () => void;
  history: { canUndo: boolean; canRedo: boolean; onUndo: () => void; onRedo: () => void } | null;
  whole: { disabled: boolean; onClick: () => void } | null;
}

/**
 * The viewer's side toolbar: tools, zoom and undo. A vertical toolbar with
 * one tab stop; ↑/↓ move between its buttons.
 */
export function ViewerToolbar(props: ToolbarProps) {
  const { tool, onTool, mark, percent, fitted, history, whole } = props;
  const groups: ToolButton[][] = [
    [
      ...(mark
        ? [
            {
              key: "mark",
              label: tx("vt.mark"),
              shortcut: "m" as const,
              icon: "square-dashed" as const,
              pressed: tool === "mark" && !mark.disabled,
              disabled: mark.disabled,
              tip: mark.disabled ? mark.reason : undefined,
              onClick: () => onTool("mark"),
            },
          ]
        : []),
      {
        key: "hand",
        label: tx("vt.hand"),
        shortcut: "h",
        icon: "hand",
        pressed: tool === "hand",
        onClick: () => onTool("hand"),
      },
    ],
    [
      { key: "in", label: tx("vt.zoom_in"), shortcut: "mod+=", icon: "zoom-in", onClick: props.onZoomIn },
      { key: "out", label: tx("vt.zoom_out"), shortcut: "mod+-", icon: "zoom-out", onClick: props.onZoomOut },
      { key: "actual", label: tx("mv.actual"), shortcut: "mod+1", text: "1:1", pressed: percent === 100, onClick: props.onActual },
      { key: "fit", label: tx("mv.fit"), shortcut: "mod+0", icon: "scan", pressed: fitted, onClick: props.onFit },
    ],
    ...(history
      ? [
          [
            {
              key: "undo",
              label: tx("vt.undo"),
              shortcut: "mod+z" as const,
              icon: "undo-2" as const,
              disabled: !history.canUndo,
              onClick: history.onUndo,
            },
            {
              key: "redo",
              label: tx("vt.redo"),
              shortcut: "shift+mod+z" as const,
              icon: "redo-2" as const,
              disabled: !history.canRedo,
              onClick: history.onRedo,
            },
          ],
        ]
      : []),
    ...(whole
      ? [[{ key: "whole", label: tx("cm.whole"), icon: "message-square" as const, disabled: whole.disabled, onClick: whole.onClick }]]
      : []),
  ];
  const flat = groups.flat();
  const [current, setCurrent] = useState("hand");
  const stop = flat.some((button) => button.key === current && !button.disabled)
    ? current
    : (flat.find((button) => !button.disabled)?.key ?? "");

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>): void {
    const keys = ["ArrowDown", "ArrowUp", "Home", "End"];
    if (!keys.includes(event.key)) return;
    event.preventDefault();
    event.stopPropagation();
    const buttons = [
      ...event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled):not([aria-disabled='true'])"),
    ];
    if (!buttons.length) return;
    const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const next =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? buttons.length - 1
          : (at + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
    buttons[next]?.focus();
  }

  const render = (button: ToolButton): ReactNode => {
    const tip = button.tip ?? toolTip(button.label, button.shortcut);
    return (
      <button
        type="button"
        className="mv-tool"
        tabIndex={button.key === stop ? 0 : -1}
        aria-label={button.label}
        aria-keyshortcuts={button.shortcut ? ariaKeys(button.shortcut) : undefined}
        aria-pressed={button.pressed === undefined ? undefined : button.pressed}
        title={tip}
        // A disabled tool with a reason stays hoverable so its tooltip shows.
        disabled={button.disabled && !button.tip}
        aria-disabled={button.disabled && button.tip ? true : undefined}
        onMouseDown={keepFocus}
        onFocus={() => setCurrent(button.key)}
        onClick={button.disabled ? undefined : button.onClick}
      >
        {button.icon ? <Icon name={button.icon} /> : <span className="mv-tool-text">{button.text}</span>}
      </button>
    );
  };

  return (
    <div className="mv-tools" role="toolbar" aria-orientation="vertical" aria-label={tx("vt.tools")} onKeyDown={onKeyDown}>
      {groups.map((group, index) => (
        <div className="mv-tool-group" key={index}>
          {group.map((button) => (
            <Fragment key={button.key}>
              {render(button)}
              {button.key === "in" ? (
                <output className="mv-zoom-pct" aria-label={tx("vt.zoom")}>
                  {percent === null ? "–" : `${percent}%`}
                </output>
              ) : null}
            </Fragment>
          ))}
        </div>
      ))}
    </div>
  );
}

/** `aria-keyshortcuts` spelling of a shortcut (both modifier keys accepted). */
function ariaKeys(key: Shortcut): string {
  const plain: Partial<Record<Shortcut, string>> = { m: "M", h: "H" };
  if (plain[key]) return plain[key]!;
  const tail = key.replace("shift+", "").replace("mod+", "");
  const name = tail === "=" ? "Plus" : tail === "-" ? "Minus" : tail.toUpperCase();
  const shift = key.startsWith("shift+") ? "Shift+" : "";
  return `Meta+${shift}${name} Control+${shift}${name}`;
}

/**
 * The comments on this image, at the viewer's right edge: page-wide number,
 * text and ×. Hovering a row lights its area; clicking it brings the area
 * into view and selects it.
 */
export function CommentRail({
  pins,
  editable,
  open,
  selected,
  onToggle,
  onHot,
  onPick,
  onRemove,
}: {
  pins: Pin[];
  editable: boolean;
  open: boolean;
  selected: number | null;
  onToggle: () => void;
  onHot: (n: number | null) => void;
  onPick: (n: number) => void;
  onRemove: (n: number) => void;
}) {
  const toggle = open ? tx("vt.hide_list") : tx("vt.show_list");
  return (
    <aside className={open ? "mv-rail open" : "mv-rail"} aria-label={tx("cm.list")}>
      <button
        type="button"
        className="mv-rail-toggle"
        aria-expanded={open}
        aria-label={toggle}
        title={toggle}
        onMouseDown={keepFocus}
        onClick={onToggle}
      >
        <Icon name="message-square" />
        <span className="mv-rail-count">{pins.length}</span>
      </button>
      {open ? (
        <ol className="mv-rail-list">
          {pins.map(({ n, comment }) => (
            <li
              key={n}
              className={n === selected ? "sel" : undefined}
              onMouseEnter={() => onHot(n)}
              onMouseLeave={() => onHot(null)}
            >
              <button
                type="button"
                className="mv-rail-row"
                aria-label={tx("vt.comment_n", { n })}
                aria-current={n === selected ? "true" : undefined}
                onMouseDown={keepFocus}
                onFocus={() => onHot(n)}
                onBlur={() => onHot(null)}
                onClick={() => onPick(n)}
              >
                <span className="cm-pin">{n}</span>
                <span className="mv-rail-text">{comment.text}</span>
              </button>
              {editable ? (
                <button
                  type="button"
                  className="mv-rail-x"
                  aria-label={tx("cm.remove", { n })}
                  title={tx("cm.remove", { n })}
                  onMouseDown={keepFocus}
                  onClick={() => onRemove(n)}
                >
                  <Icon name="x" size={14} />
                </button>
              ) : null}
            </li>
          ))}
        </ol>
      ) : null}
    </aside>
  );
}

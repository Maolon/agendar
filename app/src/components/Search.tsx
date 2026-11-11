import { tx, useI18n, type MessageKey } from "../i18n";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode, RefObject } from "react";
import { formatAgo } from "../api/format";
import type { Snapshot } from "../api/types";
import {
  GROUP_CAP,
  buildSearchIndex,
  runSearch,
  type HitKind,
  type SearchHit,
  type SearchTarget,
} from "../state/search";
import { shortcut } from "../state/shortcut";
import { Icon, type IconName } from "./Icon";

interface SearchProps {
  inputRef: RefObject<HTMLInputElement | null>;
  value: string;
  onChange: (text: string) => void;
  snapshot: Snapshot | null;
  /** Open one result in its best existing view. */
  onOpen: (target: SearchTarget) => void;
}

const GROUP_LABEL: Record<HitKind, MessageKey> = {
  need: "search.g.need",
  project: "search.g.project",
  step: "search.g.step",
  decision: "search.g.decision",
  report: "search.g.report",
  delivery: "search.g.delivery",
};

const DEBOUNCE_MS = 60;

function rowIcon(hit: SearchHit): { name: IconName; tone: string } {
  const { kind, variant } = hit.item;
  switch (kind) {
    case "need":
      return {
        name:
          variant === "review"
            ? "package"
            : variant === "rescue"
              ? "alert-triangle"
              : variant === "confirm"
                ? "circle-dashed"
                : "git-branch",
        tone: "sr-amber",
      };
    case "project":
      return { name: "box", tone: "" };
    case "step":
      if (variant === "verified" || variant === "done") return { name: "check", tone: "sr-green" };
      if (variant === "doing") return { name: "circle-dot", tone: "sr-fg" };
      if (variant === "dropped") return { name: "x", tone: "" };
      return { name: "circle-dashed", tone: "" };
    case "decision":
      return { name: "git-branch", tone: "" };
    case "report":
      return { name: "file-text", tone: "" };
    case "delivery":
      return { name: "package", tone: "" };
  }
}

function Highlighted({ text, ranges }: { text: string; ranges: [number, number][] }) {
  if (!ranges.length) return <>{text}</>;
  const parts: ReactNode[] = [];
  let at = 0;
  for (const [start, end] of ranges) {
    if (start > at) parts.push(text.slice(at, start));
    parts.push(<mark key={start}>{text.slice(start, end)}</mark>);
    at = end;
  }
  if (at < text.length) parts.push(text.slice(at));
  return <>{parts}</>;
}

type Row =
  | { type: "hit"; id: string; hit: SearchHit }
  | { type: "more"; id: string; kind: HitKind; count: number };

/**
 * The search field and its results panel: matches across every project,
 * grouped by type, opened in place. Cmd/Ctrl-K focuses and selects the field
 * (window keyboard policy); ↑/↓ move, Enter opens, Esc closes.
 */
export function Search({ inputRef, value, onChange, snapshot, onOpen }: SearchProps) {
  const { language } = useI18n();
  const base = useId();
  const listId = `${base}-results`;
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const [focused, setFocused] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [query, setQuery] = useState(value);
  const [active, setActive] = useState(0);
  const [expanded, setExpanded] = useState<ReadonlySet<HitKind>>(new Set());

  // A light debounce; clearing the field closes at once.
  useEffect(() => {
    if (!value.trim()) {
      setQuery("");
      return;
    }
    const timer = window.setTimeout(() => setQuery(value), DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [value]);

  useEffect(() => {
    setActive(0);
    setExpanded(new Set());
  }, [query]);

  // Labels inside the index follow the language.
  const index = useMemo(() => (snapshot ? buildSearchIndex(snapshot) : []), [snapshot, language]);
  const groups = useMemo(() => runSearch(index, query), [index, query]);

  const rows: Row[] = [];
  const grouped = groups.map((group) => {
    const shown = expanded.has(group.kind) ? group.hits : group.hits.slice(0, GROUP_CAP);
    const groupRows: Row[] = shown.map((hit) => ({
      type: "hit" as const,
      id: `${base}-o${rows.length}-${hit.item.key}`,
      hit,
    }));
    for (const row of groupRows) rows.push(row);
    let more: Row | null = null;
    if (shown.length < group.hits.length) {
      more = { type: "more", id: `${base}-more-${group.kind}`, kind: group.kind, count: group.hits.length - shown.length };
      rows.push(more);
    }
    return { kind: group.kind, total: group.hits.length, rows: more ? [...groupRows, more] : groupRows };
  });

  const open = focused && !dismissed && value.trim().length > 0 && query.trim().length > 0;
  const activeIndex = Math.min(active, Math.max(rows.length - 1, 0));
  const activeRow = open ? rows[activeIndex] : undefined;

  useEffect(() => {
    if (!activeRow) return;
    document.getElementById(activeRow.id)?.scrollIntoView?.({ block: "nearest" });
  }, [activeRow]);

  const anchor = snapshot?.state.evaluated_at ?? new Date().toISOString();

  function close(): void {
    setFocused(false);
    inputRef.current?.blur();
  }

  function activate(row: Row): void {
    if (row.type === "more") {
      const at = rows.indexOf(row);
      setExpanded((current) => new Set(current).add(row.kind));
      // The first newly shown row takes the place of "N more".
      setActive(at);
      return;
    }
    close();
    onOpen(row.hit.item.target);
  }

  function onKeyDown(event: ReactKeyboardEvent<HTMLInputElement>): void {
    if (event.nativeEvent.isComposing || event.key === "Process") return;
    if (event.key === "Escape") {
      event.preventDefault();
      setDismissed(true);
      close();
      return;
    }
    if (!open) {
      if (event.key === "ArrowDown" && value.trim()) {
        event.preventDefault();
        setDismissed(false);
      }
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!rows.length) return;
      const step = event.key === "ArrowDown" ? 1 : -1;
      setActive(Math.min(Math.max(activeIndex + step, 0), rows.length - 1));
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      if (activeRow) activate(activeRow);
    }
  }

  return (
    <div
      className="search-wrap"
      ref={wrapRef}
      onBlur={(event) => {
        const next = event.relatedTarget;
        if (!(next instanceof Node) || !wrapRef.current?.contains(next)) setFocused(false);
      }}
    >
      <label className="search">
        <Icon name="search" />
        <input
          aria-label={tx("search.label")}
          aria-keyshortcuts="Meta+K Control+K"
          ref={inputRef}
          type="text"
          role="combobox"
          aria-expanded={open}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={activeRow?.id}
          placeholder={tx("search.placeholder")}
          autoComplete="off"
          spellCheck={false}
          value={value}
          onFocus={() => {
            setFocused(true);
            setDismissed(false);
          }}
          onChange={(event) => {
            setDismissed(false);
            onChange(event.target.value);
          }}
          onKeyDown={onKeyDown}
        />
        {value ? (
          <button
            type="button"
            className="search-clear"
            aria-label={tx("search.clear")}
            title={tx("search.clear")}
            // Focus stays in the field.
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => {
              onChange("");
              inputRef.current?.focus();
            }}
          >
            <Icon name="x" size={13} />
          </button>
        ) : null}
        <kbd aria-hidden="true">{shortcut("mod+k")}</kbd>
      </label>
      {open ? (
        <div
          className="search-panel"
          id={listId}
          role="listbox"
          aria-label={tx("search.results")}
          // Clicks keep focus in the field, so the panel stays the field's popup.
          onMouseDown={(event) => event.preventDefault()}
        >
          {rows.length === 0 ? (
            <div className="sr-none" role="presentation">
              {tx("search.none")}
            </div>
          ) : (
            grouped.map((group) => {
              const labelId = `${base}-g-${group.kind}`;
              return (
                <div key={group.kind} role="group" aria-labelledby={labelId} className="sr-group">
                  <div id={labelId} className="sr-gh" role="presentation">
                    {tx(GROUP_LABEL[group.kind])}
                    <span className="sr-n">{group.total}</span>
                  </div>
                  {group.rows.map((row) => {
                    const index = rows.indexOf(row);
                    const selected = activeRow?.id === row.id;
                    if (row.type === "more") {
                      return (
                        <div
                          key={row.id}
                          id={row.id}
                          role="option"
                          aria-selected={selected}
                          className="sr-row sr-more"
                          onMouseMove={() => setActive(index)}
                          onClick={() => activate(row)}
                        >
                          {tx("search.more", { n: row.count })}
                        </div>
                      );
                    }
                    const { item } = row.hit;
                    const icon = rowIcon(row.hit);
                    const ago = item.ts ? formatAgo(item.ts, anchor) : "";
                    const context = [...item.context, ago].filter(Boolean).join(" · ");
                    return (
                      <div
                        key={row.id}
                        id={row.id}
                        role="option"
                        aria-selected={selected}
                        className="sr-row"
                        data-kind={item.kind}
                        onMouseMove={() => setActive(index)}
                        onClick={() => activate(row)}
                      >
                        <Icon name={icon.name} className={`sr-ic ${icon.tone}`} />
                        <span className="sr-text">
                          <span className="sr-title">
                            <Highlighted text={item.title} ranges={row.hit.ranges} />
                          </span>
                          {context ? <span className="sr-ctx">{context}</span> : null}
                        </span>
                      </div>
                    );
                  })}
                </div>
              );
            })
          )}
        </div>
      ) : null}
    </div>
  );
}

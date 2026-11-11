import { tx } from "../i18n";
import { shortcut } from "../state/shortcut";
import { useContext, useEffect, useRef, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent, ReactNode, RefObject } from "react";
import type { ApiError } from "../api/client";
import type { Snapshot, WindowChrome } from "../api/types";
import { ClientContext } from "../api/store";
import { useGlassRegions, useWindowActive } from "../hooks/useWindowChrome";
import type { SearchTarget } from "../state/search";
import type { Selection } from "../state/ui";
import { Icon } from "./Icon";
import { Search } from "./Search";
import { Sidebar } from "./Sidebar";

interface ShellProps {
  snapshot: Snapshot | null;
  phase: "loading" | "ready" | "stale" | "error";
  error: ApiError | null;
  /** Text in the search field. */
  filter: string;
  selection: Selection | null;
  quietOpen: boolean;
  devFixture: boolean;
  /** Motion is allowed right now; a pulse flashes a track segment. */
  motion: boolean;
  /** `${project}/${node}` keys pulsed in the latest batch. */
  pulses: ReadonlySet<string>;
  searchRef: RefObject<HTMLInputElement | null>;
  onFilter: (text: string) => void;
  /** Open one search result. */
  onSearchOpen?: (target: SearchTarget) => void;
  onSelect: (selection: Selection) => void;
  onToggleQuiet: () => void;
  onOpenSettings?: () => void;
  settingsOpen?: boolean;
  /** Flip the stored motion preference. */
  onRefresh: () => void;
  /** How this window paints its chrome (overlay title bar, glass or solid). */
  chrome?: WindowChrome;
  children: ReactNode;
}

export const PANE_DEFAULT = 330;
export const PANE_MIN = 200;
/** Icon-only sidebar: dragging below halfway to PANE_MIN snaps here. */
export const PANE_RAIL = 88;
export const PANE_MAX = 520;
const PANE_KEY = "agendar.pane.width";
const COLLAPSED_KEY = "agendar.pane.collapsed";

function storedNumber(key: string, fallback: number): number {
  try {
    const value = Number(window.localStorage.getItem(key));
    return Number.isFinite(value) && value > 0 ? value : fallback;
  } catch {
    return fallback;
  }
}

function store(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    /* Private mode or a full store: the layout simply is not remembered. */
  }
}

export function clampPane(width: number): number {
  if (width < (PANE_RAIL + PANE_MIN) / 2) return PANE_RAIL;
  return Math.round(Math.min(PANE_MAX, Math.max(PANE_MIN, width)));
}

/** One keyboard step; the rail and the narrowest full sidebar are one step apart. */
function stepPane(width: number, wider: boolean): number {
  if (wider && width === PANE_RAIL) return PANE_MIN;
  if (!wider && width === PANE_MIN) return PANE_RAIL;
  return clampPane(width + (wider ? 16 : -16));
}

/**
 * The left pane's width and whether it is hidden, remembered on this
 * computer only (window layout, never shared state).
 */
function usePane() {
  const [width, setWidth] = useState(() => clampPane(storedNumber(PANE_KEY, PANE_DEFAULT)));
  const [collapsed, setCollapsed] = useState(() => {
    try {
      return window.localStorage.getItem(COLLAPSED_KEY) === "1";
    } catch {
      return false;
    }
  });
  useEffect(() => store(PANE_KEY, String(width)), [width]);
  useEffect(() => store(COLLAPSED_KEY, collapsed ? "1" : "0"), [collapsed]);
  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if (event.isComposing) return;
      if ((event.metaKey || event.ctrlKey) && event.key === "\\") {
        event.preventDefault();
        setCollapsed((value) => !value);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  return { width, setWidth, collapsed, setCollapsed };
}

/**
 * Main window: a floating sidebar and one toolbar row. On macOS the traffic
 * lights sit inside the sidebar (overlay title bar) and, on macOS 26+, native
 * Liquid Glass shows through the sidebar and the toolbar capsules.
 */
export function Shell({
  snapshot,
  phase,
  error,
  filter,
  selection,
  quietOpen,
  devFixture,
  motion,
  pulses,
  searchRef,
  onFilter,
  onSearchOpen,
  onSelect,
  onToggleQuiet,
  onOpenSettings,
  settingsOpen,
  onRefresh,
  chrome,
  children,
}: ShellProps) {
  const stale = phase === "stale" && snapshot !== null;
  const pane = usePane();
  const client = useContext(ClientContext);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const glass = chrome?.material === "glass";
  const windowActive = useWindowActive();
  useGlassRegions(client, rootRef, glass, `${pane.collapsed}:${snapshot ? 1 : 0}:${devFixture}`);
  const dragging = useRef(false);
  const layout = {
    "--pane": pane.collapsed ? "0px" : `${pane.width}px`,
  } as CSSProperties;
  function onGripDown(event: ReactPointerEvent<HTMLDivElement>) {
    dragging.current = true;
    event.currentTarget.setPointerCapture(event.pointerId);
  }
  function onGripMove(event: ReactPointerEvent<HTMLDivElement>) {
    if (!dragging.current) return;
    const left = event.currentTarget.parentElement?.getBoundingClientRect().left ?? 0;
    pane.setWidth(clampPane(event.clientX - left));
  }
  function onGripUp(event: ReactPointerEvent<HTMLDivElement>) {
    dragging.current = false;
    event.currentTarget.releasePointerCapture(event.pointerId);
  }
  return (
    <div
      ref={rootRef}
      className={`win${pane.collapsed ? " pane-collapsed" : ""}`}
      style={layout}
      data-material={glass ? "glass" : "solid"}
      data-overlay={chrome?.overlay_titlebar ? "true" : "false"}
      data-inactive={windowActive ? undefined : ""}
    >
      <div className="wbody">
        {snapshot ? (
          <Sidebar
            snapshot={snapshot}
            selection={selection}
            quietOpen={quietOpen}
            motion={motion}
            pulses={pulses}
            onSelect={onSelect}
            onToggleQuiet={onToggleQuiet}
            {...(onOpenSettings ? { onOpenSettings } : {})}
            settingsOpen={settingsOpen === true}
          />
        ) : (
          <aside className="side">
            <div className="side-scroll">
              <div className="gh">
                Needs you <span className="n">…</span>
              </div>
              <div className="empty-row">{tx("shell.loading")}</div>
            </div>
          </aside>
        )}
        {pane.collapsed ? null : (
          <div
            className="pane-grip"
            role="separator"
            aria-orientation="vertical"
            aria-label={tx("shell.sidebar_width")}
            aria-valuemin={PANE_RAIL}
            aria-valuemax={PANE_MAX}
            aria-valuenow={pane.width}
            tabIndex={0}
            onPointerDown={onGripDown}
            onPointerMove={onGripMove}
            onPointerUp={onGripUp}
            onDoubleClick={() => pane.setWidth(PANE_DEFAULT)}
            onKeyDown={(event) => {
              if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
                event.preventDefault();
                pane.setWidth((width) => stepPane(width, event.key === "ArrowRight"));
              }
            }}
          />
        )}
        <div className="maincol">
          <div className="toolbar" data-tauri-drag-region>
            <button
              type="button"
              className="icon-btn cap"
              aria-label={pane.collapsed ? tx("shell.show_sidebar") : tx("shell.hide_sidebar")}
              aria-pressed={!pane.collapsed}
              title={`${pane.collapsed ? tx("shell.show_sidebar") : tx("shell.hide_sidebar")} (${shortcut("mod+\\")})`}
              onClick={() => pane.setCollapsed((value) => !value)}
            >
              <Icon name="columns-2" />
            </button>
            <Search
              inputRef={searchRef}
              value={filter}
              onChange={onFilter}
              snapshot={snapshot}
              onOpen={(target) => onSearchOpen?.(target)}
            />
            <div data-tauri-drag-region />
          </div>
          {devFixture ? (
            <div className="dev-banner" data-testid="fixture-banner" role="note">
              Fixture data — development only. No Vault is connected.
            </div>
          ) : null}
        <main className="main">
          {stale ? (
            <div className="stale" role="status">
              <Icon name="alert-triangle" />
              <span className="grow">
                Showing the last state{error ? ` — ${error.message}` : ""}. Actions are disabled
                until this refreshes.
              </span>
              <button type="button" className="link" onClick={onRefresh}>
                Refresh
              </button>
            </div>
          ) : null}
          {children}
        </main>
        </div>
      </div>
    </div>
  );
}

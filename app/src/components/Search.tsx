import { tx } from "../i18n";
import type { RefObject } from "react";
import { Icon } from "./Icon";

interface SearchProps {
  inputRef: RefObject<HTMLInputElement | null>;
  value: string;
  onChange: (text: string) => void;
}

/** Sidebar filter. Cmd/Ctrl-K focuses and selects it. */
export function Search({ inputRef, value, onChange }: SearchProps) {
  return (
    <label className="search">
      <Icon name="search" />
      <span className="sr-only">{tx("search.label")}</span>
      <input
        ref={inputRef}
        type="search"
        placeholder={tx("search.placeholder")}
        autoComplete="off"
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
      <kbd>⌘K</kbd>
    </label>
  );
}

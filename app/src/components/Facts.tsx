import { tx } from "../i18n";
import type { Fact } from "../api/types";
import { Icon } from "./Icon";

interface FactsProps {
  facts: Fact[];
}

/** "Already checked" chips: Seen is an eye, Agent says is a message. */
export function Facts({ facts }: FactsProps) {
  if (!facts.length) return null;
  return (
    <div className="facts" aria-label={tx("facts.label")}>
      <span className="lbl">{tx("facts.label")}</span>
      {facts.map((fact, index) => (
        <span className="fact" key={`${fact.text}-${index}`}>
          <Icon name={fact.claim === "seen" ? "eye" : "message-square"} className="c2" />
          <span className="sr-only">{fact.claim === "seen" ? tx("facts.seen") : tx("facts.agent")}</span>
          {fact.text}
        </span>
      ))}
    </div>
  );
}

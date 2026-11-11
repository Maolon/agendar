import { tx, type MessageKey } from "../i18n";
import type { Disclosure } from "../api/types";
import type { DisclosureDelta } from "../state/redelivery";
import { Icon, type IconName } from "./Icon";

interface DisclosureProps {
  disclosure: Disclosure;
  /** On a redelivery: lines added since, and lines removed. */
  delta?: DisclosureDelta;
}

/**
 * Equal, honest columns. An empty claim list says
 * "None claimed" — it never renders as a success.
 */
const COLUMNS: { key: keyof Disclosure; title: MessageKey; icon: IconName; className?: string }[] = [
  { key: "proven", title: "dc2.proven", icon: "badge-check" },
  { key: "not_proven_yet", title: "dl.prov.unproven", icon: "help-circle", className: "c2" },
  { key: "faked_for_now", title: "dc2.faked", icon: "drama", className: "c2" },
];

export function DisclosureView({ disclosure, delta }: DisclosureProps) {
  return (
    <div className="disc">
      {COLUMNS.map((column) => {
        const claims = disclosure[column.key] ?? [];
        const change = delta?.[column.key];
        const removed = change?.removed ?? [];
        return (
          <div key={column.key}>
            <h4>
              <Icon name={column.icon} className={column.className} />
              {tx(column.title)}
            </h4>
            {claims.length ? (
              <ul>
                {claims.map((claim, index) => (
                  <li key={`${column.key}-${index}`}>
                    {claim}
                    {change?.added.includes(index) ? (
                      <span className="rd-new">{tx("rd.new")}</span>
                    ) : null}
                  </li>
                ))}
              </ul>
            ) : (
              <p>{tx("dc2.none")}</p>
            )}
            {removed.length ? (
              <ul className="rd-removed" aria-label={tx("rd.removed")}>
                {removed.map((claim, index) => (
                  <li key={`${column.key}-removed-${index}`}>
                    <del>{claim}</del>
                  </li>
                ))}
              </ul>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}

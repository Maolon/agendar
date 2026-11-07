import { tx, type MessageKey } from "../i18n";
import type { Disclosure } from "../api/types";
import { Icon, type IconName } from "./Icon";

interface DisclosureProps {
  disclosure: Disclosure;
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

export function DisclosureView({ disclosure }: DisclosureProps) {
  return (
    <div className="disc">
      {COLUMNS.map((column) => {
        const claims = disclosure[column.key] ?? [];
        return (
          <div key={column.key}>
            <h4>
              <Icon name={column.icon} className={column.className} />
              {tx(column.title)}
            </h4>
            {claims.length ? (
              <ul>
                {claims.map((claim, index) => (
                  <li key={`${column.key}-${index}`}>{claim}</li>
                ))}
              </ul>
            ) : (
              <p>{tx("dc2.none")}</p>
            )}
          </div>
        );
      })}
    </div>
  );
}

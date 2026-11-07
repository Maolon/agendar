import { tx, type MessageKey } from "../i18n";
import { formatEtaFinish } from "../api/format";
import type { DecisionOption, Reversibility } from "../api/types";
import { Icon, isIconName, type IconName } from "./Icon";

const RISK: Record<Reversibility, { icon: IconName; label: MessageKey }> = {
  easy: { icon: "undo-2", label: "opt.easy" },
  costly: { icon: "hourglass", label: "opt.costly" },
  irreversible: { icon: "lock", label: "opt.irreversible" },
};

interface OptionCardProps {
  option: DecisionOption;
  index: number;
  selected: boolean;
  recommended: boolean;
  anchor: string;
  onSelect: (optionId: string) => void;
}

/**
 * One option card. The whole card is the choice control (`aria-pressed`), and
 * the number key is a hint, not the label. Selecting never submits.
 */
export function OptionCard({ option, index, selected, recommended, anchor, onSelect }: OptionCardProps) {
  const icon: IconName = isIconName(option.icon) ? option.icon : "arrow-right";
  const risk = RISK[option.reversibility];
  const eta = option.eta ? formatEtaFinish(option.eta, anchor) : "";
  return (
    <button
      type="button"
      className="opt"
      aria-pressed={selected}
      onClick={() => onSelect(option.id)}
    >
      <span className="key" aria-hidden="true">
        {index + 1}
      </span>
      <Icon name={icon} className="big" size={24} strokeWidth={1.7} />
      <span className="lab">{option.label}</span>
      <span className="sub">{option.consequence}</span>
      {option.cost ? <span className="sub">{option.cost}</span> : null}
      {eta ? (
        <span className="when" title={`Expected finish ${eta}`}>
          <Icon name="clock" />
          {tx("opt.likely", { eta })}
        </span>
      ) : null}
      <span className="foot">
        <span>
          <Icon name={risk.icon} />
          {tx(risk.label)}
        </span>
        {recommended ? (
          <span>
            <Icon name="star" />
            {tx("opt.recommended")}
          </span>
        ) : null}
      </span>
    </button>
  );
}

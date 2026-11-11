import type { Correction } from "../api/types";
import { correctionReason } from "../state/claim";

/**
 * One small mark on a status the board corrected (an accept closed it, a
 * silent claim was freed, …). The reason is on hover and focus; nothing else
 * is added to the line.
 */
export function CorrectionMark({ correction }: { correction: Correction }) {
  const reason = correctionReason(correction);
  return (
    <span
      className="corr-mark"
      data-rule={correction.rule}
      role="img"
      aria-label={reason}
      title={reason}
      tabIndex={0}
    />
  );
}

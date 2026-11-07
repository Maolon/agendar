import { tx } from "../i18n";
import { Icon } from "./Icon";

interface ErrorStateProps {
  title: string;
  message: string;
  hint?: string;
  retryable: boolean;
  onRetry?: () => void;
}

/** Recoverable whole-window error; never pretends a mutation succeeded. */
export function ErrorState({ title, message, hint, retryable, onRetry }: ErrorStateProps) {
  return (
    <div className="state-screen" data-tauri-drag-region>
      <div className="state-card" role="alert">
        <h1>{title}</h1>
        <p>{message}</p>
        {hint ? <p className="muted">{hint}</p> : null}
        {retryable && onRetry ? (
          <div className="acts">
            <button type="button" className="btn primary" onClick={onRetry}>
              <Icon name="refresh-cw" />
              {tx("si.try_again")}
            </button>
          </div>
        ) : null}
      </div>
    </div>
  );
}

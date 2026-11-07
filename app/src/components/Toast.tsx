import { Icon } from "./Icon";

export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface ToastMessage {
  id: number;
  message: string;
  /** `warn`: a caution the person must read now (a second confirmation). */
  tone: "status" | "alert" | "warn";
  action?: ToastAction;
}

interface ToastProps {
  toast: ToastMessage | null;
}

/** Polite status for saved receipts, alert for recoverable errors. */
export function Toast({ toast }: ToastProps) {
  if (!toast) return null;
  return (
    <div
      className={`toast on${toast.action ? " act" : ""}${toast.tone === "alert" ? " error" : ""}${toast.tone === "warn" ? " warn" : ""}`}
      role={toast.tone === "status" ? "status" : "alert"}
      aria-live={toast.tone === "status" ? "polite" : "assertive"}
    >
      {toast.tone === "status" ? null : <Icon name="alert-triangle" />}
      <span>{toast.message}</span>
      {toast.action ? (
        <button type="button" className="toast-act" onClick={toast.action.onClick}>
          {toast.action.label}
        </button>
      ) : null}
    </div>
  );
}

import { AlertTriangle, CheckCircle2, Info, X } from "lucide-react";

import { useStore } from "../lib/store";

import styles from "./Toasts.module.css";

const ICONS = {
  info: Info,
  success: CheckCircle2,
  error: AlertTriangle,
} as const;

export default function Toasts() {
  const toasts = useStore((s) => s.toasts);
  const dismiss = useStore((s) => s.dismissToast);

  // Rendered even with nothing in it. A live region has to be in the page
  // before the news arrives or the first announcement is the one that gets
  // missed, and an empty one costs nothing: the stack takes no space and no
  // pointer events.
  return (
    <div className={styles.stack} aria-live="polite" aria-atomic="false">
      {toasts.map((toast) => {
        const Icon = ICONS[toast.kind];
        return (
          <div
            key={toast.id}
            className={`${styles.toast} ${styles[toast.kind]}`}
            // A failure interrupts; everything else waits its turn.
            role={toast.kind === "error" ? "alert" : "status"}
          >
            <Icon size={16} className={styles.icon} />
            <div className={styles.text}>
              <p className={styles.title}>{toast.title}</p>
              {toast.body && <p className={styles.body}>{toast.body}</p>}
            </div>
            <button
              className={styles.close}
              onClick={() => dismiss(toast.id)}
              aria-label="Dismiss"
            >
              <X size={14} />
            </button>
          </div>
        );
      })}
    </div>
  );
}

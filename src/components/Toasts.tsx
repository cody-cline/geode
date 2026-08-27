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

  if (toasts.length === 0) return null;

  return (
    <div className={styles.stack} role="status" aria-live="polite">
      {toasts.map((toast) => {
        const Icon = ICONS[toast.kind];
        return (
          <div key={toast.id} className={`${styles.toast} ${styles[toast.kind]}`}>
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

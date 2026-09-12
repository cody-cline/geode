import { useEffect, useRef } from "react";
import { X } from "lucide-react";

import { useBackHandler } from "../lib/useBackHandler";

import styles from "./Modal.module.css";

export default function Modal({
  title,
  subtitle,
  onClose,
  children,
  footer,
  width = 480,
}: {
  title: string;
  subtitle?: string;
  onClose: () => void;
  children: React.ReactNode;
  footer?: React.ReactNode;
  width?: number;
}) {
  const panel = useRef<HTMLDivElement>(null);

  // A dialog is dismissed by back, never navigated past.
  useBackHandler(true, onClose);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    // Move focus in so Escape and Tab act on the dialog, not whatever was
    // behind it.
    panel.current?.focus();
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    // Only a primary press dismisses. The mouse's back button delivers a
    // mousedown of its own, and closing on that spends the gesture here — then
    // the matching mouseup reaches the window handler, finds nothing open, and
    // goes back a page as well. One press, two dismissals.
    <div
      className={styles.backdrop}
      onMouseDown={(e) => {
        if (e.button === 0) onClose();
      }}
    >
      <div
        ref={panel}
        className={styles.panel}
        style={{ width }}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className={styles.head}>
          <div>
            <h2 className={styles.title}>{title}</h2>
            {subtitle && <p className={styles.subtitle}>{subtitle}</p>}
          </div>
          <button className="btn btn-ghost btn-icon btn-sm" onClick={onClose} aria-label="Close">
            <X size={16} />
          </button>
        </header>

        <div className={styles.body}>{children}</div>

        {footer && <footer className={styles.foot}>{footer}</footer>}
      </div>
    </div>
  );
}

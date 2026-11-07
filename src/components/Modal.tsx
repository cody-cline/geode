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

  // A dialog is dismissed by back, never navigated past — and nothing else
  // may change the screen under it either, because this is drawn outside the
  // view and would be left floating over whatever replaced it.
  useBackHandler(true, onClose, "overlay");

  // Move focus in so Escape and Tab act on the dialog, not whatever was behind
  // it, and hand it back to whatever opened this when the dialog goes. Its own
  // effect, with no dependencies, because it must happen once: parents pass
  // `onClose` as an inline arrow, so an effect keyed on it re-runs whenever the
  // parent renders — and re-running this one would pull focus out of the field
  // somebody is typing in.
  useEffect(() => {
    const opener = document.activeElement;
    panel.current?.focus();
    return () => {
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
        return;
      }
      if (e.key !== "Tab" || !panel.current) return;
      // Tab stays inside. A dialog that lets it out leaves the keyboard on a
      // screen the dialog is covering, pressing things nobody can see.
      const stops = panel.current.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
      if (stops.length === 0) return;
      const first = stops[0];
      const last = stops[stops.length - 1];
      const here = document.activeElement;
      if (e.shiftKey && (here === first || here === panel.current)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && here === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey);
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

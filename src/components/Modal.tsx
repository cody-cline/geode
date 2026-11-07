import { useEffect } from "react";
import { X } from "lucide-react";

import { useBackHandler } from "../lib/useBackHandler";
import { useFocusTrap } from "../lib/useFocusTrap";

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
  // Focus moves in when the dialog opens, stays in while it is open, and goes
  // back to whatever opened it when it closes. Shared with `ProjectPanel`
  // rather than written twice: the second copy of this is how one of them ends
  // up with the rule the other has — which is not hypothetical, since the
  // loader tiles inside `NewInstanceModal` hand their unchosen options
  // `tabindex="-1"` and the hook's selector is the one that does not treat
  // those as an end of the cycle.
  const panel = useFocusTrap<HTMLDivElement>();

  // A dialog is dismissed by back, never navigated past — and nothing else
  // may change the screen under it either, because this is drawn outside the
  // view and would be left floating over whatever replaced it.
  useBackHandler(true, onClose, "overlay");

  // Which key dismisses a dialog stays here, beside the back handler that is
  // the same question asked of a different device. Keyed on `onClose` and so
  // re-registered whenever a parent renders, which costs nothing: adding and
  // removing a listener does not move the keyboard, which is exactly why the
  // focus half above cannot live in this effect.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
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

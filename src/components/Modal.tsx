import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";

import { useBackHandler } from "../lib/useBackHandler";
import { useFocusTrap } from "../lib/useFocusTrap";

import styles from "./Modal.module.css";

/**
 * The input types Enter has no meaning of its own in, so finishing the dialog
 * is free to be what it means. A checkbox and a radio are toggled with Space,
 * not Enter, so they are in; a date or colour picker opens its own thing on
 * Enter in some browsers, so they are not.
 */
const ONE_LINE = new Set([
  "text",
  "search",
  "url",
  "email",
  "tel",
  "password",
  "number",
  "checkbox",
  "radio",
]);

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
  const foot = useRef<HTMLElement>(null);

  // Whether Enter would press something as things stand, which is the only
  // state the hint in the footer is allowed to be printed in.
  const [armed, setArmed] = useState(false);

  // A dialog is dismissed by back, never navigated past — and nothing else
  // may change the screen under it either, because this is drawn outside the
  // view and would be left floating over whatever replaced it.
  useBackHandler(true, onClose, "overlay");

  /**
   * Whether the keyboard is somewhere Enter is allowed to finish the dialog.
   *
   * Two places, and the second is the one that makes the key worth having.
   * The panel itself is where the focus trap puts the keyboard when a dialog
   * opens, so Ctrl+N then Enter works before anything is touched — but every
   * dialog here is opened to type in, and a key that stopped working the
   * moment somebody typed their instance's name would be one nobody could
   * use for the thing they opened the dialog to do. So a single-line text
   * field inside the panel counts too, which is what Enter means in a field
   * everywhere else.
   *
   * What is still excluded is excluded because Enter already means something
   * there: a `<textarea>` takes a newline, a `<select>` closes its list, a
   * focused button is pressed, and a `<form>` submits itself. An inner dialog
   * holds the keyboard in its own trap, so it is not in this panel at all.
   *
   * A field that acts on Enter itself claims the key with `preventDefault`
   * and is handled by the listener below rather than here — four do, and the
   * version filter in the New instance dialog is the one that claims it to
   * do *nothing*, because somebody narrowing a list has not chosen yet.
   */
  function focusCanFinish(): boolean {
    const here = document.activeElement;
    if (here === panel.current) return true;
    if (!(here instanceof HTMLInputElement)) return false;
    if (!panel.current?.contains(here) || here.closest("form")) return false;
    // A field that answers Enter itself says so here. The listener below also
    // stands aside for a press that was claimed, but that is only true at the
    // moment of the press — this is what the footer's hint reads, and a hint
    // promising a key that will do nothing is the thing it exists to prevent.
    if (here.dataset.enter === "field") return false;
    return ONE_LINE.has(here.type);
  }

  /**
   * The button Enter presses, or `null` when this dialog has none to press.
   *
   * Two conditions, and each is a way of getting this wrong.
   *
   * **The keyboard is on the panel itself**, which is the whole of the safety
   * argument and is deliberately narrow. A dialog that answers Enter while
   * somebody is in a field is a dialog that submits when they meant a
   * newline, or that does the same thing twice: three dialogs here bind Enter
   * on their own fields, and a second press answered from out here would run
   * what the field just ran. A `<textarea>`, a `<select>` with its list open,
   * a focused button and anything inside a form are all somewhere other than
   * the panel, so one test excludes them all — as it does an inner dialog,
   * whose own focus trap holds the keyboard while it is up. The panel is
   * where that trap puts the keyboard when a dialog opens, which is the press
   * this is for: Ctrl+N, Enter. The cost is that typing in a field and then
   * pressing Enter still finishes nothing, and that is the side to be wrong
   * on.
   *
   * **It is the footer's one enabled primary.** `btn-primary` is this app's
   * mark for the thing a dialog is for, and a destructive one is never it —
   * every delete, stop, restore and overwrite in here is `btn-danger`,
   * including the three that pick between the classes by condition, so Enter
   * cannot answer for any of them. Two at once is a dialog asking which, and
   * a key has no way to say. Disabled is where "the dialog is busy" is
   * already answered: these footers all disable their primary while their
   * work is in flight.
   */
  function pressable(): HTMLButtonElement | null {
    if (!focusCanFinish()) return null;
    const primary = foot.current?.querySelectorAll<HTMLButtonElement>(
      "button.btn-primary:not(:disabled)",
    );
    return primary?.length === 1 ? primary[0] : null;
  }

  // Read after every render as well as on every move of the keyboard, and
  // both halves are needed: a primary that was waiting on a list arrives
  // enabled without the keyboard having moved, and the keyboard moves into a
  // field without anything else about the dialog changing. Miss either and
  // the hint outlives the press it names. Setting the value it already holds
  // renders nothing, so this settles on the next pass.
  useEffect(() => {
    setArmed(pressable() !== null);
  });

  // Which key dismisses a dialog — and which finishes one — stays here,
  // beside the back handler that is the same question asked of a different
  // device. Keyed on `onClose` and so re-registered whenever a parent
  // renders, which costs nothing: adding and removing a listener does not
  // move the keyboard, which is exactly why the focus half above cannot live
  // in this effect.
  //
  // A bare Enter and not a repeat. Held down it would press the button again
  // on every repeat, and the moment before a disabled state lands is long
  // enough for several.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
        return;
      }
      if (e.key !== "Enter" || e.repeat || e.defaultPrevented) return;
      if (e.ctrlKey || e.metaKey || e.altKey || e.shiftKey) return;
      pressable()?.click();
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
        // Focus moving anywhere inside the panel, the panel itself included,
        // which is the other half of the hint's answer above.
        onFocus={() => setArmed(pressable() !== null)}
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

        {footer && (
          <footer className={styles.foot} ref={foot}>
            {/* Printed in the row with the button it presses, and only while
                that press would land — so it is never a key that does
                nothing, and a field taking the keyboard takes the hint with
                it. */}
            {armed && <kbd className={styles.enter}>Enter</kbd>}
            {footer}
          </footer>
        )}
      </div>
    </div>
  );
}

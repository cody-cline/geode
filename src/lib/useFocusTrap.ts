import { useEffect, useRef } from "react";

/**
 * Hold the keyboard inside an overlay while one is open.
 *
 * Two halves, because a dialog nobody can tab out of is only one of them.
 * Focus moves into the panel when it opens and back to whatever opened it when
 * the panel goes; in between, Tab cycles within the panel. Without the first,
 * Escape and Tab act on the screen the overlay is covering rather than on the
 * overlay. Without the second, Tab walks straight out of it and presses
 * controls nobody can see.
 *
 * The returned ref goes on the element carrying `role="dialog"`, and that
 * element needs `tabIndex={-1}`: it has to be able to hold focus itself before
 * anything inside it has been tabbed to.
 *
 * Neither effect takes dependencies, and that is load-bearing rather than an
 * oversight. Parents pass `onClose` as an inline arrow, so an effect keyed on
 * one re-runs on every parent render — and re-running the first of these would
 * pull focus out of the field somebody is typing in. Nothing here reads a prop,
 * so there is nothing to key on.
 *
 * Escape is deliberately not here. Which key dismisses an overlay stays with
 * the overlay, next to the back handler it belongs with. `Modal` still carries
 * an inline copy of the Tab half in the same effect as its own Escape; folding
 * it onto this hook would leave one copy instead of two.
 */
export function useFocusTrap<T extends HTMLElement>() {
  const panel = useRef<T>(null);

  useEffect(() => {
    const opener = document.activeElement;
    panel.current?.focus();
    return () => {
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab" || !panel.current) return;
      // Anything Tab would reach, which is not the same as anything that can
      // be focused: a radio group hands its unchosen options `tabindex="-1"`
      // so the group is one stop, and counting those as the ends of the cycle
      // would wrap Tab onto something it can never land on.
      const stops = panel.current.querySelectorAll<HTMLElement>(
        ':is(a[href], button, input, select, textarea, [tabindex]):not([disabled]):not([tabindex="-1"])',
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
  }, []);

  return panel;
}

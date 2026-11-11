import {
  cloneElement,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import type { FocusEvent, HTMLAttributes, MouseEvent, ReactElement } from "react";

import styles from "./Tooltip.module.css";

/** Between the trigger and the bubble, and the least the bubble keeps off a window edge. */
const GAP = 7;
const EDGE = 8;

/**
 * How long the pointer has to rest on something before its bubble appears.
 *
 * The keyboard gets none. A focus is deliberate — somebody arrowed on to this
 * thing — so the explanation is what they asked for, and a delay there is a
 * bubble that arrives after the press that moved on. A pointer crossing a row
 * of badges on its way somewhere else asked for nothing, which is the whole
 * reason the two differ.
 */
const REST = 220;

/** Hold one edge of the bubble inside the window, whichever way it would run out. */
const within = (at: number, most: number) => Math.max(EDGE, Math.min(at, most - EDGE));

/**
 * An explanation attached to the thing it explains.
 *
 * What this replaces is the `title` attribute, which is how nearly every
 * explanation in this app was carried. A native tooltip waits a second or
 * more, is drawn by the platform in neither of this app's themes, and — the
 * part that makes it an accessibility failure rather than a style one — never
 * appears on keyboard focus and never on touch. So an explanation written as a
 * `title` is an explanation for a mouse, and the rows of the Content tab had
 * badges whose entire meaning lived in one.
 *
 * `title` is still right for a control whose visible label already says what
 * it does and whose tooltip only elaborates. This is for the cases where the
 * sentence *is* the information: a badge that is one jargon word, and a button
 * that has collapsed to an icon.
 *
 * Portalled to the body, for the reason `ShareMenu` is: a row of the Content
 * tab sits in a pane under a hero that carries `isolation: isolate`, and no
 * z-index written inside a stacking context escapes it. The arithmetic that
 * keeps the bubble on screen is that component's, written out again rather
 * than shared — two callers and a third file to hold eight lines is a worse
 * trade than the copy, and both are the same five-line sum.
 *
 * The trigger is the caller's own element, cloned with the four handlers and
 * the description wired to it. A caller that wants the bubble reachable by
 * keyboard gives that element a `tabIndex` of its own; this adds none, because
 * on these screens a badge belongs in its row's arrow walk and not in the tab
 * order — see the grid in `instance/ContentTab.tsx`.
 */
export default function Tooltip({
  text,
  children,
}: {
  text: string;
  children: ReactElement<HTMLAttributes<HTMLElement>>;
}) {
  const id = useId();
  const [shown, setShown] = useState(false);
  /** Viewport coordinates, because a portalled bubble has no other frame. */
  const [at, setAt] = useState({ left: 0, top: 0 });
  const trigger = useRef<HTMLElement | null>(null);
  const bubble = useRef<HTMLDivElement>(null);
  const resting = useRef<number | null>(null);

  const hide = useCallback(() => {
    if (resting.current !== null) clearTimeout(resting.current);
    resting.current = null;
    setShown(false);
  }, []);

  // A row can stop existing under the pointer — a delete, a filter, a folder
  // switch — and a timer still counting down would then show a bubble for a
  // trigger that is no longer anywhere.
  useEffect(() => hide, [hide]);

  /** Below the trigger and centred on it, or above when there is no room below. */
  const place = useCallback(() => {
    const on = trigger.current?.getBoundingClientRect();
    const box = bubble.current?.getBoundingClientRect();
    if (!on || !box) return;
    const below = on.bottom + GAP;
    const above = on.top - GAP - box.height;
    const top =
      below + box.height > window.innerHeight - EDGE && above > EDGE ? above : below;
    setAt({
      left: within(on.left + (on.width - box.width) / 2, window.innerWidth - box.width),
      top: within(top, window.innerHeight - box.height),
    });
  }, []);

  // Measured and placed before the browser paints, so the bubble is never seen
  // at the top-left corner it is first rendered into to be measured.
  useLayoutEffect(() => {
    if (shown) place();
  }, [shown, place]);

  useEffect(() => {
    if (!shown) return;

    const onKey = (e: KeyboardEvent) => {
      // Not prevented and not stopped. Escape also drops a selection on the
      // Content tab, and that listener is captured at the window, so it has
      // already run by the time this one sees the press — both are a
      // dismissal, and the one that reaches further is the one that was asked
      // for.
      if (e.key === "Escape") hide();
    };

    // A scroll dismisses rather than follows. The bubble hangs over a list
    // that scrolls under it, so what moved is the content it is pointing at.
    // Captured, because the thing that scrolls is a pane inside the view and a
    // scroll does not bubble. A resize moves the trigger, so that one is
    // followed.
    document.addEventListener("keydown", onKey);
    document.addEventListener("scroll", hide, true);
    window.addEventListener("resize", place);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("scroll", hide, true);
      window.removeEventListener("resize", place);
    };
  }, [shown, hide, place]);

  const show = (el: HTMLElement) => {
    trigger.current = el;
    setShown(true);
  };

  return (
    <>
      {cloneElement(children, {
        // Only while the bubble is in the document. An `aria-describedby`
        // naming an id that is not there describes nothing, and a screen
        // reader is entitled to say so.
        "aria-describedby": shown ? id : undefined,
        onMouseEnter: (e: MouseEvent<HTMLElement>) => {
          // Read now rather than in the timer: React clears `currentTarget`
          // once the handler returns.
          const el = e.currentTarget;
          resting.current = window.setTimeout(() => show(el), REST);
        },
        onMouseLeave: hide,
        onFocus: (e: FocusEvent<HTMLElement>) => {
          if (resting.current !== null) clearTimeout(resting.current);
          show(e.currentTarget);
        },
        onBlur: hide,
      })}

      {shown &&
        createPortal(
          <div
            ref={bubble}
            id={id}
            className={styles.bubble}
            style={{ left: at.left, top: at.top }}
            role="tooltip"
          >
            {text}
          </div>,
          document.body,
        )}
    </>
  );
}

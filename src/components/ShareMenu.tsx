import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { ReactNode } from "react";
import { ChevronDown, Share2 } from "lucide-react";

import styles from "./ShareMenu.module.css";

/**
 * One entry. A `group` is a heading rather than something to press, and a
 * `rule` is a divider — both exist so the caller describes its menu as a list
 * instead of assembling JSX, which is what keeps the instance's menu and the
 * server's the same shape.
 */
export type ShareItem =
  | { kind: "group"; label: string }
  | { kind: "rule" }
  | {
      kind: "item";
      label: string;
      /** The right-hand hint: a file extension, usually. */
      hint?: string;
      icon?: ReactNode;
      disabled?: boolean;
      onSelect: () => void;
    };

/** Between the button and the menu, and the least the menu keeps off a window edge. */
const GAP = 6;
const EDGE = 8;

/** Hold one edge of the menu inside the window, whichever way it would run out. */
const within = (at: number, most: number) => Math.max(EDGE, Math.min(at, most - EDGE));

/**
 * The Share button and the menu that drops out of it.
 *
 * Closes on an outside click, on Escape, and on choosing something — the last
 * because every item here opens a dialog or puts something on the clipboard,
 * and a menu still hanging open over the result reads as though nothing
 * happened.
 *
 * The menu is portalled to the body rather than drawn inside this component,
 * and the stacking context it has to get out of is the subject of the comment
 * on `.menu` in the stylesheet. Being out there is what the rest of this file
 * is mostly about: it has to be positioned from the button by hand, and the
 * button no longer contains it, which is a trap for anything asking whether a
 * click or the keyboard is "inside".
 */
export default function ShareMenu({ items }: { items: ShareItem[] }) {
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  /** Which end of the menu the keyboard lands on when it opens. */
  const landOn = useRef<"first" | "last">("first");
  /** Viewport coordinates, because a portalled menu has no other frame. */
  const [at, setAt] = useState({ left: 0, top: 0 });

  /**
   * Close, and put the keyboard back on the button the menu came out of.
   *
   * The order is the whole of it. Every item here opens a dialog, and `Modal`
   * takes whatever is focused when it mounts as the thing to give focus back
   * to on close — while the render that opens that dialog is the same one that
   * unmounts the item just pressed. So unless the trigger is focused *first*,
   * the dialog's opener is `<body>`, and the keyboard is stranded at the top
   * of the document twice over: once when the menu goes, once when the dialog
   * does.
   */
  const close = useCallback(() => {
    trigger.current?.focus();
    setOpen(false);
  }, []);

  /** Every item the keyboard may land on, in the order they read. */
  const stops = () =>
    Array.from(
      menu.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [],
    );

  /**
   * Put the menu below-right of the button — where `right: 0` had it while it
   * was still a child — and keep it inside the window.
   */
  const place = useCallback(() => {
    const btn = trigger.current?.getBoundingClientRect();
    const box = menu.current?.getBoundingClientRect();
    if (!btn || !box) return;
    const below = btn.bottom + GAP;
    const above = btn.top - GAP - box.height;
    // Against the bottom of the window it opens upward instead, but only if
    // there is actually more room up there. A window too short for either way
    // is what `within` is for: it slides the menu over the button rather than
    // off the screen.
    const top = below + box.height > window.innerHeight - EDGE && above > EDGE ? above : below;
    setAt({
      left: within(btn.right - box.width, window.innerWidth - box.width),
      top: within(top, window.innerHeight - box.height),
    });
  }, []);

  // Measured and placed before the browser paints, so the menu is never seen
  // at the place the last one was dismissed from.
  useLayoutEffect(() => {
    if (open) place();
  }, [open, place]);

  // The keyboard goes into the menu as it opens, which is the first half of
  // what `role="menu"` promises — the arrows below are the other half, and
  // they are worth nothing while the focus is still on the button and every
  // press is being read by the screen behind.
  useEffect(() => {
    if (!open) return;
    const list = stops();
    (landOn.current === "last" ? list[list.length - 1] : list[0])?.focus();
    landOn.current = "first";
  }, [open]);

  useEffect(() => {
    if (!open) return;

    /**
     * Inside the menu counts as inside the button. The menu is portalled out
     * of `wrap`, so `wrap.contains` alone stopped being the question: a press
     * on an item read as an outside click and closed the menu before the
     * item's own handler ran, which is the whole of the bug this replaced.
     */
    const ours = (node: Node | null) =>
      Boolean(node && (wrap.current?.contains(node) || menu.current?.contains(node)));

    function away(e: MouseEvent) {
      if (ours(e.target as Node)) return;
      const held = ours(document.activeElement);
      setOpen(false);
      if (!held) return;
      // Not `close()`: the browser does its own focus handling after a
      // mousedown listener returns, so a focus set here is overwritten a
      // moment later — by the control that was clicked, or by nothing at all
      // when the click landed on a heading or a gap. Only that second case
      // needs the keyboard putting back, and taking it off a control the
      // click did focus would be worse than the problem.
      requestAnimationFrame(() => {
        if (document.activeElement === document.body) trigger.current?.focus();
      });
    }

    /**
     * The keys `role="menu"` has been claiming. Arrows move the keyboard
     * without choosing anything, which is the one thing a menu does not share
     * with the groups `rovingFocus` serves — there, landing on an option
     * selects it, and here that would export a file. Type-ahead is left out:
     * it is optional in the pattern, and the longest of these menus is five
     * items.
     *
     * On the document rather than on the menu, because the menu holds
     * headings and a divider: clicking one of those takes the focus to
     * `<body>`, and a handler bound to the menu would never see another key.
     */
    function keys(e: KeyboardEvent) {
      // The button answers for itself. The arrow that opens the menu is still
      // bubbling when this listener goes on — React has already flushed the
      // open and moved the keyboard to the first item — so without this the
      // press that opened the menu stepped it on again and arriving by
      // ArrowDown landed on the second item.
      if (e.target === trigger.current) return;
      if (e.key === "Escape" || e.key === "Tab") {
        // Tab is not prevented. The keyboard should carry on out of the menu
        // from the button, which is where `close` has just put it, rather
        // than from an item that is about to stop existing.
        close();
        return;
      }
      const step = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
      if (step === 0 && e.key !== "Home" && e.key !== "End") return;
      const list = stops();
      if (list.length === 0) return;
      e.preventDefault();
      if (e.key === "Home") return list[0].focus();
      if (e.key === "End") return list[list.length - 1].focus();
      const from = list.indexOf(document.activeElement as HTMLButtonElement);
      // From nowhere — the focus is on `<body>` after a click on a heading —
      // a step in either direction means the end it points at.
      const next = from < 0 ? (step > 0 ? 0 : list.length - 1) : from + step;
      list[(next + list.length) % list.length].focus();
    }

    // A scroll closes the menu rather than moving it. The button is in a
    // header that does not scroll, so there is nothing to follow; what moved
    // is the content the menu is hanging over, and a menu left pinned above
    // content that slid out from under it is pointing at nothing. Captured,
    // because what scrolls is a pane inside the view and a scroll does not
    // bubble. A resize does move the button, so that one is followed.
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", keys);
    document.addEventListener("scroll", close, true);
    window.addEventListener("resize", place);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", keys);
      document.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", place);
    };
  }, [open, close, place]);

  return (
    <div className={styles.wrap} ref={wrap}>
      <button
        ref={trigger}
        className="btn"
        onClick={() => setOpen((was) => !was)}
        onKeyDown={(e) => {
          // What a menu button is expected to answer to, at the end each one
          // points at.
          if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
          e.preventDefault();
          landOn.current = e.key === "ArrowUp" ? "last" : "first";
          setOpen(true);
        }}
        aria-haspopup="menu"
        aria-expanded={open}
      >
        <Share2 size={15} />
        Share
        <ChevronDown size={13} />
      </button>

      {open &&
        createPortal(
          <div
            ref={menu}
            className={styles.menu}
            style={{ left: at.left, top: at.top }}
            role="menu"
            aria-label="Share"
          >
            {items.map((item, i) => {
              if (item.kind === "rule")
                return <div key={i} className={styles.rule} role="separator" />;
              // A heading is drawn but not announced. A menu owns items,
              // separators and groups and nothing else, and reading these out
              // as the stray text they would be is worse than the alternative,
              // which is that both of today's headings repeat what the items
              // under them already say — a column of file extensions, and
              // "Make a link…". A heading that said something new would need a
              // `role="group"` around the run of items it names.
              if (item.kind === "group")
                return (
                  <div key={i} className={styles.group} aria-hidden="true">
                    {item.label}
                  </div>
                );
              return (
                <button
                  key={i}
                  className={styles.item}
                  role="menuitem"
                  disabled={item.disabled}
                  onClick={() => {
                    close();
                    item.onSelect();
                  }}
                >
                  {item.icon}
                  <span>{item.label}</span>
                  {item.hint && <span className={styles.sub}>{item.hint}</span>}
                </button>
              );
            })}
          </div>,
          document.body,
        )}
    </div>
  );
}

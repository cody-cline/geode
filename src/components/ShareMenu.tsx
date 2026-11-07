import { useCallback, useEffect, useRef, useState } from "react";
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

/**
 * The Share button and the menu that drops out of it.
 *
 * Closes on an outside click, on Escape, and on choosing something — the last
 * because every item here opens a dialog or puts something on the clipboard,
 * and a menu still hanging open over the result reads as though nothing
 * happened.
 */
export default function ShareMenu({ items }: { items: ShareItem[] }) {
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);

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

  useEffect(() => {
    if (!open) return;
    function away(e: MouseEvent) {
      if (wrap.current?.contains(e.target as Node)) return;
      const held = wrap.current?.contains(document.activeElement);
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
    function escape(e: KeyboardEvent) {
      if (e.key === "Escape") close();
    }
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", escape);
    };
  }, [open, close]);

  return (
    <div className={styles.wrap} ref={wrap}>
      <button
        ref={trigger}
        className="btn"
        onClick={() => setOpen((was) => !was)}
        aria-haspopup="menu"
        aria-expanded={open}
      >
        <Share2 size={15} />
        Share
        <ChevronDown size={13} />
      </button>

      {open && (
        <div className={styles.menu} role="menu">
          {items.map((item, i) => {
            if (item.kind === "rule") return <div key={i} className={styles.rule} />;
            if (item.kind === "group")
              return (
                <div key={i} className={styles.group}>
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
        </div>
      )}
    </div>
  );
}

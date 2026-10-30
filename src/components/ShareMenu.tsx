import { useEffect, useRef, useState } from "react";
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

  useEffect(() => {
    if (!open) return;
    function away(e: MouseEvent) {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false);
    }
    function escape(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", escape);
    };
  }, [open]);

  return (
    <div className={styles.wrap} ref={wrap}>
      <button
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
                  setOpen(false);
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

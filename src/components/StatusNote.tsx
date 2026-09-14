import clsx from "clsx";
import { AlertTriangle, CheckCircle2, Info, Loader2, XCircle } from "lucide-react";

import type { StatusLine } from "../lib/share";

import styles from "./StatusNote.module.css";

const ICONS = {
  ok: CheckCircle2,
  info: Info,
  warn: AlertTriangle,
  bad: XCircle,
} as const;

/**
 * One sentence of state with its consequence underneath: whether the port is
 * open, whether a server was verified.
 *
 * The icon and colour carry the verdict so the title can say what happened
 * plainly, and the detail can say what that means for the person reading it.
 */
export default function StatusNote({
  line,
  busy = false,
}: {
  line: StatusLine;
  /** Something is still in progress: a spinner stands in for the icon. */
  busy?: boolean;
}) {
  const Icon = busy ? Loader2 : ICONS[line.tone];
  return (
    <div className={clsx(styles.note, styles[line.tone])}>
      <Icon size={15} className={clsx(styles.icon, busy && "spin")} />
      <span className={styles.text}>
        <strong className={styles.title}>{line.title}</strong>
        <span className={styles.detail}>{line.detail}</span>
      </span>
    </div>
  );
}

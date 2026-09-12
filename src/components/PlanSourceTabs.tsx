import clsx from "clsx";
import { Package, Puzzle } from "lucide-react";

import styles from "./PlanSourceTabs.module.css";

export type PlanSource = "mods" | "packs";

/**
 * Where a plan's next mods come from: found one at a time, or taken wholesale
 * out of a pack.
 *
 * Lives in the same row in both views, so switching between them feels like
 * changing a filter rather than leaving the screen.
 */
export default function PlanSourceTabs({
  source,
  onSource,
}: {
  source: PlanSource;
  onSource: (next: PlanSource) => void;
}) {
  return (
    <div className={styles.tabs} role="group" aria-label="What to add from">
      <button
        className={clsx(styles.tab, source === "mods" && styles.active)}
        aria-pressed={source === "mods"}
        onClick={() => onSource("mods")}
      >
        <Puzzle size={13} />
        Mods
      </button>
      <button
        className={clsx(styles.tab, source === "packs" && styles.active)}
        aria-pressed={source === "packs"}
        onClick={() => onSource("packs")}
      >
        <Package size={13} />
        Packs
      </button>
    </div>
  );
}

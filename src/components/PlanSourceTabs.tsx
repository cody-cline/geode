import type { ReactNode } from "react";
import clsx from "clsx";
import { Package, Puzzle } from "lucide-react";

import { rovingKey } from "../lib/rovingFocus";

import styles from "./PlanSourceTabs.module.css";

export type PlanSource = "mods" | "packs";

const SOURCES: { id: PlanSource; label: string; icon: ReactNode }[] = [
  { id: "mods", label: "Mods", icon: <Puzzle size={13} /> },
  { id: "packs", label: "Packs", icon: <Package size={13} /> },
];

const IDS = SOURCES.map((s) => s.id);

/**
 * Where a plan's next mods come from: found one at a time, or taken wholesale
 * out of a pack.
 *
 * Drawn as the segment every other panel-swapper in this app is drawn as — the
 * spec is at the top of `InstanceView.module.css` — because it is the same act.
 * It sits in the same row as Browse's kinds and PlanPacks' Installed/Online,
 * which are that shape already, and it used to have a fifth shape of its own: a
 * 26px accent fill inside a bordered trough, which said that choosing here was
 * a different kind of choosing than choosing six pixels to the right.
 *
 * A **tab set**, and not the `role="group"` of `aria-pressed` toggles it used
 * to claim. `aria-pressed` describes switches that happen to be beside each
 * other, each on or off by itself; this is one of two, and the one chosen swaps
 * everything below it — `PlanView` reads it and renders `PlanPacks` or
 * `Browse`, whole views rather than panels. So it is navigation by any reading,
 * and it is certainly not a filter narrowing a list that is already there.
 *
 * What it cannot say is *which* element it swaps, so no tab here carries
 * `aria-controls`. The panel is the view this strip is rendered inside: only
 * the chosen one is ever mounted, and it has no id for this component to name.
 * A reference to an id that is not in the document is worse than none, so the
 * two call sites would have to hand one down before it could be said truthfully.
 *
 * One thing about the arrows worth knowing before chasing it as a bug: the
 * press that moves the selection unmounts the view holding this strip, so focus
 * lands on nothing — exactly as a click on it does. That is the swap's doing
 * rather than the keyboard's, and belongs where the swap is decided.
 */
export default function PlanSourceTabs({
  source,
  onSource,
}: {
  source: PlanSource;
  onSource: (next: PlanSource) => void;
}) {
  return (
    <div
      className={styles.tabs}
      role="tablist"
      aria-label="What to add from"
      onKeyDown={(e) => {
        const next = rovingKey(e, IDS);
        if (next) onSource(next);
      }}
    >
      {SOURCES.map((s) => (
        <button
          key={s.id}
          className={clsx(styles.tab, source === s.id && styles.active)}
          onClick={() => onSource(s.id)}
          role="tab"
          aria-selected={source === s.id}
          // One tab stop for the strip, with the arrows moving between the two
          // from there and the one they land on chosen.
          tabIndex={source === s.id ? 0 : -1}
          data-roving={s.id}
        >
          {s.icon}
          {s.label}
        </button>
      ))}
    </div>
  );
}

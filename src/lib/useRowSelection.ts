import { useCallback, useMemo, useState } from "react";

import type { ContentItem } from "./types";

/**
 * Which rows of a content list the next press acts on.
 *
 * The two Content tabs are one screen with different folders behind them —
 * they already share a stylesheet and the list keyboard in `rowKeys` — so the
 * selection is shared the same way rather than written out twice. It lived in
 * the instance tab's file and nowhere else, which is why an admin with forty
 * plugins was making forty round trips.
 *
 * Keyed by **file name**, because that is the only thing unique to a row, and
 * read back off `items` wherever it is used: a name left in the set by a file
 * that has since gone counts for nothing anywhere on screen.
 *
 * `items` and `shown` are the same list with and without the filter applied,
 * and keeping both is what makes two of the rules below possible. A Shift span
 * is measured over `shown`, so it runs along the rows in front of you and
 * never through the ones the filter is hiding; what is *selected* is measured
 * over `items`, because a selection outlives the filter that made it and the
 * bar has to be able to say so.
 */
export function useRowSelection(
  items: ContentItem[] | null,
  shown: ContentItem[] | null,
) {
  // The rows a press is about to act on, by file name, and the row a plain
  // click last landed on so Shift can measure a span from it.
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);

  /** What is selected, as rows — see the note above about reading off `items`. */
  const picked = useMemo(
    () => items?.filter((i) => selected.has(i.fileName)) ?? [],
    [items, selected],
  );
  /** How many of them the filter is showing. */
  const pickedShown = useMemo(
    () => shown?.filter((i) => selected.has(i.fileName)).length ?? 0,
    [shown, selected],
  );

  /**
   * Take a row into the selection, or out of it.
   *
   * Shift adds the span between the row a plain click last landed on and this
   * one, rather than replacing what is selected: the gesture reads as "and
   * these too". The span is measured over what is *shown*, so it runs along
   * the rows in front of you and never through the ones the filter is hiding.
   */
  const pick = (item: ContentItem, extend: boolean) => {
    const rows = shown ?? [];
    setSelected((prev) => {
      const next = new Set(prev);
      const from = rows.findIndex((r) => r.fileName === anchor);
      const to = rows.findIndex((r) => r.fileName === item.fileName);
      if (extend && from >= 0 && to >= 0) {
        for (const row of rows.slice(Math.min(from, to), Math.max(from, to) + 1)) {
          next.add(row.fileName);
        }
        return next;
      }
      if (next.has(item.fileName)) next.delete(item.fileName);
      else next.add(item.fileName);
      return next;
    });
    // The anchor stays where a plain click put it, so a second Shift-click
    // adjusts the same span instead of walking away from it.
    if (!extend) setAnchor(item.fileName);
  };

  /** Take every row the filter is showing, which is what "all" can mean here. */
  const selectAll = () =>
    setSelected(new Set((shown ?? []).map((i) => i.fileName)));

  /**
   * Drop the selection — the Clear button, the Escape binding, and a folder
   * change, which are the three ways out of it.
   *
   * Nothing selected is nothing to clear, and a bare Escape arrives on this
   * screen whether or not anybody has selected anything, so the set is left
   * alone rather than replaced with an equal one.
   */
  const clear = useCallback(() => {
    setSelected((prev) => (prev.size === 0 ? prev : new Set<string>()));
    setAnchor(null);
  }, []);

  /**
   * Keep every file that has just been renamed in the selection.
   *
   * Turning a file on or off *is* a rename on disk — `foo.jar` becomes
   * `foo.jar.disabled` — and a selection is keyed by the file name because
   * that is the only thing unique to a row. Without this, disabling twelve
   * mods would empty the selection the moment it worked, and the obvious next
   * press would have nothing left to act on.
   */
  const followRenames = useCallback((moves: Map<string, string>) => {
    setSelected((prev) => {
      const next = new Set(prev);
      for (const [from, to] of moves) {
        if (!next.has(from)) continue;
        next.delete(from);
        next.add(to);
      }
      return next;
    });
  }, []);

  /** Take names out of the selection, for the files a delete really took. */
  const forget = useCallback((files: string[]) => {
    setSelected((prev) => {
      const next = new Set(prev);
      for (const file of files) next.delete(file);
      return next;
    });
  }, []);

  return {
    selected,
    picked,
    pickedShown,
    pick,
    selectAll,
    clear,
    followRenames,
    forget,
  };
}

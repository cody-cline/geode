import { useEffect, useRef } from "react";

import { useStore } from "./store";

let sequence = 0;

/**
 * Claim "back" while something is open on top of the view.
 *
 * Going back should close whatever is in front of you before it changes screen
 * — a panel over a list, a dialog over a form — because that is what the
 * gesture means to someone looking at it. Handlers stack, so the innermost
 * open thing is the one that closes.
 *
 * `active` is what registers it: pass the same condition that renders the
 * overlay, and it unregisters itself when that goes away.
 *
 * `kind` separates two things that both want back and want nothing else in
 * common. An **overlay** floats above the whole window — a dialog, a panel —
 * and is drawn by something that outlives a change of screen, so a shortcut
 * that changed the screen under it would leave it hanging over a view it was
 * never opened from. An **inline** claim is a screen that has replaced
 * another in place: browsing inside an instance, adding to a plan, an opened
 * pack. The sidebar is still beside those, still printing its shortcuts, and
 * they must keep working — a printed key that silently does nothing is worse
 * than no key at all. Back treats the two alike; `App` does not.
 */
export function useBackHandler(
  active: boolean,
  handler: () => void,
  kind: "overlay" | "inline" = "inline",
) {
  // Kept in a ref so a handler that closes over fresh state does not
  // re-register on every render.
  const latest = useRef(handler);
  latest.current = handler;

  useEffect(() => {
    if (!active) return;
    const id = ++sequence;
    useStore.getState().pushBackHandler(id, () => latest.current(), kind);
    return () => useStore.getState().popBackHandler(id);
  }, [active, kind]);
}

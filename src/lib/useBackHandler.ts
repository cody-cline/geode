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
 */
export function useBackHandler(active: boolean, handler: () => void) {
  // Kept in a ref so a handler that closes over fresh state does not
  // re-register on every render.
  const latest = useRef(handler);
  latest.current = handler;

  useEffect(() => {
    if (!active) return;
    const id = ++sequence;
    useStore.getState().pushBackHandler(id, () => latest.current());
    return () => useStore.getState().popBackHandler(id);
  }, [active]);
}

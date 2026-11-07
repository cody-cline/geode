import { useEffect, useRef } from "react";

import { shortcutAction } from "./shortcuts";
import type { Action } from "./shortcuts";
import { useStore } from "./store";

/**
 * Bind one of this app's screen-level actions while a screen is mounted.
 *
 * A route is reachable from anywhere and `App` binds those once at the window.
 * An action belongs to the screen that can carry it out, so the screen binds
 * it — and three screens bind the same one (`newThing` means an instance, a
 * plan or a server depending on where you are), which is why this is a hook
 * rather than the same effect written out on each of them.
 *
 * Two details here are the whole reason it is worth sharing, and both were
 * learned by getting them wrong:
 *
 * The listener is **captured**, so it runs before `Modal`'s own Escape handler
 * and can therefore still see that a dialog is open. Registered in the bubble
 * phase instead, `Modal` closes and unregisters itself first, the gate below
 * then finds nothing open, and one press does both things.
 *
 * It stands aside for an **overlay**, which is a dialog or a panel drawn over
 * this screen — not for any back handler at all. Browsing inside an instance,
 * adding to a plan and an opened pack all claim back as well and are screens
 * rather than things on top of one; gating on the whole stack made the keys
 * dead there while the same controls still worked with a mouse.
 *
 * The handler receives the event so it can decide about `preventDefault`,
 * which is not the same answer every time: a binding holding Ctrl is taking a
 * key the webview may want, while a bare Escape has no default here and
 * anything else listening for it should still get it.
 */
export function useActionShortcut(
  action: Action,
  run: (e: KeyboardEvent) => void,
) {
  // The handler is written inline by every caller, so an effect keyed on it
  // would re-register on every render of the screen. It is read through a ref
  // instead and the effect runs once.
  const latest = useRef(run);
  useEffect(() => {
    latest.current = run;
  });

  useEffect(() => {
    const onShortcut = (e: KeyboardEvent) => {
      if (useStore.getState().backHandlers.some((h) => h.overlay)) return;
      if (shortcutAction(e) !== action) return;
      latest.current(e);
    };
    window.addEventListener("keydown", onShortcut, true);
    return () => window.removeEventListener("keydown", onShortcut, true);
  }, [action]);
}

import { useEffect, useState } from "react";

import JoinModal from "./components/JoinModal";
import OfflinePlayModal from "./components/OfflinePlayModal";
import PackLinkModal from "./components/PackLinkModal";
import Sidebar from "./components/Sidebar";
import ProgressDock from "./components/ProgressDock";
import Toasts from "./components/Toasts";
import Browse from "./views/Browse";
import InstanceView from "./views/InstanceView";
import Library from "./views/Library";
import PlanView from "./views/PlanView";
import Plans from "./views/Plans";
import ServerView from "./views/ServerView";
import Servers from "./views/Servers";
import Settings from "./views/Settings";
import { shortcutRoute } from "./lib/shortcuts";
import { useStore } from "./lib/store";

import styles from "./App.module.css";

/**
 * Move the keyboard into the screen, onto the first thing on it that answers
 * to a press.
 *
 * Nine stops stand in front of main content on every route — the four sidebar
 * rows, a `+` on each of the two sections, every instance and server those
 * sections list, the account row and Settings — and all nine come before the
 * thing that was being looked at. So the keyboard needs a way past them, and a
 * route shortcut that changed the screen without taking the keyboard with it
 * left somebody at the top of the sidebar looking at a screen they had just
 * asked for. Both are this one move, because both are the same promise: the
 * keyboard ends up where the eye already is.
 *
 * *Which* control that is, is read off the document rather than written down
 * per screen, and the answer comes out right on all five: the search field on
 * Library, Browse and Servers, the New plan button under Plans, the first tab
 * of Settings. Each of those is the first stop its own header renders, so the
 * landing follows a screen that rearranges its header instead of going stale
 * beside it — Library and Servers hide search until there is something to
 * search, and the landing moves to the New button with it.
 *
 * A screen with nothing at all to press falls back to the region itself, which
 * is the only way a keyboard reaches a page that is only words.
 */
function focusScreen() {
  const main = document.querySelector("main");
  if (!(main instanceof HTMLElement)) return;
  // What Tab would reach, which is not the same as everything that can be
  // focused: a roving group parks `-1` on its unchosen members, and landing on
  // one of those is landing where Tab can never come back to. The same set
  // `useFocusTrap` walks, for the same reason.
  const first = main.querySelector<HTMLElement>(
    ':is(a[href], button, input, select, textarea, [tabindex]):not([disabled]):not([tabindex="-1"])',
  );
  if (first) {
    first.focus();
    return;
  }
  // A region is not focusable by default and should not join the tab order;
  // `-1` lets it take focus from script alone.
  main.tabIndex = -1;
  main.focus();
}

export default function App() {
  const route = useStore((s) => s.route);
  const init = useStore((s) => s.init);
  const join = useStore((s) => s.join);
  const closeJoin = useStore((s) => s.closeJoin);
  const pack = useStore((s) => s.pack);
  const closePack = useStore((s) => s.closePack);
  const offlinePlay = useStore((s) => s.offlinePlay);
  const closeOfflinePlay = useStore((s) => s.closeOfflinePlay);

  // Bumped by a route shortcut and read by the effect below, which runs once
  // the screen it asked for has rendered. A counter rather than a flag on the
  // route, because `navigate` to the screen you are already on is a no-op —
  // and that is precisely the press that has to work when the keyboard is
  // parked in the sidebar printing these very keys.
  const [pressed, setPressed] = useState(0);

  useEffect(() => {
    void init();
  }, [init]);

  // Only when a key asked. A sidebar row clicked with the mouse leaves the
  // keyboard where it was, which is under the hand that clicked it.
  useEffect(() => {
    if (pressed > 0) focusScreen();
  }, [pressed]);

  // The mouse's back button, and Alt+Left for the same gesture without one.
  // Bound once at the window rather than per view, so it reaches every screen
  // and every overlay instead of only the handful that remembered to ask.
  useEffect(() => {
    const onMouse = (e: MouseEvent) => {
      // 3 is X1 ("back"); 4 is X2, left alone so forward stays unclaimed.
      if (e.button !== 3) return;
      e.preventDefault();
      useStore.getState().goBack();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.altKey && e.key === "ArrowLeft") {
        e.preventDefault();
        useStore.getState().goBack();
        return;
      }
      // The sidebar's screens, bound here for the same reason: one definition
      // that reaches every screen. A dialog is a mode, though — a shortcut
      // that changed the screen behind one would leave it floating over a
      // view it was never opened from — so an open overlay takes the press
      // instead.
      //
      // Overlays only, not every back handler. Browsing inside an instance,
      // adding to a plan and an opened pack all claim back as well, and they
      // are screens rather than things on top of one: the sidebar is still
      // beside them printing these very keys, and gating on the whole stack
      // made all five of them dead there while the same row clicked with the
      // mouse still worked.
      const route = shortcutRoute(e);
      const overlaid = useStore
        .getState()
        .backHandlers.some((h) => h.overlay);
      if (route && !overlaid) {
        e.preventDefault();
        useStore.getState().navigate(route);
        // And the keyboard goes with it. Only Ctrl+2 ever did, and only by
        // accident: Browse's search field asks for focus when it mounts. The
        // other four moved the screen and left the person, which on a window
        // nobody has tabbed into yet means nine stops short of what they asked
        // for.
        setPressed((n) => n + 1);
      }
    };
    window.addEventListener("mouseup", onMouse);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mouseup", onMouse);
      window.removeEventListener("keydown", onKey);
    };
  }, []);

  return (
    <div className={styles.app}>
      {/* First in the document, so it is the first thing Tab reaches, and out
          of sight until it has focus. Everything the sidebar offers is in
          front of the screen on every route — see `focusScreen` for the count
          — and until this existed there was no way past it but nine presses.

          A button rather than the usual anchor: there is no element with an id
          to point at, because where this lands is whichever control the
          screen's own header renders first. */}
      <button className={styles.skip} onClick={focusScreen}>
        Skip to content
      </button>
      <Sidebar />
      <main className={styles.main}>
        {route.view === "library" && <Library />}
        {route.view === "browse" && <Browse />}
        {route.view === "plans" && <Plans />}
        {route.view === "plan" && <PlanView key={route.id} id={route.id} />}
        {route.view === "servers" && <Servers />}
        {route.view === "server" && <ServerView key={route.id} id={route.id} />}
        {route.view === "settings" && <Settings />}
        {route.view === "instance" && (
          // Remounting per instance keeps tab state from leaking between them.
          <InstanceView key={route.id} id={route.id} tab={route.tab} />
        )}
      </main>
      <ProgressDock />
      {/* Here rather than in a view, because a link can arrive on any screen.
          Keyed by request, so a different link replaces the dialog outright
          instead of stacking a second one or inheriting the first's state. */}
      {join && <JoinModal key={join.seq} request={join} onClose={closeJoin} />}
      {pack && <PackLinkModal key={pack.seq} request={pack} onClose={closePack} />}
      {/* Play is on four screens; the offer a failed sign-in raises is here, so
          it reaches the player wherever they pressed it. */}
      {offlinePlay && (
        <OfflinePlayModal
          key={offlinePlay.seq}
          request={offlinePlay}
          onClose={closeOfflinePlay}
        />
      )}
      <Toasts />
    </div>
  );
}

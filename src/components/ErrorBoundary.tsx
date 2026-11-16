import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";
import { TriangleAlert } from "lucide-react";
import styles from "./ErrorBoundary.module.css";

/**
 * The last thing between a thrown render and a black window.
 *
 * React unmounts the whole tree when a render throws and nothing catches it.
 * In a browser that leaves a blank tab somebody can reload from the address
 * bar; here there is no address bar, no reload button and no menu — Geode is
 * a Tauri window, so an uncaught throw leaves a rectangle of `--bg` and the
 * only way out is killing the process. That is the difference between a bug
 * that loses a screen and a bug that loses the app.
 *
 * It is used twice, and the two placements do different jobs:
 *
 *   - Inside `<main>`, keyed by route, so a screen that throws leaves the
 *     sidebar alive. Going anywhere else gives the boundary a new key, React
 *     remounts it, and the error state is gone — which is why there is no
 *     reset handler here. Navigating away *is* the recovery, and the control
 *     that does it is still on screen.
 *   - Around the whole app, for a throw in the frame itself — the sidebar,
 *     the dock, a toast — where there is nothing left to navigate with.
 *
 * What it shows is the one thing a person can actually use: that the data is
 * untouched, what the error said, and that the rest of Geode still works.
 * There is no "Try again", because unlike every failed read in this app there
 * is nothing to retry — the same render would throw the same way.
 */
export default class ErrorBoundary extends Component<
  { children: ReactNode; whole?: boolean },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // The component stack is the only thing that says *which* screen threw,
    // and it exists nowhere else — React logs the error itself but this is
    // the half that names the file. The devtools console is where a bug
    // report comes from, since the frontend has no route into Geode's own log.
    console.error("A screen stopped while drawing:", error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    return (
      <div className={styles.wrap}>
        <div className={styles.panel}>
          <TriangleAlert size={40} className={styles.icon} />
          <h2>
            {this.props.whole
              ? "Geode stopped while drawing its window"
              : "This screen stopped while drawing"}
          </h2>
          <p>
            Nothing has been changed or removed. This is a fault in Geode
            itself, not in anything it was reading — your instances, servers
            and worlds are exactly as they were.
          </p>
          <p>
            {this.props.whole
              ? "Reopen Geode to carry on. If it happens again on the same screen, the details below are what a bug report needs."
              : "Everything else still works: pick another screen in the sidebar and carry on. The details below are what a bug report needs."}
          </p>
          <pre className={styles.detail}>{error.message || String(error)}</pre>
        </div>
      </div>
    );
  }
}

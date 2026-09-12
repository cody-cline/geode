import { useEffect } from "react";

import Sidebar from "./components/Sidebar";
import ProgressDock from "./components/ProgressDock";
import Toasts from "./components/Toasts";
import Browse from "./views/Browse";
import InstanceView from "./views/InstanceView";
import Library from "./views/Library";
import PlanView from "./views/PlanView";
import Plans from "./views/Plans";
import Settings from "./views/Settings";
import { useStore } from "./lib/store";

import styles from "./App.module.css";

export default function App() {
  const route = useStore((s) => s.route);
  const init = useStore((s) => s.init);

  useEffect(() => {
    void init();
  }, [init]);

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
      <Sidebar />
      <main className={styles.main}>
        {route.view === "library" && <Library />}
        {route.view === "browse" && <Browse />}
        {route.view === "plans" && <Plans />}
        {route.view === "plan" && <PlanView key={route.id} id={route.id} />}
        {route.view === "settings" && <Settings />}
        {route.view === "instance" && (
          // Remounting per instance keeps tab state from leaking between them.
          <InstanceView key={route.id} id={route.id} tab={route.tab} />
        )}
      </main>
      <ProgressDock />
      <Toasts />
    </div>
  );
}

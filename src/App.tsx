import { useEffect } from "react";

import Sidebar from "./components/Sidebar";
import ProgressDock from "./components/ProgressDock";
import Toasts from "./components/Toasts";
import Browse from "./views/Browse";
import InstanceView from "./views/InstanceView";
import Library from "./views/Library";
import Settings from "./views/Settings";
import { useStore } from "./lib/store";

import styles from "./App.module.css";

export default function App() {
  const route = useStore((s) => s.route);
  const init = useStore((s) => s.init);

  useEffect(() => {
    void init();
  }, [init]);

  return (
    <div className={styles.app}>
      <Sidebar />
      <main className={styles.main}>
        {route.view === "library" && <Library />}
        {route.view === "browse" && <Browse />}
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

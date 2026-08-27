import { AlertTriangle, X } from "lucide-react";

import { api } from "../lib/api";
import { useStore } from "../lib/store";

import styles from "./ProgressDock.module.css";

/** Live install/download jobs, docked bottom-left so they never cover the
 *  primary action in whatever view is open. */
export default function ProgressDock() {
  const progress = useStore((s) => s.progress);
  const jobs = Object.values(progress);

  if (jobs.length === 0) return null;

  return (
    <div className={styles.dock}>
      {jobs.map((job) => {
        const pct = job.total > 0 ? Math.round((job.current / job.total) * 100) : null;
        return (
          <div key={job.id} className={styles.job}>
            <div className={styles.row}>
              <span className={styles.stage}>
                {job.error ? (
                  <>
                    <AlertTriangle size={13} className={styles.errIcon} />
                    Failed
                  </>
                ) : (
                  job.stage || "Working"
                )}
              </span>
              <span className={styles.count}>
                {job.error ? "" : pct !== null ? `${pct}%` : ""}
              </span>
              {!job.done && (
                <button
                  className={styles.cancel}
                  title="Cancel"
                  aria-label="Cancel"
                  onClick={() => void api.cancelTask(job.id)}
                >
                  <X size={13} />
                </button>
              )}
            </div>

            <div className={styles.track}>
              <div
                className={pct === null ? styles.barIndeterminate : styles.bar}
                style={pct === null ? undefined : { width: `${pct}%` }}
                data-error={job.error ? "true" : undefined}
              />
            </div>

            <p className={styles.detail} title={job.error ?? job.detail}>
              {job.error ?? job.detail}
            </p>
          </div>
        );
      })}
    </div>
  );
}

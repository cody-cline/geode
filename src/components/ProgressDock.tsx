import { AlertTriangle, X } from "lucide-react";

import { api } from "../lib/api";
import { useStore } from "../lib/store";

import styles from "./ProgressDock.module.css";

/** Live install/download jobs, docked bottom-left so they never cover the
 *  primary action in whatever view is open. */
export default function ProgressDock() {
  const progress = useStore((s) => s.progress);

  // A job that failed is held by the store until somebody takes it away, and
  // held beside `progress` rather than in it — these are multi-minute
  // downloads people start and walk away from, and six seconds was the wrong
  // length for the one line saying why one stopped. Drawn after the live
  // ones, and only once the live map has let go of the id, or the same job
  // would be two cards under one key.
  const failed = useStore((s) => s.failedJobs);
  const dismissJob = useStore((s) => s.dismissJob);

  const jobs = [
    ...Object.values(progress),
    ...Object.values(failed).filter((job) => !progress[job.id]),
  ];

  if (jobs.length === 0) return null;



  return (
    <div className={styles.dock}>
      {jobs.map((job) => {
        // Clamped the way ProjectCard does it. The totals are fixed at the
        // source, but a stage that learns how much work it has only part-way
        // through is an ordinary shape here, and a dock reading 400% is worse
        // than one that sits at 100% for a moment.
        const pct =
          job.total > 0
            ? Math.min(100, Math.round((job.current / job.total) * 100))
            : null;
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
              {(!job.done || job.error) && (
                <button
                  className={styles.cancel}
                  title={job.error ? "Dismiss" : "Cancel"}
                  aria-label={job.error ? "Dismiss" : "Cancel"}
                  onClick={() => {
                    // There is nothing left to cancel on a job that already
                    // stopped; the X on a failure takes the card away.
                    if (job.error) {
                      dismissJob(job.id);
                      return;
                    }
                    void api.cancelTask(job.id);
                  }}
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

            <p
              className={
                job.error
                  ? `${styles.detail} ${styles.detailError}`
                  : styles.detail
              }
              title={job.error ?? job.detail}
            >
              {job.error ?? job.detail}
            </p>
          </div>
        );
      })}
    </div>
  );
}

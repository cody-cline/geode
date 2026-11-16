import { useEffect, useRef, useState } from "react";
import { AlertTriangle, X } from "lucide-react";

import { api } from "../lib/api";
import { useStore } from "../lib/store";
import type { ProgressEvent } from "../lib/types";

import styles from "./ProgressDock.module.css";

/**
 * Whether a job's one line is the core saying it stopped because somebody
 * asked it to.
 *
 * Read off the line rather than off who pressed the button, because the X in
 * this dock is not the only Cancel in the app — `PackLinkModal` cancels the
 * `pack-link` download, and a stop asked for there is just as much a stop. A
 * dock that only knew its own button left that one sitting as a failure for
 * the rest of the session.
 *
 * Nothing in the event says so, so this reads the two shapes the core writes:
 * `Error::Cancelled` prints exactly `cancelled`, and `.ctx("…")` puts what it
 * was doing in front of it as `{what}: {e}` — the one format in `error.rs`.
 * A cancellation the core wrote a sentence about instead is deliberately not
 * one of them: that sentence was written to be read.
 */
function wasCancelled(error: string): boolean {
  return error === "cancelled" || error.endsWith(": cancelled");
}

/** Live install/download jobs, docked bottom-left so they never cover the
 *  primary action in whatever view is open. */
export default function ProgressDock() {
  const progress = useStore((s) => s.progress);

  // The dock keeps its own copy of a job that failed. The store drops every
  // finished job on a timer — a second and a bit for one that worked, six
  // seconds for one that did not — and six seconds is the wrong length for
  // the one line saying why a download stopped: these are multi-minute jobs
  // people start and walk away from, and they come back to an empty dock
  // with nothing to say what happened. So a failure stays until somebody
  // takes it away.
  const [held, setHeld] = useState<Record<string, ProgressEvent>>({});

  // An id somebody dismissed is one they have read, so the card goes at the
  // press and the store's own copy is not drawn in its place. Forgotten as
  // soon as the id is free again: the ids are built from what the job is
  // about, so a second attempt arrives under the one that just failed and
  // must not inherit what was decided about it.
  const dismissed = useRef(new Set<string>());

  useEffect(() => {
    for (const id of dismissed.current) {
      if (!progress[id]?.error) dismissed.current.delete(id);
    }
    setHeld((prev) => {
      const next: Record<string, ProgressEvent> = {};
      for (const [id, job] of Object.entries(prev)) {
        if (!progress[id] || progress[id] === job) next[id] = job;
      }
      for (const job of Object.values(progress)) {
        if (
          job.error &&
          !wasCancelled(job.error) &&
          !dismissed.current.has(job.id)
        ) {
          next[job.id] = job;
        }
      }
      const ids = Object.keys(next);
      const same =
        ids.length === Object.keys(prev).length &&
        ids.every((id) => next[id] === prev[id]);
      return same ? prev : next;
    });
  }, [progress]);

  // While the store still has a job, it is the copy that gets drawn — the
  // held one would be the same card twice under the same key. The held copy
  // takes over the moment the store lets go.
  const jobs = [
    ...Object.values(progress).filter(
      (job) => !job.error || !dismissed.current.has(job.id),
    ),
    ...Object.values(held).filter((job) => !progress[job.id]),
  ];

  if (jobs.length === 0) return null;

  const dismiss = (id: string) => {
    dismissed.current.add(id);
    setHeld((prev) => {
      const next = { ...prev };
      delete next[id];
      return next;
    });
  };

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
                      dismiss(job.id);
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

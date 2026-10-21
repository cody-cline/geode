import { useEffect, useState } from "react";
import { Loader2, PowerOff, ShieldAlert } from "lucide-react";

import { api } from "../../lib/api";
import { relativeTime } from "../../lib/format";
import { useStore } from "../../lib/store";
import type { CrashEvidence, CrashReport, CrashSuspect } from "../../lib/types";

import styles from "./CrashCard.module.css";

/**
 * What the evidence actually was, so nobody reads a guess as a finding.
 *
 * A frame is the weakest thing this says: half the stack of any modded crash
 * belongs to mods that did nothing wrong, and the topmost mod frame is
 * routinely a mod whose mixin merely sits on the method that broke.
 */
const BECAUSE: Record<CrashEvidence, string> = {
  suspected: "Minecraft's own mod loader named it as the likely cause.",
  named: "The crash report names it as the mod that failed.",
  frame: "A line in the crash came from its code, which may or may not mean it caused it.",
};

/**
 * The newest crash report, as the one thing it is for: which mod broke.
 *
 * Shown above the log files because the report is the same failure already
 * narrowed down — a wall of Java that has an answer in it, if somebody reads
 * the right four places. Nothing is invented: when the report names no mod
 * this says so, rather than blaming whichever mod appears first in a trace.
 */
export default function CrashCard({ instanceId }: { instanceId: string }) {
  const reportError = useStore((s) => s.reportError);
  const toast = useStore((s) => s.toast);

  const [report, setReport] = useState<CrashReport | null>(null);
  const [turningOff, setTurningOff] = useState<string | null>(null);
  const [off, setOff] = useState<string[]>([]);

  useEffect(() => {
    let cancelled = false;
    setReport(null);
    setOff([]);
    api
      .instanceCrash(instanceId)
      .then((found) => !cancelled && setReport(found))
      // A crash report that cannot be read is not worth a message of its own:
      // the log files below are still there and are what the tab is for.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [instanceId]);

  async function turnOff(suspect: CrashSuspect) {
    if (!suspect.file) return;
    setTurningOff(suspect.file);
    try {
      // The same path the Content tab uses — nothing here renames a file
      // itself, so a mod turned off from a crash report is turned off exactly
      // as one turned off by hand, and reads that way to every other launcher.
      await api.setContentEnabled(instanceId, "mod", suspect.file, false);
      setOff((was) => [...was, suspect.file!]);
      toast({
        kind: "success",
        title: `Turned off ${suspect.name ?? suspect.modId ?? suspect.file}`,
        body: "Launch again to see whether that was it. The file is still there, renamed, so turning it back on is one click in Content.",
      });
    } catch (e) {
      reportError("Could not turn that mod off", e);
    } finally {
      setTurningOff(null);
    }
  }

  if (!report) return null;

  return (
    <div className={styles.card}>
      <div className={styles.head}>
        <ShieldAlert size={15} color="var(--danger)" />
        <span className={styles.title}>Minecraft crashed</span>
        <span className={styles.when}>
          {relativeTime(report.modified * 1000)} · {report.name}
        </span>
      </div>

      <p className={styles.what}>
        {report.description || "The report does not say what the game was doing."}
        {report.exception && <code className={styles.exception}>{report.exception}</code>}
      </p>

      {report.suspects.length === 0 ? (
        <p className={styles.nothing}>
          Nothing in this report names a mod. The crash is in the game's own
          code or the loader's, so turning a mod off is a guess — send the
          report to someone who can read it with “Copy for an assistant” below.
        </p>
      ) : (
        <div className={styles.suspects}>
          {report.suspects.map((suspect, i) => {
            const label = suspect.name ?? suspect.jar ?? suspect.modId ?? "An unnamed mod";
            const already = suspect.file ? off.includes(suspect.file) : false;
            return (
              <div key={`${suspect.modId ?? suspect.jar ?? i}`} className={styles.suspect}>
                <div className={styles.who}>
                  <span className={styles.name}>{label}</span>
                  {suspect.modId && suspect.modId !== label && (
                    <span className={styles.id}>{suspect.modId}</span>
                  )}
                  <p className={styles.because}>
                    {BECAUSE[suspect.evidence]}
                    <code className={styles.quote}>{suspect.quote}</code>
                  </p>
                </div>
                {/* Only for a mod this instance has a record of: a report from
                    a pack that has since changed names mods that are not here,
                    and a button that cannot do anything is worse than none. */}
                {suspect.file && (suspect.enabled || already) && (
                  <button
                    className="btn btn-sm"
                    onClick={() => void turnOff(suspect)}
                    disabled={already || turningOff !== null}
                    title={`Rename ${suspect.file} so the loader skips it`}
                  >
                    {turningOff === suspect.file ? (
                      <Loader2 size={13} className="spin" />
                    ) : (
                      <PowerOff size={13} />
                    )}
                    {already ? "Turned off" : "Turn off"}
                  </button>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

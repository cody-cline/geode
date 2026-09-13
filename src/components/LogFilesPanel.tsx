import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { Bot, Check, Copy, FileText, Loader2, Trash2 } from "lucide-react";

import { useStore } from "../lib/store";
import { fileSize, relativeTime } from "../lib/format";
import type { LogFile } from "../lib/types";

import styles from "./LogFilesPanel.module.css";

/**
 * Where logs come from. An instance and a server keep the same files in
 * different folders, so the panel takes the four calls rather than knowing
 * which kind it is looking at.
 */
export interface LogSource {
  list: () => Promise<LogFile[]>;
  read: (name: string) => Promise<string>;
  remove: (name: string) => Promise<void>;
  /** A pasteable report. `name` picks a log; omitted means the current one. */
  report: (name?: string) => Promise<string>;
}

/**
 * Past runs, not just this one.
 *
 * The console only holds what was captured since the window opened, and the
 * run that broke is usually the one nobody was watching. These are the files
 * Minecraft wrote itself, so they survive a restart and cover the whole run.
 */
export default function LogFilesPanel({ source }: { source: LogSource }) {
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  const [files, setFiles] = useState<LogFile[] | null>(null);
  const [chosen, setChosen] = useState<string | null>(null);
  const [text, setText] = useState<string | null>(null);
  const [copied, setCopied] = useState<"none" | "text" | "report">("none");
  const [busy, setBusy] = useState(false);

  const scroller = useRef<HTMLPreElement>(null);

  const refresh = useCallback(() => {
    source
      .list()
      .then(setFiles)
      .catch(() => setFiles([]));
  }, [source]);

  useEffect(refresh, [refresh]);

  // Open the newest on arrival. Landing on an empty pane and having to pick
  // helps nobody: the newest log is what you came for nine times in ten.
  useEffect(() => {
    if (chosen !== null || !files?.length) return;
    setChosen(files[0].name);
  }, [files, chosen]);

  useEffect(() => {
    if (chosen === null) {
      setText(null);
      return;
    }
    let cancelled = false;
    setText(null);
    source
      .read(chosen)
      .then((body) => !cancelled && setText(body))
      .catch((e) => {
        if (cancelled) return;
        reportError("Could not read that log", e);
        setChosen(null);
      });
    return () => {
      cancelled = true;
    };
  }, [chosen, source, reportError]);

  const shown = text ?? "";

  // Jump to the end, which is where a failure is.
  useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [shown]);

  const flash = (what: "text" | "report") => {
    setCopied(what);
    setTimeout(() => setCopied("none"), 1600);
  };

  async function copyText() {
    await navigator.clipboard.writeText(shown);
    flash("text");
  }

  async function copyReport() {
    setBusy(true);
    try {
      const body = await source.report(chosen ?? undefined);
      await navigator.clipboard.writeText(body);
      flash("report");
      toast({
        kind: "success",
        title: "Copied a report to the clipboard",
        body: "It includes what is installed and the crash report, with a prompt on top. Paste it to an assistant.",
      });
    } catch (e) {
      reportError("Could not build the report", e);
    } finally {
      setBusy(false);
    }
  }

  async function remove(name: string) {
    try {
      await source.remove(name);
      if (chosen === name) setChosen(null);
      refresh();
    } catch (e) {
      reportError("Could not delete that log", e);
    }
  }

  const empty = useMemo(() => files !== null && files.length === 0, [files]);

  return (
    <div className={styles.panel}>
      <aside className={styles.list}>
        {files === null ? (
          <div className={styles.pending}>
            <Loader2 size={14} className="spin" />
          </div>
        ) : (
          files.map((file) => (
            <div key={file.name} className={styles.row}>
              <button
                className={clsx(
                  styles.entry,
                  chosen === file.name && styles.entryActive,
                )}
                onClick={() => setChosen(file.name)}
              >
                <FileText size={13} />
                <span className={styles.entryText}>
                  <span className={styles.entryName}>
                    {file.current ? "Latest" : file.name.replace(/\.log(\.gz)?$/, "")}
                  </span>
                  <span className={styles.entryMeta}>
                    {relativeTime(file.modified * 1000)} · {fileSize(file.size)}
                  </span>
                </span>
              </button>
              {/* The live log is the one being written to; deleting it would
                  achieve nothing and confuse whatever holds it open. */}
              {!file.current && (
                <button
                  className={styles.remove}
                  onClick={() => void remove(file.name)}
                  title={`Delete ${file.name}`}
                  aria-label={`Delete ${file.name}`}
                >
                  <Trash2 size={12} />
                </button>
              )}
            </div>
          ))
        )}

        {empty && (
          <p className={styles.none}>
            No logs yet. Minecraft writes one each time it runs.
          </p>
        )}
      </aside>

      <div className={styles.viewer}>
        <div className={styles.actions}>
          <button className="btn btn-sm" onClick={copyText} disabled={!shown}>
            {copied === "text" ? <Check size={13} /> : <Copy size={13} />}
            {copied === "text" ? "Copied" : "Copy log"}
          </button>
          {/* The useful one: the log alone rarely explains anything, and the
              list of what is installed is what turns it into a diagnosis. */}
          <button
            className="btn btn-sm btn-primary"
            onClick={copyReport}
            disabled={busy}
            title="Copies the log together with your setup, what is installed, and any crash report — with a prompt asking for the cause"
          >
            {busy ? (
              <Loader2 size={13} className="spin" />
            ) : copied === "report" ? (
              <Check size={13} />
            ) : (
              <Bot size={13} />
            )}
            {copied === "report" ? "Copied" : "Copy for an assistant"}
          </button>
        </div>

        <pre ref={scroller} className={styles.text}>
          {chosen !== null && text === null ? "Reading…" : shown || "Nothing here."}
        </pre>
      </div>
    </div>
  );
}

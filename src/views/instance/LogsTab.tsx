import { useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { ArrowDownToLine, Copy, Search, Terminal, Trash2 } from "lucide-react";

import LogFilesPanel from "../../components/LogFilesPanel";
import type { LogSource } from "../../components/LogFilesPanel";
import CrashCard from "./CrashCard";
import { api } from "../../lib/api";
import { useStore } from "../../lib/store";
import type { LogEntry } from "../../lib/types";

import styles from "./LogsTab.module.css";

const LEVELS = ["ALL", "INFO", "WARN", "ERROR"] as const;
type Level = (typeof LEVELS)[number];

export default function LogsTab({ instanceId }: { instanceId: string }) {
  const logs = useStore((s) => s.logs[instanceId]);
  const clearLogs = useStore((s) => s.clearLogs);
  const toast = useStore((s) => s.toast);

  // The console is this run; the files are every run, including the one that
  // crashed while nobody was looking.
  const [view, setView] = useState<"console" | "files">("console");
  const [level, setLevel] = useState<Level>("ALL");
  const [filter, setFilter] = useState("");
  const [follow, setFollow] = useState(true);

  const scroller = useRef<HTMLDivElement>(null);

  const entries = useMemo(() => {
    const list = logs ?? [];
    const q = filter.trim().toLowerCase();
    return list.filter((entry) => {
      if (level !== "ALL" && !matchesLevel(entry, level)) return false;
      if (q && !entry.message.toLowerCase().includes(q)) return false;
      return true;
    });
  }, [logs, level, filter]);

  // Stick to the bottom while following, the way a terminal does.
  useEffect(() => {
    if (!follow) return;
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [entries, follow]);

  function onScroll() {
    const el = scroller.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    // Scrolling up to read should pause the auto-scroll, not fight it.
    if (atBottom !== follow) setFollow(atBottom);
  }

  async function copyAll() {
    const text = entries
      .map((e) => `[${e.level}]${e.thread ? ` [${e.thread}]` : ""} ${e.message}`)
      .join("\n");
    await navigator.clipboard.writeText(text);
    toast({ kind: "success", title: `Copied ${entries.length} lines` });
  }

  const source: LogSource = useMemo(
    () => ({
      list: () => api.listInstanceLogs(instanceId),
      read: (name) => api.readInstanceLog(instanceId, name),
      remove: (name) => api.deleteInstanceLog(instanceId, name),
      report: (name) => api.instanceReport(instanceId, name),
    }),
    [instanceId],
  );

  return (
    <div className={styles.wrap}>
      <div className={styles.views}>
        <button
          className={clsx(styles.viewTab, view === "console" && styles.viewActive)}
          onClick={() => setView("console")}
        >
          Console
        </button>
        <button
          className={clsx(styles.viewTab, view === "files" && styles.viewActive)}
          onClick={() => setView("files")}
        >
          Log files
        </button>
      </div>

      {view === "files" ? (
        <div className={styles.filesView}>
          {/* Above the files, because the crash report is the same failure
              already narrowed down to a mod. */}
          <CrashCard instanceId={instanceId} />
          <div className={styles.filesBody}>
            <LogFilesPanel source={source} />
          </div>
        </div>
      ) : (
      <>
      <div className={styles.toolbar}>
        <div className={styles.levels}>
          {LEVELS.map((l) => (
            <button
              key={l}
              className={clsx(styles.levelTab, level === l && styles.levelActive)}
              onClick={() => setLevel(l)}
            >
              {l === "ALL" ? "All" : l}
            </button>
          ))}
        </div>

        <div className={styles.tools}>
          <div className={styles.searchWrap}>
            <Search size={14} className={styles.searchIcon} />
            <input
              className={clsx("input", styles.search)}
              placeholder="Search output"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          </div>
          <button
            className={clsx("btn", follow && styles.followOn)}
            onClick={() => {
              setFollow(true);
              const el = scroller.current;
              if (el) el.scrollTop = el.scrollHeight;
            }}
            title="Jump to the newest line"
          >
            <ArrowDownToLine size={14} />
            Follow
          </button>
          <button className="btn" onClick={copyAll} disabled={entries.length === 0}>
            <Copy size={14} />
            Copy
          </button>
          <button
            className="btn"
            onClick={() => clearLogs(instanceId)}
            disabled={!logs || logs.length === 0}
          >
            <Trash2 size={14} />
            Clear
          </button>
        </div>
      </div>

      <div ref={scroller} className={styles.console} onScroll={onScroll}>
        {entries.length === 0 ? (
          <div className={styles.empty}>
            <Terminal size={30} className={styles.emptyIcon} />
            <p>
              {logs && logs.length > 0
                ? "No lines match the current filter."
                : "Launch the instance and its output shows up here."}
            </p>
          </div>
        ) : (
          entries.map((entry, i) => (
            <div key={i} className={clsx(styles.line, styles[levelClass(entry.level)])}>
              <span className={styles.level}>{shortLevel(entry.level)}</span>
              {entry.thread && <span className={styles.thread}>{entry.thread}</span>}
              <span className={styles.message}>{entry.message}</span>
            </div>
          ))
        )}
      </div>
      </>
      )}
    </div>
  );
}

function matchesLevel(entry: LogEntry, level: Level): boolean {
  const actual = entry.level.toUpperCase();
  if (level === "ERROR") return actual === "ERROR" || actual === "FATAL";
  if (level === "WARN") return actual === "WARN" || actual === "ERROR" || actual === "FATAL";
  return true;
}

function levelClass(level: string): "err" | "warn" | "debug" | "info" {
  const upper = level.toUpperCase();
  if (upper === "ERROR" || upper === "FATAL") return "err";
  if (upper === "WARN") return "warn";
  if (upper === "DEBUG" || upper === "TRACE") return "debug";
  return "info";
}

function shortLevel(level: string): string {
  return level.toUpperCase().slice(0, 4);
}

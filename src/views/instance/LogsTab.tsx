import { useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { ArrowDownToLine, Check, Copy, Search, Terminal, Trash2 } from "lucide-react";

import LogFilesPanel from "../../components/LogFilesPanel";
import type { LogSource } from "../../components/LogFilesPanel";
import CrashCard from "./CrashCard";
import { api } from "../../lib/api";
import { rovingKey } from "../../lib/rovingFocus";
import { useStore } from "../../lib/store";
import type { LogEntry } from "../../lib/types";

import styles from "./LogsTab.module.css";

const LEVELS = ["ALL", "INFO", "WARN", "ERROR"] as const;
type Level = (typeof LEVELS)[number];

/** The two panels, in the order the strip above them prints them. */
const VIEWS = ["console", "files"] as const;
type View = (typeof VIEWS)[number];

export default function LogsTab({ instanceId }: { instanceId: string }) {
  const logs = useStore((s) => s.logs[instanceId]);
  const clearLogs = useStore((s) => s.clearLogs);

  // The console is this run; the files are every run, including the one that
  // crashed while nobody was looking.
  const [view, setView] = useState<View>("console");
  const [level, setLevel] = useState<Level>("ALL");
  const [filter, setFilter] = useState("");
  const [follow, setFollow] = useState(true);
  const [copied, setCopied] = useState(false);

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

  // Said on the button, the way the log-file panel and the share links say it.
  // This was the one copy in the app that answered in a corner toast instead,
  // which is a different gesture for the same action — and the only one where
  // what you pressed does not react.
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1800);
    return () => clearTimeout(timer);
  }, [copied]);

  async function copyAll() {
    const text = entries
      .map((e) => `[${e.level}]${e.thread ? ` [${e.thread}]` : ""} ${e.message}`)
      .join("\n");
    await navigator.clipboard.writeText(text);
    setCopied(true);
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
      {/* The strip swaps the whole panel below it, so it is a tab set and says
          so: one tab stop for the pair, arrows between them, and the panel
          named by the tab that opened it. Saying `role="tab"` without the
          arrows would be worse than saying nothing, which is why the keyboard
          comes from the shared helper rather than from here. */}
      <div
        className={styles.views}
        role="tablist"
        aria-label="Log view"
        onKeyDown={(e) => {
          const next = rovingKey(e, VIEWS);
          if (next) setView(next);
        }}
      >
        <button
          id="logs-view-console"
          className={clsx(styles.viewTab, view === "console" && styles.viewActive)}
          onClick={() => setView("console")}
          role="tab"
          aria-selected={view === "console"}
          aria-controls="logs-panel-console"
          tabIndex={view === "console" ? 0 : -1}
          data-roving="console"
        >
          Console
        </button>
        <button
          id="logs-view-files"
          className={clsx(styles.viewTab, view === "files" && styles.viewActive)}
          onClick={() => setView("files")}
          role="tab"
          aria-selected={view === "files"}
          aria-controls="logs-panel-files"
          tabIndex={view === "files" ? 0 : -1}
          data-roving="files"
        >
          Log files
        </button>
      </div>

      {view === "files" ? (
        <div
          id="logs-panel-files"
          className={styles.filesView}
          role="tabpanel"
          aria-labelledby="logs-view-files"
          tabIndex={0}
        >
          {/* Above the files, because the crash report is the same failure
              already narrowed down to a mod. */}
          <CrashCard instanceId={instanceId} />
          <div className={styles.filesBody}>
            <LogFilesPanel source={source} />
          </div>
        </div>
      ) : (
      <div
        id="logs-panel-console"
        className={styles.consoleView}
        role="tabpanel"
        aria-labelledby="logs-view-console"
        tabIndex={0}
      >
      <div className={styles.toolbar}>
        {/* One of four, and what it picks narrows the lines already in this
            panel rather than swapping the panel — so a radio group and not a
            tab set, which is also why these are drawn as pills and the strip
            above them as segments. Same keyboard either way: one tab stop for
            the row, arrows between the four from there and the one they land
            on in force. Named here, since there is no heading to point at. */}
        <div
          className={styles.levels}
          role="radiogroup"
          aria-label="Log level"
          onKeyDown={(e) => {
            const next = rovingKey(e, LEVELS);
            if (next) setLevel(next);
          }}
        >
          {LEVELS.map((l) => (
            <button
              key={l}
              className={clsx(styles.levelTab, level === l && styles.levelActive)}
              onClick={() => setLevel(l)}
              role="radio"
              aria-checked={level === l}
              tabIndex={level === l ? 0 : -1}
              data-roving={l}
            >
              {l === "ALL" ? "All" : l}
            </button>
          ))}
        </div>

        <div className={styles.tools}>
          <div className={styles.searchWrap}>
            <Search size={14} className={styles.searchIcon} />
            {/* Off on the same condition as Clear beside it, and for the same
                reason Copy and Clear learned to go grey: there is nothing in
                this panel to search. The level pills stay live — they are four
                fixed values, not a claim that something here can be found. */}
            <input
              className={clsx("input", styles.search)}
              placeholder="Search output"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              disabled={!logs || logs.length === 0}
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
            {copied ? <Check size={14} /> : <Copy size={14} />}
            {copied ? "Copied" : "Copy"}
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
      </div>
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

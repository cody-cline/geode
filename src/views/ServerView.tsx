import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import {
  ArrowLeft,
  ChevronRight,
  ExternalLink,
  FolderOpen,
  Loader2,
  Play,
  Square,
  Trash2,
} from "lucide-react";

import Modal from "../components/Modal";
import { api } from "../lib/api";
import { useStore } from "../lib/store";
import type { Server, ServerLine, ServerSettings } from "../lib/types";

import styles from "./ServerView.module.css";

const TABS = [
  { id: "console", label: "Console" },
  { id: "settings", label: "Settings" },
] as const;

type TabId = (typeof TABS)[number]["id"];

/**
 * A server's console line, as the server writes it:
 * `[03:52:13] [Server thread/INFO]: Done (5.123s)!`
 *
 * Plain text on purpose — unlike the game, a server does not emit structured
 * log4j XML — so the shape is read back out of the line itself.
 */
const LINE = /^\[([^\]]+)\]\s*\[([^\]]*?)\/([A-Z]+)\]:?\s?(.*)$/;

function levelOf(entry: ServerLine): string {
  if (entry.stream === "err") return "error";
  const match = LINE.exec(entry.line);
  const level = match?.[3]?.toLowerCase() ?? "";
  if (level === "warn" || level === "warning") return "warn";
  if (level === "error" || level === "fatal" || level === "severe") return "error";
  if (level === "debug" || level === "trace") return "debug";
  return "info";
}

export default function ServerView({ id }: { id: string }) {
  const servers = useStore((s) => s.servers);
  const navigate = useStore((s) => s.navigate);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const refreshServers = useStore((s) => s.refreshServers);
  const lines = useStore((s) => s.serverLogs[id]);
  const clearServerLog = useStore((s) => s.clearServerLog);

  const [active, setActive] = useState<TabId>("console");
  const [busy, setBusy] = useState(false);
  const [askEula, setAskEula] = useState(false);
  const [command, setCommand] = useState("");
  const [history, setHistory] = useState<string[]>([]);
  const [historyAt, setHistoryAt] = useState<number | null>(null);

  const server = servers.find((s) => s.id === id);

  useEffect(() => {
    if (servers.length > 0 && !server) navigate({ view: "servers" });
  }, [servers, server, navigate]);

  // Follow the tail unless the reader has scrolled away from it.
  const scroller = useRef<HTMLDivElement>(null);
  const stuck = useRef(true);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stuck.current) el.scrollTop = el.scrollHeight;
  }, [lines]);

  const shown = useMemo(() => lines ?? [], [lines]);

  if (!server) {
    return (
      <div className={styles.loading}>
        <Loader2 size={20} className="spin" />
      </div>
    );
  }

  async function start(server: Server) {
    // Mojang's EULA is the user's to accept, and asking before the first start
    // means the answer is not a failed boot followed by a second one.
    if (!server.eulaAccepted && server.kind !== "velocity" && server.kind !== "waterfall") {
      setAskEula(true);
      return;
    }
    setBusy(true);
    try {
      await api.startServer(server.id);
      await refreshServers();
    } catch (e) {
      reportError(`Could not start ${server.name}`, e);
    } finally {
      setBusy(false);
    }
  }

  async function acceptAndStart(server: Server) {
    setBusy(true);
    try {
      await api.acceptServerEula(server.id);
      await api.startServer(server.id);
      await refreshServers();
      setAskEula(false);
    } catch (e) {
      reportError(`Could not start ${server.name}`, e);
    } finally {
      setBusy(false);
    }
  }

  async function stop(server: Server) {
    setBusy(true);
    try {
      await api.stopServer(server.id);
      toast({
        kind: "info",
        title: `Stopping ${server.name}`,
        body: "Waiting for it to save and shut down.",
      });
    } catch (e) {
      reportError(`Could not stop ${server.name}`, e);
    } finally {
      setBusy(false);
    }
  }

  async function send() {
    const text = command.trim();
    if (!text || !server?.running) return;
    setCommand("");
    setHistory((past) => [...past.filter((c) => c !== text), text].slice(-100));
    setHistoryAt(null);
    try {
      await api.sendServerCommand(id, text);
    } catch (e) {
      reportError("Could not send that command", e);
    }
  }

  /** Up and down walk previous commands, as a terminal does. */
  function onKey(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter") {
      void send();
      return;
    }
    if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
    if (history.length === 0) return;
    e.preventDefault();

    const next =
      e.key === "ArrowUp"
        ? historyAt === null
          ? history.length - 1
          : Math.max(0, historyAt - 1)
        : historyAt === null
          ? null
          : historyAt + 1 >= history.length
            ? null
            : historyAt + 1;

    setHistoryAt(next);
    setCommand(next === null ? "" : history[next]);
  }

  async function openFolder() {
    try {
      await revealItemInDir(await api.serverFolder(id));
    } catch (e) {
      reportError("Could not open the folder", e);
    }
  }

  return (
    <>
      <header className={styles.header}>
        <button
          className={styles.back}
          onClick={() => navigate({ view: "servers" })}
          aria-label="Back to servers"
        >
          <ArrowLeft size={17} />
        </button>

        <div className={styles.headText}>
          <h1 className={styles.name}>{server.name}</h1>
          <div className={styles.meta}>
            <span className="badge badge-accent">{server.kind}</span>
            <span>{server.mcVersion}</span>
            {server.build && <span>build {server.build}</span>}
            {server.running && <span className={styles.live}>running</span>}
          </div>
        </div>

        <div className={styles.actions}>
          <button className="btn" onClick={openFolder}>
            <FolderOpen size={15} />
            Folder
          </button>
          {server.running ? (
            <button
              className="btn btn-danger"
              onClick={() => void stop(server)}
              disabled={busy}
            >
              {busy ? (
                <Loader2 size={14} className="spin" />
              ) : (
                <Square size={13} fill="currentColor" />
              )}
              Stop
            </button>
          ) : (
            <button
              className="btn btn-primary"
              onClick={() => void start(server)}
              disabled={busy || !server.jar}
              title={server.jar ? undefined : "This server has no jar yet"}
            >
              {busy ? (
                <Loader2 size={14} className="spin" />
              ) : (
                <Play size={14} fill="currentColor" />
              )}
              Start
            </button>
          )}
        </div>
      </header>

      <nav className={styles.tabs}>
        {TABS.map((tab) => (
          <button
            key={tab.id}
            className={clsx(styles.tab, active === tab.id && styles.tabActive)}
            onClick={() => setActive(tab.id)}
          >
            {tab.label}
          </button>
        ))}
        <div style={{ flex: 1 }} />
        {active === "console" && shown.length > 0 && (
          <button className="btn btn-sm" onClick={() => clearServerLog(id)}>
            <Trash2 size={13} />
            Clear
          </button>
        )}
      </nav>

      {active === "console" ? (
        <div className={styles.console}>
          <div
            className={styles.scroll}
            ref={scroller}
            onScroll={(e) => {
              const el = e.currentTarget;
              stuck.current =
                el.scrollHeight - el.scrollTop - el.clientHeight < 40;
            }}
          >
            {shown.length === 0 ? (
              <p className={styles.quiet}>
                {server.running
                  ? "Waiting for output…"
                  : "Nothing yet. Start the server and its console appears here."}
              </p>
            ) : (
              shown.map((entry, i) => (
                <div
                  key={i}
                  className={clsx(styles.line, styles[levelOf(entry)])}
                >
                  {entry.line}
                </div>
              ))
            )}
          </div>

          <div className={styles.prompt}>
            <ChevronRight size={15} className={styles.caret} />
            <input
              className={styles.input}
              value={command}
              placeholder={
                server.running ? "Type a command and press Enter" : "Server is not running"
              }
              disabled={!server.running}
              onChange={(e) => setCommand(e.target.value)}
              onKeyDown={onKey}
              spellCheck={false}
            />
          </div>
        </div>
      ) : (
        <SettingsTab server={server} />
      )}

      {askEula && (
        <Modal
          title="Accept the Minecraft EULA"
          subtitle="Mojang requires this before any server will start."
          onClose={() => setAskEula(false)}
          width={480}
          footer={
            <>
              <button className="btn" onClick={() => setAskEula(false)}>
                Cancel
              </button>
              <button
                className="btn btn-primary"
                onClick={() => void acceptAndStart(server)}
                disabled={busy}
              >
                {busy && <Loader2 size={14} className="spin" />}
                Accept and start
              </button>
            </>
          }
        >
          <p className={styles.eula}>
            Running a Minecraft server means agreeing to Mojang&apos;s End User
            Licence Agreement. Accepting here writes <code>eula=true</code> into
            this server&apos;s <code>eula.txt</code>, which is exactly what you
            would do by hand.
          </p>
          <p className={styles.eula}>
            This is your agreement to make, so Geode asks rather than assuming —
            and asks now, before the first start, instead of letting it fail and
            making you start it twice.
          </p>
          <button
            className={styles.eulaLink}
            onClick={() => void openUrl("https://aka.ms/MinecraftEULA")}
          >
            Read the EULA
            <ExternalLink size={12} />
          </button>
        </Modal>
      )}
    </>
  );
}

function SettingsTab({ server }: { server: Server }) {
  const refreshServers = useStore((s) => s.refreshServers);
  const reportError = useStore((s) => s.reportError);
  const [draft, setDraft] = useState<ServerSettings>(server.settings);
  const [saving, setSaving] = useState(false);

  async function save() {
    setSaving(true);
    try {
      await api.updateServer(server.id, { settings: draft });
      await refreshServers();
    } catch (e) {
      reportError("Could not save settings", e);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className={styles.settings}>
      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Java</h2>
        <div className={styles.grid}>
          <div>
            <label className="label" htmlFor="mem">
              Memory (MB)
            </label>
            <input
              id="mem"
              className="input"
              type="number"
              min={512}
              step={512}
              value={draft.memoryMb}
              onChange={(e) =>
                setDraft({ ...draft, memoryMb: Number(e.target.value) || 512 })
              }
            />
          </div>
          <div>
            <label className="label" htmlFor="javapath">
              Java path
            </label>
            <input
              id="javapath"
              className="input"
              value={draft.javaPath ?? ""}
              placeholder={
                server.javaMajor
                  ? `Automatic (Java ${server.javaMajor})`
                  : "Automatic"
              }
              onChange={(e) =>
                setDraft({ ...draft, javaPath: e.target.value || null })
              }
            />
          </div>
        </div>
        <div style={{ marginTop: 12 }}>
          <label className="label" htmlFor="javaargs">
            Extra Java arguments
          </label>
          <input
            id="javaargs"
            className="input"
            value={draft.javaArgs ?? ""}
            placeholder="-XX:+UseG1GC"
            onChange={(e) =>
              setDraft({ ...draft, javaArgs: e.target.value || null })
            }
          />
        </div>
      </section>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Server</h2>
        <p className={styles.pending}>
          Editing <code>server.properties</code> is not built yet — use the
          Folder button for now. Backups, scheduling and plugin browsing are
          also still to come.
        </p>
      </section>

      <div className={styles.saveRow}>
        <button className="btn btn-primary" onClick={save} disabled={saving}>
          {saving && <Loader2 size={14} className="spin" />}
          Save
        </button>
      </div>
    </div>
  );
}

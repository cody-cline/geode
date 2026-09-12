import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { KeyboardEvent } from "react";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import {
  ArrowLeft,
  ChevronRight,
  ExternalLink,
  FolderOpen,
  Loader2,
  Package,
  Play,
  Plus,
  Square,
  Trash2,
} from "lucide-react";

import Modal from "../components/Modal";
import Browse from "./Browse";
import { api } from "../lib/api";
import { fileSize, providerShort, relativeTime } from "../lib/format";
import { useStore } from "../lib/store";
import type {
  BackupInfo,
  ContentItem,
  ContentKind,
  Schedule,
  Server,
  ServerKindInfo,
  ServerLine,
  ServerProperty,
  ServerSettings,
} from "../lib/types";

import styles from "./ServerView.module.css";

const TABS = [
  { id: "console", label: "Console" },
  { id: "content", label: "Content" },
  // Backups on a timer and commands on a timer are the same idea — things the
  // server does without being asked — so they share a tab.
  { id: "backups", label: "Automation" },
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

  // Which folder is being added to, and what is already in it — so browsing
  // can mark the rows that are.
  const [adding, setAdding] = useState<ContentKind | null>(null);
  const [present, setPresent] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (!adding) return;
    let cancelled = false;
    api
      .listServerContent(id, adding)
      .then((items) => {
        if (cancelled) return;
        setPresent(
          new Set(
            items
              .filter((i) => i.provider && i.projectId)
              .map((i) => `${i.provider}:${i.projectId}`),
          ),
        );
      })
      .catch(() => !cancelled && setPresent(new Set()));
    return () => {
      cancelled = true;
    };
  }, [adding, id]);

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

  // Adding takes over the view, the way it does for an instance: this is the
  // browse experience and it needs the room.
  if (adding) {
    return (
      <Browse
        onDone={() => setAdding(null)}
        server={{
          name: server.name,
          installed: present,
          kind: adding === "plugin" ? "plugin" : "mod",
          onInstall: (project) => {
            void api
              .installServerContent(id, project.provider, project.id, adding)
              .then((report) => {
                setPresent((was) =>
                  new Set(was).add(`${project.provider}:${project.id}`),
                );
                if (report.skipped.length > 0) {
                  toast({
                    kind: "error",
                    title: `${project.name} needs more than itself`,
                    body: report.skipped.join("; "),
                  });
                }
              })
              .catch((e) => reportError(`Could not install ${project.name}`, e));
          },
        }}
      />
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
      ) : active === "content" ? (
        <ContentPane server={server} onAdd={setAdding} />
      ) : active === "backups" ? (
        <BackupsPane server={server} />
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

/**
 * What is installed in the server's folders.
 *
 * Which folders exist depends on the software, and the backend is the one that
 * knows: a hybrid has both `plugins/` and `mods/`, and it is the only case
 * where the switcher is worth showing.
 */
function ContentPane({
  server,
  onAdd,
}: {
  server: Server;
  onAdd: (kind: ContentKind) => void;
}) {
  const reportError = useStore((s) => s.reportError);

  const [info, setInfo] = useState<ServerKindInfo | null>(null);
  const [folder, setFolder] = useState<ContentKind>("plugin");
  const [items, setItems] = useState<ContentItem[] | null>(null);

  useEffect(() => {
    api
      .serverKinds()
      .then((kinds) => {
        const found = kinds.find((k) => k.id === server.kind) ?? null;
        setInfo(found);
        if (found) setFolder(found.takesPlugins ? "plugin" : "mod");
      })
      .catch(() => setInfo(null));
  }, [server.kind]);

  const load = useCallback(async () => {
    try {
      setItems(await api.listServerContent(server.id, folder));
    } catch (e) {
      reportError("Could not read the server folder", e);
      setItems([]);
    }
  }, [server.id, folder, reportError]);

  useEffect(() => {
    setItems(null);
    void load();
  }, [load]);

  async function toggle(item: ContentItem) {
    try {
      await api.setServerContentEnabled(
        server.id,
        folder,
        item.fileName,
        !item.enabled,
      );
      await load();
    } catch (e) {
      reportError("Could not toggle that file", e);
    }
  }

  async function remove(item: ContentItem) {
    try {
      await api.removeServerContent(server.id, folder, item.fileName);
      await load();
    } catch (e) {
      reportError(`Could not delete ${item.name}`, e);
    }
  }

  const both = Boolean(info?.takesPlugins && info?.takesMods);
  const takesNothing = Boolean(info && !info.takesPlugins && !info.takesMods);
  const noun = folder === "plugin" ? "plugins" : "mods";

  if (takesNothing) {
    return (
      <div className={styles.settings}>
        <p className={styles.pending}>
          A {info?.name} server loads neither plugins nor mods — it is the game
          and nothing else.
        </p>
      </div>
    );
  }

  return (
    <div className={styles.settings}>
      <div className={styles.contentHead}>
        {both && (
          <div className={styles.folders}>
            {(["plugin", "mod"] as const).map((option) => (
              <button
                key={option}
                className={clsx(
                  styles.tab,
                  folder === option && styles.tabActive,
                )}
                onClick={() => setFolder(option)}
              >
                {option === "plugin" ? "Plugins" : "Mods"}
              </button>
            ))}
          </div>
        )}
        <div style={{ flex: 1 }} />
        <button className="btn btn-sm btn-primary" onClick={() => onAdd(folder)}>
          <Plus size={13} />
          Add {noun}
        </button>
      </div>

      {items === null ? (
        <div className={styles.rows}>
          {Array.from({ length: 4 }, (_, i) => (
            <div key={i} className="skeleton" style={{ height: 46 }} />
          ))}
        </div>
      ) : items.length === 0 ? (
        <p className={styles.pending}>
          No {noun} installed. Anything you add lands in the server&apos;s{" "}
          <code>{folder === "plugin" ? "plugins" : "mods"}</code> folder.
        </p>
      ) : (
        <div className={styles.rows}>
          {items.map((item) => (
            <div
              key={item.fileName}
              className={clsx(styles.row, !item.enabled && styles.rowOff)}
            >
              <label className={styles.rowToggle}>
                <input
                  type="checkbox"
                  checked={item.enabled}
                  onChange={() => void toggle(item)}
                  aria-label={`${item.enabled ? "Disable" : "Enable"} ${item.name}`}
                />
              </label>

              <span className={styles.rowIcon}>
                {item.iconUrl ? (
                  <img src={item.iconUrl} alt="" loading="lazy" />
                ) : (
                  <Package size={15} />
                )}
              </span>

              <span className={styles.rowText}>
                <span className={styles.rowName}>{item.name}</span>
                <span className={styles.rowMeta}>
                  {item.version && <span>{item.version}</span>}
                  <span>{fileSize(item.size)}</span>
                  {item.fromDisk && <span>added by hand</span>}
                </span>
              </span>

              {item.provider && (
                <span className={clsx("badge", `badge-${item.provider}`)}>
                  {providerShort(item.provider)}
                </span>
              )}

              <button
                className={styles.rowRemove}
                onClick={() => void remove(item)}
                title={`Delete ${item.name}`}
                aria-label={`Delete ${item.name}`}
              >
                <Trash2 size={14} />
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * A server's backups: taking them, putting them back, and not keeping them
 * forever.
 */
function BackupsPane({ server }: { server: Server }) {
  const reportError = useStore((s) => s.reportError);
  const toast = useStore((s) => s.toast);
  const refreshServers = useStore((s) => s.refreshServers);

  const [items, setItems] = useState<BackupInfo[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState<BackupInfo | null>(null);
  const [keep, setKeep] = useState(server.settings.keepBackups);

  const load = useCallback(async () => {
    try {
      setItems(await api.listServerBackups(server.id));
    } catch (e) {
      reportError("Could not list backups", e);
      setItems([]);
    }
  }, [server.id, reportError]);

  useEffect(() => {
    void load();
  }, [load]);

  async function take() {
    setBusy(true);
    try {
      const info = await api.createServerBackup(server.id);
      await load();
      toast({
        kind: "success",
        title: "Backup taken",
        body: `${info.name} · ${fileSize(info.size)}`,
      });
    } catch (e) {
      reportError("Could not back up the server", e);
    } finally {
      setBusy(false);
    }
  }

  async function restore(info: BackupInfo) {
    setBusy(true);
    setConfirming(null);
    try {
      await api.restoreServerBackup(server.id, info.name);
      toast({ kind: "success", title: `Restored ${info.name}` });
    } catch (e) {
      reportError("Could not restore that backup", e);
    } finally {
      setBusy(false);
    }
  }

  async function remove(info: BackupInfo) {
    try {
      await api.deleteServerBackup(server.id, info.name);
      await load();
    } catch (e) {
      reportError("Could not delete that backup", e);
    }
  }

  async function saveRetention(value: number | null) {
    setKeep(value);
    try {
      await api.updateServer(server.id, {
        settings: { ...server.settings, keepBackups: value },
      });
      await refreshServers();
    } catch (e) {
      reportError("Could not save the retention setting", e);
    }
  }

  return (
    <div className={styles.settings}>
      <div className={styles.contentHead}>
        <label className={styles.retention}>
          Keep
          <input
            className={clsx("input", styles.keepInput)}
            type="number"
            min={1}
            max={999}
            value={keep ?? ""}
            placeholder="all"
            onChange={(e) =>
              void saveRetention(
                e.target.value ? Number(e.target.value) : null,
              )
            }
          />
          {keep === null ? "backups — every one is kept" : "newest backups"}
        </label>
        <div style={{ flex: 1 }} />
        <button
          className="btn btn-sm btn-primary"
          onClick={take}
          disabled={busy}
        >
          {busy ? <Loader2 size={13} className="spin" /> : <Plus size={13} />}
          Back up now
        </button>
      </div>

      {items === null ? (
        <div className={styles.rows}>
          {Array.from({ length: 3 }, (_, i) => (
            <div key={i} className="skeleton" style={{ height: 44 }} />
          ))}
        </div>
      ) : items.length === 0 ? (
        <p className={styles.pending}>
          No backups yet. A backup is a zip of everything the server needs to be
          itself — its world, configs and plugins — without the logs it writes
          afresh every run.
        </p>
      ) : (
        <div className={styles.rows}>
          {items.map((info) => (
            <div key={info.name} className={styles.row}>
              <span className={styles.rowText}>
                <span className={styles.rowName}>{info.name}</span>
                <span className={styles.rowMeta}>
                  <span>{relativeTime(info.createdAt)}</span>
                  <span>{fileSize(info.size)}</span>
                </span>
              </span>

              <button
                className="btn btn-sm"
                onClick={() => setConfirming(info)}
                disabled={busy || server.running}
                title={
                  server.running
                    ? "Stop the server before restoring"
                    : "Put these files back"
                }
              >
                Restore
              </button>
              <button
                className={styles.rowRemove}
                onClick={() => void remove(info)}
                title={`Delete ${info.name}`}
                aria-label={`Delete ${info.name}`}
              >
                <Trash2 size={14} />
              </button>
            </div>
          ))}
        </div>
      )}

      <SchedulesSection server={server} />

      {confirming && (
        <Modal
          title="Restore this backup?"
          subtitle={confirming.name}
          onClose={() => setConfirming(null)}
          width={460}
          footer={
            <>
              <button className="btn" onClick={() => setConfirming(null)}>
                Cancel
              </button>
              <button
                className="btn btn-danger"
                onClick={() => void restore(confirming)}
                disabled={busy}
              >
                Restore
              </button>
            </>
          }
        >
          <p className={styles.eula}>
            Every file in the backup is written over what is there now. Files
            added since — a plugin you installed today, say — are left alone
            rather than deleted, so this puts back rather than rewinds.
          </p>
        </Modal>
      )}
    </div>
  );
}

/** "every 360 minutes" is a number; "every 6 hours" is an interval. */
function everyLabel(minutes: number): string {
  if (minutes <= 0) return "never";
  if (minutes < 60) return `every ${minutes} minutes`;
  if (minutes % 1440 === 0) {
    const days = minutes / 1440;
    return days === 1 ? "every day" : `every ${days} days`;
  }
  if (minutes % 60 === 0) {
    const hours = minutes / 60;
    return hours === 1 ? "every hour" : `every ${hours} hours`;
  }
  return `every ${minutes} minutes`;
}

/**
 * Backups and commands the server runs on its own.
 *
 * Edited as a list and saved as a list: there are only ever a handful, and a
 * save per keystroke while typing a command would be absurd.
 */
function SchedulesSection({ server }: { server: Server }) {
  const reportError = useStore((s) => s.reportError);
  const refreshServers = useStore((s) => s.refreshServers);

  const [draft, setDraft] = useState<Schedule[]>(server.schedules ?? []);
  const [saving, setSaving] = useState(false);
  const dirty = JSON.stringify(draft) !== JSON.stringify(server.schedules ?? []);

  function add() {
    setDraft((was) => [
      ...was,
      {
        id: crypto.randomUUID(),
        action: { kind: "backup" },
        everyMinutes: 360,
        enabled: true,
        lastRun: null,
      },
    ]);
  }

  function update(id: string, patch: Partial<Schedule>) {
    setDraft((was) =>
      was.map((s) => (s.id === id ? { ...s, ...patch } : s)),
    );
  }

  async function save() {
    setSaving(true);
    try {
      await api.updateServer(server.id, { schedules: draft });
      await refreshServers();
    } catch (e) {
      reportError("Could not save schedules", e);
    } finally {
      setSaving(false);
    }
  }

  return (
    <section className={styles.section}>
      <div className={styles.contentHead}>
        <h2 className={styles.sectionTitle}>On a timer</h2>
        <div style={{ flex: 1 }} />
        {dirty && (
          <button
            className="btn btn-sm btn-primary"
            onClick={save}
            disabled={saving}
          >
            {saving && <Loader2 size={12} className="spin" />}
            Save
          </button>
        )}
        <button className="btn btn-sm" onClick={add}>
          <Plus size={13} />
          Add
        </button>
      </div>

      {draft.length === 0 ? (
        <p className={styles.pending}>
          Nothing scheduled. A server can back itself up on a timer, or run a
          command — a restart warning, a broadcast, whatever you would type at
          the console.
        </p>
      ) : (
        <div className={styles.rows}>
          {draft.map((schedule) => (
            <div key={schedule.id} className={styles.row}>
              <label className={styles.rowToggle}>
                <input
                  type="checkbox"
                  checked={schedule.enabled}
                  onChange={(e) =>
                    update(schedule.id, { enabled: e.target.checked })
                  }
                  aria-label="Enabled"
                />
              </label>

              <select
                className={clsx("select", styles.scheduleKind)}
                value={schedule.action.kind}
                onChange={(e) =>
                  update(schedule.id, {
                    action:
                      e.target.value === "backup"
                        ? { kind: "backup" }
                        : { kind: "command", value: "" },
                  })
                }
              >
                <option value="backup">Back up</option>
                <option value="command">Run command</option>
              </select>

              {schedule.action.kind === "command" && (
                <input
                  className={clsx("input", styles.scheduleCommand)}
                  value={schedule.action.value}
                  placeholder="say Restarting in 5 minutes"
                  spellCheck={false}
                  onChange={(e) =>
                    update(schedule.id, {
                      action: { kind: "command", value: e.target.value },
                    })
                  }
                />
              )}

              <span className={styles.scheduleEvery}>
                <input
                  className={clsx("input", styles.keepInput)}
                  type="number"
                  min={1}
                  value={schedule.everyMinutes}
                  onChange={(e) =>
                    update(schedule.id, {
                      everyMinutes: Number(e.target.value) || 0,
                    })
                  }
                />
                <span className={styles.rowMeta}>
                  {everyLabel(schedule.everyMinutes)}
                </span>
              </span>

              <button
                className={styles.rowRemove}
                onClick={() =>
                  setDraft((was) => was.filter((s) => s.id !== schedule.id))
                }
                title="Remove this schedule"
                aria-label="Remove this schedule"
              >
                <Trash2 size={14} />
              </button>
            </div>
          ))}
        </div>
      )}

      <p className={styles.pending}>
        A scheduled backup flushes the world first when the server is running. A
        scheduled command is skipped while it is stopped — there is no console
        to type at.
      </p>
    </section>
  );
}

function SettingsTab({ server }: { server: Server }) {
  const refreshServers = useStore((s) => s.refreshServers);
  const reportError = useStore((s) => s.reportError);
  const [draft, setDraft] = useState<ServerSettings>(server.settings);
  const [saving, setSaving] = useState(false);

  const [props, setProps] = useState<ServerProperty[] | null>(null);
  const [edited, setEdited] = useState<Record<string, string>>({});
  const [filter, setFilter] = useState("");

  useEffect(() => {
    api
      .readServerProperties(server.id)
      .then(setProps)
      .catch(() => setProps([]));
  }, [server.id]);

  const shown = useMemo(() => {
    const query = filter.trim().toLowerCase();
    return (props ?? []).filter(
      (p) => !query || p.key.toLowerCase().includes(query),
    );
  }, [props, filter]);

  const dirty = Object.keys(edited).length > 0;
  const valueOf = (p: ServerProperty) => edited[p.key] ?? p.value;
  const edit = (key: string, value: string) =>
    setEdited((was) => ({ ...was, [key]: value }));

  /**
   * Only the edited settings are sent. The backend leaves everything else in
   * the file exactly as it found it, comments and order included.
   */
  async function saveProperties() {
    setSaving(true);
    try {
      await api.writeServerProperties(
        server.id,
        Object.entries(edited).map(([key, value]) => ({ key, value })),
      );
      setProps(await api.readServerProperties(server.id));
      setEdited({});
    } catch (e) {
      reportError("Could not save server.properties", e);
    } finally {
      setSaving(false);
    }
  }

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
        <div className={styles.propsHead}>
          <h2 className={styles.sectionTitle}>server.properties</h2>
          {props !== null && props.length > 0 && (
            <input
              className={clsx("input", styles.propSearch)}
              placeholder="Filter settings"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          )}
        </div>

        {props === null ? (
          <div className="skeleton" style={{ height: 90 }} />
        ) : props.length === 0 ? (
          <p className={styles.pending}>
            This file appears the first time the server starts — it is written
            by the server, not by Geode. Start it once and its settings show up
            here.
          </p>
        ) : (
          <>
            <div className={styles.props}>
              {shown.map((p) => {
                const value = valueOf(p);
                const changed = p.key in edited;
                // The control follows the value the server wrote, so a setting
                // added by a plugin gets a sensible field without us knowing
                // anything about it.
                const bool = value === "true" || value === "false";
                const numeric = !bool && /^-?\d+$/.test(p.value);
                return (
                  <label
                    key={p.key}
                    className={clsx(styles.prop, changed && styles.propChanged)}
                  >
                    <span className={styles.propKey}>{p.key}</span>
                    {bool ? (
                      <input
                        type="checkbox"
                        className={styles.propCheck}
                        checked={value === "true"}
                        onChange={(e) =>
                          edit(p.key, e.target.checked ? "true" : "false")
                        }
                      />
                    ) : (
                      <input
                        className={clsx("input", styles.propValue)}
                        type={numeric ? "number" : "text"}
                        value={value}
                        onChange={(e) => edit(p.key, e.target.value)}
                      />
                    )}
                  </label>
                );
              })}
            </div>

            <div className={styles.propsFoot}>
              <span className={styles.pending}>
                {dirty
                  ? `${Object.keys(edited).length} changed. Restart the server to apply.`
                  : "Changes apply the next time the server starts."}
              </span>
              <button
                className="btn btn-sm btn-primary"
                onClick={saveProperties}
                disabled={!dirty || saving}
              >
                {saving && <Loader2 size={12} className="spin" />}
                Save properties
              </button>
            </div>
          </>
        )}
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

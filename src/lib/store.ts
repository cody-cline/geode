import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";

import { api, errorMessage } from "./api";
import { linkKind, nextJoin, nextPack, sentence } from "./share";
import type { JoinRequest, PackRequest } from "./share";
import { availableToast } from "./update";
import type {
  Account,
  Config,
  ExitEvent,
  Instance,
  LogEntry,
  Notice,
  ProgressEvent,
  Server,
  ServerExit,
  ServerLine,
  ServerStarted,
  ShareListenNotice,
  SharePackFailed,
  ShareStatus,
} from "./types";

export type Route =
  | { view: "library" }
  | { view: "browse" }
  | { view: "plans" }
  | { view: "plan"; id: string }
  | { view: "servers" }
  | { view: "server"; id: string; tab?: string }
  | { view: "settings"; tab?: string }
  | { view: "instance"; id: string; tab?: string };

export interface Toast {
  id: number;
  kind: "info" | "success" | "error";
  title: string;
  body?: string;
}

/** Keep the console bounded — a modded startup can emit tens of thousands of
 *  lines and unbounded retention is how a launcher ends up using a gigabyte. */
const MAX_LOG_LINES = 3000;

/** Two routes that would render the same screen. */
function sameRoute(a: Route, b: Route): boolean {
  if (a.view !== b.view) return false;
  if (a.view === "instance" && b.view === "instance") {
    return a.id === b.id && a.tab === b.tab;
  }
  if (a.view === "plan" && b.view === "plan") return a.id === b.id;
  if (a.view === "server" && b.view === "server") {
    return a.id === b.id && a.tab === b.tab;
  }
  if (a.view === "settings" && b.view === "settings") return a.tab === b.tab;
  return true;
}

/** A session's worth of screens, not a lifetime's. */
const MAX_HISTORY = 50;

interface State {
  route: Route;
  /** Where back goes, oldest first. */
  history: Route[];
  navigate: (route: Route) => void;
  goBack: () => void;
  /**
   * Overlays that want back for themselves, outermost first. Registered
   * through `useBackHandler` rather than touched directly.
   */
  backHandlers: { id: number; run: () => void }[];
  pushBackHandler: (id: number, run: () => void) => void;
  popBackHandler: (id: number) => void;

  config: Config | null;
  loadConfig: () => Promise<void>;
  patchConfig: (patch: Record<string, unknown>) => Promise<void>;

  instances: Instance[];
  instancesLoaded: boolean;
  refreshInstances: () => Promise<void>;

  servers: Server[];
  serversLoaded: boolean;
  refreshServers: () => Promise<void>;
  /** Console output per server, newest last. */
  serverLogs: Record<string, ServerLine[]>;
  clearServerLog: (serverId: string) => void;

  accounts: Account[];
  refreshAccounts: () => Promise<void>;
  activeAccount: () => Account | undefined;

  progress: Record<string, ProgressEvent>;
  logs: Record<string, LogEntry[]>;
  clearLogs: (instanceId: string) => void;

  toasts: Toast[];
  toast: (t: Omit<Toast, "id">) => void;
  dismissToast: (id: number) => void;
  reportError: (title: string, e: unknown) => void;

  /**
   * The pack server and the router, shared by every shared server — so kept
   * here, where the one event that updates it has one subscriber.
   */
  shareStatus: ShareStatus | null;
  loadShareStatus: () => Promise<void>;
  setShareStatus: (status: ShareStatus) => void;

  /** The join dialog, which the app root renders. At most one, ever. */
  join: JoinRequest | null;
  openJoin: (link?: string | null) => void;
  closeJoin: () => void;

  /** The pack dialog, the same way. A link opens one or the other, never both. */
  pack: PackRequest | null;
  closePack: () => void;
  /** Dispatch a `geode://` link to whichever dialog it belongs to. */
  openLink: (raw: string) => void;

  init: () => Promise<void>;
}

let toastSeq = 0;
let joinSeq = 0;
let packSeq = 0;

/**
 * Whether the one-time setup has run.
 *
 * Module scope rather than store state on purpose: it has to survive the store
 * being read before it is written, and it is not something any view should see.
 */
let started = false;

export const useStore = create<State>((set, get) => ({
  route: { view: "library" },
  history: [],
  navigate: (route) =>
    set((s) =>
      sameRoute(s.route, route)
        ? {}
        : { route, history: [...s.history, s.route].slice(-MAX_HISTORY) },
    ),

  backHandlers: [],
  pushBackHandler: (id, run) =>
    set((s) => ({ backHandlers: [...s.backHandlers, { id, run }] })),
  popBackHandler: (id) =>
    set((s) => ({ backHandlers: s.backHandlers.filter((h) => h.id !== id) })),

  goBack: () => {
    // Whatever is open on top goes first. It removes its own handler as it
    // closes, so there is nothing to pop here.
    const open = get().backHandlers;
    const innermost = open[open.length - 1];
    if (innermost) {
      innermost.run();
      return;
    }
    set((s) => {
      if (s.history.length === 0) return {};
      const history = [...s.history];
      const route = history.pop()!;
      return { route, history };
    });
  },

  config: null,
  loadConfig: async () => {
    const config = await api.getConfig();
    set({ config });
    applyTheme(config);
  },
  patchConfig: async (patch) => {
    const config = await api.setConfig(patch);
    set({ config });
    applyTheme(config);
  },

  instances: [],
  instancesLoaded: false,
  refreshInstances: async () => {
    try {
      set({ instances: await api.listInstances(), instancesLoaded: true });
    } catch (e) {
      get().reportError("Could not load instances", e);
      set({ instancesLoaded: true });
    }
  },

  servers: [],
  serversLoaded: false,
  refreshServers: async () => {
    try {
      set({ servers: await api.listServers(), serversLoaded: true });
    } catch (e) {
      get().reportError("Could not load servers", e);
      set({ serversLoaded: true });
    }
  },
  serverLogs: {},
  clearServerLog: (serverId) =>
    set((s) => ({ serverLogs: { ...s.serverLogs, [serverId]: [] } })),

  accounts: [],
  refreshAccounts: async () => {
    try {
      set({ accounts: await api.listAccounts() });
    } catch (e) {
      get().reportError("Could not load accounts", e);
    }
  },
  activeAccount: () => get().accounts.find((a) => a.active),

  progress: {},
  logs: {},
  clearLogs: (instanceId) =>
    set((s) => ({ logs: { ...s.logs, [instanceId]: [] } })),

  toasts: [],
  toast: (t) => {
    const id = ++toastSeq;
    set((s) => ({ toasts: [...s.toasts, { ...t, id }] }));
    const ttl = t.kind === "error" ? 9000 : 4500;
    setTimeout(() => get().dismissToast(id), ttl);
  },
  dismissToast: (id) =>
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
  reportError: (title, e) =>
    get().toast({ kind: "error", title, body: errorMessage(e) }),

  shareStatus: null,
  loadShareStatus: async () => {
    set({ shareStatus: await api.getShareStatus() });
  },
  setShareStatus: (shareStatus) => set({ shareStatus }),

  join: null,
  openJoin: (link = null) => {
    const current = get().join;
    const join = nextJoin(current, link, joinSeq + 1);
    if (join === current) return;
    joinSeq = join.seq;
    set({ join });
  },
  closeJoin: () => set({ join: null }),

  pack: null,
  closePack: () => set({ pack: null }),
  // The one place a link chooses its dialog. `linkKind` reads only the host;
  // the real parsing is strict and happens in Rust, on the way in and again
  // on anything that comes back out of the window.
  openLink: (raw) => {
    if (linkKind(raw) !== "pack") {
      get().openJoin(raw);
      return;
    }
    const current = get().pack;
    const pack = nextPack(current, raw, packSeq + 1);
    if (pack === current) return;
    packSeq = pack.seq;
    // A pack link replaces a join dialog rather than stacking over it: the
    // user clicked one link, so one thing should be asking them about it.
    set({ pack, join: null });
  },

  init: async () => {
    // React's StrictMode mounts, unmounts and mounts again in development, so
    // this runs twice. Every `listen` below is registered for the lifetime of
    // the process and never torn down, so without this guard each event has two
    // subscribers — which is why every console line appeared twice.
    if (started) return;
    started = true;

    await Promise.all([
      get().loadConfig().catch((e) => get().reportError("Could not load settings", e)),
      get().refreshInstances(),
      get().refreshServers(),
      get().refreshAccounts(),
      // The server lists badge a share whose address has moved, so they need
      // the address now without waiting for a Share tab to be opened: the
      // backend announces it when it changes, and a change that happened while
      // the app was shut is announced to nobody. Quiet on failure — the Share
      // tab asks again and has somewhere to say so.
      get().loadShareStatus().catch(() => {}),
    ]);

    // Once, at start, and quietly. A check that cannot reach GitHub says
    // nothing — nobody asked for it, and a launcher that complains about its
    // own update server on every cold start is a launcher people learn to
    // ignore. A version that is waiting is mentioned and never installed:
    // Settings → About holds Install, Not now and Skip this one, and is
    // where somebody looking for their version number already is. A build
    // with no key gets the same mention and a different sentence, which is
    // `availableToast`'s to write.
    api
      .checkForUpdate()
      .then((status) => {
        if (!status.available) return;
        get().toast({ kind: "info", ...availableToast(status) });
      })
      .catch(() => {});

    await listen<ProgressEvent>("geode://progress", ({ payload }) => {
      set((s) => ({ progress: { ...s.progress, [payload.id]: payload } }));
      if (payload.done) {
        // Leave a finished bar on screen briefly so the completion registers.
        setTimeout(() => {
          set((s) => {
            const next = { ...s.progress };
            if (next[payload.id]?.done) delete next[payload.id];
            return { progress: next };
          });
        }, payload.error ? 6000 : 1200);
      }
    });

    // A job's own result says whether it worked; this is for what the user has
    // to be told along the way, and it is told once per thing that happened.
    // The sentence is written in the core because three install shapes owe the
    // same one, so there is nothing to phrase here.
    await listen<Notice>("geode://notice", ({ payload }) => {
      get().toast({
        kind: "info",
        title: payload.title,
        body: payload.body,
      });
    });

    await listen<LogEntry>("geode://log", ({ payload }) => {
      set((s) => {
        const existing = s.logs[payload.instanceId] ?? [];
        const next =
          existing.length >= MAX_LOG_LINES
            ? [...existing.slice(existing.length - MAX_LOG_LINES + 1), payload]
            : [...existing, payload];
        return { logs: { ...s.logs, [payload.instanceId]: next } };
      });
    });

    // A server's console is plain text on two streams, unlike the game's
    // structured log, so it is kept as lines and rendered as a terminal.
    await listen<ServerLine>("geode://server-log", ({ payload }) => {
      set((s) => {
        const existing = s.serverLogs[payload.serverId] ?? [];
        const next =
          existing.length >= MAX_LOG_LINES
            ? [...existing.slice(existing.length - MAX_LOG_LINES + 1), payload]
            : [...existing, payload];
        return { serverLogs: { ...s.serverLogs, [payload.serverId]: next } };
      });
    });

    await listen<ServerExit>("geode://server-exit", ({ payload }) => {
      get().refreshServers();
      const name =
        get().servers.find((s) => s.id === payload.serverId)?.name ??
        payload.serverId;
      // An exit Geode is about to answer is a different thing to be told from
      // one it is leaving alone, and giving up is a third.
      if (payload.gaveUp) {
        get().toast({
          kind: "error",
          title: `${name} will not stay up`,
          body: `Geode started it again ${payload.restarts} times and it exited every time, so it has stopped restarting it. The console says how each try ended.`,
        });
        return;
      }
      if (payload.restartInSeconds !== null) {
        get().toast({
          kind: "info",
          title: `${name} exited — starting it again`,
          body: `Ran for ${formatDuration(payload.ranSeconds)}. Restart ${payload.restarts}, in ${payload.restartInSeconds}s.`,
        });
        return;
      }
      get().toast({
        kind: payload.success ? "info" : "error",
        title: payload.success ? `${name} stopped` : `${name} exited`,
        body: payload.success
          ? `Ran for ${formatDuration(payload.ranSeconds)}.`
          : payload.code === null
            ? "The process was stopped."
            : `Exit code ${payload.code}. Check the console for the cause.`,
      });
    });

    // Whoever started it. A server Geode started again on its own would
    // otherwise still look stopped on every screen, with its Start button and
    // its players list talking about a process that is long gone.
    await listen<ServerStarted>("geode://server-started", ({ payload }) => {
      get().refreshServers();
      if (payload.restarts === 0) return;
      const name =
        get().servers.find((s) => s.id === payload.serverId)?.name ??
        payload.serverId;
      get().toast({
        kind: "success",
        title: `${name} is running again`,
        body:
          payload.restarts === 1
            ? "It exited without being asked to, so Geode started it again."
            : `It exited without being asked to again — this is restart ${payload.restarts}.`,
      });
    });

    await listen<ShareStatus>("geode://share-status", ({ payload }) => {
      set({ shareStatus: payload });
    });

    // Sent once when retrying has stopped being the answer, and once when it
    // turns out to have been: the Share tab carries the running state, and
    // this is what reaches an admin who is not looking at it.
    await listen<ShareListenNotice>("geode://share-listen", ({ payload }) => {
      get().toast(
        payload.listening
          ? {
              kind: "success",
              title: `Sharing is listening again on port ${payload.port}`,
              body: "Players can download packs from this computer again. The links you sent still work.",
            }
          : {
              kind: "error",
              title: `Sharing cannot listen on port ${payload.port}`,
              body: `${sentence(payload.error ?? "The port could not be opened")} Geode keeps trying, but this needs you — a server's Share tab says what to do. Do not change the port unless you can send everyone a new link.`,
            },
      );
    });

    // Only the admin can put a pack that will not build right, and the player
    // who asked for it was told nothing but to ask them.
    await listen<SharePackFailed>("geode://share-pack-failed", ({ payload }) => {
      get().toast({
        kind: "error",
        title: `${payload.serverName} could not build its pack`,
        body: `${sentence(payload.error)} A player was told to ask you to check Geode.`,
      });
    });

    // A link opened while Geode runs arrives as an event; one that started
    // Geode came before any window could hear it and is parked instead. Listen
    // first and take second, so a link landing between the two is caught by
    // one or the other — and if by both, `openJoin` sees the same link twice
    // and opens one dialog.
    await listen<string>("geode://join-link", ({ payload }) => {
      get().openLink(payload);
      // Clear any parked copy too, so a reload of the window does not find
      // this link waiting and offer the same join again.
      api.takePendingJoin().catch(() => {});
    });
    try {
      const pending = await api.takePendingJoin();
      if (pending) get().openLink(pending);
    } catch (e) {
      get().reportError("Could not read the link Geode was opened with", e);
    }

    await listen<ExitEvent>("geode://exit", ({ payload }) => {
      get().refreshInstances();
      const name =
        get().instances.find((i) => i.id === payload.instanceId)?.name ??
        payload.instanceId;
      if (payload.success) {
        get().toast({
          kind: "info",
          title: `${name} closed`,
          body: `Played for ${formatDuration(payload.playedSeconds)}.`,
        });
      } else {
        get().toast({
          kind: "error",
          title: `${name} crashed`,
          body:
            payload.code === null
              ? "The game was stopped."
              : `Exit code ${payload.code}. Check the Logs tab for the cause.`,
        });
      }
    });
  },
}));

export function applyTheme(config: Config) {
  const root = document.documentElement;
  root.dataset.theme = config.theme === "light" ? "light" : "dark";
  root.style.setProperty("--accent", config.accent);
  // Pick readable ink for buttons filled with the accent colour.
  root.style.setProperty("--accent-ink", contrastInk(config.accent));
}

/** Relative luminance test, so a pale accent gets dark text and vice versa. */
function contrastInk(hex: string): string {
  const m = /^#?([\da-f]{6})$/i.exec(hex.trim());
  if (!m) return "#04140a";
  const int = parseInt(m[1], 16);
  const [r, g, b] = [(int >> 16) & 255, (int >> 8) & 255, int & 255].map((c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  });
  const luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
  return luminance > 0.45 ? "#0b0f14" : "#ffffff";
}

export function formatDuration(seconds: number): string {
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const rem = minutes % 60;
  return rem ? `${hours}h ${rem}m` : `${hours}h`;
}

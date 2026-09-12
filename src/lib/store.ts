import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";

import { api, errorMessage } from "./api";
import type {
  Account,
  Config,
  ExitEvent,
  Instance,
  LogEntry,
  ProgressEvent,
  Server,
  ServerExit,
  ServerLine,
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

  init: () => Promise<void>;
}

let toastSeq = 0;

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

  init: async () => {
    await Promise.all([
      get().loadConfig().catch((e) => get().reportError("Could not load settings", e)),
      get().refreshInstances(),
      get().refreshServers(),
      get().refreshAccounts(),
    ]);

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

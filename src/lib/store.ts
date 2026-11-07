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

/** What the offline-play dialog needs to ask its question. */
export interface OfflinePlayRequest {
  seq: number;
  instanceId: string;
  instanceName: string;
  /** The account's real name, which is also the name it would play under. */
  username: string;
  /** Why the sign-in did not work, in Geode's own words. */
  reason: string;
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
   * Things that want back for themselves, outermost first. Registered through
   * `useBackHandler` rather than touched directly. `overlay` marks the ones
   * drawn above the whole window rather than in place of a screen — see that
   * hook for why the difference is worth carrying.
   */
  backHandlers: { id: number; run: () => void; overlay: boolean }[];
  pushBackHandler: (
    id: number,
    run: () => void,
    kind: "overlay" | "inline",
  ) => void;
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

  /**
   * The offer to play offline, raised by a launch that could not sign in.
   *
   * Here rather than in a view because Play is on four screens, and a dialog
   * because this is a choice: nothing plays offline unless the player says so.
   */
  offlinePlay: OfflinePlayRequest | null;
  offerOfflinePlay: (offer: Omit<OfflinePlayRequest, "seq">) => void;
  closeOfflinePlay: () => void;

  init: () => Promise<void>;
}

let toastSeq = 0;
let joinSeq = 0;
let packSeq = 0;
let offlinePlaySeq = 0;

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
  pushBackHandler: (id, run, kind) =>
    set((s) => ({
      backHandlers: [...s.backHandlers, { id, run, overlay: kind === "overlay" }],
    })),
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

  offlinePlay: null,
  offerOfflinePlay: (offer) =>
    set({ offlinePlay: { ...offer, seq: ++offlinePlaySeq } }),
  closeOfflinePlay: () => set({ offlinePlay: null }),

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
      // Whether anybody asked, not whether the code was zero. A server that
      // could not bind its port exits 0 within seconds, and reading that as
      // success told the admin their server had "stopped" when it had never
      // started.
      if (payload.askedFor) {
        get().toast({
          kind: "info",
          title: `${name} stopped`,
          body: `Ran for ${formatDuration(payload.ranSeconds)}.`,
        });
        return;
      }
      get().toast({
        kind: "error",
        title:
          payload.ranSeconds < 30
            ? `${name} did not start`
            : `${name} exited on its own`,
        body:
          payload.code === null
            ? "The process ended without saying why. The console has its last words."
            : `Exit code ${payload.code} after ${formatDuration(payload.ranSeconds)}. The console says why.`,
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
  const light = config.theme === "light";
  root.dataset.theme = light ? "light" : "dark";
  root.style.setProperty("--accent", config.accent);
  // Pick readable ink for buttons filled with the accent colour.
  root.style.setProperty("--accent-ink", contrastInk(config.accent));
  // And the mirror-image question: the accent used *as* text, on this theme's
  // paper rather than on itself.
  root.style.setProperty("--accent-text", accentText(config.accent, light));
  // Quiet text that lands on a tint of the accent rather than on a surface.
  root.style.setProperty("--text-on-tint", textOnTint(config.accent, light));
}

/** Every background this theme writes `--accent-text` on, lightest-vs-darkest. */
const SURFACES = {
  dark: ["#0b0f14", "#11171f", "#161d27", "#1c2531"],
  light: ["#f5f7fa", "#ffffff", "#f0f3f7", "#e6ebf1"],
};

function channels(hex: string): [number, number, number] | null {
  const m = /^#?([\da-f]{6})$/i.exec(hex.trim());
  if (!m) return null;
  const int = parseInt(m[1], 16);
  return [(int >> 16) & 255, (int >> 8) & 255, int & 255];
}

function relativeLuminance([r, g, b]: [number, number, number]): number {
  const [lr, lg, lb] = [r, g, b].map((c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
}

function ratio(a: [number, number, number], b: [number, number, number]): number {
  const [hi, lo] = [relativeLuminance(a), relativeLuminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * The accent as a colour to be *read in*, which is not the same question as
 * the accent as a colour to paint with.
 *
 * `--accent` is a fill: a button, a bar, a tint behind a badge, and it is
 * whatever the user picked. Used as text it has to hold its own against the
 * page instead, and the default green that reads at 10:1 on this app's near
 * black reads at 1.7:1 on its white — a word nobody can make out. So the
 * accent is walked toward the paper's opposite until the worst background it
 * lands on clears 4.5:1, and the result is published as its own property.
 *
 * Its own tint counts as a background: a badge paints that tint from
 * `--accent` and then writes this on top, so the two are measured together.
 *
 * Twenty steps, then give up and answer with the theme's body colour. Giving
 * up has to mean *legible*, never *nearly the accent*: an accent this cannot
 * satisfy is one no amount of nudging will save, and ordinary text is the one
 * answer that is always readable. `contrastInk` gives up the same way and
 * says why.
 */
function accentText(hex: string, light: boolean): string {
  const accent = channels(hex);
  if (!accent) return light ? "#131a22" : "#e6edf3";
  const paper = channels(light ? "#ffffff" : "#11171f")!;
  const against = [
    ...SURFACES[light ? "light" : "dark"].map((s) => channels(s)!),
    tint(accent, paper, 0.14),
  ];
  return walk(accent, light, against);
}

/**
 * Quiet text sitting on a tint of the accent instead of on a surface — the
 * sidebar's shortcut hint on whichever row is current, and nothing else today.
 *
 * Derived rather than written down for the reason the tint itself is: its
 * background is a percentage of a colour the user chose, and the picker takes
 * any colour at all. A fixed grey can be made to work for the eight swatches
 * and still fail on a white accent, where 18% over this theme's surface lifts
 * the row far enough that the only greys that clear it are lighter than
 * `--text-dim` — which would print the quiet tier louder than the ordinary
 * one. So it starts from `--text-faint` and moves only as far as it must.
 */
function textOnTint(hex: string, light: boolean): string {
  const accent = channels(hex);
  const faint = channels(light ? "#5e6974" : "#8795a5")!;
  if (!accent) return `#${faint.map((c) => c.toString(16).padStart(2, "0")).join("")}`;
  const paper = channels(light ? "#ffffff" : "#11171f")!;
  // Both the resting tint and the hover one, so pointing at the row does not
  // undo it.
  const against = [tint(accent, paper, 0.13), tint(accent, paper, 0.18)];
  return walk(faint, light, against);
}

/** `pct` of `fg` painted over `bg`, which is what `color-mix` leaves behind. */
function tint(
  fg: [number, number, number],
  bg: [number, number, number],
  pct: number,
): [number, number, number] {
  return fg.map((c, i) => Math.round(c * pct + bg[i] * (1 - pct))) as [
    number,
    number,
    number,
  ];
}

/**
 * Move a colour toward this theme's far end until it clears 4.5:1 on every
 * background it has to sit on.
 *
 * Twenty steps, then give up and answer with the theme's body colour. Giving
 * up has to mean *legible*, never *nearly the colour asked for*: a colour this
 * cannot satisfy is one no amount of nudging will save, and ordinary text is
 * the one answer that is always readable. `contrastInk` gives up the same way
 * and says why.
 */
function walk(
  from: [number, number, number],
  light: boolean,
  against: [number, number, number][],
): string {
  for (let step = 0; step <= 20; step++) {
    const t = step / 20;
    const moved = from.map((c) =>
      Math.round(light ? c * (1 - t) : c + (255 - c) * t),
    ) as [number, number, number];
    if (against.every((s) => ratio(moved, s) >= 4.5)) {
      return `#${moved.map((c) => c.toString(16).padStart(2, "0")).join("")}`;
    }
  }
  return light ? "#131a22" : "#e6edf3";
}

/** Relative luminance test, so a pale accent gets dark text and vice versa. */
function contrastInk(hex: string): string {
  const m = /^#?([\da-f]{6})$/i.exec(hex.trim());
  // White, not the near-black this used to return. An unparseable accent is
  // something that got past `config.rs` into `--accent`, and nothing here
  // knows what colour the browser made of it — but every accent this app has
  // ever shipped is dark enough to read white on, and dark ink on an unknown
  // colour is the one guess that can disappear entirely.
  if (!m) return "#ffffff";
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

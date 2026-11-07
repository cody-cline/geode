import { useCallback, useEffect, useState } from "react";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import {
  AlertTriangle,
  Check,
  Coffee,
  Download,
  ExternalLink,
  FolderOpen,
  Info,
  Loader2,
  Palette,
  Pipette,
  RefreshCw,
  Server as ServerIcon,
  Share2,
  Trash2,
  Unplug,
  UserRound,
} from "lucide-react";

import AccountAvatar from "../components/AccountAvatar";
import AccountsModal from "../components/AccountsModal";
import { api, errorMessage } from "../lib/api";
import { radioGroupKey } from "../lib/radioGroup";
import { DEFAULT_SHARE_PORT, parsePort } from "../lib/share";
import { RELEASES_URL, panelHint, panelLine } from "../lib/update";
import { useStore } from "../lib/store";
import StatusNote from "../components/StatusNote";
import type {
  AppInfo,
  RemoteHost,
  JavaInstall,
  ShareSettings,
  UpdateStatus,
} from "../lib/types";

import styles from "./Settings.module.css";

const TABS = [
  { id: "appearance", label: "Appearance", icon: Palette },
  { id: "java", label: "Java & Memory", icon: Coffee },
  { id: "accounts", label: "Accounts", icon: UserRound },
  { id: "sharing", label: "Server sharing", icon: Share2 },
  { id: "hosts", label: "Server hosts", icon: ServerIcon },
  { id: "about", label: "About", icon: Info },
] as const;

type TabId = (typeof TABS)[number]["id"];

const THEMES = ["dark", "light"] as const;

/**
 * The eight accent presets, each with the word for it.
 *
 * The word is the whole point: a swatch named `Accent #4ade80` is read out as
 * nine characters of hex, which tells nobody which colour they are on when the
 * colour is the only thing the control is for. Plain words rather than the
 * names a palette would use — somebody choosing an accent is picking green,
 * not emerald-400. The hex stays on the `title` for anyone who wants it.
 *
 * Paired here rather than looked up from a table beside the list, the way
 * `LOADER_BLURB` is a `Record` over the loaders: a swatch added with no word
 * for it should fail the typecheck, not render as hex again.
 */
const ACCENTS = [
  { hex: "#4ade80", name: "Green" },
  { hex: "#38bdf8", name: "Blue" },
  { hex: "#a78bfa", name: "Violet" },
  { hex: "#f472b6", name: "Pink" },
  { hex: "#fb923c", name: "Orange" },
  { hex: "#facc15", name: "Yellow" },
  { hex: "#f87171", name: "Red" },
  { hex: "#2dd4bf", name: "Teal" },
] as const;

/**
 * A `#rrggbb` to compare against `ACCENTS` and to hand `<input type="color">`.
 *
 * Normally `config.accent` already is one — every swatch and the picker
 * produce hex, and `config.rs` now refuses anything else on the way in. A
 * `config.json` edited by hand can still hold `red`, and then the comparison
 * below matched no swatch at all while the picker, which coerces what it
 * cannot parse to black, drew a void on the end of the row. `ACCENTS[0]` is
 * the built-in default, same value as `default_accent()` in Rust.
 */
function hexAccent(raw: string): string {
  const hex = raw.trim().toLowerCase();
  return /^#[\da-f]{6}$/.test(hex) ? hex : ACCENTS[0].hex;
}

export default function Settings() {
  const route = useStore((s) => s.route);
  const config = useStore((s) => s.config);
  const patchConfig = useStore((s) => s.patchConfig);
  const reportError = useStore((s) => s.reportError);

  const [tab, setTab] = useState<TabId>(
    route.view === "settings" && TABS.some((t) => t.id === route.tab)
      ? (route.tab as TabId)
      : "appearance",
  );

  if (!config) {
    return (
      <div className={styles.loading}>
        <Loader2 size={20} className="spin" />
      </div>
    );
  }

  const update = (patch: Record<string, unknown>) =>
    patchConfig(patch).catch((e) => reportError("Could not save settings", e));

  return (
    <>
      <header className={styles.header}>
        <h1 className={styles.heading}>Settings</h1>
      </header>

      <div className={styles.body}>
        <nav className={styles.nav}>
          {TABS.map((t) => (
            <button
              key={t.id}
              className={clsx(styles.navItem, tab === t.id && styles.navActive)}
              onClick={() => setTab(t.id)}
            >
              <t.icon size={15} />
              {t.label}
            </button>
          ))}
        </nav>

        <div className={styles.content}>
          {tab === "appearance" && <Appearance update={update} />}
          {tab === "java" && <JavaSettings update={update} />}
          {tab === "accounts" && <AccountsSettings />}
          {tab === "sharing" && <SharingSettings />}
          {tab === "hosts" && <HostSettings />}
          {tab === "about" && <About />}
        </div>
      </div>
    </>
  );
}

// Returns the save, so a panel that has to read something back after it can
// wait for it. Handlers that only fire and forget ignore the promise.
type Updater = (patch: Record<string, unknown>) => Promise<void>;

function Appearance({ update }: { update: Updater }) {
  const config = useStore((s) => s.config)!;
  const current = hexAccent(config.accent);
  /** The accent is one nobody offered, so the picker is the control holding it. */
  const custom = !ACCENTS.some((a) => a.hex === current);

  // A radio group has exactly one tab stop, and it is the chosen option — or
  // the first one when nothing in the group is chosen, which is the accent
  // swatches whenever the colour in use came from the picker beside them, and
  // the theme row if `config.json` were hand-edited to a third value. Without
  // the fallback neither group can be reached by Tab at all.
  const themeStop = THEMES.find((t) => t === config.theme) ?? THEMES[0];
  const accentStop = ACCENTS.find((a) => a.hex === current)?.hex ?? ACCENTS[0].hex;

  return (
    <div className={styles.form}>
      <p className={styles.saveModel}>
        Everything on this screen is saved the moment you change it. There is
        no Save button and nothing to lose by leaving.
      </p>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle} id="theme-label">
          Theme
        </h2>
        {/* One of two, so a radio group: the tick said which was in force to
            anyone who could see it and nothing said it to anyone who could
            not. Named from the heading above it. */}
        <div
          className={styles.themeRow}
          role="radiogroup"
          aria-labelledby="theme-label"
          onKeyDown={(e) => {
            const next = radioGroupKey(e, THEMES);
            if (next) void update({ theme: next });
          }}
        >
          {THEMES.map((theme) => (
            <button
              key={theme}
              className={clsx(
                styles.themeCard,
                config.theme === theme && styles.themeActive,
              )}
              onClick={() => update({ theme })}
              role="radio"
              aria-checked={config.theme === theme}
              tabIndex={theme === themeStop ? 0 : -1}
              data-radio={theme}
            >
              <span
                className={styles.themeSwatch}
                data-theme={theme}
                aria-hidden="true"
              />
              {theme === "dark" ? "Dark" : "Light"}
              {config.theme === theme && <Check size={14} />}
            </button>
          ))}
        </div>
      </section>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle} id="accent-label">
          Accent colour
        </h2>
        <div className={styles.accents}>
          {/* The eight presets are a radio group and the picker on the end is
              not one of them, so the group is drawn around the eight alone.
              When the colour in use is the picker's, none of the eight is
              checked — which is what is true, and is why the tab stop above
              has a fallback. */}
          <div
            className={styles.accentPresets}
            role="radiogroup"
            aria-labelledby="accent-label"
            onKeyDown={(e) => {
              const next = radioGroupKey(
                e,
                ACCENTS.map((a) => a.hex),
              );
              if (next) void update({ accent: next });
            }}
          >
            {ACCENTS.map((accent) => (
              <button
                key={accent.hex}
                className={clsx(
                  styles.accent,
                  current === accent.hex && styles.accentActive,
                )}
                style={{ background: accent.hex }}
                onClick={() => update({ accent: accent.hex })}
                role="radio"
                aria-checked={current === accent.hex}
                tabIndex={accent.hex === accentStop ? 0 : -1}
                data-radio={accent.hex}
                aria-label={accent.name}
                title={`${accent.name} · ${accent.hex}`}
              >
                {current === accent.hex && <Check size={14} />}
              </button>
            ))}
          </div>
          {/* The ninth control in the row was the colour input itself, drawn
              as a swatch of whatever accent was in use — which is to say a
              tenth green whenever that was the default one, with nothing on
              it to say it did something the other eight do not. It says so
              now, and shows a colour only when the colour is one no preset
              offers. */}
          <label
            className={clsx(styles.accentCustom, custom && styles.accentCustomOn)}
            title="Pick any colour"
          >
            {custom ? (
              <span
                className={styles.accentDot}
                style={{ background: current }}
                aria-hidden="true"
              />
            ) : (
              <Pipette size={13} />
            )}
            Your own
            {/* Named with the words on the tile inside the name, because
                "Custom accent colour" was a name with none of them in it:
                somebody driving this by voice says what they can read. */}
            <input
              type="color"
              className={styles.accentInput}
              value={current}
              onChange={(e) => update({ accent: e.target.value })}
              aria-label="Your own accent colour"
            />
          </label>
        </div>
      </section>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Behaviour</h2>
        <label className={styles.toggleRow}>
          <input
            type="checkbox"
            checked={config.hideOnLaunch}
            onChange={(e) => update({ hideOnLaunch: e.target.checked })}
          />
          <span>
            <strong>Hide the launcher while playing</strong>
            <span className={styles.toggleHint}>
              The window reappears when you close the game.
            </span>
          </span>
        </label>
        <label className={styles.toggleRow}>
          <input
            type="checkbox"
            checked={config.showSnapshots}
            onChange={(e) => update({ showSnapshots: e.target.checked })}
          />
          <span>
            <strong>Show snapshots by default</strong>
            <span className={styles.toggleHint}>
              Includes pre-releases and release candidates in version pickers.
            </span>
          </span>
        </label>
        <label className={styles.toggleRow}>
          <input
            type="checkbox"
            checked={config.closeToTray}
            onChange={(e) => update({ closeToTray: e.target.checked })}
          />
          <span>
            <strong>Keep Geode in the tray when the window closes</strong>
            <span className={styles.toggleHint}>
              Closing the window leaves Geode in the tray with your servers and
              games still running, and the icon&rsquo;s menu brings it back or exits
              and stops them. A desktop with no tray cannot do that, so there
              closing exits instead &mdash; which is what turning this off does
              everywhere.
            </span>
          </span>
        </label>
      </section>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Downloads</h2>
        <div className={styles.field}>
          <label className="label" htmlFor="concurrency">
            Parallel downloads
          </label>
          <input
            id="concurrency"
            className="input"
            style={{ width: 110 }}
            type="number"
            min={1}
            max={64}
            value={config.concurrentDownloads}
            onChange={(e) =>
              update({ concurrentDownloads: Number(e.target.value) })
            }
          />
          <p className="hint">
            Higher is faster on good connections. Drop it if downloads keep
            failing.
          </p>
        </div>
      </section>
    </div>
  );
}

function JavaSettings({ update }: { update: Updater }) {
  const config = useStore((s) => s.config)!;
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  const [installs, setInstalls] = useState<JavaInstall[] | null>(null);
  const [systemMemory, setSystemMemory] = useState<number | null>(null);
  const [installing, setInstalling] = useState<number | null>(null);
  const [args, setArgs] = useState(config.javaArgs);

  const refresh = () =>
    api
      .detectJava()
      .then(setInstalls)
      .catch(() => setInstalls([]));

  useEffect(() => {
    void refresh();
    api.systemMemoryMb().then(setSystemMemory).catch(() => {});
  }, []);

  async function install(major: number) {
    setInstalling(major);
    try {
      const installed = await api.installJava(major);
      await refresh();
      toast({
        kind: "success",
        title: `Installed Java ${installed.major}`,
        body: installed.version,
      });
    } catch (e) {
      reportError(`Could not install Java ${major}`, e);
    } finally {
      setInstalling(null);
    }
  }

  // The majors Minecraft actually asks for across its supported range.
  const wanted = [8, 17, 21];
  const have = new Set(installs?.map((i) => i.major));

  return (
    <div className={styles.form}>
      <p className={styles.saveModel}>
        Everything on this screen is saved the moment you change it — a box you
        are typing in, when you click away from it. There is no Save button.
      </p>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Default memory</h2>
        <div className={styles.memoryRow}>
          <input
            type="range"
            min={1024}
            max={systemMemory ?? 16384}
            step={512}
            value={config.memoryMb}
            onChange={(e) => update({ memoryMb: Number(e.target.value) })}
            className={styles.slider}
          />
          <input
            className={clsx("input", styles.memoryNumber)}
            type="number"
            value={config.memoryMb}
            onChange={(e) => update({ memoryMb: Number(e.target.value) })}
          />
          <span className={styles.unit}>MB</span>
        </div>
        <p className="hint">
          Applies to every instance that has not set its own value.
          {systemMemory && ` This machine has ${(systemMemory / 1024).toFixed(1)} GB.`}
        </p>
      </section>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Default JVM arguments</h2>
        <textarea
          className="textarea"
          rows={3}
          value={args}
          onChange={(e) => setArgs(e.target.value)}
          onBlur={() => args !== config.javaArgs && update({ javaArgs: args })}
        />
        {/* "Saved when you click away" used to be here and nowhere else, so
            this was the only field on any of these screens that said what
            happens to it. The line at the top of the screen says it for all
            of them. */}
        <p className="hint">Instances can override this individually.</p>
      </section>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Runtimes</h2>

        <div className={styles.javaGrid}>
          {wanted.map((major) => (
            <div key={major} className={styles.javaSlot}>
              <div>
                <strong>Java {major}</strong>
                <span className={styles.javaFor}>
                  {major === 8
                    ? "Minecraft 1.16 and older"
                    : major === 17
                      ? "1.17 – 1.20.4"
                      : "1.20.5 and newer"}
                </span>
              </div>
              {have.has(major) ? (
                <span className={styles.javaOk}>
                  <Check size={14} />
                  Installed
                </span>
              ) : (
                <button
                  className="btn btn-sm"
                  onClick={() => install(major)}
                  disabled={installing !== null}
                >
                  {installing === major ? (
                    <Loader2 size={12} className="spin" />
                  ) : (
                    <Download size={13} />
                  )}
                  Install
                </button>
              )}
            </div>
          ))}
        </div>

        <p className="hint">
          Geode downloads a matching Eclipse Temurin runtime automatically when
          you launch something that needs one, so this is only here if you would
          rather do it up front.
        </p>

        {installs === null ? (
          <div className="skeleton" style={{ height: 60 }} />
        ) : installs.length > 0 ? (
          <details className={styles.details}>
            <summary>Detected runtimes ({installs.length})</summary>
            <ul className={styles.javaList}>
              {installs.map((java) => (
                <li key={java.path} className={styles.javaItem}>
                  <span className={styles.javaVersion}>
                    Java {java.major}
                    <span className={styles.javaFull}>{java.version}</span>
                    {java.managed && <span className="badge badge-accent">managed</span>}
                  </span>
                  <code className={styles.javaPath}>{java.path}</code>
                </li>
              ))}
            </ul>
          </details>
        ) : (
          <p className="hint">No Java runtimes found on this system yet.</p>
        )}

        <div className={styles.field}>
          <label className="label" htmlFor="global-java">
            Force a specific Java executable
          </label>
          <input
            id="global-java"
            className="input"
            value={config.javaPath ?? ""}
            placeholder="Leave empty to pick automatically per version"
            onChange={(e) => update({ javaPath: e.target.value || null })}
          />
          <p className="hint">
            Overriding this globally is usually a mistake — different Minecraft
            versions need different Java majors.
          </p>
        </div>
      </section>
    </div>
  );
}

function AccountsSettings() {
  const accounts = useStore((s) => s.accounts);
  const refreshAccounts = useStore((s) => s.refreshAccounts);
  const reportError = useStore((s) => s.reportError);

  const [managing, setManaging] = useState(false);

  return (
    <div className={styles.form}>
      <section className={styles.section}>
        {/* "Signed in" over "No accounts yet." asserted a state that was not
            true. This heading names the list under it either way, and claims
            nothing about a token Geode has not tried to use. */}
        <h2 className={styles.sectionTitle}>Accounts you can play as</h2>
        {accounts.length === 0 ? (
          <p className="hint">No accounts yet.</p>
        ) : (
          <div className={styles.accountList}>
            {accounts.map((account) => (
              <div key={account.id} className={styles.accountRow}>
                <AccountAvatar account={account} size={30} />
                <span className={styles.accountText}>
                  <strong>{account.username}</strong>
                  <span className={styles.accountKind}>
                    {account.kind === "microsoft" ? "Microsoft" : "Offline"}
                    {account.active && " · active"}
                  </span>
                </span>
                <button
                  className="btn btn-ghost btn-icon btn-sm"
                  onClick={() =>
                    api
                      .removeAccount(account.id)
                      .then(refreshAccounts)
                      .catch((e) => reportError("Could not remove", e))
                  }
                  aria-label={`Remove ${account.username}`}
                >
                  <Trash2 size={14} />
                </button>
              </div>
            ))}
          </div>
        )}
        <button className="btn" onClick={() => setManaging(true)}>
          <UserRound size={14} />
          Manage accounts
        </button>
        {/* This is the moment somebody signs in to a launcher that is not
            Mojang's, and the only place that said so was the About page.
            Both halves are facts about the code rather than reassurance:
            `auth/mod.rs` uses the device code grant, so the password is
            typed on Microsoft's own page in the person's own browser and
            never reaches Geode, and `auth/store.rs` writes `accounts.json`
            with owner-only permissions. Nothing beyond those two is
            claimed. */}
        <p className="hint">
          Geode is not Mojang or Microsoft, and never asks you for your
          password: signing in opens Microsoft&apos;s own page in your browser.
          What Geode keeps afterwards is the token Microsoft hands back, in a
          file only your user account can read.
        </p>
      </section>

      {managing && <AccountsModal onClose={() => setManaging(false)} />}
    </div>
  );
}

/**
 * The machines running Geode headless that this copy can reach.
 *
 * One token per host and no accounts at all, which is `daemon/token.rs`'s
 * decision rather than this panel's: whoever holds a host's token can do
 * everything on it. So the panel says that, says plainly that a plain `http://`
 * address carries the token in the clear, and never shows a token back — not
 * even a masked one, because what is kept is on disk and what crosses the IPC
 * boundary has no token field at all.
 */
function HostSettings() {
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  const [hosts, setHosts] = useState<RemoteHost[] | null>(null);
  const [name, setName] = useState("");
  const [address, setAddress] = useState("");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .remoteHosts()
      .then(setHosts)
      .catch((e) => {
        setHosts([]);
        reportError("Could not read your list of hosts", e);
      });
  }, [reportError]);

  async function add() {
    setBusy(true);
    try {
      setHosts(await api.addRemoteHost(name, address, token));
      // Cleared the moment it is saved: nothing here ever reads a token back,
      // and a field still holding one is a field that can be read over a
      // shoulder.
      setName("");
      setAddress("");
      setToken("");
      toast({ kind: "success", title: "Host added" });
    } catch (e) {
      reportError("Could not save that host", e);
    } finally {
      setBusy(false);
    }
  }

  async function forget(host: RemoteHost) {
    setBusy(true);
    try {
      setHosts(await api.forgetRemoteHost(host.id));
      toast({
        kind: "info",
        title: `Forgot ${host.name}`,
        body: "Nothing on the host itself was touched — only this machine's note of how to reach it.",
      });
    } catch (e) {
      reportError("Could not forget that host", e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className={styles.form}>
      <p className={styles.saveModel}>
        Nothing on this screen waits to be saved: a host is added or forgotten
        the moment you press the button for it.
      </p>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Hosts you can send a server to</h2>

        {hosts === null ? (
          <div className={styles.loading}>
            <Loader2 size={16} className="spin" />
          </div>
        ) : hosts.length === 0 ? (
          <p className={styles.calloutNote}>
            None yet. A host is a machine running Geode with no window —{" "}
            <code>geoded</code>, from the container files — and it is where a
            server can keep running when this computer is off.
          </p>
        ) : (
          <div className={styles.accountList}>
            {hosts.map((host) => (
              <div key={host.id} className={styles.accountRow}>
                <ServerIcon size={16} />
                <span className={styles.accountText}>
                  {host.name}
                  <span className={styles.accountKind}>
                    {host.address}
                    {host.plainHttp && " · the token crosses in the clear"}
                  </span>
                </span>
                <button
                  className="btn btn-sm"
                  onClick={() => void forget(host)}
                  disabled={busy}
                >
                  <Unplug size={13} />
                  Forget
                </button>
              </div>
            ))}
          </div>
        )}
      </section>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Add a host</h2>
        <div className={styles.field}>
          <label className="label" htmlFor="host-name">
            Name
          </label>
          <input
            id="host-name"
            className="input"
            value={name}
            placeholder="The box in the cupboard"
            onChange={(e) => setName(e.target.value)}
          />
        </div>
        <div className={styles.field}>
          <label className="label" htmlFor="host-address">
            Address
          </label>
          <input
            id="host-address"
            className="input"
            value={address}
            placeholder="box.example:9090"
            onChange={(e) => setAddress(e.target.value)}
          />
        </div>
        <div className={styles.field}>
          <label className="label" htmlFor="host-token">
            Token
          </label>
          <input
            id="host-token"
            className="input"
            type="password"
            value={token}
            placeholder="the GEODE_TOKEN its compose file was given"
            onChange={(e) => setToken(e.target.value)}
          />
        </div>
        <div className={styles.inlineRow}>
          <button
            className="btn btn-primary"
            onClick={() => void add()}
            disabled={busy || !name.trim() || !address.trim() || !token.trim()}
          >
            {busy && <Loader2 size={13} className="spin" />}
            {/* "Save host" read as the Save of a settings form, which is the
                one thing this is not: there is nothing here to come back and
                change. */}
            Add host
          </button>
        </div>

        <div className={styles.callout}>
          <p>
            <strong>One token, and it can do everything.</strong> A host has no
            accounts and no roles: whoever holds its token can start, stop,
            reconfigure and delete every server on it. Keep it as you would an
            API key, and generate a new one on the host if it ever leaks.
          </p>
          <p>
            A host serves plain HTTP and expects a tunnel, a VPN or a reverse
            proxy with TLS in front of it. Over a bare <code>http://</code>{" "}
            address the token is readable by anything between here and there —
            so on anything but your own network, put something in front of it
            and give Geode the <code>https://</code> address instead.
          </p>
          <p className={styles.calloutNote}>
            Forgetting a host removes nothing but this machine&apos;s note of how
            to reach it. The servers on it keep running.
          </p>
        </div>
      </section>
    </div>
  );
}

function SharingSettings() {
  const config = useStore((s) => s.config)!;
  const loadConfig = useStore((s) => s.loadConfig);
  const setShareStatus = useStore((s) => s.setShareStatus);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  const saved = config.share;
  const [port, setPort] = useState(String(saved.port));
  const [mapping, setMapping] = useState(saved.mapping);
  const [address, setAddress] = useState(saved.publicAddress ?? "");
  const [saving, setSaving] = useState(false);

  const parsedPort = parsePort(port);
  const publicAddress = address.trim() || null;
  const dirty =
    parsedPort !== saved.port ||
    mapping !== saved.mapping ||
    publicAddress !== saved.publicAddress;

  function reset(from: ShareSettings) {
    setPort(String(from.port));
    setMapping(from.mapping);
    setAddress(from.publicAddress ?? "");
  }

  async function save() {
    if (parsedPort === null) return;
    setSaving(true);
    try {
      setShareStatus(
        await api.setShareSettings({ port: parsedPort, mapping, publicAddress }),
      );
      await loadConfig();
      // Show what was stored, which the backend may have tidied.
      const stored = useStore.getState().config?.share;
      if (stored) reset(stored);
      toast({ kind: "success", title: "Sharing settings saved" });
    } catch (e) {
      reportError("Could not save sharing settings", e);
    } finally {
      setSaving(false);
    }
  }

  return (
    <>
      <div className={styles.form}>
        <p className={styles.saveModel}>
          Nothing on this screen changes until you press Save. The port, the
          router and the address are one route to this computer between them,
          so they are written together or not at all.
        </p>

        <section className={styles.section}>
          <h2 className={styles.sectionTitle}>Port</h2>
          <div className={styles.field}>
            <label className="label" htmlFor="share-port">
              Port for shared packs
            </label>
            <input
              id="share-port"
              className="input"
              style={{ width: 110 }}
              inputMode="numeric"
              value={port}
              onChange={(e) => setPort(e.target.value)}
            />
            {parsedPort === null ? (
              <p className={styles.checkBad}>
                <AlertTriangle size={13} />
                <span>A whole number from 1024 to 65535.</span>
              </p>
            ) : (
              <p className="hint">
                Every server you share is served on this one port. It goes in
                the link; a player who types your address by hand has to type
                it too, unless it is {DEFAULT_SHARE_PORT}. Changing it changes
                every shared server&apos;s link and address.
              </p>
            )}
          </div>
        </section>

        <section className={styles.section}>
          <h2 className={styles.sectionTitle}>Router</h2>
          <label className={styles.toggleRow}>
            <input
              type="radio"
              name="share-mapping"
              checked={mapping === "auto"}
              onChange={() => setMapping("auto")}
            />
            <span>
              <strong>Open the port automatically</strong>
              <span className={styles.toggleHint}>
                Geode asks your router over UPnP while a server is shared, keeps
                asking so the opening does not lapse, and closes it when Geode
                exits. The server page says whether the router opened it and
                whether the address it reports is one players could reach; Geode
                cannot test the connection from outside.
              </span>
            </span>
          </label>
          <label className={styles.toggleRow}>
            <input
              type="radio"
              name="share-mapping"
              checked={mapping === "manual"}
              onChange={() => setMapping("manual")}
            />
            <span>
              <strong>Set it up myself</strong>
              <span className={styles.toggleHint}>
                Geode changes nothing on the router. Forward TCP port{" "}
                {parsedPort ?? saved.port} to this computer, or run a tunnel to
                it. Geode still asks the router what its public address is, and
                links use it when it really is a public one; when the router will
                not say, set the public address below.
              </span>
            </span>
          </label>
        </section>

        <section className={styles.section}>
          <h2 className={styles.sectionTitle}>Public address</h2>
          <div className={styles.field}>
            <label className="label" htmlFor="share-address">
              Address players connect to
            </label>
            <input
              id="share-address"
              className="input"
              value={address}
              placeholder="play.example.com, host.playit.gg:12345, https://pack.example.com"
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => setAddress(e.target.value)}
            />
            <p className="hint">
              Leave it empty to use the address your router reports. Set it when
              players reach you another way: a domain pointed at your router, or
              a TCP tunnel&apos;s host and port, such as playit.gg gives you.
              Both are still checked against this computer&apos;s certificate,
              and a TCP tunnel only has to pass the connection through, so it is
              the simpler of the tunnels to set up.
            </p>
            {/* Said up front, because a tunnel set up the usual way fails for
                every player while nothing on this computer looks wrong: Geode
                only speaks TLS, with a certificate no authority signed. */}
            <p className="hint">
              An https:// address is checked by its certificate authority
              instead, and whatever serves it — a tunnel or a reverse proxy —
              has to pass requests on to https://localhost:
              {parsedPort ?? saved.port}, not http://, with its check of this
              computer&apos;s certificate turned off, since that certificate is
              self-signed. With cloudflared, that is --no-tls-verify. Set up any
              other way, players cannot connect, and nothing here shows it.
            </p>
          </div>
        </section>
      </div>

      {/* The same bar the instance Settings tab and server Properties use,
          rather than the Save button this screen used to end with: the end of
          a long scroll is the one place somebody who has finished editing is
          not looking, and leaving without pressing it lost the lot. */}
      {dirty && (
        <div className={clsx("save-bar", styles.saveBar)}>
          <span className="save-hint">
            {parsedPort === null
              ? "The port has to be a whole number from 1024 to 65535 before this can be saved."
              : "You have unsaved changes."}
          </span>
          <button className="btn" onClick={() => reset(saved)} disabled={saving}>
            Discard
          </button>
          <button
            className="btn btn-primary"
            onClick={() => void save()}
            disabled={parsedPort === null || saving}
          >
            {saving && <Loader2 size={13} className="spin" />}
            Save changes
          </button>
        </div>
      )}
    </>
  );
}

/**
 * Updating Geode itself.
 *
 * In About rather than behind a tab of its own: the version somebody is
 * running is printed two sections above this, and "what have I got, is there
 * anything newer" is one question rather than two places to look for it.
 *
 * Every answer is on screen and none of them is Geode's to assume. An update
 * that installed itself while somebody was mid-game would be worse than no
 * updater at all, so nothing is replaced until Install is pressed — and the
 * backend turns Install away anyway while a game or a server is running, which
 * is what `blocked` carries up here to say before the button is pressed rather
 * than after.
 *
 * `status.configured` splits the panel in two, and it is the only thing that
 * does. False means this build was compiled without the key an update is
 * verified against: the check still ran and the version comparison still
 * happened, off the public releases API instead of the signed manifest, so
 * there is just as much to say — but Install is not on the screen at all,
 * because there would be nothing to check a download against. In its place is
 * the releases page, and the copy says plainly that downloading and running it
 * is the person's own job.
 */
function Updates() {
  const reportError = useStore((s) => s.reportError);
  const toast = useStore((s) => s.toast);
  const patchConfig = useStore((s) => s.patchConfig);

  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [checking, setChecking] = useState(true);
  const [installing, setInstalling] = useState(false);
  /** "Not now": the offer is put away for this sitting, not refused. */
  const [putOff, setPutOff] = useState(false);
  /**
   * Why the last check came back with nothing at all, kept because a failure
   * leaves no status and every control here is drawn from one. Without it the
   * panel claimed to still be looking, for ever, with no button to press.
   */
  const [failure, setFailure] = useState<string | null>(null);

  // Quiet when the screen opens, because nobody asked and a network that is
  // down is not worth a toast; loud when Check again is pressed, because that
  // is somebody asking and silence would read as "nothing newer". Quiet is
  // about the toast, though, and not about the panel: the failure is written
  // down either way, because the panel is where somebody looks next.
  const check = useCallback(
    async (asked: boolean) => {
      setChecking(true);
      try {
        setStatus(await api.checkForUpdate());
        setFailure(null);
        setPutOff(false);
      } catch (e) {
        const why = errorMessage(e);
        setFailure(why);
        // The one place the request URL and the HTTP status still land. A
        // panel and a toast are both read by somebody who wanted to know
        // whether there is a newer Geode, and neither of them is a log.
        console.warn("Geode's update check failed:", why);
        // So the toast says what the panel is about to say underneath it,
        // rather than reporting the error — which is where those internals
        // used to reach a player.
        if (asked) {
          const said = panelLine({ status: null, failure: why, putOff: false });
          toast({ kind: "error", title: said.title, body: said.detail });
        }
      } finally {
        setChecking(false);
      }
    },
    [toast],
  );

  useEffect(() => {
    void check(false);
  }, [check]);

  async function install() {
    setInstalling(true);
    try {
      await api.installUpdate();
      // Reached only if nothing restarted: on Windows the installer closes
      // Geode itself, and everywhere else this call ends in a restart. So
      // arriving here means the install did not happen and what is on screen
      // is now stale.
      await check(false);
    } catch (e) {
      reportError("Could not install the update", e);
    } finally {
      setInstalling(false);
    }
  }

  async function setSkipped(version: string | null) {
    try {
      await patchConfig({ skippedUpdate: version });
      await check(false);
      if (version) {
        toast({
          kind: "info",
          title: `Skipping Geode ${version}`,
          body: "You will not be asked about this version again. The next release is offered as usual.",
        });
      }
    } catch (e) {
      reportError("Could not save that", e);
    }
  }

  /** Whether the offer on screen is itself a link to the releases page. */
  const offeredAsALink = status?.configured === false && Boolean(status.available) && !putOff;

  const hint = panelHint(status);

  return (
    <section className={styles.section}>
      <h2 className={styles.sectionTitle}>Updates</h2>

      <StatusNote line={panelLine({ status, failure, putOff })} busy={checking || installing} />

      {status?.available && status.notes && (
        <details className={styles.details}>
          <summary>What changed in {status.available}</summary>
          <p className={styles.releaseNotes}>{status.notes}</p>
        </details>
      )}

      {status?.available && status.blocked && <p className="hint">{status.blocked}</p>}

      <div className={styles.inlineRow}>
        {status?.available && !putOff && (
          <>
            {/*
              Install exists only in a build that can verify what it would
              install. Without the key there is nothing to check a download
              against, so the offer is a link and the person downloads and runs
              the installer themselves — which is also the one thing this
              button has to make obvious.
            */}
            {status.configured ? (
              <button
                className="btn btn-primary"
                disabled={installing || Boolean(status.blocked)}
                onClick={() => void install()}
              >
                {installing ? <Loader2 size={13} className="spin" /> : <Download size={13} />}
                Install {status.available}
              </button>
            ) : (
              <button className="btn btn-primary" onClick={() => void openUrl(RELEASES_URL)}>
                <ExternalLink size={13} />
                Get {status.available} from the releases page
              </button>
            )}
            <button className="btn" disabled={installing} onClick={() => setPutOff(true)}>
              Not now
            </button>
            <button
              className="btn"
              disabled={installing}
              onClick={() => void setSkipped(status.available)}
            >
              Skip this one
            </button>
          </>
        )}

        {/*
          Putting an offer away is a local decision, so taking it back is one
          too: without this the only route back to Install was Check again,
          which runs a fresh network check to undo a button press.
        */}
        {status?.available && putOff && (
          <button className="btn" onClick={() => setPutOff(false)}>
            Show it again
          </button>
        )}

        {status?.skipped && !status.available && (
          <button className="btn" onClick={() => void setSkipped(null)}>
            Stop skipping {status.skipped}
          </button>
        )}

        {/*
          Both of these also appear when the check failed, which is when they
          are needed most: a failure leaves no status, so gating them on one
          left the row empty on the screen that was still saying "Looking for
          a newer Geode". Check again is offered to a build with no key too,
          because such a build does check — it reads the public releases API,
          which needs no key — and only stops short of installing.
        */}
        {(status || failure) && (
          <button
            className="btn"
            disabled={checking || installing}
            onClick={() => void check(true)}
          >
            {checking ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
            Check again
          </button>
        )}

        {/*
          For a build that cannot install — where this page is the only way to
          get a new version — and after a check that failed, which says to take
          one by hand and needs somewhere to do it. Not beside the offer's own
          button, which is already this link under another name.
        */}
        {(failure || status?.configured === false) && !offeredAsALink && (
          <button className="btn" onClick={() => void openUrl(RELEASES_URL)}>
            <ExternalLink size={13} />
            Open the releases page
          </button>
        )}
      </div>

      {/*
        Which of the two paragraphs this is, or neither, is `panelHint`'s to
        decide: a check that failed leaves no status, and so leaves nobody
        able to say which half of the feature this build is.
      */}
      {hint && <p className="hint">{hint}</p>}
    </section>
  );
}

function About() {
  const reportError = useStore((s) => s.reportError);
  const [info, setInfo] = useState<AppInfo | null>(null);

  useEffect(() => {
    api.appInfo().then(setInfo).catch(() => {});
  }, []);

  return (
    <div className={styles.form}>
      <section className={styles.section}>
        <div className={styles.aboutHead}>
          <svg viewBox="0 0 512 512" className={styles.aboutMark} aria-hidden="true">
            <path
              d="M256 60 L420 154 L420 358 L256 452 L92 358 L92 154 Z"
              fill="none"
              stroke="currentColor"
              strokeWidth="30"
              strokeLinejoin="round"
              opacity="0.35"
            />
            <path d="M256 132 L322 240 L256 300 L190 240 Z" fill="currentColor" />
            <path d="M190 240 L256 300 L232 386 L162 306 Z" fill="currentColor" opacity="0.75" />
            <path d="M322 240 L256 300 L280 386 L350 306 Z" fill="currentColor" opacity="0.55" />
          </svg>
          <div>
            <h2 className={styles.aboutName}>Geode</h2>
            <p className={styles.aboutVersion}>
              {info ? `Version ${info.version} · ${info.platform}` : "…"}
            </p>
          </div>
        </div>
        <p className="hint" style={{ marginTop: 0 }}>
          A fast, native Minecraft launcher and mod manager. Not affiliated with
          Mojang, Microsoft, Modrinth or CurseForge.
        </p>
      </section>

      <Updates />

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Data</h2>
        <div className={styles.field}>
          <label className="label">Everything lives here</label>
          <div className={styles.inlineRow}>
            <code className={styles.path}>{info?.dataDir ?? "…"}</code>
            <button
              className="btn"
              disabled={!info}
              onClick={() =>
                info &&
                revealItemInDir(info.dataDir).catch((e) =>
                  reportError("Could not open the folder", e),
                )
              }
            >
              <FolderOpen size={14} />
              Open
            </button>
          </div>
          <p className="hint">
            Instances, downloaded Minecraft versions, shared libraries, assets and
            Java runtimes. Deleting this folder resets Geode completely.
          </p>
        </div>
      </section>
    </div>
  );
}

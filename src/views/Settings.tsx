import { useEffect, useState } from "react";
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
  KeyRound,
  Loader2,
  Palette,
  Trash2,
  UserRound,
} from "lucide-react";

import AccountsModal from "../components/AccountsModal";
import { api } from "../lib/api";
import { hashHue, initials } from "../lib/format";
import { useStore } from "../lib/store";
import type { AppInfo, JavaInstall, KeyCheck } from "../lib/types";

import styles from "./Settings.module.css";

const TABS = [
  { id: "appearance", label: "Appearance", icon: Palette },
  { id: "java", label: "Java & Memory", icon: Coffee },
  { id: "accounts", label: "Accounts", icon: UserRound },
  { id: "sources", label: "Mod sources", icon: KeyRound },
  { id: "about", label: "About", icon: Info },
] as const;

type TabId = (typeof TABS)[number]["id"];

/**
 * Muted rather than bright, and named after materials rather than hues.
 *
 * The stock choice here is the full-saturation spectrum every framework ships
 * with, which fights a warm grey interface and makes whichever one you pick
 * look like a default. These are dialled back far enough to sit on the
 * surfaces without shouting, and far enough apart in hue to be a real choice.
 */
const ACCENTS = [
  "#d2a24c",
  "#c96f43",
  "#8fa65a",
  "#5f9ea0",
  "#6f8fbf",
  "#9b7cb8",
  "#c76a8a",
  "#a8a29a",
];

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
          {tab === "accounts" && <AccountsSettings update={update} />}
          {tab === "sources" && <Sources />}
          {tab === "about" && <About />}
        </div>
      </div>
    </>
  );
}

type Updater = (patch: Record<string, unknown>) => void;

function Appearance({ update }: { update: Updater }) {
  const config = useStore((s) => s.config)!;

  return (
    <div className={styles.form}>
      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Theme</h2>
        <div className={styles.themeRow}>
          {(["dark", "light"] as const).map((theme) => (
            <button
              key={theme}
              className={clsx(
                styles.themeCard,
                config.theme === theme && styles.themeActive,
              )}
              onClick={() => update({ theme })}
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
        <h2 className={styles.sectionTitle}>Accent colour</h2>
        <div className={styles.accents}>
          {ACCENTS.map((accent) => (
            <button
              key={accent}
              className={clsx(
                styles.accent,
                config.accent.toLowerCase() === accent && styles.accentActive,
              )}
              style={{ background: accent }}
              onClick={() => update({ accent })}
              aria-label={`Accent ${accent}`}
            >
              {config.accent.toLowerCase() === accent && <Check size={14} />}
            </button>
          ))}
          <input
            type="color"
            className={styles.accentPicker}
            value={config.accent}
            onChange={(e) => update({ accent: e.target.value })}
            aria-label="Custom accent colour"
          />
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
        <p className="hint">
          Saved when you click away. Instances can override this individually.
        </p>
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

function AccountsSettings({ update }: { update: Updater }) {
  const config = useStore((s) => s.config)!;
  const accounts = useStore((s) => s.accounts);
  const refreshAccounts = useStore((s) => s.refreshAccounts);
  const reportError = useStore((s) => s.reportError);

  const [clientId, setClientId] = useState(config.clientId ?? "");
  const [managing, setManaging] = useState(false);

  return (
    <div className={styles.form}>
      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Signed in</h2>
        {accounts.length === 0 ? (
          <p className="hint">No accounts yet.</p>
        ) : (
          <div className={styles.accountList}>
            {accounts.map((account) => (
              <div key={account.id} className={styles.accountRow}>
                <span
                  className={styles.avatar}
                  style={{
                    background: `linear-gradient(135deg, hsl(${hashHue(account.id)} 55% 42%), hsl(${(hashHue(account.id) + 40) % 360} 55% 30%))`,
                  }}
                >
                  {initials(account.username)}
                </span>
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
      </section>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Microsoft sign-in</h2>
        <div className={styles.callout}>
          <p>
            Microsoft issues OAuth client IDs per application, and Mojang has to
            approve each one for Minecraft API access before it works. Those
            terms are why Geode cannot ship a shared ID — you register your own,
            once, and it belongs to you.
          </p>
          <ol className={styles.steps}>
            <li>
              Create an app registration in the Azure portal (public client, no
              secret needed).
            </li>
            <li>
              Enable <em>Allow public client flows</em> so the device-code flow
              works.
            </li>
            <li>
              Apply for Minecraft API access with Mojang, then paste the
              Application (client) ID below.
            </li>
          </ol>
          <div className={styles.calloutLinks}>
            <button
              className="btn btn-sm"
              onClick={() => void openUrl("https://portal.azure.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade")}
            >
              <ExternalLink size={13} />
              Azure app registrations
            </button>
            <button
              className="btn btn-sm"
              onClick={() => void openUrl("https://help.minecraft.net/hc/en-us/articles/16254801392141")}
            >
              <ExternalLink size={13} />
              Mojang approval form
            </button>
          </div>
        </div>

        <div className={styles.field}>
          <label className="label" htmlFor="client-id">
            Azure client ID
          </label>
          <div className={styles.inlineRow}>
            <input
              id="client-id"
              className="input"
              value={clientId}
              placeholder="00000000-0000-0000-0000-000000000000"
              onChange={(e) => setClientId(e.target.value)}
            />
            <button
              className="btn"
              onClick={() => update({ clientId: clientId.trim() || null })}
              disabled={clientId.trim() === (config.clientId ?? "")}
            >
              Save
            </button>
          </div>
          <p className="hint">
            Offline accounts work without this. Only online play needs it.
          </p>
        </div>
      </section>

      {managing && <AccountsModal onClose={() => setManaging(false)} />}
    </div>
  );
}

function Sources() {
  const config = useStore((s) => s.config)!;
  const loadConfig = useStore((s) => s.loadConfig);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  const [key, setKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [check, setCheck] = useState<KeyCheck | null>(null);

  async function testKey() {
    setTesting(true);
    setCheck(null);
    try {
      // Test what is typed if anything, else whatever is currently in effect.
      setCheck(await api.testCurseforgeKey(key.trim() || undefined));
    } catch (e) {
      reportError("Could not test the key", e);
    } finally {
      setTesting(false);
    }
  }

  async function saveKey(value: string | null) {
    setSaving(true);
    try {
      await api.setCurseforgeKey(value);
      await loadConfig();
      setKey("");
      toast({
        kind: "success",
        title: value ? "CurseForge enabled" : "CurseForge key removed",
      });
    } catch (e) {
      reportError("Could not save the key", e);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className={styles.form}>
      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>Modrinth</h2>
        <div className={styles.sourceRow}>
          <span className="badge badge-modrinth">Modrinth</span>
          <span className={styles.sourceState}>
            <Check size={14} /> Always on — no key required
          </span>
        </div>
        <p className="hint">
          Modrinth's API is open, so search, installs and updates work out of the
          box.
        </p>
      </section>

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>CurseForge</h2>
        <div className={styles.sourceRow}>
          <span className="badge badge-curseforge">CurseForge</span>
          <span className={styles.sourceState}>
            {config.hasCurseforgeKey ? (
              <>
                <Check size={14} />
                {config.curseforgeKeyFromBuild ? "Enabled (compiled in)" : "Enabled"}
              </>
            ) : (
              "Not configured"
            )}
          </span>
        </div>

        <div className={styles.callout}>
          <p>
            CurseForge issues API keys per developer, and their terms say a key
            is non-transferable and must not be disclosed to a third party. A
            key shipped in a public build is exactly that — Prism and most
            open-source launchers do it anyway, but Geode does not by default.
            Keys are free to request.
          </p>
          <p className={styles.calloutNote}>
            For a build you keep to yourself there is no third party, so you can
            compile your own key in instead of pasting it here:{" "}
            <code>GEODE_CURSEFORGE_KEY=… npm run tauri build</code>. Share that
            binary and the clause applies again.
          </p>
          <p className={styles.calloutNote}>
            Some authors also disable third-party downloads for their projects.
            Those show a <strong>Manual</strong> badge and link out to the
            project page — Geode will not work around that setting.
          </p>
          <div className={styles.calloutLinks}>
            <button
              className="btn btn-sm"
              onClick={() => void openUrl("https://console.curseforge.com/")}
            >
              <ExternalLink size={13} />
              CurseForge developer console
            </button>
          </div>
        </div>

        <div className={styles.field}>
          <label className="label" htmlFor="cf-key">
            API key
          </label>
          <div className={styles.inlineRow}>
            <input
              id="cf-key"
              className="input"
              type="password"
              value={key}
              placeholder={
                config.curseforgeKeyFromBuild
                  ? "using the compiled-in key — paste one to override"
                  : config.hasCurseforgeKey
                    ? "•••••••••••••••• (stored)"
                    : "Paste your key"
              }
              onChange={(e) => setKey(e.target.value)}
            />
            <button
              className="btn"
              onClick={testKey}
              disabled={testing || (!key.trim() && !config.hasCurseforgeKey)}
            >
              {testing ? <Loader2 size={13} className="spin" /> : <Check size={13} />}
              Test
            </button>
            <button
              className="btn btn-primary"
              onClick={() => saveKey(key.trim())}
              disabled={!key.trim() || saving}
            >
              {saving && <Loader2 size={13} className="spin" />}
              Save
            </button>
            {config.hasCurseforgeKey && !config.curseforgeKeyFromBuild && (
              <button
                className="btn btn-danger"
                onClick={() => saveKey(null)}
                disabled={saving}
              >
                Remove
              </button>
            )}
          </div>
          {check && (
            <div className={check.ok ? styles.checkOk : styles.checkBad}>
              {check.ok ? <Check size={13} /> : <AlertTriangle size={13} />}
              <span>
                {check.message}
                {check.fingerprint && (
                  <span className={styles.fingerprint}>
                    Tested {check.source}: <code>{check.fingerprint}</code>
                  </span>
                )}
              </span>
            </div>
          )}
          <p className="hint">
            Stored in your Geode config directory with owner-only permissions and
            never sent anywhere except CurseForge.
          </p>
        </div>
      </section>
    </div>
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

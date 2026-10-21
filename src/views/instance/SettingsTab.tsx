import { useEffect, useState } from "react";
import { Copy, Loader2, PackageOpen, Trash2 } from "lucide-react";

import ExportModal from "../../components/ExportModal";
import ImageField from "../../components/ImageField";
import Modal from "../../components/Modal";
import { api } from "../../lib/api";
import { forbiddenPlayerHint } from "../../lib/share";
import { useStore } from "../../lib/store";
import { LOADERS } from "../../lib/types";
import type { Instance, InstanceOverrides } from "../../lib/types";

import viewStyles from "../ServerView.module.css";
import styles from "./SettingsTab.module.css";

/** Empty inputs mean "inherit the global default", not "set to zero". */
function numberOrNull(value: string): number | null {
  const trimmed = value.trim();
  if (trimmed === "") return null;
  const n = Number(trimmed);
  return Number.isFinite(n) ? n : null;
}

function textOrNull(value: string): string | null {
  const trimmed = value.trim();
  return trimmed === "" ? null : trimmed;
}

export default function SettingsTab({ instance }: { instance: Instance }) {
  const config = useStore((s) => s.config);
  const navigate = useStore((s) => s.navigate);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const refreshInstances = useStore((s) => s.refreshInstances);

  const [name, setName] = useState(instance.name);
  const [group, setGroup] = useState(instance.group ?? "");
  const [notes, setNotes] = useState(instance.notes);
  const [overrides, setOverrides] = useState<InstanceOverrides>(instance.overrides);

  const [loaderVersions, setLoaderVersions] = useState<string[]>([]);
  const [loader, setLoader] = useState(instance.loader);
  const [loaderVersion, setLoaderVersion] = useState(instance.loaderVersion);

  const [saving, setSaving] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [duplicating, setDuplicating] = useState(false);
  const [changingServerSide, setChangingServerSide] = useState(false);
  const [systemMemory, setSystemMemory] = useState<number | null>(null);

  useEffect(() => {
    api.systemMemoryMb().then(setSystemMemory).catch(() => setSystemMemory(null));
  }, []);

  useEffect(() => {
    if (loader === "vanilla") {
      setLoaderVersions([]);
      return;
    }
    let cancelled = false;
    api
      .listLoaderVersions(loader, instance.mcVersion)
      .then((list) => {
        if (cancelled) return;
        setLoaderVersions(list);
        // Switching loader family invalidates the old version string.
        if (!list.includes(loaderVersion)) setLoaderVersion(list[0] ?? "");
      })
      .catch(() => !cancelled && setLoaderVersions([]));
    return () => {
      cancelled = true;
    };
    // `loaderVersion` is intentionally not a dependency: it is what we set.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loader, instance.mcVersion]);

  const dirty =
    name !== instance.name ||
    group !== (instance.group ?? "") ||
    notes !== instance.notes ||
    loader !== instance.loader ||
    loaderVersion !== instance.loaderVersion ||
    JSON.stringify(overrides) !== JSON.stringify(instance.overrides);

  async function save() {
    setSaving(true);
    try {
      await api.updateInstance(instance.id, {
        name,
        group: textOrNull(group),
        notes,
        loader,
        loaderVersion,
        overrides,
      });
      await refreshInstances();
      toast({ kind: "success", title: "Saved" });
    } catch (e) {
      reportError("Could not save", e);
    } finally {
      setSaving(false);
    }
  }

  // Applied the moment it is ticked rather than with Save: it renames files on
  // disk, and a box that says off while the mods are still on would be a lie.
  async function setServerSideMods(on: boolean) {
    setChangingServerSide(true);
    try {
      const changed = await api.setServerSideMods(instance.id, on);
      await refreshInstances();
      toast({
        kind: "success",
        title: on ? "Server-side mods on" : "Server-side mods off",
        body:
          changed > 0
            ? `Turned ${on ? "on" : "off"} ${changed} mod${changed === 1 ? "" : "s"}.`
            : undefined,
      });
    } catch (e) {
      reportError("Could not change server-side mods", e);
    } finally {
      setChangingServerSide(false);
    }
  }

  async function duplicate() {
    setDuplicating(true);
    try {
      const copy = await api.duplicateInstance(instance.id, `${instance.name} copy`);
      await refreshInstances();
      toast({ kind: "success", title: `Created ${copy.name}` });
      navigate({ view: "instance", id: copy.id });
    } catch (e) {
      reportError("Could not duplicate", e);
    } finally {
      setDuplicating(false);
    }
  }

  async function destroy() {
    try {
      await api.deleteInstance(instance.id);
      await refreshInstances();
      toast({ kind: "success", title: `Deleted ${instance.name}` });
      navigate({ view: "library" });
    } catch (e) {
      reportError("Could not delete", e);
      setConfirmDelete(false);
    }
  }

  const set = <K extends keyof InstanceOverrides>(key: K, value: InstanceOverrides[K]) =>
    setOverrides((prev) => ({ ...prev, [key]: value }));

  return (
    <div className={styles.scroll}>
      <div className={styles.form}>
        <section className={styles.section}>
          <h2 className={styles.sectionTitle}>Identity</h2>

          {/* Pictures save the moment they are chosen rather than waiting for
              the Save button: the file is copied on the spot, and showing a
              picture that is not actually set yet would be a lie. */}
          <ImageField
            label="Icon"
            hint="Shown in the sidebar and on the library card. Leave it empty for the generated initials tile."
            path={instance.iconPath}
            onChoose={async (source) => {
              await api.setInstanceImage(instance.id, "icon", source);
              await refreshInstances();
            }}
            onClear={async () => {
              await api.setInstanceImage(instance.id, "icon", null);
              await refreshInstances();
            }}
          />

          <ImageField
            label="Banner"
            hint="The artwork across the top of this instance's page. A modpack's own banner is used when there is none."
            path={instance.bannerPath}
            wide
            onChoose={async (source) => {
              await api.setInstanceImage(instance.id, "banner", source);
              await refreshInstances();
            }}
            onClear={async () => {
              await api.setInstanceImage(instance.id, "banner", null);
              await refreshInstances();
            }}
          />

          <div className={styles.grid2}>
            <div>
              <label className="label" htmlFor="inst-name">
                Name
              </label>
              <input
                id="inst-name"
                className="input"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </div>
            <div>
              <label className="label" htmlFor="inst-group">
                Group
              </label>
              <input
                id="inst-group"
                className="input"
                value={group}
                placeholder="Ungrouped"
                onChange={(e) => setGroup(e.target.value)}
              />
            </div>
          </div>

          <div>
            <label className="label" htmlFor="inst-notes">
              Notes
            </label>
            <textarea
              id="inst-notes"
              className="textarea"
              rows={3}
              value={notes}
              placeholder="What is this instance for?"
              onChange={(e) => setNotes(e.target.value)}
            />
          </div>
        </section>

        <section className={styles.section}>
          <h2 className={styles.sectionTitle}>Mod loader</h2>
          <div className={styles.grid2}>
            <div>
              <label className="label" htmlFor="inst-loader">
                Loader
              </label>
              <select
                id="inst-loader"
                className="select"
                value={loader}
                onChange={(e) => setLoader(e.target.value as typeof loader)}
              >
                {LOADERS.map((l) => (
                  <option key={l.id} value={l.id}>
                    {l.label}
                  </option>
                ))}
              </select>
            </div>
            {loader !== "vanilla" && (
              <div>
                <label className="label" htmlFor="inst-loader-version">
                  Version
                </label>
                <select
                  id="inst-loader-version"
                  className="select"
                  value={loaderVersion}
                  onChange={(e) => setLoaderVersion(e.target.value)}
                >
                  {loaderVersions.length === 0 && (
                    <option value={loaderVersion}>{loaderVersion || "—"}</option>
                  )}
                  {loaderVersions.map((v, i) => (
                    <option key={v} value={v}>
                      {v}
                      {i === 0 ? "  (latest)" : ""}
                    </option>
                  ))}
                </select>
              </div>
            )}
          </div>
          <p className="hint">
            Minecraft {instance.mcVersion}. Changing the loader keeps your mods
            folder as-is — mods built for a different loader will not load.
          </p>
        </section>

        <section className={styles.section}>
          <h2 className={styles.sectionTitle}>Java &amp; performance</h2>

          <div>
            <label className="label" htmlFor="inst-memory">
              Memory
              <span className={styles.inherit}>
                {overrides.memoryMb == null &&
                  ` — using the global ${config?.memoryMb ?? 4096} MB`}
              </span>
            </label>
            <div className={styles.memoryRow}>
              <input
                id="inst-memory"
                type="range"
                min={1024}
                max={systemMemory ?? 16384}
                step={512}
                value={overrides.memoryMb ?? config?.memoryMb ?? 4096}
                onChange={(e) => set("memoryMb", Number(e.target.value))}
                className={styles.slider}
              />
              <input
                className={`input ${styles.memoryNumber}`}
                type="number"
                value={overrides.memoryMb ?? ""}
                placeholder={String(config?.memoryMb ?? 4096)}
                onChange={(e) => set("memoryMb", numberOrNull(e.target.value))}
              />
              <span className={styles.unit}>MB</span>
            </div>
            {systemMemory && (
              <p className="hint">
                This machine has {(systemMemory / 1024).toFixed(1)} GB. Giving
                Minecraft more than about half of it usually makes stutter worse,
                not better.
              </p>
            )}
          </div>

          <div>
            <label className="label" htmlFor="inst-jvm">
              JVM arguments
            </label>
            <textarea
              id="inst-jvm"
              className="textarea"
              rows={3}
              value={overrides.javaArgs ?? ""}
              placeholder={config?.javaArgs ?? ""}
              onChange={(e) => set("javaArgs", textOrNull(e.target.value))}
            />
            <p className="hint">Empty inherits the global arguments.</p>
          </div>

          <div>
            <label className="label" htmlFor="inst-java">
              Java executable
            </label>
            <input
              id="inst-java"
              className="input"
              value={overrides.javaPath ?? ""}
              placeholder="Detected automatically"
              onChange={(e) => set("javaPath", textOrNull(e.target.value))}
            />
          </div>
        </section>

        <section className={styles.section}>
          <h2 className={styles.sectionTitle}>Window</h2>
          <div className={styles.grid3}>
            <div>
              <label className="label" htmlFor="inst-w">
                Width
              </label>
              <input
                id="inst-w"
                className="input"
                type="number"
                value={overrides.width ?? ""}
                placeholder="854"
                onChange={(e) => set("width", numberOrNull(e.target.value))}
              />
            </div>
            <div>
              <label className="label" htmlFor="inst-h">
                Height
              </label>
              <input
                id="inst-h"
                className="input"
                type="number"
                value={overrides.height ?? ""}
                placeholder="480"
                onChange={(e) => set("height", numberOrNull(e.target.value))}
              />
            </div>
            <label className={styles.checkbox}>
              <input
                type="checkbox"
                checked={overrides.fullscreen ?? false}
                onChange={(e) => set("fullscreen", e.target.checked || null)}
              />
              Start fullscreen
            </label>
          </div>

          <div>
            <label className="label" htmlFor="inst-server">
              Join a server on launch
            </label>
            <input
              id="inst-server"
              className="input"
              value={overrides.quickPlayServer ?? ""}
              placeholder="play.example.com"
              onChange={(e) => set("quickPlayServer", textOrNull(e.target.value))}
            />
            <p className="hint">Needs Minecraft 1.20 or newer.</p>
          </div>
        </section>

        <section className={styles.section}>
          <h2 className={styles.sectionTitle}>Hooks</h2>
          <div>
            <label className="label" htmlFor="inst-pre">
              Before launch
            </label>
            <input
              id="inst-pre"
              className="input"
              value={overrides.preLaunchCommand ?? ""}
              placeholder="Shell command — a non-zero exit cancels the launch"
              onChange={(e) => set("preLaunchCommand", textOrNull(e.target.value))}
            />
          </div>
          <div>
            <label className="label" htmlFor="inst-post">
              After exit
            </label>
            <input
              id="inst-post"
              className="input"
              value={overrides.postExitCommand ?? ""}
              placeholder="Shell command"
              onChange={(e) => set("postExitCommand", textOrNull(e.target.value))}
            />
          </div>
          <p className="hint">
            Hooks run in the instance's game directory with your shell.
          </p>
        </section>

        {instance.joined && (
          <section className={styles.section}>
            <h2 className={styles.sectionTitle}>Server</h2>
            <label
              className={viewStyles.settingToggle}
              title={instance.running ? "Stop the game to change this" : undefined}
            >
              <input
                type="checkbox"
                checked={!instance.serverSideModsOff}
                disabled={changingServerSide || instance.running}
                onChange={(e) => void setServerSideMods(e.target.checked)}
              />
              <span>
                <strong>Server-side mods</strong>
                <span className={viewStyles.settingWhy}>
                  Mods only the server needs. Keep them on to match the server
                  exactly, or turn them off for better performance. This
                  applies straight away, and to everything the server sends
                  later.
                </span>
              </span>
            </label>
            {/* The promise, whether or not this server has any rules: nothing
                of theirs is deleted, and a mod turned back on does not stay
                on — which they would otherwise learn from a Play that takes a
                minute to go wrong. */}
            <p className="hint">{forbiddenPlayerHint()}</p>
          </section>
        )}

        <section className={styles.section}>
          <h2 className={styles.sectionTitle}>Manage</h2>
          <div className={styles.manageRow}>
            <button className="btn" onClick={duplicate} disabled={duplicating}>
              {duplicating ? <Loader2 size={14} className="spin" /> : <Copy size={14} />}
              Duplicate
            </button>
            <button
              className="btn"
              onClick={() => setExporting(true)}
              title="Pick a format, choose what travels, and see what each mod will do before anything is written"
            >
              <PackageOpen size={14} />
              Export
            </button>
            <div style={{ flex: 1 }} />
            <button
              className="btn btn-danger"
              onClick={() => setConfirmDelete(true)}
              disabled={instance.running}
              title={instance.running ? "Stop the game first" : undefined}
            >
              <Trash2 size={14} />
              Delete instance
            </button>
          </div>
        </section>
      </div>

      {dirty && (
        <div className={styles.saveBar}>
          <span className={styles.saveHint}>You have unsaved changes.</span>
          <button
            className="btn"
            onClick={() => {
              setName(instance.name);
              setGroup(instance.group ?? "");
              setNotes(instance.notes);
              setLoader(instance.loader);
              setLoaderVersion(instance.loaderVersion);
              setOverrides(instance.overrides);
            }}
          >
            Discard
          </button>
          <button className="btn btn-primary" onClick={save} disabled={saving}>
            {saving && <Loader2 size={14} className="spin" />}
            Save changes
          </button>
        </div>
      )}

      {exporting && (
        <ExportModal instance={instance} onClose={() => setExporting(false)} />
      )}

      {confirmDelete && (
        <Modal
          title={`Delete ${instance.name}?`}
          subtitle="This removes the instance folder, including its worlds, configs and mods. It cannot be undone."
          onClose={() => setConfirmDelete(false)}
          width={420}
          footer={
            <>
              <button className="btn" onClick={() => setConfirmDelete(false)}>
                Cancel
              </button>
              <button className="btn btn-danger" onClick={destroy}>
                Delete permanently
              </button>
            </>
          }
        >
          <p style={{ fontSize: 13, color: "var(--text-dim)", lineHeight: 1.6 }}>
            Shared downloads (Minecraft versions, libraries, assets) stay put and
            will be reused by your other instances.
          </p>
        </Modal>
      )}
    </div>
  );
}

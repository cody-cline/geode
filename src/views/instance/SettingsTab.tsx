import { useEffect, useState } from "react";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { Copy, Loader2, PackageOpen, Trash2 } from "lucide-react";

import ImageField from "../../components/ImageField";
import Modal from "../../components/Modal";
import { api } from "../../lib/api";
import { useStore } from "../../lib/store";
import { LOADERS } from "../../lib/types";
import type { ExportReport, Instance, InstanceOverrides } from "../../lib/types";

import viewStyles from "../ServerView.module.css";
import styles from "./SettingsTab.module.css";

/** Empty inputs mean "inherit the global default", not "set to zero". */
function numberOrNull(value: string): number | null {
  const trimmed = value.trim();
  if (trimmed === "") return null;
  const n = Number(trimmed);
  return Number.isFinite(n) ? n : null;
}

/**
 * The formats an instance exports as: what to save it under, what the save
 * dialog's filter is called, and what to call the format in a sentence.
 *
 * A CurseForge pack is a plain `.zip` — the format has no extension of its own
 * — so what a pack is saved as and what it is called are no longer the same
 * string.
 */
const FORMATS = {
  geodepack: { ext: "geodepack", filter: "Geode pack", name: "a .geodepack" },
  mrpack: { ext: "mrpack", filter: "Modrinth pack", name: "a .mrpack" },
  curseforge: { ext: "zip", filter: "CurseForge pack", name: "a CurseForge pack" },
  prism: { ext: "zip", filter: "Prism instance", name: "a Prism instance" },
} as const;

type Format = keyof typeof FORMATS;

/**
 * What the export actually did, in the words the recipient's experience turns
 * on. The old copy promised that everything unlinked was "bundled in, jars
 * included", which stopped being true the moment the exporter started asking
 * Modrinth what it holds — and a count is the cheapest honest answer.
 *
 * One sentence per reason, because a single count cannot say which of them
 * applied: "nothing Geode asked has this mod" is something nobody can fix, "this
 * format cannot link to where it came from" and "this format cannot say a mod is
 * off" are answered by exporting another format, "a second copy of a mod this
 * pack already names" costs bytes and nothing else, and "the author turned
 * third-party downloads off" is somebody else's decision that no format gets
 * around — which is why it is counted only where no other platform names the
 * same bytes. Where one does, the backend reports the format's own limit
 * instead, because exporting a .geodepack or .mrpack would link it.
 */
function exportSummary(format: Format, report: ExportReport): string {
  const {
    referenced,
    embeddedUnknown,
    embeddedElsewhere,
    embeddedTurnedOff,
    embeddedBlocked,
    embeddedDuplicate,
  } = report;
  const bundled =
    embeddedUnknown + embeddedElsewhere + embeddedTurnedOff + embeddedBlocked + embeddedDuplicate;
  const lines: string[] = [];
  // A Prism instance zip is the one format where a link is not an alternative
  // to the bytes: Prism's own importer takes the extracted tree as it finds it
  // and downloads nothing, so every jar travels either way. The same counts
  // therefore mean something else here — how many mods arrive named, with an
  // update path, rather than how many the zip did not have to carry — and
  // saying "linked for the recipient to download" would be a plain lie.
  if (format === "prism") {
    if (referenced === 0 && bundled === 0) {
      lines.push("There were no mods, packs or datapacks to carry.");
    } else if (bundled === 0) {
      lines.push(`All ${referenced} named, so Prism can check every one for updates.`);
    } else {
      lines.push(`${referenced} named, so Prism can check them for updates.`);
      lines.push(
        `${bundled} arrive as plain jars with no update path, because nothing Geode asked could name them or this format has nowhere to record it.`,
      );
    }
    lines.push(
      "Every jar travels inside the zip: a Prism instance has no way to say \u201cdownload this\u201d. Export .mrpack instead if you want Prism to fetch the mods itself.",
    );
    lines.push(...report.trouble);
    if (report.unnameable.length > 0) {
      lines.push(`Left out, because a zip cannot carry these names: ${report.unnameable.join(", ")}.`);
    }
    return lines.join(" ");
  }
  if (referenced === 0 && bundled === 0) {
    lines.push("There were no mods, packs or datapacks to carry.");
  } else if (bundled === 0) {
    lines.push(`All ${referenced} linked for the recipient to download, nothing bundled in.`);
  } else {
    lines.push(`${referenced} linked for the recipient to download.`);
    if (embeddedUnknown > 0) {
      // Not "no platform has them": an export asks the platforms whose answer
      // the format it is writing could use, so a .mrpack export asks Modrinth
      // alone — a fingerprint costs a second read of every file in the
      // instance, and a .mrpack cannot carry a CurseForge reference whatever
      // the answer. Claiming more than was asked is the kind of wrong a user
      // can catch.
      lines.push(`${embeddedUnknown} bundled in because nothing Geode asked has them.`);
    }
    if (embeddedElsewhere > 0) {
      lines.push(
        `${embeddedElsewhere} bundled in because ${FORMATS[format].name} cannot link to where they came from.`,
      );
    }
    if (embeddedTurnedOff > 0) {
      lines.push(
        `${embeddedTurnedOff} bundled in because ${FORMATS[format].name} cannot record that a mod is turned off.`,
      );
    }
    if (embeddedBlocked > 0) {
      lines.push(
        `${embeddedBlocked} bundled in because their author has turned off third-party downloads, so a link would be one nobody could follow.`,
      );
    }
    if (embeddedDuplicate > 0) {
      lines.push(
        `${embeddedDuplicate} bundled in as a second copy of a mod this pack already links.`,
      );
    }
  }
  if (format === "geodepack") lines.push("Your settings and artwork travel with it.");
  if (format === "curseforge") {
    // The limit worth hearing even when nothing hit it this time: the format
    // names ids and no folder, so only a mod can be linked and a resource pack
    // would arrive wherever the recipient's own guess put it.
    lines.push(
      "Only mods can be linked in this format, because it records no folder — resource packs, shaders and data packs are always bundled in.",
    );
    // The one thing in a pack that is about the sharer rather than the
    // instance, and the export never asked: the format requires an author, so
    // the signed-in account's name went in. Said here until the export dialog
    // asks for it, because a name travels with the file.
    if (report.author) lines.push(`Credited to ${report.author}.`);
  }
  lines.push(...report.trouble);
  if (report.unnameable.length > 0) {
    lines.push(`Left out, because a pack cannot carry these names: ${report.unnameable.join(", ")}.`);
  }
  return lines.join(" ");
}

/**
 * A name made safe to suggest as a file name. Instance names are free text —
 * and a pack's manifest can set one — so "ATM10 / hardcore" would otherwise be
 * read by the dialog as a folder that does not exist. Same rules as the
 * backend's own `share::pack::file_name`.
 */
function safeFileStem(name: string): string {
  const cleaned = name.replace(/[/\\:*?"<>|]/g, "-").trim();
  return cleaned || "pack";
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

  function exportAs(format: Format, dest: string) {
    switch (format) {
      case "mrpack":
        return api.exportModpack(instance.id, dest, false);
      case "geodepack":
        return api.exportGeodepack(instance.id, dest, false);
      case "curseforge":
        // No author: the dialog that asks for one is not built yet, and the
        // backend stands in the signed-in account's name, which is the only
        // answer it holds to whose pack this is.
        return api.exportCurseforgePack(instance.id, dest, null, false);
      case "prism":
        return api.exportPrismInstance(instance.id, dest, false);
    }
  }

  // The format is picked by which button was pressed, never read back off the
  // file name: the save dialog does not report which filter was chosen, and on
  // Linux it does not add the extension either, so "ForPrism" typed under the
  // Modrinth filter would come back with no extension at all. And two of the
  // four save under a name the format does not own — a CurseForge pack and a
  // Prism instance are both plain .zips. The four differ in what they can say,
  // so guessing wrong silently changes what the file contains: a .geodepack
  // names a CurseForge mod and lets the recipient fetch it, a .mrpack has
  // nowhere to record that and has to carry the jar, a CurseForge pack is the
  // same trade the other way round, and a Prism instance carries every jar
  // whatever it can name.
  async function exportPack(format: Format) {
    const { ext, filter, name } = FORMATS[format];
    const chosen = await saveDialog({
      defaultPath: `${safeFileStem(instance.name)}.${ext}`,
      filters: [{ name: filter, extensions: [ext] }],
    });
    if (!chosen) return;
    const dest = chosen.toLowerCase().endsWith(`.${ext}`) ? chosen : `${chosen}.${ext}`;

    try {
      const report = await exportAs(format, dest);
      toast({
        kind: "success",
        title: `Exported ${name}`,
        body: exportSummary(format, report),
      });
    } catch (e) {
      reportError("Export failed", e);
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
              onClick={() => exportPack("geodepack")}
              title="Settings and artwork included; mods are linked rather than copied"
            >
              <PackageOpen size={14} />
              Export .geodepack
            </button>
            <button
              className="btn"
              onClick={() => exportPack("mrpack")}
              title="For Prism, the Modrinth app and other launchers; mods Modrinth has are linked, the rest are bundled in"
            >
              <PackageOpen size={14} />
              Export .mrpack
            </button>
            <button
              className="btn"
              onClick={() => exportPack("curseforge")}
              title="A .zip for the CurseForge app and Prism; mods CurseForge has are linked by id, and everything else — including a Modrinth mod it does not have — is bundled in. The format requires an author, so the pack credits your signed-in account"
            >
              <PackageOpen size={14} />
              Export CurseForge .zip
            </button>
            <button
              className="btn"
              onClick={() => exportPack("prism")}
              title="A .zip Prism opens as a full instance — settings, loader and all — with every mod named so Prism can check it for updates. Every jar travels inside it: a Prism instance has no way to say “download this”. Export .mrpack instead if you want Prism to fetch the mods itself"
            >
              <PackageOpen size={14} />
              Export Prism instance
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

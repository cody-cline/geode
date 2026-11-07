import { useCallback, useEffect, useMemo, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import {
  Archive,
  ArchiveRestore,
  Copy,
  Earth,
  FolderInput,
  Loader2,
  Pencil,
  Search,
  TriangleAlert,
  Trash2,
} from "lucide-react";

import Modal from "../../components/Modal";
import { api } from "../../lib/api";
import { GAME_MODES, fileSize, relativeTime } from "../../lib/format";
import { savedByNewer } from "../../lib/mcversion";
import { useStore } from "../../lib/store";
import type { Instance, World, WorldBackup } from "../../lib/types";

import styles from "./WorldsTab.module.css";

/** Which dialog is open, and about which world. */
type Asking =
  | { kind: "rename"; world: World }
  | { kind: "copy"; world: World }
  | { kind: "delete"; world: World }
  | { kind: "backups"; world: World }
  | null;


/**
 * An instance's singleplayer worlds.
 *
 * Everything here but the two listings is refused by the backend while the
 * game is running, because Minecraft holds its world open and only writes
 * `level.dat` when it saves — so a copy taken underneath it has region files
 * and nothing to identify them. The buttons are disabled while it runs and the
 * banner says why, rather than letting somebody press one and read a refusal.
 *
 * The folder a world lives in is shown beside its name on purpose. They are
 * two different things and routinely differ, because Minecraft derives the
 * folder once and then only ever changes the name.
 */
export default function WorldsTab({ instance }: { instance: Instance }) {
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  const [worlds, setWorlds] = useState<World[] | null>(null);
  const [backups, setBackups] = useState<WorldBackup[]>([]);
  const [filter, setFilter] = useState("");
  const [asking, setAsking] = useState<Asking>(null);
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [found, kept] = await Promise.all([
        api.listWorlds(instance.id),
        api.listWorldBackups(instance.id),
      ]);
      setWorlds(found);
      setBackups(kept);
    } catch (e) {
      reportError("Could not read this instance's worlds", e);
      setWorlds([]);
    }
  }, [instance.id, reportError]);

  useEffect(() => {
    setWorlds(null);
    void load();
  }, [load]);

  const shown = useMemo(() => {
    if (!worlds) return null;
    const q = filter.trim().toLowerCase();
    return q
      ? worlds.filter(
          (w) =>
            w.name.toLowerCase().includes(q) ||
            w.folder.toLowerCase().includes(q),
        )
      : worlds;
  }, [worlds, filter]);

  const backupsFor = (folder: string) => backups.filter((b) => b.world === folder);

  function close() {
    setAsking(null);
    setTyped("");
  }

  /** Run one world operation, keeping the list and the dialogs honest. */
  async function run<T>(
    key: string,
    whenItFails: string,
    job: () => Promise<T>,
    said?: (result: T) => { title: string; body?: string },
  ) {
    setBusy(key);
    try {
      const result = await job();
      await load();
      close();
      if (said) toast({ kind: "success", ...said(result) });
    } catch (e) {
      reportError(whenItFails, e);
    } finally {
      setBusy(null);
    }
  }

  async function importWorld() {
    // A world travels as a zip, and sits on disk as a folder — so both are
    // offered rather than making somebody zip a folder to bring it in.
    const picked = await openDialog({
      multiple: false,
      directory: false,
      filters: [{ name: "World", extensions: ["zip"] }],
    });
    if (typeof picked !== "string") return;
    await run(
      "import",
      "Nothing was imported",
      () => api.importWorld(instance.id, picked),
      (world) => ({
        title: `Imported ${world.name}`,
        body: `It is in saves/${world.folder}.`,
      }),
    );
  }

  async function importFolder() {
    const picked = await openDialog({ multiple: false, directory: true });
    if (typeof picked !== "string") return;
    await run(
      "import",
      "Nothing was imported",
      () => api.importWorld(instance.id, picked),
      (world) => ({
        title: `Imported ${world.name}`,
        body: `It is in saves/${world.folder}.`,
      }),
    );
  }

  const running = instance.running;

  return (
    <div className={styles.wrap}>
      <div className={styles.toolbar}>
        <div className={styles.searchWrap}>
          <Search size={14} className={styles.searchIcon} />
          <input
            className={clsx("input", styles.search)}
            placeholder="Filter worlds"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        </div>

        <div className={styles.tools}>
          <button
            className="btn"
            onClick={importFolder}
            disabled={running || busy === "import"}
            title="Bring in a world that is a folder on this machine"
          >
            <FolderInput size={14} />
            Import folder
          </button>
          <button
            className="btn btn-primary"
            onClick={importWorld}
            disabled={running || busy === "import"}
          >
            {busy === "import" ? (
              <Loader2 size={14} className="spin" />
            ) : (
              <FolderInput size={14} />
            )}
            Import a world
          </button>
        </div>
      </div>

      {running && (
        <div className={styles.locked}>
          <TriangleAlert size={15} />
          <span>
            The game has this instance open. Minecraft keeps its world open
            while it runs and only writes <code>level.dat</code> when it saves,
            so a copy taken now would be missing the part that identifies it.
            Stop the game to change anything here.
          </span>
        </div>
      )}

      <div className={styles.list}>
        {shown === null ? (
          <div className={styles.skeletons}>
            {Array.from({ length: 4 }, (_, i) => (
              <div key={i} className="skeleton" style={{ height: 58 }} />
            ))}
          </div>
        ) : shown.length === 0 ? (
          <div className={styles.empty}>
            <Earth size={32} className={styles.emptyIcon} />
            <p>
              {filter
                ? `No world matches “${filter}”.`
                : "No worlds yet. Play the instance to make one, or import a world you already have."}
            </p>
          </div>
        ) : (
          shown.map((world) => {
            const kept = backupsFor(world.folder);
            const mode =
              world.gameMode === undefined ? null : GAME_MODES[world.gameMode];
            // The app held both numbers and never compared them, so a world
            // one Minecraft ahead of its instance read as another genre tag.
            const ahead = Boolean(
              world.version && savedByNewer(world.version, instance.mcVersion),
            );
            return (
              <div
                key={world.folder}
                className={clsx(styles.row, world.trouble && styles.troubled)}
              >
                <div className={styles.worldIcon}>
                  {world.trouble ? <TriangleAlert size={16} /> : <Earth size={16} />}
                </div>

                <div className={styles.itemText}>
                  <div className={styles.itemTitle}>
                    <span className={styles.itemName}>{world.name}</span>
                    {/* Capitalised like the game mode beside it: both are
                        badges naming how the world plays, and one of them
                        was lowercase. */}
                    {world.hardcore && (
                      <span className="badge" title="Hardcore: one life">
                        Hardcore
                      </span>
                    )}
                    {mode && <span className="badge">{mode}</span>}
                    {world.version && (
                      <span
                        className={clsx("badge", ahead && "badge-warn")}
                        title={
                          ahead
                            ? `Saved by Minecraft ${world.version}; this instance runs ${instance.mcVersion}`
                            : "The Minecraft version this world was last opened with"
                        }
                      >
                        {ahead && <TriangleAlert size={10} />}
                        {world.version}
                      </span>
                    )}
                    {kept.length > 0 && (
                      <span className="badge" title="Backups kept for this world">
                        {kept.length} backup{kept.length === 1 ? "" : "s"}
                      </span>
                    )}
                  </div>
                  <div className={styles.itemMeta}>
                    {/* The folder, because it is not the name and somebody
                        will eventually need to find it on disk. */}
                    <span className={styles.folder}>saves/{world.folder}</span>
                    <span>{fileSize(world.size)}</span>
                    {world.lastPlayed ? (
                      // The game stores milliseconds; relativeTime takes
                      // seconds, like every other timestamp in the app.
                      <span>Last played {relativeTime(Math.floor(world.lastPlayed / 1000))}</span>
                    ) : (
                      <span>Never played</span>
                    )}
                  </div>
                  {world.trouble && <p className={styles.trouble}>{world.trouble}</p>}
                  {/* Said on the row and not only in the badge's tooltip: a
                      tooltip is read after, and what this costs is a world.
                      The badge's colour is what makes somebody look here. */}
                  {ahead && (
                    <p className={styles.ahead}>
                      Minecraft upgrades a world the first time a newer version
                      opens it and never puts it back, so opening this on{" "}
                      {instance.mcVersion} can drop what {world.version} wrote
                      — and a world is the one thing with no upstream to fetch
                      it back from. Back it up first, or run this instance on{" "}
                      {world.version}.
                    </p>
                  )}
                </div>

                <button
                  className="btn btn-ghost btn-icon btn-sm"
                  onClick={() => {
                    setAsking({ kind: "rename", world });
                    setTyped(world.name);
                  }}
                  disabled={running || Boolean(world.trouble)}
                  title={
                    world.trouble
                      ? "There is no level.dat here to hold a name"
                      : "Rename"
                  }
                  aria-label={`Rename ${world.name}`}
                >
                  <Pencil size={14} />
                </button>
                <button
                  className="btn btn-ghost btn-icon btn-sm"
                  onClick={() => {
                    setAsking({ kind: "copy", world });
                    setTyped(`${world.name} copy`);
                  }}
                  disabled={running}
                  title="Make a copy"
                  aria-label={`Copy ${world.name}`}
                >
                  <Copy size={14} />
                </button>
                <button
                  className="btn btn-sm"
                  onClick={() =>
                    void run(
                      world.folder,
                      `Could not back up ${world.name}`,
                      () => api.backUpWorld(instance.id, world.folder),
                      (backup) => ({
                        title: `Backed up ${world.name}`,
                        body: `${fileSize(backup.size)} kept beside the instance.`,
                      }),
                    )
                  }
                  disabled={running || busy === world.folder}
                  title="Zip the whole world and keep it beside the instance"
                >
                  {busy === world.folder ? (
                    <Loader2 size={13} className="spin" />
                  ) : (
                    <Archive size={13} />
                  )}
                  Back up
                </button>
                <button
                  className="btn btn-ghost btn-icon btn-sm"
                  onClick={() => setAsking({ kind: "backups", world })}
                  disabled={kept.length === 0}
                  title={
                    kept.length === 0
                      ? "No backups of this world yet"
                      : "Restore from a backup"
                  }
                  aria-label={`Backups of ${world.name}`}
                >
                  <ArchiveRestore size={14} />
                </button>
                <button
                  className="btn btn-ghost btn-icon btn-sm"
                  onClick={() => setAsking({ kind: "delete", world })}
                  disabled={running}
                  title="Delete"
                  aria-label={`Delete ${world.name}`}
                >
                  <Trash2 size={14} />
                </button>
              </div>
            );
          })
        )}
      </div>

      {asking?.kind === "rename" && (
        <Modal
          title={`Rename ${asking.world.name}`}
          subtitle="This changes the name Minecraft shows. The folder it lives in stays as it is, which is what the game itself does."
          onClose={close}
          width={440}
          footer={
            <>
              <button className="btn" onClick={close}>
                Cancel
              </button>
              <button
                className="btn btn-primary"
                onClick={() =>
                  void run(
                    "rename",
                    `Could not rename ${asking.world.name}`,
                    () => api.renameWorld(instance.id, asking.world.folder, typed),
                    (world) => ({ title: `Renamed to ${world.name}` }),
                  )
                }
                disabled={!typed.trim() || busy === "rename"}
              >
                {busy === "rename" && <Loader2 size={14} className="spin" />}
                Rename
              </button>
            </>
          }
        >
          <label className={styles.field}>
            <span className={styles.label}>World name</span>
            <input
              className="input"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              autoFocus
            />
          </label>
          <p className={styles.note}>
            It will stay in <code>saves/{asking.world.folder}</code>. Only the
            name inside <code>level.dat</code> changes; every other key in that
            file, including anything your mods wrote, is left untouched.
          </p>
        </Modal>
      )}

      {asking?.kind === "copy" && (
        <Modal
          title={`Copy ${asking.world.name}`}
          subtitle="A new world with its own folder, independent of this one."
          onClose={close}
          width={440}
          footer={
            <>
              <button className="btn" onClick={close}>
                Cancel
              </button>
              <button
                className="btn btn-primary"
                onClick={() =>
                  void run(
                    "copy",
                    `Could not copy ${asking.world.name}`,
                    () => api.copyWorld(instance.id, asking.world.folder, typed),
                    (world) => ({
                      title: `Copied to ${world.name}`,
                      body: `It is in saves/${world.folder}.`,
                    }),
                  )
                }
                disabled={!typed.trim() || busy === "copy"}
              >
                {busy === "copy" && <Loader2 size={14} className="spin" />}
                Make a copy
              </button>
            </>
          }
        >
          <label className={styles.field}>
            <span className={styles.label}>Name for the copy</span>
            <input
              className="input"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              autoFocus
            />
          </label>
          <p className={styles.note}>
            The copy gets this name both in its folder and inside its{" "}
            <code>level.dat</code>, so the two agree. If a folder of that name
            already exists it is refused rather than written over.
          </p>
        </Modal>
      )}

      {asking?.kind === "delete" && (
        <Modal
          title={`Delete ${asking.world.name}?`}
          subtitle="A backup is taken first, and it stays after the world is gone."
          onClose={close}
          width={460}
          footer={
            <>
              <button className="btn" onClick={close}>
                Cancel
              </button>
              <button
                className="btn btn-danger"
                onClick={() =>
                  void run(
                    "delete",
                    `Could not delete ${asking.world.name}`,
                    () => api.deleteWorld(instance.id, asking.world.folder),
                    (backup) => ({
                      title: `Deleted ${asking.world.name}`,
                      body: `Its backup, ${backup.name}, is under Backups on this screen.`,
                    }),
                  )
                }
                disabled={busy === "delete"}
              >
                {busy === "delete" && <Loader2 size={14} className="spin" />}
                Back up and delete
              </button>
            </>
          }
        >
          <p className={styles.note}>
            {fileSize(asking.world.size)} will be zipped into this instance's{" "}
            <code>world-backups</code> folder before{" "}
            <code>saves/{asking.world.folder}</code> is removed. The backup is
            not optional — a world is hours of play and there is no undo
            anywhere else. You can restore it from this screen, or delete the
            backup too once you are sure.
          </p>
        </Modal>
      )}

      {asking?.kind === "backups" && (
        <Modal
          title={`Backups of ${asking.world.name}`}
          subtitle="Restoring puts these files back over the world. Anything added since is left alone."
          onClose={close}
          width={520}
        >
          <div className={styles.backups}>
            {backupsFor(asking.world.folder).map((backup) => (
              <div key={backup.name} className={styles.backupRow}>
                <div className={styles.itemText}>
                  <span className={styles.backupName}>{backup.name}</span>
                  <div className={styles.itemMeta}>
                    <span>{fileSize(backup.size)}</span>
                    <span>{relativeTime(backup.createdAt)}</span>
                  </div>
                </div>
                <button
                  className="btn btn-sm"
                  onClick={() =>
                    void run(
                      backup.name,
                      `Could not restore ${backup.name}`,
                      () =>
                        api.restoreWorld(
                          instance.id,
                          asking.world.folder,
                          backup.name,
                        ),
                      (world) => ({ title: `Restored ${world.name}` }),
                    )
                  }
                  disabled={running || busy === backup.name}
                >
                  {busy === backup.name ? (
                    <Loader2 size={13} className="spin" />
                  ) : (
                    <ArchiveRestore size={13} />
                  )}
                  Restore
                </button>
                <button
                  className="btn btn-ghost btn-icon btn-sm"
                  onClick={() =>
                    void run(backup.name, `Could not delete ${backup.name}`, () =>
                      api.deleteWorldBackup(
                        instance.id,
                        asking.world.folder,
                        backup.name,
                      ),
                    )
                  }
                  disabled={busy === backup.name}
                  title="Delete this backup"
                  aria-label={`Delete backup ${backup.name}`}
                >
                  <Trash2 size={14} />
                </button>
              </div>
            ))}
          </div>
        </Modal>
      )}
    </div>
  );
}

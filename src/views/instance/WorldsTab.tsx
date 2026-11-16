import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import {
  Archive,
  ArchiveRestore,
  Copy,
  Earth,
  FileArchive,
  FolderInput,
  Loader2,
  Pencil,
  Search,
  TriangleAlert,
  Trash2,
} from "lucide-react";

import Modal from "../../components/Modal";
import Tooltip from "../../components/Tooltip";
import { api, errorMessage } from "../../lib/api";
import { GAME_MODES, fileSize, relativeTime } from "../../lib/format";
import { savedByNewer } from "../../lib/mcversion";
import { actionHint } from "../../lib/shortcuts";
import { useActionShortcut } from "../../lib/useActionShortcut";
import { useStore } from "../../lib/store";
import type { Instance, World, WorldBackup } from "../../lib/types";

import styles from "./WorldsTab.module.css";

/** Which dialog is open, and about which world. */
type Asking =
  | { kind: "rename"; world: World }
  | { kind: "copy"; world: World }
  | { kind: "delete"; world: World }
  | { kind: "backups"; world: World }
  | { kind: "deleteBackup"; world: World; backup: WorldBackup }
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
  const hasWorlds = Boolean(worlds && worlds.length > 0);
  /**
   * Why the folder could not be read, when it could not.
   *
   * A world is the one thing in this app with nothing upstream to fetch it
   * back from, so the difference between "there are none" and "I could not
   * look" is the difference between a fact and a false alarm about data
   * nobody can restore.
   */
  const [failed, setFailed] = useState<string | null>(null);
  const [backups, setBackups] = useState<WorldBackup[]>([]);
  const [filter, setFilter] = useState("");
  const filterBox = useRef<HTMLInputElement>(null);

  /**
   * Ctrl+F reaches this tab's filter, as it does the two Content tabs'.
   *
   * Only once there is something to filter: the box is off until a world
   * exists, and a key that focuses a disabled input would cost the webview's
   * own find bar and give nothing back. The hint beside it is gated the same
   * way, so what is printed and what works are the same thing.
   */
  useActionShortcut("focusFilter", (e) => {
    const box = filterBox.current;
    if (!box || box.disabled) return;
    e.preventDefault();
    box.focus();
    box.select();
  });
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
      setFailed(null);
    } catch (e) {
      reportError("Could not read this instance's worlds", e);
      // Still [] rather than null, so the skeletons stop; the branch for this
      // is checked before the one for an empty folder.
      setWorlds([]);
      setFailed(errorMessage(e));
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

  /**
   * Whether anything in the list was saved by a newer Minecraft than this
   * instance runs.
   *
   * What that costs and what to do about it is the same paragraph for every
   * one of them, so it is said once above the list rather than once per row —
   * an instance whose version was rolled back has every world in this state,
   * and the paragraph printed all the way down it. The rows keep the half
   * that differs, which is their own two version numbers.
   */
  const anyAhead = useMemo(
    () =>
      (shown ?? []).some(
        (w) => w.version && savedByNewer(w.version, instance.mcVersion),
      ),
    [shown, instance.mcVersion],
  );

  const backupsFor = (folder: string) => backups.filter((b) => b.world === folder);

  /**
   * Backups whose world is no longer on disk, newest first, by folder.
   *
   * Every route to a backup used to run through its world's row — the button
   * at the end of it is the only thing that opens the list — so deleting a
   * world put its backup out of reach the moment the row went. Which is the
   * one outcome `world-backups/` living outside `.minecraft` exists to
   * prevent: a copy is supposed to outlive the world it is a copy of.
   *
   * The backend was always willing. `worlds::restore` does `create_dir_all`
   * on the world folder before extracting, so restoring into a world that is
   * gone is what it already does; only the way to ask was missing.
   */
  const orphaned = useMemo(() => {
    if (!worlds) return [];
    const live = new Set(worlds.map((w) => w.folder));
    const byFolder = new Map<string, WorldBackup[]>();
    for (const b of backups) {
      if (live.has(b.world)) continue;
      const list = byFolder.get(b.world) ?? [];
      list.push(b);
      byFolder.set(b.world, list);
    }
    return [...byFolder.entries()]
      .map(([folder, list]) => ({
        folder,
        list: [...list].sort((a, b) => b.createdAt - a.createdAt),
      }))
      .sort((a, b) => b.list[0].createdAt - a.list[0].createdAt);
  }, [worlds, backups]);

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

  /**
   * Delete one backup, and come back to the list rather than closing it.
   *
   * Every other operation here ends in `close()`, which is right when the
   * dialog was about the thing that just changed. This one is about one row of
   * several: closing would take away the only place the rest can be read, so
   * the single visible consequence of the delete would be the whole dialog
   * disappearing. It closes only when that was the last backup, because an
   * empty list says nothing — which is the same reason the button that opens
   * the dialog is disabled when there are none.
   */
  async function deleteBackup(world: World, backup: WorldBackup) {
    const last = backupsFor(world.folder).length <= 1;
    setBusy(backup.name);
    try {
      await api.deleteWorldBackup(instance.id, world.folder, backup.name);
      await load();
      setAsking(last ? null : { kind: "backups", world });
      toast({
        kind: "success",
        title: `Deleted ${backup.name}`,
        body: last
          ? `${world.name} has no backups left.`
          : `The other backups of ${world.name} are untouched.`,
      });
    } catch (e) {
      reportError(`Could not delete ${backup.name}`, e);
    } finally {
      setBusy(null);
    }
  }

  // The two buttons reach the same import — the backend takes a path and looks
  // at what is on the end of it — and differ in exactly one thing, which is
  // what each can pick: a `.zip` file, or a directory. So that is what their
  // labels say. Their keys are apart so the spinner lands on the button that
  // was pressed; it used to be one key, and a folder import spun the other.
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
      "importZip",
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
      "importFolder",
      "Nothing was imported",
      () => api.importWorld(instance.id, picked),
      (world) => ({
        title: `Imported ${world.name}`,
        body: `It is in saves/${world.folder}.`,
      }),
    );
  }

  const running = instance.running;
  /** One import at a time, whichever of the two buttons started it. */
  const importing = busy === "importZip" || busy === "importFolder";
  // The backups dialog's delete button, said twice down there — once in the
  // bubble, once in a `title` for the states where the button is off — so the
  // two cannot drift, the same way `renameSays` is.
  const deleteBackupSays = running
    ? "Stop the game to delete a backup"
    : "Delete this backup";

  return (
    <div className={styles.wrap}>
      <div className={styles.toolbar}>
        <div className={styles.searchWrap}>
          <Search size={14} className={styles.searchIcon} />
          {/* Off while there is nothing to filter, which is what the
              Dependencies tab's own Find box already does. A live box over an
              empty list answered "No worlds yet. Play the instance to make
              one" with "No world matches “zzz”" — a correct sentence and the
              wrong diagnosis of why the screen is empty. */}
          <input
            ref={filterBox}
            className={clsx("input", styles.search, hasWorlds && styles.searchHinted)}
            placeholder="Filter worlds"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            disabled={!hasWorlds}
          />
          {/* The binding, printed in the box it reaches — an action has no
              sidebar row to be found from, so the thing it presses is the
              only place it can be come across. Not while the box is off:
              a key printed beside a disabled control is one that does
              nothing when pressed. */}
          {hasWorlds && (
            <kbd className={styles.searchKey}>{actionHint("focusFilter")}</kbd>
          )}
        </div>

        <div className={styles.tools}>
          <button
            className="btn"
            onClick={importFolder}
            disabled={running || importing}
            title="Bring in a world that is already a folder on this machine — another launcher's saves, or one copied off a server"
          >
            {busy === "importFolder" ? (
              <Loader2 size={14} className="spin" />
            ) : (
              <FolderInput size={14} />
            )}
            Import a folder
          </button>
          <button
            className="btn btn-primary"
            onClick={importWorld}
            disabled={running || importing}
            title="Bring in a world that arrived as a .zip, the way a downloaded one does"
          >
            {busy === "importZip" ? (
              <Loader2 size={14} className="spin" />
            ) : (
              <FileArchive size={14} />
            )}
            Import a .zip
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

      {anyAhead && (
        <div className={styles.aheadBanner}>
          <TriangleAlert size={15} />
          <span>
            Something below was saved by a newer Minecraft than this instance
            runs. Minecraft upgrades a world the first time a newer version
            opens it and never puts it back, so opening one of those here can
            drop what the newer version wrote — and a world is the one thing
            with no upstream to fetch it back from. Back it up first, or run
            this instance on the version that wrote it.
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
        ) : failed ? (
          /* Before the empty state, and the reason is the whole file: the
             folder was not read, so whether it holds twenty worlds or none is
             not known here. Saying "No worlds yet" and inviting somebody to go
             and make one is the same mistake the filter box above already has
             a comment about — a correct-sounding sentence and the wrong
             diagnosis of why the screen is empty — except that this screen is
             about the one thing Geode cannot fetch again, so being wrong here
             reads as "they are gone". */
          <div className={styles.empty}>
            <TriangleAlert size={40} className={styles.emptyIcon} />
            <h2>Could not read this instance&apos;s worlds</h2>
            <p>
              Which is not the same as there being none. Nothing here has been
              changed or removed — the folder was not read, so what is in it is
              unknown.
            </p>
            <p className={styles.why}>{failed}</p>
            <button className="btn" onClick={() => void load()}>
              Try again
            </button>
          </div>
        ) : worlds?.length === 0 ? (
          /* The shape Library and Plans use, less the button: the thing that
             makes a world is the game, not this screen, and the two ways to
             bring one in are already a press away in the toolbar above. A
             third copy of them here is the two-primaries problem those screens
             solved by not duplicating. What the heading and the body add is
             what the one sentence left out — that worlds come from inside
             Minecraft, and which two forms an import takes. */
          <div className={styles.empty}>
            <Earth size={40} className={styles.emptyIcon} />
            <h2>No worlds yet</h2>
            <p>
              Minecraft makes the worlds: launch this instance, create one in
              the game, and it is listed here. A world you already have can
              come in instead — as the .zip a download arrives in, or the
              folder another launcher keeps it in.
            </p>
          </div>
        ) : shown.length === 0 ? (
          /* A filter that matched nothing, which is a different fact from an
             empty instance and gets the one line Library and Servers give it
             rather than a screen of its own. Only reachable with worlds in the
             list, because the box above is off without them. */
          <p className={styles.noResults}>No world matches “{filter}”.</p>
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
            // Said twice below — once in the bubble, once in a `title` for
            // the states where the button is off — so the two cannot drift.
            const renameSays = world.trouble
              ? "There is no level.dat here to hold a name"
              : "Rename";
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
                    {/* The badges carry their sentence in this app's own
                        bubble, the way the Content tab's do: the word on a
                        badge is the short form and the sentence is the whole
                        of what it means, which is a `title`'s worst case — a
                        second of waiting, drawn in neither theme. No
                        `tabIndex` with it, unlike that tab's: there is no
                        arrow walk along this row for a badge to join, so the
                        only stop on offer is a tab stop, and three more of
                        them before the row's buttons cost more than the
                        bubble is worth. */}
                    {/* Capitalised like the game mode beside it: both are
                        badges naming how the world plays, and one of them
                        was lowercase. */}
                    {world.hardcore && (
                      <Tooltip text="Hardcore: one life">
                        <span className="badge">Hardcore</span>
                      </Tooltip>
                    )}
                    {mode && <span className="badge">{mode}</span>}
                    {world.version && (
                      <Tooltip
                        text={
                          ahead
                            ? `Saved by Minecraft ${world.version}; this instance runs ${instance.mcVersion}`
                            : "The Minecraft version this world was last opened with"
                        }
                      >
                        <span className={clsx("badge", ahead && "badge-warn")}>
                          {ahead && <TriangleAlert size={10} />}
                          {world.version}
                        </span>
                      </Tooltip>
                    )}
                    {kept.length > 0 && (
                      <Tooltip text="Backups kept for this world">
                        <span className="badge">
                          {kept.length} backup{kept.length === 1 ? "" : "s"}
                        </span>
                      </Tooltip>
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
                      The badge's colour is what makes somebody look here.
                      Only the two version numbers, because they are the only
                      part of this that differs row to row — what the mismatch
                      costs is said once in the banner above the list. */}
                  {ahead && (
                    <p className={styles.ahead}>
                      Saved by Minecraft {world.version}; this instance runs{" "}
                      {instance.mcVersion}.
                    </p>
                  )}
                </div>

                {/* The four icon-only buttons say what they do in this app's
                    own bubble rather than the platform's: a `title` waits out
                    the platform's delay, is drawn in neither theme, and never
                    appears on keyboard focus — and the pair around "Back up"
                    is the same archive shape twice, one writing and one
                    reading back, which is not a difference anybody should
                    have to hover to find. The `title` stays for the states
                    where a button is off, because a disabled control receives
                    neither pointer nor focus; it says what the press would
                    do, and why it is off is the banner's job. "Back up" keeps
                    its `title` alone — it has a visible word, and its
                    sentence only elaborates. */}
                <Tooltip text={renameSays}>
                  <button
                    className="btn btn-ghost btn-icon btn-sm"
                    onClick={() => {
                      setAsking({ kind: "rename", world });
                      setTyped(world.name);
                    }}
                    disabled={running || Boolean(world.trouble)}
                    title={running || world.trouble ? renameSays : undefined}
                    aria-label={`Rename ${world.name}`}
                  >
                    <Pencil size={14} />
                  </button>
                </Tooltip>
                <Tooltip text="Make a copy">
                  <button
                    className="btn btn-ghost btn-icon btn-sm"
                    onClick={() => {
                      setAsking({ kind: "copy", world });
                      setTyped(`${world.name} copy`);
                    }}
                    disabled={running}
                    title={running ? "Make a copy" : undefined}
                    aria-label={`Copy ${world.name}`}
                  >
                    <Copy size={14} />
                  </button>
                </Tooltip>
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
                <Tooltip
                  text={
                    kept.length === 0
                      ? "No backups of this world yet"
                      : "Restore from a backup"
                  }
                >
                  <button
                    className="btn btn-ghost btn-icon btn-sm"
                    onClick={() => setAsking({ kind: "backups", world })}
                    disabled={kept.length === 0}
                    title={
                      kept.length === 0 ? "No backups of this world yet" : undefined
                    }
                    aria-label={`Backups of ${world.name}`}
                  >
                    <ArchiveRestore size={14} />
                  </button>
                </Tooltip>
                <Tooltip text="Delete">
                  <button
                    className="btn btn-ghost btn-icon btn-sm"
                    onClick={() => setAsking({ kind: "delete", world })}
                    disabled={running}
                    title={running ? "Delete" : undefined}
                    aria-label={`Delete ${world.name}`}
                  >
                    <Trash2 size={14} />
                  </button>
                </Tooltip>
              </div>
            );
          })
        )}
      </div>

      {/* Only ever non-empty after a delete, so it is not a section somebody
          has to read past: it appears when there is something in it and is
          gone again the moment the world is back. Restoring the newest is
          enough to un-strand the rest — the folder exists again afterwards,
          its row comes back, and the row's own button lists every backup it
          has, older ones included. */}
      {orphaned.length > 0 && (
        <div className={styles.orphans}>
          <h2 className={styles.orphansTitle}>Backups with no world</h2>
          <p className={styles.orphansNote}>
            These outlived the world they were taken from. Restoring one puts
            the world back where it was.
          </p>
          {orphaned.map(({ folder, list }) => (
            <div key={folder} className={styles.row}>
              <div className={styles.worldIcon}>
                <FileArchive size={16} />
              </div>
              <div className={styles.itemText}>
                <div className={styles.itemTitle}>
                  <span className={styles.itemName}>{folder}</span>
                </div>
                <div className={styles.itemMeta}>
                  <span>
                    {list.length === 1 ? "1 backup" : `${list.length} backups`}
                  </span>
                  {/* The whole group's size, because the count beside it is the
                      whole group's. Printing the newest one's bytes here read
                      as the size of all of them — on the one row in this app
                      that is somebody deciding whether a thing they cannot
                      re-download is worth keeping. The timestamp is the
                      newest and says so. */}
                  <span>
                    {fileSize(list.reduce((n, b) => n + b.size, 0))}
                  </span>
                  <span>newest {relativeTime(list[0].createdAt)}</span>
                </div>
              </div>
              <button
                className="btn btn-sm"
                disabled={running || busy === folder}
                // What it does, not why it is off: the banner above says that
                // once for the whole screen, and the six buttons on the row
                // above keep their ordinary titles while disabled.
                title={`Put ${folder} back from ${list[0].name}`}
                onClick={() =>
                  void run(
                    folder,
                    `Could not restore ${folder}`,
                    () => api.restoreWorld(instance.id, folder, list[0].name),
                    (world) => ({ title: `Restored ${world.name}` }),
                  )
                }
              >
                {busy === folder ? (
                  <Loader2 size={14} className="spin" />
                ) : (
                  <ArchiveRestore size={14} />
                )}
                Restore
              </button>
            </div>
          ))}
        </div>
      )}

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
                      body: `Its backup, ${backup.name}, is under "Backups with no world" below.`,
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
            anywhere else. You can restore it from this screen.
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
                {/* Guarded on `running` like Restore beside it, though for a
                    different reason. The backend does not refuse this one —
                    a zip in `world-backups/` is not a file the game holds
                    open — but while Minecraft runs, Restore is off and Back
                    up is off, so this is the one moment when a backup is the
                    only state you could get back to and you can neither use
                    it nor take another. It is also what the banner above
                    already promises: stop the game to change anything here. */}
                <Tooltip text={deleteBackupSays}>
                  <button
                    className="btn btn-ghost btn-icon btn-sm"
                    onClick={() =>
                      setAsking({ kind: "deleteBackup", world: asking.world, backup })
                    }
                    disabled={running || busy === backup.name}
                    title={
                      running || busy === backup.name ? deleteBackupSays : undefined
                    }
                    aria-label={`Delete backup ${backup.name}`}
                  >
                    <Trash2 size={14} />
                  </button>
                </Tooltip>
              </div>
            ))}
          </div>
        </Modal>
      )}

      {/* Asked as a dialog of its own rather than one nested in the list,
          because two Modals open at once both listen for Escape on the
          document and one press would dismiss both. Cancel comes straight
          back to the list, and so does a successful delete. */}
      {asking?.kind === "deleteBackup" && (
        <Modal
          title={`Delete this backup of ${asking.world.name}?`}
          subtitle="Nothing is taken first this time. The backup is the copy."
          onClose={() => setAsking({ kind: "backups", world: asking.world })}
          width={460}
          footer={
            <>
              <button
                className="btn"
                onClick={() => setAsking({ kind: "backups", world: asking.world })}
              >
                Cancel
              </button>
              <button
                className="btn btn-danger"
                onClick={() => void deleteBackup(asking.world, asking.backup)}
                disabled={busy === asking.backup.name}
              >
                {busy === asking.backup.name && (
                  <Loader2 size={14} className="spin" />
                )}
                Delete the backup
              </button>
            </>
          }
        >
          <p className={styles.note}>
            <code>{asking.backup.name}</code> is{" "}
            {fileSize(asking.backup.size)} taken{" "}
            {relativeTime(asking.backup.createdAt)}, and it is the only copy of{" "}
            {asking.world.name} as it was then. A world has no upstream to
            fetch a region file back from, so once this zip is gone that state
            is nowhere — the file is removed outright, not moved to a trash.
            {backupsFor(asking.world.folder).length <= 1 &&
              ` It is also the last backup of ${asking.world.name}, which would be left with none.`}
          </p>
        </Modal>
      )}
    </div>
  );
}

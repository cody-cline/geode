import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import {
  ArrowLeftRight,
  ArrowUpCircle,
  FilePlus2,
  Link2,
  ListChecks,
  Loader2,
  Package,
  Power,
  PowerOff,
  RefreshCw,
  Search,
  Server as ServerIcon,
  Trash2,
  TriangleAlert,
} from "lucide-react";

import Modal from "../../components/Modal";
import Tooltip from "../../components/Tooltip";
import { api, errorMessage } from "../../lib/api";
import { fileSize, providerLabel, shownName, titleIsFileName } from "../../lib/format";
import { rovingKey } from "../../lib/rovingFocus";
import { actionHint } from "../../lib/shortcuts";
import { useActionShortcut } from "../../lib/useActionShortcut";
import { useRowSelection } from "../../lib/useRowSelection";
import { useStore } from "../../lib/store";
import type {
  ContentItem,
  ListedClientOnly,
  Server,
  Sided,
  UpdateCandidate,
} from "../../lib/types";

// The instance tab's stylesheet, deliberately. The two panes are the same
// screen with different folders behind them, and a second copy of these rules
// would start identical and drift.
import styles from "../instance/ContentTab.module.css";

// And its list keyboard, for exactly the same reason: one list, two folders
// behind it. What a row is — the one tab stop, arrows between rows and along
// one — is a fact about this screen, and the screen is over there.
import { rowKeys } from "../instance/ContentTab";

/**
 * The folders a server actually has. Narrower than `ContentKind`: a server has
 * no resource packs or shaders of its own, and offering to install one would
 * promise somewhere to put it that does not exist. Client mods are the mods it
 * keeps for its players and never loads.
 */
export type ServerFolder = "plugin" | "mod" | "clientmod" | "datapack";

const PICKS: Record<ServerFolder, { label: string; extensions: string[] }> = {
  plugin: { label: "Plugins", extensions: ["jar"] },
  mod: { label: "Mods", extensions: ["jar"] },
  clientmod: { label: "Client mods", extensions: ["jar"] },
  datapack: { label: "Data Packs", extensions: ["zip"] },
};

/** The same update: one project, in one folder. */
const sameUpdate = (a: UpdateCandidate, b: UpdateCandidate) =>
  a.provider === b.provider && a.projectId === b.projectId && a.kind === b.kind;

/** Whose word a side is on, said in a sentence an admin can act on. */
const SAID_BY: Record<Sided["saidBy"], string> = {
  admin: "You said so, and nothing here overrules it.",
  jar: "Its own jar declares it.",
  listing: "The platform lists it that way, which an author can get wrong.",
  server: "The server it came from classified it.",
  folder: "It sits in Client mods.",
  nobody: "",
};

/**
 * The side badge for a row in Mods: where the mod belongs, and on whose word.
 *
 * Every mod gets one, the ones nobody has sided included. Badges used to
 * appear only for client and server, so a mod no source says anything about —
 * most of NeoForge, and every CurseForge mod — looked exactly like one
 * genuinely needed on both sides, and an admin could not tell which row they
 * were reading.
 */
function sideBadge(sided: Sided): { label: string; title: string } {
  const why = SAID_BY[sided.saidBy];
  switch (sided.side) {
    case "client":
      return {
        label: "client",
        title: `Runs only in players' games. ${why} Players get it with the pack; move it to Client mods to keep it out of this server's mods folder.`,
      };
    case "server":
      return {
        label: "server",
        title: `Only this server needs it. ${why} Players get it too, and can keep server-side mods turned off.`,
      };
    case "both":
      return sided.saidBy === "nobody"
        ? {
            label: "side not declared",
            title:
              "Nothing says which side this belongs on — not its own jar, not the platform's listing. So this server loads it and players get it, which is what every loader does with a mod that does not say.",
          }
        : {
            label: "both",
            title: `Needed on this server and in players' games. ${why}`,
          };
  }
}

export default function ServerContentTab({
  server,
  onBrowse,
}: {
  server: Server;
  onBrowse: (folder: ServerFolder) => void;
}) {
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const refreshServers = useStore((s) => s.refreshServers);

  // Capabilities come from the server rather than its kind: a Fabric server
  // with Cardboard installed takes Bukkit plugins, and knowing what "fabric"
  // means would never reveal that. Datapacks belong to the world, so even a
  // vanilla server has somewhere to put them.
  const folders = useMemo(() => {
    const out: { id: ServerFolder; label: string }[] = [];
    if (server.takesPlugins) out.push({ id: "plugin", label: PICKS.plugin.label });
    if (server.takesMods) {
      out.push({ id: "mod", label: PICKS.mod.label });
      out.push({ id: "clientmod", label: PICKS.clientmod.label });
    }
    if (server.hasWorld) out.push({ id: "datapack", label: PICKS.datapack.label });
    return out;
  }, [server.takesPlugins, server.takesMods, server.hasWorld]);

  const [folder, setFolder] = useState<ServerFolder>(folders[0]?.id ?? "mod");
  const [items, setItems] = useState<ContentItem[] | null>(null);
  /**
   * Why the folder could not be read, when it could not.
   *
   * `items` at `[]` is what makes this panel say the folder is empty *and*
   * that anything added lands in it safely. On a server a failed listing is
   * most often a permission or a mount problem, which is exactly the case
   * where the next write fails too — so the one claim is unknown and the
   * other is wrong.
   */
  const [failed, setFailed] = useState<string | null>(null);
  const [clientOnly, setClientOnly] = useState<ListedClientOnly[]>([]);
  const [filter, setFilter] = useState("");
  const filterBox = useRef<HTMLInputElement>(null);
  const [updates, setUpdates] = useState<UpdateCandidate[]>([]);
  const [checking, setChecking] = useState(false);
  const [updating, setUpdating] = useState<UpdateCandidate | null>(null);
  const [linking, setLinking] = useState(false);
  const [addingCardboard, setAddingCardboard] = useState(false);
  // The row the keyboard was last on, which is the row that answers Tab.
  const [keyed, setKeyed] = useState<string | null>(null);
  const [bulkBusy, setBulkBusy] = useState(false);
  const [askingDelete, setAskingDelete] = useState(false);

  const shown = useMemo(() => {
    if (!items) return null;
    const q = filter.trim().toLowerCase();
    return q
      ? items.filter(
          (i) =>
            i.name.toLowerCase().includes(q) ||
            i.fileName.toLowerCase().includes(q),
        )
      : items;
  }, [items, filter]);

  // Which rows the next press acts on, out of the same hook the instance's
  // pane uses — the third thing these two share, after the stylesheet and the
  // list keyboard below. An admin with forty plugins was making forty round
  // trips because this lived in that file and nowhere else.
  const {
    selected,
    picked,
    pickedShown,
    pick,
    selectAll,
    clear,
    followRenames,
    forget,
  } = useRowSelection(items, shown);

  const load = useCallback(async () => {
    try {
      setItems(await api.listServerContent(server.id, folder));
      setFailed(null);
    } catch (e) {
      reportError("Could not read the server folder", e);
      // Still [] rather than null, so the skeletons stop; the branch for this
      // is checked before the one for an empty folder.
      setItems([]);
      setFailed(errorMessage(e));
    }
    // The offer about mods only the listing calls client-side. In its own try,
    // and silent if it fails: it is a suggestion, and it must not be able to
    // stand in the way of the folder it is about.
    try {
      setClientOnly(
        folder === "mod" ? await api.listServerClientOnlyMods(server.id) : [],
      );
    } catch {
      setClientOnly([]);
    }
  }, [server.id, folder, reportError]);

  // Read again when the server starts or stops as well: starting moves client
  // mods out of Mods, and a row left behind would name a file no longer there.
  //
  // `failed` is cleared here beside `items`, not only on the next success: the
  // Check-updates title reads it to say which folder could not be read, and a
  // flag left standing from the last folder would put that sentence over a
  // folder nothing has tried to read yet.
  useEffect(() => {
    setItems(null);
    setFailed(null);
    // A selection is of files in one folder, so changing folders drops it
    // rather than carrying names that are not in the list any more. Starting
    // the server moves client mods out of Mods, which is the same thing
    // happening to the list under a selection, so that drops it too.
    clear();
    void load();
  }, [load, server.running, clear]);

  /**
   * The same key into the Filter box as the instance tab's, bound here for the
   * same reason the list keyboard is shared: these two panes are one screen
   * with different folders behind them, and a key that worked on one half and
   * not the other would leave somebody having to remember which half they are
   * on. Bound with the tab rather than at the window, since the press means
   * nothing on a screen with no filter to reach.
   */
  useActionShortcut("focusFilter", (e) => {
    const box = filterBox.current;
    // Only once there is a box. A proxy's Content tab is the panel below
    // instead, with no filter in it, and taking the key there would cost the
    // webview's find bar and give nothing back.
    if (!box) return;
    // Where there is one, the webview's find bar is not what this is for, and
    // the text that is there is selected so the next keystroke replaces the
    // old filter.
    e.preventDefault();
    box.focus();
    box.select();
  });

  // Not prevented: Escape has no default of its own here, and anything else
  // listening for it should still get it. There is no drawer over this
  // toolbar to hold the press back for, the way the instance's pane has — the
  // one mode on top of this screen is the delete dialog, and `Modal` claims
  // back as an overlay, which `useActionShortcut` already stands aside for.
  useActionShortcut("clearSelection", clear);

  const offCount = picked.filter((i) => !i.enabled).length;
  const onCount = picked.filter((i) => i.enabled).length;

  // The same project can sit in Mods and in Client mods, and each row is
  // offered only its own folder's update.
  const updateFor = (item: ContentItem) =>
    updates.find(
      (u) =>
        u.projectId === item.projectId &&
        u.provider === item.provider &&
        u.kind === folder,
    );

  async function toggle(item: ContentItem) {
    try {
      const renamed = await api.setServerContentEnabled(
        server.id,
        folder,
        item.fileName,
        !item.enabled,
      );
      followRenames(new Map([[item.fileName, renamed]]));
      await load();
    } catch (e) {
      reportError("Could not toggle that file", e);
    }
  }

  /**
   * Turn every selected file on, or every one off.
   *
   * A loop over the same command a row presses, not a new one: each rename is
   * independent of the others and the list is reread once at the end rather
   * than after every one. Files already the way they are being asked to be are
   * left out, so "Disable" on a mixed selection is not a round trip per row
   * that changes nothing.
   */
  async function setPickedEnabled(enable: boolean) {
    const targets = picked.filter((i) => i.enabled !== enable);
    if (targets.length === 0) return;
    setBulkBusy(true);
    const moves = new Map<string, string>();
    try {
      for (const item of targets) {
        moves.set(
          item.fileName,
          await api.setServerContentEnabled(
            server.id,
            folder,
            item.fileName,
            enable,
          ),
        );
      }
    } catch (e) {
      // Whatever landed before the failure has landed, so this says how far it
      // got instead of implying none of it happened.
      reportError(
        moves.size > 0
          ? `${enable ? "Enabled" : "Disabled"} ${moves.size} of ${targets.length} files, then stopped`
          : `Nothing was ${enable ? "enabled" : "disabled"}`,
        e,
      );
    } finally {
      followRenames(moves);
      setBulkBusy(false);
      await load();
    }
  }

  /**
   * Delete every selected file.
   *
   * No exemption to count around, unlike the instance's pane: nothing in a
   * server's own folders belongs to anybody else, so the row's own Delete is
   * offered on every row here and a bulk delete can take all of them.
   */
  async function deletePicked() {
    setBulkBusy(true);
    const gone: string[] = [];
    try {
      for (const item of picked) {
        await api.removeServerContent(server.id, folder, item.fileName);
        gone.push(item.fileName);
      }
    } catch (e) {
      reportError(
        gone.length > 0
          ? `Deleted ${gone.length} of ${picked.length} files, then stopped`
          : "Nothing was deleted",
        e,
      );
    } finally {
      forget(gone);
      setAskingDelete(false);
      setBulkBusy(false);
      await Promise.all([load(), refreshServers()]);
    }
  }

  /**
   * Move a mod between Mods and Client mods, record and on/off state with it.
   * Only while the server is stopped: it read its mods folder when it started.
   *
   * Named by file rather than by row, because the offer above the list moves
   * mods too and has no row to hand over.
   */
  async function move(
    mod: { fileName: string; name: string },
    toClient = folder === "mod",
  ) {
    try {
      await api.moveServerMod(server.id, mod.fileName, toClient);
      await Promise.all([load(), refreshServers()]);
      toast({
        kind: "success",
        title: `Moved ${mod.name} to ${toClient ? "Client mods" : "Mods"}`,
      });
    } catch (e) {
      reportError(`Could not move ${mod.name}`, e);
    }
  }

  /**
   * Decline the offer for one jar. What that records is the one thing the
   * press actually said — this mod runs on this server — which is also what
   * stops it being offered again, since a person's answer outranks a listing.
   */
  async function keepInMods(mod: ListedClientOnly) {
    try {
      await api.setServerModSide(server.id, mod.file, "both");
      await load();
      toast({
        kind: "success",
        title: `${mod.name} stays in Mods`,
        body: "Marked as needed on both sides, which the Side control can change.",
      });
    } catch (e) {
      reportError(`Could not keep ${mod.name} in Mods`, e);
    }
  }

  /**
   * Say which side a mod belongs on, or hand the question back to the jar and
   * the listing. Nothing on disk moves: what changes is what this server's
   * pack tells players, and what this list says.
   */
  async function chooseSide(
    item: ContentItem,
    side: "client" | "server" | "both" | null,
  ) {
    try {
      await api.setServerModSide(server.id, item.fileName, side);
      await load();
    } catch (e) {
      reportError(`Could not set which side ${item.name} is on`, e);
    }
  }

  /**
   * Keep a mod on this server, or stop keeping it. The server goes on loading
   * it either way; what changes is that its pack carries nothing of it.
   */
  async function keepHere(item: ContentItem, keep: boolean) {
    try {
      await api.setServerModKept(server.id, folder, item.fileName, keep);
      await load();
      toast({
        kind: "success",
        title: keep
          ? `${item.name} stays on this server`
          : `${item.name} goes to players again`,
        body: keep
          ? "Players get nothing of it: not the file, not a link to it, not its name."
          : undefined,
      });
    } catch (e) {
      reportError(`Could not change what players get of ${item.name}`, e);
    }
  }

  async function remove(item: ContentItem) {
    try {
      await api.removeServerContent(server.id, folder, item.fileName);
      await Promise.all([load(), refreshServers()]);
    } catch (e) {
      reportError(`Could not delete ${item.name}`, e);
    }
  }

  async function addFiles() {
    const spec = PICKS[folder];
    const picked = await openDialog({
      multiple: true,
      filters: [{ name: spec.label, extensions: spec.extensions }],
    });
    const files = Array.isArray(picked) ? picked : picked ? [picked] : [];
    if (files.length === 0) return;

    try {
      const added = await api.addLocalServerContent(server.id, folder, files);
      await load();
      toast({
        kind: "success",
        title: `Added ${added} file${added === 1 ? "" : "s"}`,
      });
    } catch (e) {
      reportError("Could not add those files", e);
    }
  }

  async function linkUntracked() {
    setLinking(true);
    try {
      const report = await api.linkUntrackedServerContent(server.id);
      await Promise.all([load(), refreshServers()]);
      toast({
        kind: report.identified > 0 ? "success" : "info",
        title:
          report.identified > 0
            ? `Linked ${report.identified} file${report.identified === 1 ? "" : "s"}`
            : "Nothing new could be identified",
        body:
          report.unknown > 0
            ? `${report.unknown} file${report.unknown === 1 ? " could" : "s could"} not be named, so ${report.unknown === 1 ? "it stays" : "they stay"} untracked.`
            : undefined,
      });
    } catch (e) {
      reportError("Could not identify files", e);
    } finally {
      setLinking(false);
    }
  }

  async function checkUpdates() {
    setChecking(true);
    try {
      const found = await api.checkServerContentUpdates(server.id);
      setUpdates(found);
      toast({
        kind: found.length > 0 ? "info" : "success",
        title:
          found.length > 0
            ? `${found.length} update${found.length === 1 ? "" : "s"} available`
            : "Everything is up to date",
      });
    } catch (e) {
      reportError("Update check failed", e);
    } finally {
      setChecking(false);
    }
  }

  async function applyUpdate(candidate: UpdateCandidate) {
    setUpdating(candidate);
    try {
      await api.applyServerContentUpdate(
        server.id,
        candidate.provider,
        candidate.projectId,
        candidate.newVersionId,
        candidate.kind,
      );
      // Only this folder's copy was updated; the project's copy in the other
      // folder, if any, still has its own update waiting.
      setUpdates((prev) => prev.filter((u) => !sameUpdate(u, candidate)));
      await Promise.all([load(), refreshServers()]);
      toast({
        kind: "success",
        title: `Updated ${candidate.name}`,
        body: `${candidate.currentVersion} → ${candidate.newVersionNumber}`,
      });
    } catch (e) {
      reportError(`Could not update ${candidate.name}`, e);
    } finally {
      setUpdating(null);
    }
  }

  async function updateAll() {
    for (const candidate of [...updates]) {
      await applyUpdate(candidate);
    }
  }

  async function addCardboard() {
    setAddingCardboard(true);
    try {
      await api.addCardboard(server.id);
      // The server's capabilities change with it, so the view has to be
      // re-read rather than patched here.
      await refreshServers();
    } catch (e) {
      reportError("Could not add Cardboard", e);
    } finally {
      setAddingCardboard(false);
    }
  }

  const trackedCount = items?.filter((i) => i.projectId).length ?? 0;
  const untrackedCount = items?.filter((i) => i.fromDisk).length ?? 0;
  const noun = PICKS[folder].label.toLowerCase();

  // Which folder carries the strip's one tab stop. Normally the chosen one —
  // but what a server can hold is read off the server and changes under this
  // screen (adding Cardboard grows a Plugins folder), so a folder that has gone
  // must not leave the strip with no tab stop at all and nothing to Tab into.
  const folderStop = folders.some((f) => f.id === folder) ? folder : folders[0]?.id;

  // The one row that answers Tab. Checked against the list rather than
  // trusted, because `keyed` outlives the folder and the filter it was set in.
  const rowStop = shown?.some((i) => i.fileName === keyed)
    ? keyed
    : shown?.[0]?.fileName;

  if (folders.length === 0) {
    return (
      <div className={styles.wrap}>
        <div className={styles.empty}>
          <Package size={32} className={styles.emptyIcon} />
          <p>
            A proxy hosts no world of its own — it sits in front of other
            servers, so there is nothing here to install into.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.wrap}>
      <div className={styles.toolbar}>
        {/* The instance tab's strip, said the same way — it swaps the list
            below it, and the roles belong on it for the same reason. The role
            goes straight on the strip: it holds the folders and nothing else,
            since the tools beside it are a sibling of this element rather than
            a child. */}
        <div
          className={styles.kinds}
          role="tablist"
          aria-label="Content folders"
          onKeyDown={(e) => {
            const next = rovingKey(
              e,
              folders.map((f) => f.id),
            );
            if (next) setFolder(next);
          }}
        >
          {folders.map((option) => (
            <button
              key={option.id}
              id={`server-content-tab-${option.id}`}
              className={clsx(
                styles.kindTab,
                folder === option.id && styles.kindActive,
              )}
              onClick={() => setFolder(option.id)}
              role="tab"
              aria-selected={folder === option.id}
              aria-controls="server-content-panel"
              tabIndex={option.id === folderStop ? 0 : -1}
              data-roving={option.id}
            >
              {option.label}
            </button>
          ))}
        </div>

        <div className={styles.tools}>
          <div className={styles.searchWrap}>
            <Search size={14} className={styles.searchIcon} />
            <input
              ref={filterBox}
              // `.searchHinted` is the shared stylesheet's box-with-a-key-in-it:
              // it keeps the room on the right that stops the typed text
              // running under the hint. This box is never switched off, so
              // unlike the instance tab's it wears the class unconditionally.
              className={clsx("input", styles.search, styles.searchHinted)}
              placeholder="Filter"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
            {/* The binding, printed in the box it reaches. An action has no
                sidebar row to be found from, so the thing it presses is the
                only place it can be come across at all. */}
            <kbd className={styles.searchKey}>{actionHint("focusFilter")}</kbd>
          </div>
          {/* Taking every row at once, in the open rather than behind the
              disclosure the instance's toolbar puts it behind. That toolbar
              was rebuilt around one primary action and has a drawer for its
              occasional acts; this one is still the plain row of buttons it
              always was — see `.searchGrow` in the shared stylesheet, which
              says the same thing about the filter box. A button here is the
              register this toolbar already speaks in.
              Only with more than one row to take, since on one row it is the
              checkbox beside it with extra steps. */}
          {shown && shown.length > 1 && (
            <button className="btn" onClick={selectAll}>
              <ListChecks size={14} />
              Select all {shown.length}
            </button>
          )}
          <button className="btn" onClick={addFiles}>
            <FilePlus2 size={14} />
            Add file
          </button>
          {untrackedCount > 0 && (
            <button
              className="btn"
              onClick={linkUntracked}
              disabled={linking}
              title="Match untracked files against Modrinth and CurseForge by file hash"
            >
              {linking ? (
                <Loader2 size={14} className="spin" />
              ) : (
                <Link2 size={14} />
              )}
              Identify {untrackedCount}
            </button>
          )}
          <button
            className="btn"
            onClick={checkUpdates}
            disabled={checking || trackedCount === 0}
            // A failed read also leaves `trackedCount` at zero, and saying
            // nothing here came from Modrinth or CurseForge would be a claim
            // about files that were never listed.
            title={
              failed
                ? `The ${noun} folder could not be read, so there is nothing to check`
                : trackedCount === 0
                  ? "Nothing here was installed from Modrinth or CurseForge"
                  : "Check for newer versions"
            }
          >
            {checking ? (
              <Loader2 size={14} className="spin" />
            ) : (
              <RefreshCw size={14} />
            )}
            Check updates
          </button>
          <button
            className="btn btn-primary"
            onClick={() => onBrowse(folder)}
          >
            Browse
          </button>
        </div>
      </div>

      {/* Cardboard is a mod, but what it does is change what the server is:
          afterwards this pane grows a Plugins folder that was not there. */}
      {server.canAddCardboard && (
        <div className={styles.updateBanner}>
          <Package size={15} />
          <span>This server runs mods but not Bukkit plugins.</span>
          <button
            className="btn btn-sm"
            onClick={() => void addCardboard()}
            disabled={addingCardboard}
          >
            {addingCardboard && <Loader2 size={12} className="spin" />}
            Add Cardboard
          </button>
        </div>
      )}

      {/* An offer, not a move. Starting the server moves out the jars that
          declare themselves client-only; these are known only from a listing,
          which is an author's tick-box and is sometimes wrong, so a mod taken
          out on its word alone would simply be missing from the server. */}
      {folder === "mod" && clientOnly.length > 0 && (
        <div className={styles.offer}>
          <p className={styles.offerLead}>
            {clientOnly.length === 1
              ? "This mod looks like it is only for players: the platform lists it as client-side, while its own jar says nothing about it."
              : `These ${clientOnly.length} mods look like they are only for players: the platform lists them as client-side, while their own jars say nothing about it.`}{" "}
            A listing can be wrong, so nothing has moved. Client mods still go
            to players with the pack; this server stops loading them.
          </p>
          {clientOnly.map((mod) => (
            <div key={mod.file} className={styles.offerRow}>
              <span className={styles.offerName}>{mod.name}</span>
              {!titleIsFileName(mod.file, mod.name) && (
                <span className={styles.fileName}>{mod.file}</span>
              )}
              <button
                className="btn btn-sm"
                onClick={() =>
                  void move({ fileName: mod.file, name: mod.name }, true)
                }
                disabled={server.running}
                title={
                  server.running
                    ? "Stop the server to move mods between folders"
                    : "Move to Client mods — players still get it, this server stops loading it"
                }
              >
                Move to Client mods
              </button>
              <button
                className="btn btn-sm btn-ghost"
                onClick={() => void keepInMods(mod)}
                title="Leave it where it is. Recorded as needed on both sides, so you are not asked about it again."
              >
                Keep in Mods
              </button>
            </div>
          ))}
        </div>
      )}

      {updates.length > 0 && (
        <div className={styles.updateBanner}>
          <ArrowUpCircle size={15} />
          <span>
            {updates.length} update{updates.length === 1 ? "" : "s"} available
          </span>
          <button
            className="btn btn-sm btn-primary"
            onClick={updateAll}
            disabled={updating !== null}
          >
            {updating !== null && <Loader2 size={12} className="spin" />}
            Update all
          </button>
        </div>
      )}

      {/* Only while something is selected, and every press on it names what it
          will do to how many — a bar that says "Disable" over a selection you
          have scrolled away from is a bar that acts on rows you are not
          looking at. The instance's pane's bar, with the one count it carries
          for the server's own mods left out: see `deletePicked` above. */}
      {picked.length > 0 && (
        <div className={styles.selectBar}>
          <div className={styles.selectText}>
            {bulkBusy && <Loader2 size={13} className="spin" />}
            <span className={styles.selectCount}>{picked.length} selected</span>
            {/* Why "Enable 1" sits beside "Disable 5" under a count of six:
                the two presses act on the part of the selection that is not
                already the way it is being asked to be. Said only when the
                selection is mixed, because that is the only time the numbers
                differ from the count and need accounting for. */}
            {onCount > 0 && offCount > 0 && (
              <span className={styles.selectNote}>
                {onCount} on, {offCount} off
              </span>
            )}
            {/* A selection outlives the filter that made it, so the count says
                so rather than letting a press act on rows nobody can see. */}
            {picked.length > pickedShown && (
              <span className={styles.selectNote}>
                {picked.length - pickedShown} not shown by this filter
              </span>
            )}
          </div>
          <button className="btn btn-sm btn-ghost" onClick={clear}>
            Clear
            <kbd className={styles.selectKey}>
              {actionHint("clearSelection")}
            </kbd>
          </button>
          <button
            className="btn btn-sm"
            onClick={() => void setPickedEnabled(true)}
            disabled={bulkBusy || offCount === 0}
          >
            Enable {offCount}
          </button>
          <button
            className="btn btn-sm"
            onClick={() => void setPickedEnabled(false)}
            disabled={bulkBusy || onCount === 0}
          >
            Disable {onCount}
          </button>
          {/* Set apart from the two presses that can be taken back, and it
              asks before it does anything. */}
          <button
            className={clsx("btn btn-sm btn-danger", styles.selectDelete)}
            onClick={() => setAskingDelete(true)}
            disabled={bulkBusy}
          >
            <Trash2 size={13} />
            Delete {picked.length}
          </button>
        </div>
      )}

      {/* The one panel the folders swap, named by whichever of them is showing.
          Focusable because it is the thing the strip switches and it scrolls:
          while it is loading or empty there is nothing inside it to focus, so
          Tab out of the strip would otherwise skip it entirely. */}
      <div
        className={styles.list}
        id="server-content-panel"
        role="tabpanel"
        aria-labelledby={`server-content-tab-${folderStop}`}
        tabIndex={0}
      >
        {shown === null ? (
          <div className={styles.skeletons}>
            {Array.from({ length: 5 }, (_, i) => (
              <div key={i} className="skeleton" style={{ height: 54 }} />
            ))}
          </div>
        ) : failed ? (
          /* Before the empty state, because this panel's empty state says two
             things and a failed read supports neither: that the folder has
             nothing in it, and that whatever is added next lands in it. On a
             server that read usually fails on a permission or a mount, which
             is the case where the write fails too — so "Anything you add lands
             in the server's mods folder" is left to a folder that really is
             empty. */
          <div className={styles.empty}>
            <TriangleAlert size={32} className={styles.emptyIcon} />
            <p>
              Could not read the {noun} folder, which is not the same as there
              being none. Nothing has been changed or removed — the folder was
              not read, so what is in it is unknown.
            </p>
            {/* On its own line because it is the backend's sentence, not this
                screen's: a refusal that states a fact is a lowercase fragment
                with no full stop, which runs into the next sentence if it is
                set inline. */}
            <p className={styles.why}>{failed}</p>
            <button className="btn btn-sm" onClick={() => void load()}>
              Try again
            </button>
          </div>
        ) : shown.length === 0 ? (
          <div className={styles.empty}>
            <Package size={32} className={styles.emptyIcon} />
            <p>
              {filter
                ? `Nothing matches “${filter}”.`
                : folder === "clientmod"
                  ? "No client mods yet. Players get these with the server's pack; this server never loads them."
                  : `No ${noun} yet. Anything you add lands in ${
                      folder === "datapack"
                        ? "the world's datapacks folder"
                        : `the server's ${folder}s folder`
                    }.`}
            </p>
            {!filter && (
              <button
                className="btn btn-primary btn-sm"
                onClick={() => onBrowse(folder)}
              >
                Find some
              </button>
            )}
          </div>
        ) : (
          /* The instance list's grid, said the same way and for the same
             reasons — see the comment on that one, which argues the whole
             shape: the row is the tab stop, Up and Down move between rows,
             Left and Right along one, and Space takes a row into the
             selection. This pane now selects too, so there is nothing left to
             say differently here. */
          <div
            className={styles.rows}
            role="grid"
            aria-labelledby={`server-content-tab-${folderStop}`}
            aria-multiselectable="true"
            onKeyDown={(e) =>
              rowKeys(e, (file, extend) => {
                const row = shown.find((i) => i.fileName === file);
                if (row) pick(row, extend);
              })
            }
            onFocus={(e) => {
              const row = (e.target as HTMLElement).closest<HTMLElement>("[data-row]");
              if (row?.dataset.row) setKeyed(row.dataset.row);
            }}
          >
          {shown.map((item) => {
            const update = updateFor(item);
            // Only in Mods: every row in Client mods is a client mod by virtue
            // of the folder it is in, and a badge on each would say nothing.
            const side =
              folder === "mod" && item.sided ? sideBadge(item.sided) : null;
            // Blank unless the admin answered: the control offers their own
            // answer, not a reading of whatever the jar happens to say.
            const chosen = item.sided?.saidBy === "admin" ? item.sided.side : "";
            const isPicked = selected.has(item.fileName);
            return (
              <div
                key={item.fileName}
                className={clsx(styles.row, isPicked && styles.rowPicked)}
                role="row"
                aria-selected={isPicked}
                data-row={item.fileName}
                tabIndex={item.fileName === rowStop ? 0 : -1}
              >
                {/* Selecting, and the only checkbox on the row — the same
                    column, in the same place, meaning the same thing as on the
                    instance's list. The two panes are one screen, so a box in
                    the leading column meaning "picked" on one and nothing at
                    all on the other was the thing to avoid; whether this
                    server loads the file stays a button among the row's other
                    buttons, where every other verb in the row already is. */}
                <label className={styles.selectCell} role="gridcell">
                  <input
                    type="checkbox"
                    checked={isPicked}
                    // Shift off the event the browser raised, so the gesture
                    // is the same from the pointer and from Space.
                    onChange={(e) => {
                      const raised = e.nativeEvent;
                      pick(item, raised instanceof MouseEvent && raised.shiftKey);
                    }}
                    aria-label={`Select ${item.name}`}
                    tabIndex={-1}
                  />
                </label>

                <div className={styles.itemIcon} role="gridcell">
                  {item.iconUrl ? (
                    <img src={item.iconUrl} alt="" loading="lazy" />
                  ) : (
                    <Package size={16} />
                  )}
                </div>

                <div className={styles.itemText} role="gridcell">
                  <div className={styles.itemTitle}>
                    <span className={styles.itemName}>{item.name}</span>
                    {/* The same word the instance's list says, for the same
                        reason: the row used to say it by being drawn at half
                        strength, which reads as "loading" as readily as "off"
                        and put its text under 2.5:1. These two panes are one
                        screen and share a stylesheet, so they had to stop
                        together. */}
                    {/* Each of these is a word whose meaning is the sentence
                        attached to it, and a `title` carries a sentence to the
                        pointer and to nothing else — not to a focus, not to a
                        touch. `side` is the one that makes it plainest: "both"
                        and "side not declared" are the same badge shape saying
                        two different things about whose word a side is on, and
                        all of that was readable only by hovering. So each
                        carries a real tooltip and a `tabIndex` of -1, which
                        puts it in its row's arrow walk and in no tab order at
                        all — the same trade the instance list makes, argued
                        there. The provider badge takes neither: "Modrinth" is
                        not a word that needs a sentence. */}
                    {!item.enabled && (
                      <Tooltip text="Still here, and this server skips it.">
                        <span className="badge" tabIndex={-1}>
                          disabled
                        </span>
                      </Tooltip>
                    )}
                    {item.dependency && (
                      <Tooltip text="Installed as a dependency">
                        <span className="badge" tabIndex={-1}>
                          dependency
                        </span>
                      </Tooltip>
                    )}
                    {item.fromDisk && (
                      <Tooltip text="Added by hand — not tracked for updates">
                        <span className="badge" tabIndex={-1}>
                          untracked
                        </span>
                      </Tooltip>
                    )}
                    {item.provider && (
                      <span className={clsx("badge", `badge-${item.provider}`)}>
                        {providerLabel(item.provider)}
                      </span>
                    )}
                    {side && (
                      <Tooltip text={side.title}>
                        <span className="badge" tabIndex={-1}>
                          {side.label}
                        </span>
                      </Tooltip>
                    )}
                    {item.keepOnServer && (
                      <Tooltip text="Kept on this server. No pack carries this file: not its bytes, not a link to it, not even its name.">
                        <span className="badge" tabIndex={-1}>
                          kept here
                        </span>
                      </Tooltip>
                    )}
                  </div>
                  <div className={styles.itemMeta}>
                    {item.version && <span>{item.version}</span>}
                    {/* See the same line in the instance's pane: a row whose
                        title is its file name does not repeat it here. */}
                    {!titleIsFileName(item.fileName, item.name) && (
                      <span className={styles.fileName}>{shownName(item.fileName)}</span>
                    )}
                    <span>{fileSize(item.size)}</span>
                  </div>
                </div>

                {/* The row's controls, as one cell. A row here holds one of
                    them in Plugins and five in Mods, so one cell each would
                    give the grid a column count that is never the same twice;
                    the arrows walk the controls rather than the cells, so
                    nothing is lost by holding them together. */}
                <div className={styles.actions} role="gridcell">
                  {update && (
                    <button
                      className="btn btn-sm btn-primary"
                      onClick={() => applyUpdate(update)}
                      disabled={updating !== null}
                      title={`Update to ${update.newVersionNumber}`}
                      tabIndex={-1}
                    >
                      {updating && sameUpdate(updating, update) ? (
                        <Loader2 size={12} className="spin" />
                      ) : (
                        <ArrowUpCircle size={13} />
                      )}
                      Update
                    </button>
                  )}

                  {/* Whether this server loads the file, which used to be the
                      checkbox at the head of the row. The glyph carries the
                      state rather than a colour. */}
                  <Tooltip text={item.enabled ? "Turn off" : "Turn on"}>
                    <button
                      className="btn btn-ghost btn-icon btn-sm"
                      onClick={() => toggle(item)}
                      aria-label={`${item.enabled ? "Disable" : "Enable"} ${item.name}`}
                      tabIndex={-1}
                    >
                      {item.enabled ? (
                        <Power size={14} />
                      ) : (
                        <PowerOff size={14} />
                      )}
                    </button>
                  </Tooltip>

                  {folder === "mod" && (
                    <select
                      className={clsx("select", styles.sideChoice)}
                      value={chosen}
                      onChange={(e) =>
                        void chooseSide(
                          item,
                          (e.target.value || null) as
                            | "client"
                            | "server"
                            | "both"
                            | null,
                        )
                      }
                      aria-label={`Which side ${item.name} belongs on`}
                      title="Which side this mod belongs on. Worked out from its jar and its listing until you say otherwise, and your answer is what the pack tells players. A mod only players need belongs in Client mods — the move button does that."
                      tabIndex={-1}
                    >
                      <option value="">Side: worked out</option>
                      {/* Not offered, because leaving a client-only mod in Mods
                          has the server load it anyway; shown when something
                          else recorded it, so this never misreports the mod. */}
                      {chosen === "client" && (
                        <option value="client">Side: client only</option>
                      )}
                      <option value="server">Side: server only</option>
                      <option value="both">Side: both sides</option>
                    </select>
                  )}

                  {(folder === "mod" || folder === "clientmod") && (
                    <button
                      className={clsx(
                        "btn btn-ghost btn-icon btn-sm",
                        item.keepOnServer && styles.keptHere,
                      )}
                      onClick={() => void keepHere(item, !item.keepOnServer)}
                      title={
                        item.keepOnServer
                          ? "Kept on this server. Press to let players have it again."
                          : "Keep on this server: players get nothing of this file. For your own tooling, and for a jar whose licence forbids passing it on."
                      }
                      aria-label={
                        item.keepOnServer
                          ? `Let players have ${item.name}`
                          : `Keep ${item.name} on this server`
                      }
                      aria-pressed={item.keepOnServer ?? false}
                      tabIndex={-1}
                    >
                      <ServerIcon size={14} />
                    </button>
                  )}

                  {(folder === "mod" || folder === "clientmod") && (
                    <button
                      className="btn btn-ghost btn-icon btn-sm"
                      onClick={() => void move(item)}
                      disabled={server.running}
                      title={
                        server.running
                          ? "Stop the server to move mods between folders"
                          : folder === "mod"
                            ? "Move to Client mods — players get it, this server stops loading it"
                            : "Move to Mods — this server loads it too. A mod that itself says it only runs in players' games goes back to Client mods when the server starts."
                      }
                      aria-label={`Move ${item.name} to ${
                        folder === "mod" ? "Client mods" : "Mods"
                      }`}
                      tabIndex={-1}
                    >
                      <ArrowLeftRight size={14} />
                    </button>
                  )}

                  <button
                    className="btn btn-ghost btn-icon btn-sm"
                    onClick={() => remove(item)}
                    title="Delete"
                    aria-label={`Delete ${item.name}`}
                    tabIndex={-1}
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              </div>
            );
          })}
          </div>
        )}
      </div>

      {/* A row's own Delete asks nothing — one press, one file, the name right
          under the cursor. A press that takes a dozen at once is not that, so
          this names every one of them: a count is what was clicked, the names
          are what is about to go, and a selection made three filters ago is
          the easy way to delete a plugin you had forgotten was in it. */}
      {askingDelete && (
        <Modal
          title={`Delete ${picked.length} file${picked.length === 1 ? "" : "s"}?`}
          subtitle={`Removed from this server's ${noun}. It cannot be undone.`}
          onClose={() => setAskingDelete(false)}
          width={480}
          footer={
            <>
              <button className="btn" onClick={() => setAskingDelete(false)}>
                Cancel
              </button>
              <button
                className="btn btn-danger"
                onClick={() => void deletePicked()}
                disabled={bulkBusy}
              >
                {bulkBusy && <Loader2 size={14} className="spin" />}
                Delete permanently
              </button>
            </>
          }
        >
          <ul className={styles.confirmList}>
            {picked.map((item) => (
              <li key={item.fileName}>
                <span className={styles.confirmName}>{item.name}</span>
                {!titleIsFileName(item.fileName, item.name) && (
                  <span className={styles.fileName}>
                    {shownName(item.fileName)}
                  </span>
                )}
              </li>
            ))}
          </ul>
        </Modal>
      )}
    </div>
  );
}

import { useCallback, useEffect, useMemo, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import {
  ArrowLeftRight,
  ArrowUpCircle,
  FilePlus2,
  Link2,
  Loader2,
  Package,
  RefreshCw,
  Search,
  Server as ServerIcon,
  Trash2,
} from "lucide-react";

import { api } from "../../lib/api";
import { fileSize, providerShort } from "../../lib/format";
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
  const [clientOnly, setClientOnly] = useState<ListedClientOnly[]>([]);
  const [filter, setFilter] = useState("");
  const [updates, setUpdates] = useState<UpdateCandidate[]>([]);
  const [checking, setChecking] = useState(false);
  const [updating, setUpdating] = useState<UpdateCandidate | null>(null);
  const [linking, setLinking] = useState(false);
  const [addingCardboard, setAddingCardboard] = useState(false);

  const load = useCallback(async () => {
    try {
      setItems(await api.listServerContent(server.id, folder));
    } catch (e) {
      reportError("Could not read the server folder", e);
      setItems([]);
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
  useEffect(() => {
    setItems(null);
    void load();
  }, [load, server.running]);

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
      await api.setServerContentEnabled(
        server.id,
        folder,
        item.fileName,
        !item.enabled,
      );
      await load();
    } catch (e) {
      reportError("Could not toggle that file", e);
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
            ? `${report.unknown} file${report.unknown === 1 ? " is" : "s are"} not on Modrinth, so ${report.unknown === 1 ? "it stays" : "they stay"} untracked.`
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
        <div className={styles.kinds}>
          {folders.map((option) => (
            <button
              key={option.id}
              className={clsx(
                styles.kindTab,
                folder === option.id && styles.kindActive,
              )}
              onClick={() => setFolder(option.id)}
            >
              {option.label}
            </button>
          ))}
        </div>

        <div className={styles.tools}>
          <div className={styles.searchWrap}>
            <Search size={14} className={styles.searchIcon} />
            <input
              className={clsx("input", styles.search)}
              placeholder="Filter"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          </div>
          <button className="btn" onClick={addFiles}>
            <FilePlus2 size={14} />
            Add file
          </button>
          {untrackedCount > 0 && (
            <button
              className="btn"
              onClick={linkUntracked}
              disabled={linking}
              title="Match untracked files against Modrinth by file hash"
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
            title={
              trackedCount === 0
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
              <span className={styles.fileName}>{mod.file}</span>
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

      <div className={styles.list}>
        {shown === null ? (
          <div className={styles.skeletons}>
            {Array.from({ length: 5 }, (_, i) => (
              <div key={i} className="skeleton" style={{ height: 54 }} />
            ))}
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
          shown.map((item) => {
            const update = updateFor(item);
            // Only in Mods: every row in Client mods is a client mod by virtue
            // of the folder it is in, and a badge on each would say nothing.
            const side =
              folder === "mod" && item.sided ? sideBadge(item.sided) : null;
            // Blank unless the admin answered: the control offers their own
            // answer, not a reading of whatever the jar happens to say.
            const chosen = item.sided?.saidBy === "admin" ? item.sided.side : "";
            return (
              <div
                key={item.fileName}
                className={clsx(styles.row, !item.enabled && styles.disabled)}
              >
                <label className={styles.toggle}>
                  <input
                    type="checkbox"
                    checked={item.enabled}
                    onChange={() => toggle(item)}
                    aria-label={`${item.enabled ? "Disable" : "Enable"} ${item.name}`}
                  />
                </label>

                <div className={styles.itemIcon}>
                  {item.iconUrl ? (
                    <img src={item.iconUrl} alt="" loading="lazy" />
                  ) : (
                    <Package size={16} />
                  )}
                </div>

                <div className={styles.itemText}>
                  <div className={styles.itemTitle}>
                    <span className={styles.itemName}>{item.name}</span>
                    {item.dependency && (
                      <span className="badge" title="Installed as a dependency">
                        dependency
                      </span>
                    )}
                    {item.fromDisk && (
                      <span
                        className="badge"
                        title="Added by hand — not tracked for updates"
                      >
                        untracked
                      </span>
                    )}
                    {item.provider && (
                      <span className={clsx("badge", `badge-${item.provider}`)}>
                        {providerShort(item.provider)}
                      </span>
                    )}
                    {side && (
                      <span className="badge" title={side.title}>
                        {side.label}
                      </span>
                    )}
                    {item.keepOnServer && (
                      <span
                        className="badge"
                        title="Kept on this server. No pack carries this file: not its bytes, not a link to it, not even its name."
                      >
                        kept here
                      </span>
                    )}
                  </div>
                  <div className={styles.itemMeta}>
                    {item.version && <span>{item.version}</span>}
                    <span className={styles.fileName}>{item.fileName}</span>
                    <span>{fileSize(item.size)}</span>
                  </div>
                </div>

                {update && (
                  <button
                    className="btn btn-sm btn-primary"
                    onClick={() => applyUpdate(update)}
                    disabled={updating !== null}
                    title={`Update to ${update.newVersionNumber}`}
                  >
                    {updating && sameUpdate(updating, update) ? (
                      <Loader2 size={12} className="spin" />
                    ) : (
                      <ArrowUpCircle size={13} />
                    )}
                    Update
                  </button>
                )}

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
                  >
                    <ArrowLeftRight size={14} />
                  </button>
                )}

                <button
                  className="btn btn-ghost btn-icon btn-sm"
                  onClick={() => remove(item)}
                  title="Delete"
                  aria-label={`Delete ${item.name}`}
                >
                  <Trash2 size={14} />
                </button>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

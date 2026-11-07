import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import {
  ArrowUpCircle,
  Check,
  FilePlus2,
  Link2,
  ListChecks,
  Loader2,
  Package,
  RefreshCw,
  Search,
  Trash2,
  Unlock,
} from "lucide-react";

import Modal from "../../components/Modal";
import { api } from "../../lib/api";
import { fileSize, providerLabel } from "../../lib/format";
import { actionHint, shortcutAction } from "../../lib/shortcuts";
import { useStore } from "../../lib/store";
import type { ContentItem, ContentKind, Instance, UpdateCandidate } from "../../lib/types";

import styles from "./ContentTab.module.css";

const KINDS: { id: ContentKind; label: string; extensions: string[] }[] = [
  { id: "mod", label: "Mods", extensions: ["jar"] },
  { id: "resourcepack", label: "Resource Packs", extensions: ["zip"] },
  { id: "shaderpack", label: "Shaders", extensions: ["zip"] },
  { id: "datapack", label: "Data Packs", extensions: ["zip"] },
];

/**
 * A selection, with every file that has just been renamed following its row.
 *
 * Turning a file on or off *is* a rename on disk — `foo.jar` becomes
 * `foo.jar.disabled` — and a selection is keyed by the file name because that
 * is the only thing unique to a row. Without this, disabling twelve mods would
 * empty the selection the moment it worked, and the obvious next press would
 * have nothing left to act on.
 */
function followRenames(selected: Set<string>, moves: Map<string, string>) {
  const next = new Set(selected);
  for (const [from, to] of moves) {
    if (!next.has(from)) continue;
    next.delete(from);
    next.add(to);
  }
  return next;
}

export default function ContentTab({
  instance,
  onBrowse,
}: {
  instance: Instance;
  onBrowse: () => void;
}) {
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const refreshInstances = useStore((s) => s.refreshInstances);

  const [kind, setKind] = useState<ContentKind>("mod");
  const [items, setItems] = useState<ContentItem[] | null>(null);
  const [filter, setFilter] = useState("");
  const [updates, setUpdates] = useState<UpdateCandidate[]>([]);
  const [checking, setChecking] = useState(false);
  const [updating, setUpdating] = useState<string | null>(null);
  const [linking, setLinking] = useState(false);

  // The rows a press is about to act on, by file name, and the row a plain
  // click last landed on so Shift can measure a span from it.
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [bulkBusy, setBulkBusy] = useState(false);
  const [askingDelete, setAskingDelete] = useState(false);
  const filterBox = useRef<HTMLInputElement>(null);

  const load = useCallback(async () => {
    try {
      setItems(await api.listContent(instance.id, kind));
    } catch (e) {
      reportError("Could not read the content folder", e);
      setItems([]);
    }
  }, [instance.id, kind, reportError]);

  useEffect(() => {
    setItems(null);
    // A selection is of files in one folder, so changing folders drops it
    // rather than carrying names that are not in the list any more.
    setSelected(new Set());
    setAnchor(null);
    void load();
  }, [load]);

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

  // Everything about the selection is read back off `items`, so a name left in
  // it by a file that has since gone counts for nothing anywhere on screen.
  const picked = useMemo(
    () => items?.filter((i) => selected.has(i.fileName)) ?? [],
    [items, selected],
  );
  const pickedShown = useMemo(
    () => shown?.filter((i) => selected.has(i.fileName)).length ?? 0,
    [shown, selected],
  );
  // The server's own mods are not the player's to delete — the row's own
  // button is disabled for them and Rust refuses it as well — so a bulk delete
  // counts what it can really take.
  const deletable = picked.filter((i) => i.origin !== "server");
  const offCount = picked.filter((i) => !i.enabled).length;
  const onCount = picked.filter((i) => i.enabled).length;

  /**
   * Take a row into the selection, or out of it.
   *
   * Shift adds the span between the row a plain click last landed on and this
   * one, rather than replacing what is selected: the gesture reads as "and
   * these too". The span is measured over what is *shown*, so it runs along
   * the rows in front of you and never through the ones the filter is hiding.
   */
  function pick(item: ContentItem, extend: boolean) {
    const rows = shown ?? [];
    setSelected((prev) => {
      const next = new Set(prev);
      const from = rows.findIndex((r) => r.fileName === anchor);
      const to = rows.findIndex((r) => r.fileName === item.fileName);
      if (extend && from >= 0 && to >= 0) {
        for (const row of rows.slice(Math.min(from, to), Math.max(from, to) + 1)) {
          next.add(row.fileName);
        }
        return next;
      }
      if (next.has(item.fileName)) next.delete(item.fileName);
      else next.add(item.fileName);
      return next;
    });
    // The anchor stays where a plain click put it, so a second Shift-click
    // adjusts the same span instead of walking away from it.
    if (!extend) setAnchor(item.fileName);
  }

  function clearSelection() {
    setSelected(new Set());
    setAnchor(null);
  }

  /**
   * The Content tab's own keys, bound while the tab is on screen rather than
   * at the window: an action shortcut means nothing on a screen that cannot
   * carry it out, and `InstanceView` mounts this tab only while it is the one
   * showing, so leaving the listener with the component is the whole of
   * keeping these keys in context.
   *
   * Captured, so it runs before `Modal`'s own Escape handler and can still see
   * that a dialog is open. A dialog is a mode — the same reason `App` holds the
   * sidebar's keys back for one — and a press that belongs to it must not also
   * reach the screen underneath.
   */
  useEffect(() => {
    const onShortcut = (e: KeyboardEvent) => {
      if (useStore.getState().backHandlers.some((h) => h.overlay)) return;
      switch (shortcutAction(e)) {
        case "focusFilter": {
          // The webview's find bar is not what this is for, and the text that
          // is there is selected so the next keystroke replaces the old filter.
          e.preventDefault();
          const box = filterBox.current;
          if (box) {
            box.focus();
            box.select();
          }
          break;
        }
        case "clearSelection":
          // Not prevented: Escape has no default of its own here, and anything
          // else listening for it should still get it.
          setSelected((prev) => (prev.size === 0 ? prev : new Set<string>()));
          setAnchor(null);
          break;
      }
    };
    window.addEventListener("keydown", onShortcut, true);
    return () => window.removeEventListener("keydown", onShortcut, true);
  }, []);

  // The folder as well as the project: the check spans every folder while this
  // list shows one, so a row takes only a candidate from its own folder — the
  // same rule the server tab keeps.
  const updateFor = (item: ContentItem) =>
    updates.find(
      (u) =>
        u.projectId === item.projectId &&
        u.provider === item.provider &&
        u.kind === kind,
    );

  async function toggle(item: ContentItem) {
    try {
      const renamed = await api.setContentEnabled(
        instance.id,
        kind,
        item.fileName,
        !item.enabled,
      );
      setSelected((prev) =>
        followRenames(prev, new Map([[item.fileName, renamed]])),
      );
      await load();
    } catch (e) {
      reportError("Could not toggle that file", e);
    }
  }

  async function remove(item: ContentItem) {
    try {
      await api.removeContent(instance.id, kind, item.fileName);
      await Promise.all([load(), refreshInstances()]);
    } catch (e) {
      reportError(`Could not delete ${item.name}`, e);
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
          await api.setContentEnabled(instance.id, kind, item.fileName, enable),
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
      setSelected((prev) => followRenames(prev, moves));
      setBulkBusy(false);
      await load();
    }
  }

  /** Delete every selected file that is this instance's to delete. */
  async function deletePicked() {
    const targets = deletable;
    setBulkBusy(true);
    const gone: string[] = [];
    try {
      for (const item of targets) {
        await api.removeContent(instance.id, kind, item.fileName);
        gone.push(item.fileName);
      }
    } catch (e) {
      reportError(
        gone.length > 0
          ? `Deleted ${gone.length} of ${targets.length} files, then stopped`
          : "Nothing was deleted",
        e,
      );
    } finally {
      setSelected((prev) => {
        const next = new Set(prev);
        for (const file of gone) next.delete(file);
        return next;
      });
      setAskingDelete(false);
      setBulkBusy(false);
      await Promise.all([load(), refreshInstances()]);
    }
  }

  /**
   * Stop asking which Minecraft version this mod's builds are for, or start
   * again.
   *
   * The toast says what the choice does and does not do rather than only that
   * it was saved: nothing has looked inside the jar, so the one honest claim
   * is about the question Geode will put next time.
   */
  async function ignoreVersion(item: ContentItem, ignore: boolean) {
    try {
      await api.setContentIgnoreGameVersion(
        instance.id,
        kind,
        item.fileName,
        ignore,
      );
      await load();
      toast({
        kind: "success",
        title: ignore
          ? `${item.name} ignores the Minecraft version`
          : `${item.name} needs a build for Minecraft ${instance.mcVersion}`,
        body: ignore
          ? "Updates and version changes will take its newest build for this loader, whatever version it was built for. It may stop the game starting."
          : undefined,
      });
    } catch (e) {
      reportError(`Could not change what ${item.name} asks for`, e);
    }
  }

  async function addFiles() {
    const spec = KINDS.find((k) => k.id === kind)!;
    const picked = await openDialog({
      multiple: true,
      filters: [{ name: spec.label, extensions: spec.extensions }],
    });
    const files = Array.isArray(picked) ? picked : picked ? [picked] : [];
    if (files.length === 0) return;

    try {
      const added = await api.addLocalContent(instance.id, kind, files);
      await Promise.all([load(), refreshInstances()]);
      toast({
        kind: "success",
        title: `Added ${added} file${added === 1 ? "" : "s"}`,
      });
    } catch (e) {
      // A pick holding one file that would land on the server's own mod is
      // refused whole, before anything is copied, so the title says what the
      // reason under it cannot: the rest did not go in either.
      reportError("Nothing was added", e);
    }
  }

  async function linkUntracked() {
    setLinking(true);
    try {
      const report = await api.linkUntrackedContent(instance.id);
      await Promise.all([load(), refreshInstances()]);
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
      const found = await api.checkContentUpdates(instance.id);
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
    setUpdating(candidate.projectId);
    try {
      await api.applyContentUpdate(
        instance.id,
        candidate.provider,
        candidate.projectId,
        candidate.newVersionId,
      );
      setUpdates((prev) =>
        prev.filter(
          (u) =>
            u.provider !== candidate.provider ||
            u.projectId !== candidate.projectId ||
            u.kind !== candidate.kind,
        ),
      );
      await Promise.all([load(), refreshInstances()]);
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

  const trackedCount = items?.filter((i) => i.projectId).length ?? 0;
  const untrackedCount = items?.filter((i) => i.fromDisk).length ?? 0;

  return (
    <div className={styles.wrap}>
      <div className={styles.toolbar}>
        <div className={styles.kinds}>
          {KINDS.map((k) => (
            <button
              key={k.id}
              className={clsx(styles.kindTab, kind === k.id && styles.kindActive)}
              onClick={() => setKind(k.id)}
            >
              {k.label}
            </button>
          ))}
        </div>

        <div className={styles.tools}>
          <div className={styles.searchWrap}>
            <Search size={14} className={styles.searchIcon} />
            <input
              ref={filterBox}
              className={clsx("input", styles.search, styles.searchHinted)}
              placeholder="Filter"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
            {/* The binding, printed in the box it reaches. The sidebar is
                where a screen's shortcut is discoverable; an action's only
                home is the thing it acts on. */}
            <kbd className={styles.searchKey}>{actionHint("focusFilter")}</kbd>
          </div>
          {/* The way into selecting at all, and the one place the gesture is
              written down: an icon that appears on hover explains itself to
              nobody who has not hovered it. */}
          {shown && shown.length > 1 && (
            <button
              className="btn"
              onClick={() => setSelected(new Set(shown.map((i) => i.fileName)))}
              title={`Select all ${shown.length} files listed here, to enable, disable or delete them together. One at a time: click a row's icon, then Shift-click another to take everything between them.`}
            >
              <ListChecks size={14} />
              Select all
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
          <button className="btn btn-primary" onClick={onBrowse}>
            Browse
          </button>
        </div>
      </div>

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
          looking at. */}
      {picked.length > 0 && (
        <div className={styles.selectBar}>
          <div className={styles.selectText}>
            {bulkBusy && <Loader2 size={13} className="spin" />}
            <span className={styles.selectCount}>{picked.length} selected</span>
            {/* A selection outlives the filter that made it, so the count says
                so rather than letting a press act on rows nobody can see. */}
            {picked.length > pickedShown && (
              <span className={styles.selectNote}>
                {picked.length - pickedShown} not shown by this filter
              </span>
            )}
          </div>
          <button className="btn btn-sm btn-ghost" onClick={clearSelection}>
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
            disabled={bulkBusy || deletable.length === 0}
            title={
              deletable.length === 0
                ? "Every selected file is from the server's pack — turn those off instead"
                : undefined
            }
          >
            <Trash2 size={13} />
            Delete {deletable.length}
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
                : instance.loader === "vanilla" && kind === "mod"
                  ? "This is a vanilla instance — switch it to a mod loader in Settings to use mods."
                  : `No ${KINDS.find((k) => k.id === kind)?.label.toLowerCase()} yet.`}
            </p>
            {!filter && (
              <button className="btn btn-primary btn-sm" onClick={onBrowse}>
                Find some
              </button>
            )}
          </div>
        ) : (
          shown.map((item) => {
            const update = updateFor(item);
            const isPicked = selected.has(item.fileName);
            return (
              <div
                key={item.fileName}
                className={clsx(
                  styles.row,
                  !item.enabled && styles.disabled,
                  isPicked && styles.rowPicked,
                )}
              >
                <label className={styles.toggle}>
                  <input
                    type="checkbox"
                    checked={item.enabled}
                    onChange={() => toggle(item)}
                    aria-label={`${item.enabled ? "Disable" : "Enable"} ${item.name}`}
                  />
                </label>

                {/* Selecting, and deliberately not a second checkbox: the
                    square one to the left decides whether the game loads this
                    file, which is a different question from whether the next
                    press is about to act on it. A round tick over the artwork
                    cannot be mistaken for the other one. */}
                <button
                  type="button"
                  className={clsx(
                    styles.itemIcon,
                    styles.pick,
                    isPicked && styles.picked,
                  )}
                  onClick={(e) => pick(item, e.shiftKey)}
                  title="Select this file. Shift-click another to take everything between them."
                  aria-label={`${isPicked ? "Deselect" : "Select"} ${item.name}`}
                  aria-pressed={isPicked}
                >
                  {item.iconUrl ? (
                    <img src={item.iconUrl} alt="" loading="lazy" />
                  ) : (
                    <Package size={16} />
                  )}
                  <span className={styles.pickMark} aria-hidden="true">
                    <Check size={14} />
                  </span>
                </button>

                <div className={styles.itemText}>
                  <div className={styles.itemTitle}>
                    <span className={styles.itemName}>{item.name}</span>
                    {item.dependency && (
                      <span className="badge" title="Installed as a dependency">
                        dependency
                      </span>
                    )}
                    {item.fromDisk && (
                      <span className="badge" title="Added manually — not tracked for updates">
                        untracked
                      </span>
                    )}
                    {item.origin === "server" && (
                      <span
                        className="badge"
                        title="From the server's pack. You can turn it off, but the server decides what is installed."
                      >
                        server
                      </span>
                    )}
                    {item.origin === "server" && item.sided?.side === "server" && (
                      <span
                        className="badge"
                        title="Only the server needs this. Turn all of these on or off in Settings, under Server."
                      >
                        server-side
                      </span>
                    )}
                    {item.ignoreGameVersion && (
                      <span
                        className="badge"
                        title="Its Minecraft version is ignored. Geode takes its newest build for this instance’s loader, whatever Minecraft version that build was made for, instead of refusing to find one."
                      >
                        any version
                      </span>
                    )}
                    {/* Spelled out, the way Browse spells it. "MR" and
                        "Modrinth" are the same provider two screens apart,
                        and the short form saved a few pixels on a row that
                        has them. */}
                    {item.provider && (
                      <span className={clsx("badge", `badge-${item.provider}`)}>
                        {providerLabel(item.provider)}
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
                    {updating === update.projectId ? (
                      <Loader2 size={12} className="spin" />
                    ) : (
                      <ArrowUpCircle size={13} />
                    )}
                    Update
                  </button>
                )}

                {/* Offered only where it could mean anything: a mod, from a
                    platform that can be asked for another build, and not one
                    the server owns. The title is the one place this is
                    explained, and it says the risk in plain words — a mod
                    running outside what it declares can stop the game
                    starting, and nothing here has looked inside the jar. */}
                {kind === "mod" &&
                  item.provider &&
                  item.origin !== "server" && (
                    <button
                      className={clsx(
                        "btn btn-ghost btn-icon btn-sm",
                        item.ignoreGameVersion && styles.anyVersion,
                      )}
                      onClick={() =>
                        void ignoreVersion(item, !item.ignoreGameVersion)
                      }
                      title={
                        item.ignoreGameVersion
                          ? `Ignoring the Minecraft version: its newest build for this loader is used whatever version it was built for, and it may stop the game starting. Press to require a build for Minecraft ${instance.mcVersion} again.`
                          : `Ignore the Minecraft version: use its newest build for this loader even when its author has not listed Minecraft ${instance.mcVersion}. Mods usually run on a little newer than they say — but one that does not can stop the game starting, and nothing here has read the jar. The loader is still matched.`
                      }
                      aria-label={
                        item.ignoreGameVersion
                          ? `Require a build for Minecraft ${instance.mcVersion} for ${item.name}`
                          : `Ignore the Minecraft version for ${item.name}`
                      }
                      aria-pressed={item.ignoreGameVersion ?? false}
                    >
                      <Unlock size={14} />
                    </button>
                  )}

                {/* The server's own mods stay. Offering a button that always
                    fails is worse than not offering one, so the row explains
                    itself instead — turning it off is right there. */}
                <button
                  className="btn btn-ghost btn-icon btn-sm"
                  onClick={() => remove(item)}
                  disabled={item.origin === "server"}
                  title={
                    item.origin === "server"
                      ? "From the server's pack — turn it off instead"
                      : "Delete"
                  }
                  aria-label={`Delete ${item.name}`}
                >
                  <Trash2 size={14} />
                </button>
              </div>
            );
          })
        )}
      </div>

      {/* A row's own Delete button asks nothing — one press, one file, and the
          name is right under the cursor. A press that takes a dozen at once is
          not that, so this names every one of them: a count is what was
          clicked, the names are what is about to go, and a selection made
          three filters ago is the easy way to delete something you had
          forgotten was in it. */}
      {askingDelete && (
        <Modal
          title={`Delete ${deletable.length} file${deletable.length === 1 ? "" : "s"}?`}
          subtitle={`Removed from this instance's ${KINDS.find((k) => k.id === kind)?.label.toLowerCase()} folder. It cannot be undone.`}
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
            {deletable.map((item) => (
              <li key={item.fileName}>
                <span className={styles.confirmName}>{item.name}</span>
                <span className={styles.fileName}>{item.fileName}</span>
              </li>
            ))}
          </ul>
          {picked.length > deletable.length && (
            <p className={styles.confirmNote}>
              {picked.length - deletable.length === 1
                ? "One more file in the selection came from the server's pack and stays — turn it off instead if you do not want it."
                : `${picked.length - deletable.length} more files in the selection came from the server's pack and stay — turn those off instead if you do not want them.`}
            </p>
          )}
        </Modal>
      )}
    </div>
  );
}

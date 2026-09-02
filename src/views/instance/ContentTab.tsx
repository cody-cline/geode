import { useCallback, useEffect, useMemo, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import {
  ArrowUpCircle,
  ChevronDown,
  ChevronRight,
  CornerDownRight,
  FilePlus2,
  Link2,
  Loader2,
  Package,
  RefreshCw,
  Search,
  Trash2,
} from "lucide-react";

import { api } from "../../lib/api";
import { fileSize } from "../../lib/format";
import { useStore } from "../../lib/store";
import type { ContentItem, ContentKind, Instance, UpdateCandidate } from "../../lib/types";

import styles from "./ContentTab.module.css";

const KINDS: { id: ContentKind; label: string; extensions: string[] }[] = [
  { id: "mod", label: "Mods", extensions: ["jar"] },
  { id: "resourcepack", label: "Resource Packs", extensions: ["zip"] },
  { id: "shaderpack", label: "Shaders", extensions: ["zip"] },
  { id: "datapack", label: "Data Packs", extensions: ["zip"] },
];

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
  const [sweeping, setSweeping] = useState(false);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

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

  const byFile = useMemo(
    () => new Map((items ?? []).map((i) => [i.fileName, i])),
    [items],
  );
  const nameOf = useCallback(
    (file: string) => byFile.get(file)?.name ?? file,
    [byFile],
  );

  /**
   * Three groups rather than a tree. The dependency graph is a graph — one
   * library can be held up by six mods — so nesting it either duplicates that
   * library under every parent or picks one parent and lies about the rest.
   * Grouping plus "required by" says the same thing without either.
   */
  const groups = useMemo(() => {
    if (!shown || kind !== "mod") return null;
    return [
      {
        id: "unused",
        title: "Not needed any more",
        note: "Installed for something that has since gone. Nothing left requires them.",
        items: shown.filter((i) => i.unused),
      },
      {
        id: "chosen",
        title: "Added by you",
        note: null,
        items: shown.filter((i) => !i.unused && !i.dependency),
      },
      {
        id: "libraries",
        title: "Dependencies",
        note: null,
        items: shown.filter((i) => !i.unused && i.dependency),
      },
    ].filter((g) => g.items.length > 0);
  }, [shown, kind]);

  // A jar we could not read might be the one thing keeping a library alive, so
  // say so rather than letting the unused list look more certain than it is.
  const unreadable = useMemo(
    () => (items ?? []).filter((i) => kind === "mod" && !i.declaresDeps).length,
    [items, kind],
  );

  function renderRow(item: ContentItem) {
    const update = updateFor(item);
    const isOpen = expanded.has(item.fileName);
    const needs = item.requires.map(nameOf);
    const holders = item.requiredBy.map(nameOf);

    return (
      <div key={item.fileName} className={styles.entry}>
        <div className={clsx(styles.row, !item.enabled && styles.disabled)}>
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
              {item.unused && (
                <span
                  className="badge badge-warn"
                  title="Nothing installed requires this any more"
                >
                  not needed
                </span>
              )}
              {item.fromDisk && (
                <span
                  className="badge"
                  title="Added manually — not tracked for updates"
                >
                  untracked
                </span>
              )}
              {item.provider && (
                <span className={clsx("badge", `badge-${item.provider}`)}>
                  {item.provider === "modrinth" ? "MR" : "CF"}
                </span>
              )}
            </div>

            <div className={styles.itemMeta}>
              {item.version && <span>{item.version}</span>}
              <span className={styles.fileName}>{item.fileName}</span>
              <span>{fileSize(item.size)}</span>
            </div>

            {/* The fan-in is the whole reason this is not a tree: one library
                can be held up by any number of mods, and that list is what
                tells you whether it is safe to delete. */}
            {holders.length > 0 && (
              <p className={styles.holders} title={holders.join(", ")}>
                Needed by {holders.slice(0, 3).join(", ")}
                {holders.length > 3 && ` and ${holders.length - 3} more`}
              </p>
            )}
          </div>

          {needs.length > 0 && (
            <button
              className="btn btn-ghost btn-sm"
              onClick={() =>
                setExpanded((prev) => {
                  const next = new Set(prev);
                  if (!next.delete(item.fileName)) next.add(item.fileName);
                  return next;
                })
              }
              aria-expanded={isOpen}
              title={`What ${item.name} needs`}
            >
              {isOpen ? (
                <ChevronDown size={13} />
              ) : (
                <ChevronRight size={13} />
              )}
              {needs.length}
            </button>
          )}

          {item.unused && (
            <button
              className="btn btn-sm"
              onClick={() => keep(item)}
              title="Stop offering this for cleanup — treat it as one you chose"
            >
              Keep
            </button>
          )}

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

          <button
            className="btn btn-ghost btn-icon btn-sm"
            onClick={() => remove(item)}
            title="Delete"
            aria-label={`Delete ${item.name}`}
          >
            <Trash2 size={14} />
          </button>
        </div>

        {isOpen && (
          <ul className={styles.needs}>
            {item.requires.map((file) => {
              const dep = byFile.get(file);
              return (
                <li key={file} className={styles.need}>
                  <CornerDownRight size={12} className={styles.needArrow} />
                  <span>{nameOf(file)}</span>
                  {dep && dep.requiredBy.length > 1 && (
                    <span className={styles.needShared}>
                      shared with {dep.requiredBy.length - 1} other
                      {dep.requiredBy.length > 2 ? "s" : ""}
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    );
  }

  const updateFor = (item: ContentItem) =>
    updates.find(
      (u) => u.projectId === item.projectId && u.provider === item.provider,
    );

  async function toggle(item: ContentItem) {
    try {
      await api.setContentEnabled(
        instance.id,
        kind,
        item.fileName,
        !item.enabled,
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
   * Removing one library can strand the next: a chain pulled in for a mod that
   * is gone comes loose a link at a time. Re-reading between passes lets the
   * whole tail go, and the pass count stops a cycle from spinning.
   */
  async function removeUnused(unused: ContentItem[]) {
    setSweeping(true);
    try {
      let batch = unused;
      for (let pass = 0; pass < 8 && batch.length > 0; pass++) {
        for (const item of batch) {
          await api.removeContent(instance.id, kind, item.fileName);
        }
        const left = await api.listContent(instance.id, kind);
        setItems(left);
        batch = left.filter((i) => i.unused);
      }
      await refreshInstances();
    } catch (e) {
      reportError("Could not remove those", e);
      await load();
    } finally {
      setSweeping(false);
    }
  }

  async function keep(item: ContentItem) {
    try {
      await api.setContentKept(instance.id, kind, item.fileName, true);
      await load();
    } catch (e) {
      reportError(`Could not keep ${item.name}`, e);
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
      reportError("Could not add those files", e);
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
      setUpdates((prev) => prev.filter((u) => u.projectId !== candidate.projectId));
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

      {unreadable > 0 && groups?.some((g) => g.id === "unused") && (
        <p className={styles.caveat}>
          {unreadable} mod{unreadable === 1 ? "" : "s"} did not state what
          {unreadable === 1 ? " it needs" : " they need"}, so something below
          may still be in use.
        </p>
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
          <>
            {groups
              ? groups.map((group) => (
                  <section key={group.id} className={styles.group}>
                    <div className={styles.groupHead}>
                      <h3 className={styles.groupTitle}>{group.title}</h3>
                      <span className={styles.groupCount}>
                        {group.items.length}
                      </span>
                      {group.note && (
                        <span className={styles.groupNote}>{group.note}</span>
                      )}
                      {group.id === "unused" && (
                        <button
                          className="btn btn-sm"
                          onClick={() => removeUnused(group.items)}
                          disabled={sweeping}
                        >
                          {sweeping ? (
                            <Loader2 size={12} className="spin" />
                          ) : (
                            <Trash2 size={13} />
                          )}
                          Remove {group.items.length}
                        </button>
                      )}
                    </div>
                    {group.items.map(renderRow)}
                  </section>
                ))
              : shown.map(renderRow)}
          </>
        )}
      </div>
    </div>
  );
}

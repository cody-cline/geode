import { useCallback, useEffect, useMemo, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import {
  ArrowUpCircle,
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
                      <span className="badge" title="Added manually — not tracked for updates">
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

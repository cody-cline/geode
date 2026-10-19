import { useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import { FileArchive, Loader2, Search, Users } from "lucide-react";

import Modal from "./Modal";
import { api } from "../lib/api";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type { GameVersion, Loader, PackPreview } from "../lib/types";

import styles from "./NewInstanceModal.module.css";

export default function NewInstanceModal({ onClose }: { onClose: () => void }) {
  const navigate = useStore((s) => s.navigate);
  const refresh = useStore((s) => s.refreshInstances);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const config = useStore((s) => s.config);
  const openJoin = useStore((s) => s.openJoin);

  const [name, setName] = useState("");
  const [nameTouched, setNameTouched] = useState(false);
  const [versions, setVersions] = useState<GameVersion[]>([]);
  const [showSnapshots, setShowSnapshots] = useState(config?.showSnapshots ?? false);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<string>("");
  const [loader, setLoader] = useState<Loader>("fabric");
  const [loaderVersions, setLoaderVersions] = useState<string[]>([]);
  const [loaderVersion, setLoaderVersion] = useState("");
  const [loadingLoader, setLoadingLoader] = useState(false);
  const [creating, setCreating] = useState(false);

  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    api
      .listGameVersions(showSnapshots)
      .then((v) => {
        setVersions(v);
        // Default to the newest stable so the common case is one click.
        setSelected((current) => {
          if (current && v.some((x) => x.id === current)) return current;
          return v.find((x) => x.kind === "release")?.id ?? v[0]?.id ?? "";
        });
      })
      .catch((e) => reportError("Could not load Minecraft versions", e));
  }, [showSnapshots, reportError]);

  // Loader builds depend on the chosen game version, so refetch on either.
  useEffect(() => {
    if (loader === "vanilla" || !selected) {
      setLoaderVersions([]);
      setLoaderVersion("");
      return;
    }
    let cancelled = false;
    setLoadingLoader(true);
    setLoaderVersions([]);
    setLoaderVersion("");

    api
      .listLoaderVersions(loader, selected)
      .then((list) => {
        if (cancelled) return;
        setLoaderVersions(list);
        setLoaderVersion(list[0] ?? "");
      })
      .catch(() => {
        if (!cancelled) setLoaderVersions([]);
      })
      .finally(() => {
        if (!cancelled) setLoadingLoader(false);
      });

    return () => {
      cancelled = true;
    };
  }, [loader, selected]);

  const filtered = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return q ? versions.filter((v) => v.id.toLowerCase().includes(q)) : versions;
  }, [versions, filter]);

  const virtualizer = useVirtualizer({
    count: filtered.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => 32,
    overscan: 8,
  });

  // Suggest a name from the selection until the user types their own.
  const effectiveName =
    name.trim() ||
    (selected
      ? loader === "vanilla"
        ? selected
        : `${selected} ${LOADERS.find((l) => l.id === loader)?.label}`
      : "");

  const loaderReady =
    loader === "vanilla" || (!loadingLoader && loaderVersion !== "");
  const canCreate = Boolean(selected) && effectiveName !== "" && loaderReady && !creating;

  async function create() {
    if (!canCreate) return;
    setCreating(true);
    try {
      const instance = await api.createInstance(
        effectiveName,
        selected,
        loader,
        loaderVersion,
      );
      await refresh();
      toast({ kind: "success", title: `Created ${instance.name}` });
      navigate({ view: "instance", id: instance.id });
      onClose();
    } catch (e) {
      reportError("Could not create the instance", e);
    } finally {
      setCreating(false);
    }
  }

  async function importPack() {
    const picked = await openDialog({
      multiple: false,
      filters: [{ name: "Modpack", extensions: ["geodepack", "mrpack", "zip"] }],
    });
    if (typeof picked !== "string") return;

    let preview: PackPreview;
    try {
      preview = await api.previewModpack(picked);
    } catch (e) {
      reportError("That file is not a modpack", e);
      return;
    }

    if (preview.needsCurseforgeKey && !config?.hasCurseforgeKey) {
      reportError(
        "CurseForge key needed",
        "This is a CurseForge pack. Add your CurseForge API key in Settings first.",
      );
      return;
    }

    setCreating(true);
    try {
      const { instance, problems } = await api.importModpack(picked);
      await refresh();
      // An instance was made either way, so it is opened either way; what the
      // import could not fetch is reported beside it rather than in place of
      // it, because importing again would only make a second one.
      if (problems.length === 0) {
        toast({
          kind: "success",
          title: `Imported ${instance.name}`,
          body: `${preview.fileCount} files, Minecraft ${preview.mcVersion}.`,
        });
      } else {
        reportError(`Imported ${instance.name}, with problems`, problems.join(" "));
      }
      navigate({ view: "instance", id: instance.id });
      onClose();
    } catch (e) {
      // Nothing was made: an import that gets as far as an instance comes back
      // as an outcome, problems and all.
      await refresh();
      reportError("Could not import that pack", e);
    } finally {
      setCreating(false);
    }
  }

  return (
    <Modal
      title="New instance"
      subtitle="Pick a Minecraft version and a mod loader."
      onClose={onClose}
      width={620}
      footer={
        <>
          <button className="btn btn-ghost" onClick={importPack} disabled={creating}>
            <FileArchive size={15} />
            Import modpack
          </button>
          {/* A joined server becomes an instance like any other, so this is
              where someone looking to add one would go looking. */}
          <button
            className="btn btn-ghost"
            onClick={() => {
              onClose();
              openJoin();
            }}
            disabled={creating}
          >
            <Users size={15} />
            Join a server
          </button>
          <div style={{ flex: 1 }} />
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn btn-primary" onClick={create} disabled={!canCreate}>
            {creating && <Loader2 size={14} className="spin" />}
            Create
          </button>
        </>
      }
    >
      <div>
        <label className="label" htmlFor="instance-name">
          Name
        </label>
        <input
          id="instance-name"
          className="input"
          value={nameTouched ? name : effectiveName}
          placeholder="My instance"
          onChange={(e) => {
            setNameTouched(true);
            setName(e.target.value);
          }}
          onFocus={() => {
            if (!nameTouched) {
              setNameTouched(true);
              setName(effectiveName);
            }
          }}
        />
      </div>

      <div>
        <span className="label">Mod loader</span>
        <div className={styles.loaderRow}>
          {LOADERS.map((l) => (
            <button
              key={l.id}
              className={clsx(styles.loaderChip, loader === l.id && styles.loaderActive)}
              onClick={() => setLoader(l.id)}
            >
              {l.label}
            </button>
          ))}
        </div>

        {loader !== "vanilla" && (
          <div className={styles.loaderVersion}>
            {loadingLoader ? (
              <span className={styles.loaderHint}>
                <Loader2 size={13} className="spin" /> Looking up {loader} builds…
              </span>
            ) : loaderVersions.length === 0 ? (
              <span className={styles.loaderWarn}>
                No {LOADERS.find((l) => l.id === loader)?.label} builds for{" "}
                {selected || "this version"}.
              </span>
            ) : (
              <select
                className="select"
                value={loaderVersion}
                onChange={(e) => setLoaderVersion(e.target.value)}
              >
                {loaderVersions.map((v, i) => (
                  <option key={v} value={v}>
                    {v}
                    {i === 0 ? "  (latest)" : ""}
                  </option>
                ))}
              </select>
            )}
          </div>
        )}
      </div>

      <div>
        <div className={styles.versionHead}>
          <span className="label" style={{ margin: 0 }}>
            Minecraft version
          </span>
          <label className={styles.snapshotToggle}>
            <input
              type="checkbox"
              checked={showSnapshots}
              onChange={(e) => setShowSnapshots(e.target.checked)}
            />
            Show snapshots
          </label>
        </div>

        <div className={styles.searchWrap}>
          <Search size={14} className={styles.searchIcon} />
          <input
            className={clsx("input", styles.searchInput)}
            placeholder="Filter versions"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        </div>

        <div ref={listRef} className={styles.versionList}>
          {filtered.length === 0 ? (
            <p className={styles.noResults}>No versions match “{filter}”.</p>
          ) : (
            <div
              style={{ height: virtualizer.getTotalSize(), position: "relative" }}
            >
              {virtualizer.getVirtualItems().map((row) => {
                const version = filtered[row.index];
                return (
                  <button
                    key={version.id}
                    className={clsx(
                      styles.versionRow,
                      selected === version.id && styles.versionActive,
                    )}
                    style={{
                      position: "absolute",
                      top: 0,
                      left: 0,
                      width: "100%",
                      height: row.size,
                      transform: `translateY(${row.start}px)`,
                    }}
                    onClick={() => setSelected(version.id)}
                  >
                    <span className={styles.versionId}>{version.id}</span>
                    {version.kind !== "release" && (
                      <span className="badge">{version.kind}</span>
                    )}
                    <span className={styles.versionDate}>
                      {version.releaseTime.slice(0, 10)}
                    </span>
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </Modal>
  );
}

import { useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import { FileArchive, FolderOpen, Loader2, Search, Users } from "lucide-react";

import Modal from "./Modal";
import { api } from "../lib/api";
import { rovingKey } from "../lib/rovingFocus";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type { FolderInstance, GameVersion, Loader, PackPreview } from "../lib/types";

import styles from "./NewInstanceModal.module.css";

/**
 * What each loader *is*, in one line.
 *
 * The first decision anyone makes in this app was five bare proper nouns, in a
 * product that explains what a plan is, what a proxy is and what Java 17 is
 * for. Somebody who has never modded Minecraft cannot pick between Quilt and
 * NeoForge from the names, and picking wrong means every mod they then find is
 * the wrong one.
 *
 * Descriptions of the software, never claims about what it supports: which
 * loader has a build for which Minecraft version is read from the upstream at
 * runtime — the picker below already says "No Fabric builds for 1.21.4" when
 * that is the answer — and a sentence here asserting it would be the same
 * hardcoding this codebase refuses everywhere else.
 *
 * `Record<Loader, string>` rather than a list, so a loader added to `LOADERS`
 * and forgotten here fails the typecheck instead of rendering a blank row.
 */
const LOADER_BLURB: Record<Loader, string> = {
  vanilla: "Minecraft as it comes, with no mod loader at all.",
  fabric: "Light and quick to update. Most new mods are built for it.",
  quilt: "A fork of Fabric. It runs Fabric's mods as well as its own.",
  neoforge: "A fork of Forge, and the newer of the two.",
  forge: "The older line, and what many long-running packs are built on.",
};

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
  // The instances found inside a folder that was not itself one, waiting to be
  // picked from.
  const [inside, setInside] = useState<FolderInstance[] | null>(null);

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
      filters: [
        { name: "Modpack or instance", extensions: ["geodepack", "mrpack", "zip"] },
      ],
    });
    if (typeof picked !== "string") return;
    await importFrom(picked);
  }

  // A Prism or MultiMC instance folder, read where the user points at it:
  // Geode never goes looking through another launcher's data directory.
  async function importFolder() {
    const picked = await openDialog({ multiple: false, directory: true });
    if (typeof picked !== "string") return;
    // The folder a person thinks of as "my instances" is the one holding all
    // of them, so that is what they point at. When it is not itself an
    // instance but holds some, the instances are offered to pick from — ten of
    // them is a worse thing to refuse than to list, and which one they want is
    // theirs to say, so none is imported here.
    const found = await api.instancesInFolder(picked).catch(() => []);
    if (found.length > 0) {
      setInside(found);
      return;
    }
    await importFrom(picked);
  }

  async function importFrom(picked: string) {
    let preview: PackPreview;
    try {
      preview = await api.previewModpack(picked);
    } catch (e) {
      reportError("That is not a modpack or an instance", e);
      return;
    }

    if (preview.needsCurseforgeKey && !config?.hasCurseforgeKey) {
      reportError(
        "CurseForge packs are not available",
        "This pack's mods come from CurseForge, which this copy of Geode cannot reach.",
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

  // Which of a folder's instances to bring across is the person's choice, so
  // the dialog becomes that list until they make it or go back.
  if (inside) {
    return (
      <Modal
        title="Pick an instance"
        subtitle={`That folder is not an instance itself, but it holds ${
          inside.length === 1 ? "one" : inside.length
        }.`}
        onClose={onClose}
        width={520}
        footer={
          <>
            <button
              className="btn btn-ghost"
              onClick={() => setInside(null)}
              disabled={creating}
            >
              Back
            </button>
            <div style={{ flex: 1 }} />
            <button className="btn" onClick={onClose}>
              Cancel
            </button>
          </>
        }
      >
        <div className={styles.foundList}>
          {inside.map((found) => (
            <button
              key={found.path}
              className={styles.foundRow}
              disabled={creating}
              onClick={() => importFrom(found.path)}
            >
              <span className={styles.foundName}>{found.name}</span>
              <span className={styles.foundMeta}>
                {found.mcVersion}
                {found.loader !== "vanilla" &&
                  ` · ${LOADERS.find((l) => l.id === found.loader)?.label}`}
              </span>
              <span className={styles.foundHint}>Import</span>
            </button>
          ))}
        </div>
      </Modal>
    );
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
          {/* An instance folder rather than a file: a Prism or MultiMC
              instance is a directory, and a zipped one goes through the
              button beside this. */}
          <button
            className="btn btn-ghost"
            onClick={importFolder}
            disabled={creating}
            title="Import a Prism or MultiMC instance folder"
          >
            <FolderOpen size={15} />
            Import folder
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
        <span className="label" id="loader-label">
          Mod loader
        </span>
        {/* Five alternatives, one of which is in force — which is a radio
            group, and was five separate toggle buttons: four off, one on, and
            nothing saying that picking one unpicks the rest. The group is
            named from the `span`, which is not a `<label>` and so names
            nothing on its own. */}
        <div
          className={styles.loaderRow}
          role="radiogroup"
          aria-labelledby="loader-label"
          onKeyDown={(e) => {
            const next = rovingKey(
              e,
              LOADERS.map((l) => l.id),
            );
            if (next) setLoader(next);
          }}
        >
          {LOADERS.map((l) => (
            <button
              key={l.id}
              className={clsx(styles.loaderChip, loader === l.id && styles.loaderActive)}
              onClick={() => setLoader(l.id)}
              role="radio"
              aria-checked={loader === l.id}
              // One tab stop for the group, as a native radio row has: Tab
              // reaches the chosen loader, and the arrows move from there.
              tabIndex={loader === l.id ? 0 : -1}
              data-roving={l.id}
            >
              <span className={styles.loaderName}>{l.label}</span>
              <span className={styles.loaderBlurb}>{LOADER_BLURB[l.id]}</span>
            </button>
          ))}
        </div>

        {loader !== "vanilla" && (
          <div className={styles.loaderVersion}>
            {/* Named, because the grid above now has enough structure that an
                unlabelled select under it reads as belonging to nothing. */}
            <label className="label" htmlFor="loader-version">
              {LOADERS.find((l) => l.id === loader)?.label} version
            </label>
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
                id="loader-version"
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

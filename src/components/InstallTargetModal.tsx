import { useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Check, Loader2, Package, Plus } from "lucide-react";

import InstanceIcon from "./InstanceIcon";
import Modal from "./Modal";
import { api } from "../lib/api";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type { Instance, Loader, Project } from "../lib/types";

import styles from "./InstallTargetModal.module.css";

/**
 * Why an instance may not suit this project, or `null` if it does.
 *
 * Incompatible instances are shown greyed rather than hidden, and stay
 * selectable: a mod frequently runs on a version its metadata has not listed
 * yet, and refusing outright would block installs that would have worked. The
 * warning is the point, not the block.
 */
function compatibility(project: Project, instance: Instance): string | null {
  const needsLoader = project.kind === "mod";

  if (needsLoader && instance.loader === "vanilla") {
    return "no mod loader";
  }
  if (
    needsLoader &&
    project.loaders.length > 0 &&
    !project.loaders.includes(instance.loader)
  ) {
    return `no ${LOADERS.find((l) => l.id === instance.loader)?.label} build`;
  }
  if (
    project.gameVersions.length > 0 &&
    !project.gameVersions.includes(instance.mcVersion)
  ) {
    return `nothing for ${instance.mcVersion}`;
  }
  return null;
}

/** Newest first, by numeric segments, so "1.21.10" sorts above "1.21.9". */
function sortVersionsDescending(versions: string[]): string[] {
  return [...versions].sort((a, b) => {
    const pa = a.split(/[.\-+]/);
    const pb = b.split(/[.\-+]/);
    for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
      const na = Number(pa[i]);
      const nb = Number(pb[i]);
      if (Number.isNaN(na) || Number.isNaN(nb)) {
        const cmp = (pb[i] ?? "").localeCompare(pa[i] ?? "");
        if (cmp !== 0) return cmp;
      } else if (na !== nb) {
        return nb - na;
      }
    }
    return 0;
  });
}

export default function InstallTargetModal({
  project,
  preselect,
  installedIn,
  busy,
  onPick,
  onCreateAndInstall,
  onClose,
}: {
  project: Project;
  preselect?: string;
  installedIn?: Set<string>;
  busy: boolean;
  onPick: (instance: Instance) => void;
  onCreateAndInstall: (
    name: string,
    gameVersion: string,
    loader: Loader,
  ) => void;
  onClose: () => void;
}) {
  const instances = useStore((s) => s.instances);

  const compatible = useMemo(
    () => instances.filter((i) => !compatibility(project, i)),
    [instances, project],
  );

  const [mode, setMode] = useState<"existing" | "new">(
    // With nothing that fits, creating one is the only useful action.
    instances.length === 0 || compatible.length === 0 ? "new" : "existing",
  );

  const [selected, setSelected] = useState<string>(
    preselect && compatible.some((i) => i.id === preselect)
      ? preselect
      : (compatible[0]?.id ?? instances[0]?.id ?? ""),
  );

  // Suitable instances first, so the obvious choice sits at the top.
  const ordered = useMemo(
    () =>
      [...instances].sort(
        (a, b) =>
          (compatibility(project, a) ? 1 : 0) -
          (compatibility(project, b) ? 1 : 0),
      ),
    [instances, project],
  );

  // --- new instance ---

  const versionOptions = useMemo(
    () => sortVersionsDescending(project.gameVersions),
    [project.gameVersions],
  );
  const loaderOptions = useMemo(() => {
    if (project.kind !== "mod") return [];
    return LOADERS.filter(
      (l) => l.id !== "vanilla" && project.loaders.includes(l.id),
    );
  }, [project.kind, project.loaders]);

  const [newVersion, setNewVersion] = useState(versionOptions[0] ?? "");
  const [newLoader, setNewLoader] = useState<Loader>(
    loaderOptions[0]?.id ?? "vanilla",
  );
  const [newName, setNewName] = useState("");

  // Only some loaders are published for any given game version.
  const [loaderAvailable, setLoaderAvailable] = useState<boolean | null>(null);
  useEffect(() => {
    if (newLoader === "vanilla" || !newVersion) {
      setLoaderAvailable(true);
      return;
    }
    let cancelled = false;
    setLoaderAvailable(null);
    api
      .listLoaderVersions(newLoader, newVersion)
      .then((list) => !cancelled && setLoaderAvailable(list.length > 0))
      .catch(() => !cancelled && setLoaderAvailable(false));
    return () => {
      cancelled = true;
    };
  }, [newLoader, newVersion]);

  const suggestedName =
    newName.trim() ||
    `${project.name} ${newVersion}`.trim() ||
    project.name;

  const target = instances.find((i) => i.id === selected);
  const targetProblem = target ? compatibility(project, target) : null;

  const canCreate =
    Boolean(newVersion) && loaderAvailable === true && !busy;

  return (
    <Modal
      title={`Install ${project.name}`}
      subtitle="Choose where it goes."
      onClose={onClose}
      width={480}
      footer={
        mode === "existing" ? (
          <>
            <button className="btn" onClick={onClose}>
              Cancel
            </button>
            <button
              className={clsx("btn", targetProblem ? "btn-danger" : "btn-primary")}
              onClick={() => target && onPick(target)}
              disabled={!target || busy}
            >
              {busy && <Loader2 size={14} className="spin" />}
              {!target
                ? "Install"
                : targetProblem
                  ? `Install anyway into ${target.name}`
                  : `Install into ${target.name}`}
            </button>
          </>
        ) : (
          <>
            <button className="btn" onClick={onClose}>
              Cancel
            </button>
            <button
              className="btn btn-primary"
              onClick={() =>
                onCreateAndInstall(suggestedName, newVersion, newLoader)
              }
              disabled={!canCreate}
            >
              {busy ? <Loader2 size={14} className="spin" /> : <Plus size={14} />}
              Create and install
            </button>
          </>
        )
      }
    >
      {instances.length > 0 && (
        <div className={styles.modes}>
          <button
            className={clsx(styles.mode, mode === "existing" && styles.modeActive)}
            onClick={() => setMode("existing")}
          >
            Existing instance
          </button>
          <button
            className={clsx(styles.mode, mode === "new" && styles.modeActive)}
            onClick={() => setMode("new")}
          >
            New instance
          </button>
        </div>
      )}

      {mode === "existing" ? (
        <div className={styles.list}>
          {ordered.map((instance) => {
            const problem = compatibility(project, instance);
            const already = installedIn?.has(instance.id);
            return (
              <button
                key={instance.id}
                className={clsx(
                  styles.row,
                  problem && styles.incompatible,
                  selected === instance.id && styles.selected,
                )}
                onClick={() => setSelected(instance.id)}
                onDoubleClick={() => !problem && onPick(instance)}
              >
                <InstanceIcon instance={instance} size={32} />
                <span className={styles.meta}>
                  <span className={styles.name}>{instance.name}</span>
                  <span className={styles.detail}>
                    {instance.mcVersion}
                    {instance.loader !== "vanilla" &&
                      ` · ${LOADERS.find((l) => l.id === instance.loader)?.label}`}
                  </span>
                </span>

                {already && (
                  <span className={styles.already} title="Already installed here">
                    <Package size={12} />
                    installed
                  </span>
                )}
                {problem && (
                  <span className={styles.warn}>
                    <AlertTriangle size={12} />
                    {problem}
                  </span>
                )}
                {selected === instance.id && !problem && (
                  <Check size={15} className={styles.check} />
                )}
              </button>
            );
          })}
        </div>
      ) : (
        <>
          {versionOptions.length === 0 ? (
            <p className={styles.note}>
              {project.name} does not list any Minecraft versions, so an instance
              cannot be built from it automatically.
            </p>
          ) : (
            <>
              <div className={styles.newGrid}>
                <div>
                  <label className="label" htmlFor="new-mc">
                    Minecraft version
                  </label>
                  <select
                    id="new-mc"
                    className="select"
                    value={newVersion}
                    onChange={(e) => setNewVersion(e.target.value)}
                  >
                    {versionOptions.map((v) => (
                      <option key={v} value={v}>
                        {v}
                      </option>
                    ))}
                  </select>
                </div>

                {loaderOptions.length > 0 && (
                  <div>
                    <label className="label" htmlFor="new-loader">
                      Mod loader
                    </label>
                    <select
                      id="new-loader"
                      className="select"
                      value={newLoader}
                      onChange={(e) => setNewLoader(e.target.value as Loader)}
                    >
                      {loaderOptions.map((l) => (
                        <option key={l.id} value={l.id}>
                          {l.label}
                        </option>
                      ))}
                    </select>
                  </div>
                )}
              </div>

              <div>
                <label className="label" htmlFor="new-name">
                  Instance name
                </label>
                <input
                  id="new-name"
                  className="input"
                  value={newName}
                  placeholder={suggestedName}
                  onChange={(e) => setNewName(e.target.value)}
                />
              </div>

              <p className={styles.note}>
                {loaderAvailable === null ? (
                  <>
                    <Loader2 size={12} className="spin" /> Checking{" "}
                    {LOADERS.find((l) => l.id === newLoader)?.label} builds for{" "}
                    {newVersion}…
                  </>
                ) : loaderAvailable ? (
                  <>
                    Creates a {newVersion}
                    {newLoader !== "vanilla" &&
                      ` ${LOADERS.find((l) => l.id === newLoader)?.label}`}{" "}
                    instance using the newest loader build, then installs{" "}
                    {project.name} into it.
                  </>
                ) : (
                  <span className={styles.noteWarn}>
                    <AlertTriangle size={12} />
                    No {LOADERS.find((l) => l.id === newLoader)?.label} build
                    exists for {newVersion}. Pick another combination.
                  </span>
                )}
              </p>
            </>
          )}
        </>
      )}
    </Modal>
  );
}

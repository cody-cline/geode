import { useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Check, Loader2, Package, Plus } from "lucide-react";

import InstanceIcon from "./InstanceIcon";
import Modal from "./Modal";
import { api } from "../lib/api";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type { Instance, Loader, Project, ProjectVersion } from "../lib/types";

import styles from "./InstallTargetModal.module.css";

/**
 * Why an instance may not suit this project, or `null` if it does.
 *
 * Judged against the project's actual files where they are known, because
 * `project.loaders` and `project.gameVersions` are independent unions: a mod
 * listing both Fabric and 1.21.1 does not necessarily have a Fabric file *for*
 * 1.21.1. Falls back to those unions only while the files are still loading.
 *
 * Incompatible instances are shown greyed rather than hidden, and stay
 * selectable: metadata frequently lags reality, and refusing outright would
 * block installs that would have worked. The warning is the point, not the
 * block.
 */
function compatibility(
  project: Project,
  instance: Instance,
  files: ProjectVersion[] | null,
): string | null {
  const needsLoader = project.kind === "mod";
  const loaderLabel = LOADERS.find((l) => l.id === instance.loader)?.label;

  if (needsLoader && instance.loader === "vanilla") {
    return "no mod loader";
  }

  if (files === null) {
    if (
      needsLoader &&
      project.loaders.length > 0 &&
      !project.loaders.includes(instance.loader)
    ) {
      return `no ${loaderLabel} build`;
    }
    if (
      project.gameVersions.length > 0 &&
      !project.gameVersions.includes(instance.mcVersion)
    ) {
      return `nothing for ${instance.mcVersion}`;
    }
    return null;
  }

  const forLoader = needsLoader
    ? files.filter((f) => f.loaders.includes(instance.loader))
    : files;

  if (needsLoader && forLoader.length === 0) {
    return `no ${loaderLabel} build`;
  }
  if (!forLoader.some((f) => f.gameVersions.includes(instance.mcVersion))) {
    return needsLoader
      ? `no ${loaderLabel} build for ${instance.mcVersion}`
      : `nothing for ${instance.mcVersion}`;
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

  // --- new instance ---
  //
  // Options come from the project's actual files, not from `project.loaders`
  // and `project.gameVersions`. Those are flattened unions: a mod on both
  // Fabric and NeoForge that only ever shipped NeoForge for older versions
  // would otherwise offer NeoForge + a Fabric-only version, a pair that has no
  // file behind it. Each file carries its loaders and game versions together,
  // so pairing them here means every combination on offer is real.
  const [files, setFiles] = useState<ProjectVersion[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    // Unfiltered on purpose — this is what defines the choices.
    api
      .getProjectVersions(project.provider, project.id)
      .then((list) => !cancelled && setFiles(list))
      .catch(() => !cancelled && setFiles([]));
    return () => {
      cancelled = true;
    };
  }, [project.provider, project.id]);

  const installable = useMemo(
    () => (files ?? []).filter((f) => !f.blockedReason && f.files.length > 0),
    [files],
  );

  const loaderOptions = useMemo(() => {
    if (project.kind !== "mod") return [];
    const present = new Set(installable.flatMap((f) => f.loaders));
    return LOADERS.filter((l) => l.id !== "vanilla" && present.has(l.id));
  }, [project.kind, installable]);

  const [newLoader, setNewLoader] = useState<Loader>("vanilla");
  const [newVersion, setNewVersion] = useState("");
  const [newName, setNewName] = useState("");

  const compatible = useMemo(
    () => instances.filter((i) => !compatibility(project, i, files)),
    [instances, project, files],
  );

  const [mode, setMode] = useState<"existing" | "new">(
    instances.length === 0 ? "new" : "existing",
  );

  // Once the files land, a set of instances that all fail is a dead end —
  // switch to the only useful action, unless the user has already chosen.
  const [modeTouched, setModeTouched] = useState(false);
  useEffect(() => {
    if (!modeTouched && files !== null && compatible.length === 0) {
      setMode("new");
    }
  }, [files, compatible.length, modeTouched]);

  const [selected, setSelected] = useState<string>(instances[0]?.id ?? "");
  useEffect(() => {
    const preferred =
      preselect && compatible.some((i) => i.id === preselect)
        ? preselect
        : compatible[0]?.id;
    if (preferred) setSelected(preferred);
    // Only re-home the selection when the compatible set itself changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [compatible.length, preselect]);

  // Suitable instances first, so the obvious choice sits at the top.
  const ordered = useMemo(
    () =>
      [...instances].sort(
        (a, b) =>
          (compatibility(project, a, files) ? 1 : 0) -
          (compatibility(project, b, files) ? 1 : 0),
      ),
    [instances, project, files],
  );

  // Versions that have a file for the chosen loader — nothing else.
  const versionOptions = useMemo(() => {
    const relevant =
      newLoader === "vanilla"
        ? installable
        : installable.filter((f) => f.loaders.includes(newLoader));
    return sortVersionsDescending([
      ...new Set(relevant.flatMap((f) => f.gameVersions)),
    ]);
  }, [installable, newLoader]);

  // Settle on a loader once the files arrive, then keep the version valid for
  // whichever loader is chosen.
  useEffect(() => {
    if (loaderOptions.length > 0 && !loaderOptions.some((l) => l.id === newLoader)) {
      setNewLoader(loaderOptions[0].id);
    }
  }, [loaderOptions, newLoader]);

  useEffect(() => {
    if (versionOptions.length > 0 && !versionOptions.includes(newVersion)) {
      setNewVersion(versionOptions[0]);
    }
  }, [versionOptions, newVersion]);

  const suggestedName =
    newName.trim() ||
    `${project.name} ${newVersion}`.trim() ||
    project.name;

  const target = instances.find((i) => i.id === selected);
  const targetProblem = target ? compatibility(project, target, files) : null;

  const canCreate = Boolean(newVersion) && !busy;

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
            onClick={() => {
              setModeTouched(true);
              setMode("existing");
            }}
          >
            Existing instance
          </button>
          <button
            className={clsx(styles.mode, mode === "new" && styles.modeActive)}
            onClick={() => {
              setModeTouched(true);
              setMode("new");
            }}
          >
            New instance
          </button>
        </div>
      )}

      {mode === "existing" ? (
        <div className={styles.list}>
          {ordered.map((instance) => {
            const problem = compatibility(project, instance, files);
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
          {files === null ? (
            <p className={styles.note}>
              <Loader2 size={12} className="spin" />
              Reading the versions {project.name} publishes…
            </p>
          ) : versionOptions.length === 0 ? (
            <p className={styles.note}>
              {project.name} has no downloadable files listed, so an instance
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
                Creates a {newVersion}
                {newLoader !== "vanilla" &&
                  ` ${LOADERS.find((l) => l.id === newLoader)?.label}`}{" "}
                instance using the newest loader build, then installs{" "}
                {project.name} into it.
              </p>
            </>
          )}
        </>
      )}
    </Modal>
  );
}

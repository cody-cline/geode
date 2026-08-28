import { useMemo, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Check, Loader2, Package } from "lucide-react";

import InstanceIcon from "./InstanceIcon";
import Modal from "./Modal";
import { titleCase } from "../lib/format";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type { Instance, Project } from "../lib/types";

import styles from "./InstallTargetModal.module.css";

/**
 * Why a given instance may not suit this project.
 *
 * Mismatches are shown rather than hidden: an instance can be perfectly
 * capable of running a mod whose metadata simply hasn't listed the version yet,
 * and second-guessing that would block legitimate installs. The user picks; we
 * just make the risk visible.
 */
function compatibility(project: Project, instance: Instance): string | null {
  const wantsLoader = project.kind === "mod";

  if (
    wantsLoader &&
    instance.loader === "vanilla" &&
    project.kind === "mod"
  ) {
    return "no mod loader — mods will not load";
  }
  if (
    wantsLoader &&
    project.loaders.length > 0 &&
    !project.loaders.includes(instance.loader)
  ) {
    const label = LOADERS.find((l) => l.id === instance.loader)?.label;
    return `no ${label} build listed`;
  }
  if (
    project.gameVersions.length > 0 &&
    !project.gameVersions.includes(instance.mcVersion)
  ) {
    return `nothing listed for ${instance.mcVersion}`;
  }
  return null;
}

export default function InstallTargetModal({
  project,
  preselect,
  installedIn,
  busy,
  onPick,
  onClose,
}: {
  project: Project;
  preselect?: string;
  /** Instance ids that already have this project. */
  installedIn?: Set<string>;
  busy: boolean;
  onPick: (instance: Instance) => void;
  onClose: () => void;
}) {
  const instances = useStore((s) => s.instances);
  const navigate = useStore((s) => s.navigate);

  const [selected, setSelected] = useState<string>(
    preselect && instances.some((i) => i.id === preselect)
      ? preselect
      : (instances.find((i) => !compatibility(project, i))?.id ??
        instances[0]?.id ??
        ""),
  );

  // Suitable instances first, so the obvious choice is at the top.
  const ordered = useMemo(() => {
    return [...instances].sort((a, b) => {
      const aBad = compatibility(project, a) ? 1 : 0;
      const bBad = compatibility(project, b) ? 1 : 0;
      return aBad - bBad;
    });
  }, [instances, project]);

  const target = instances.find((i) => i.id === selected);

  if (instances.length === 0) {
    return (
      <Modal
        title="No instances yet"
        subtitle={`Create one before installing ${project.name}.`}
        onClose={onClose}
        width={400}
        footer={
          <>
            <button className="btn" onClick={onClose}>
              Cancel
            </button>
            <button
              className="btn btn-primary"
              onClick={() => {
                navigate({ view: "library" });
                onClose();
              }}
            >
              Go to Library
            </button>
          </>
        }
      >
        <p className={styles.emptyNote}>
          Mods install into an instance, so there needs to be one first.
        </p>
      </Modal>
    );
  }

  return (
    <Modal
      title={`Install ${project.name}`}
      subtitle="Choose which instance it goes into."
      onClose={onClose}
      width={460}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn btn-primary"
            onClick={() => target && onPick(target)}
            disabled={!target || busy}
          >
            {busy && <Loader2 size={14} className="spin" />}
            {target ? `Install into ${target.name}` : "Install"}
          </button>
        </>
      }
    >
      <div className={styles.list}>
        {ordered.map((instance) => {
          const problem = compatibility(project, instance);
          const already = installedIn?.has(instance.id);
          return (
            <button
              key={instance.id}
              className={clsx(
                styles.row,
                selected === instance.id && styles.selected,
              )}
              onClick={() => setSelected(instance.id)}
              onDoubleClick={() => onPick(instance)}
            >
              <InstanceIcon instance={instance} size={32} />
              <span className={styles.meta}>
                <span className={styles.name}>{instance.name}</span>
                <span className={styles.detail}>
                  {instance.mcVersion}
                  {instance.loader !== "vanilla" &&
                    ` · ${titleCase(instance.loader)}`}
                </span>
              </span>

              {already && (
                <span className={styles.already} title="Already installed here">
                  <Package size={12} />
                  installed
                </span>
              )}
              {problem && (
                <span className={styles.warn} title={problem}>
                  <AlertTriangle size={12} />
                  {problem}
                </span>
              )}
              {selected === instance.id && (
                <Check size={15} className={styles.check} />
              )}
            </button>
          );
        })}
      </div>
    </Modal>
  );
}

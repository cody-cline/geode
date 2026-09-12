import { useEffect, useState } from "react";
import clsx from "clsx";
import { Blocks, Loader2, Puzzle } from "lucide-react";

import Modal from "./Modal";
import { api } from "../lib/api";
import { useStore } from "../lib/store";
import type { Project, ServerKindInfo } from "../lib/types";

import styles from "./PackServerModal.module.css";

/**
 * Which server software should host this modpack.
 *
 * Installing a pack as an instance asks nothing — the pack names its own
 * loader. Hosting it has a real choice, because more than one server runs any
 * given loader: a Forge pack runs on Forge, and equally on Mohist or Arclight,
 * which run the same mods and take Bukkit plugins besides.
 */
export default function PackServerModal({
  project,
  versionId,
  onClose,
  onChoose,
}: {
  project: Project;
  versionId?: string;
  onClose: () => void;
  onChoose: (kind: string) => void;
}) {
  const reportError = useStore((s) => s.reportError);

  const [kinds, setKinds] = useState<ServerKindInfo[] | null>(null);
  const [kind, setKind] = useState("");

  useEffect(() => {
    let cancelled = false;
    api
      .serverKindsForPack(project.provider, project.id, versionId)
      .then((found) => {
        if (cancelled) return;
        setKinds(found);
        setKind(found[0]?.id ?? "");
      })
      .catch((e) => {
        if (cancelled) return;
        reportError("Could not work out which servers run this pack", e);
        setKinds([]);
      });
    return () => {
      cancelled = true;
    };
  }, [project.provider, project.id, versionId, reportError]);

  const extras = (kinds ?? []).filter((k) => k.takesPlugins).length;

  return (
    <Modal
      title={`Host ${project.name}`}
      subtitle="Its mods, minus the ones that only matter on a client."
      onClose={onClose}
      width={500}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn btn-primary"
            onClick={() => onChoose(kind)}
            disabled={!kind}
          >
            Create server
          </button>
        </>
      }
    >
      {kinds === null ? (
        <p className={styles.pending}>
          <Loader2 size={13} className="spin" />
          Reading what this pack was built for…
        </p>
      ) : kinds.length === 0 ? (
        <p className={styles.pending}>
          Nothing Geode installs runs this pack's loader.
        </p>
      ) : (
        <>
          <span className="label">Server software</span>
          <div className={styles.kinds}>
            {kinds.map((option) => (
              <button
                key={option.id}
                className={clsx(styles.kind, kind === option.id && styles.chosen)}
                onClick={() => setKind(option.id)}
              >
                <span className={styles.kindName}>{option.name}</span>
                <span className={styles.kindTags}>
                  {option.takesMods && (
                    <span className={styles.tag}>
                      <Blocks size={9} />
                      mods
                    </span>
                  )}
                  {option.takesPlugins && (
                    <span className={styles.tag}>
                      <Puzzle size={9} />
                      plugins
                    </span>
                  )}
                </span>
              </button>
            ))}
          </div>
          {extras > 0 && (
            <p className="hint">
              The first runs the pack as it was built. The rest run the same
              mods and take Bukkit plugins as well, which the pack's own loader
              cannot.
            </p>
          )}
        </>
      )}
    </Modal>
  );
}

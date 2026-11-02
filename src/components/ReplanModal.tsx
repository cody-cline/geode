import { useEffect, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Blocks, Loader2 } from "lucide-react";

import Modal from "./Modal";
import { api, errorMessage } from "../lib/api";
import { titleCase } from "../lib/format";
import { LOADERS } from "../lib/types";
import type { Instance, PlanResolution, PlanTarget } from "../lib/types";

import planStyles from "../views/PlanView.module.css";
import styles from "./ReplanModal.module.css";

/** Loaders have fixed capitalisation ("NeoForge"), so never title-case them. */
function loaderLabel(id: string): string {
  return LOADERS.find((l) => l.id === id)?.label ?? titleCase(id);
}

/**
 * What moving this instance to another loader or Minecraft version would cost.
 *
 * The same answer the Plans view gives a versionless plan, asked of something
 * already installed — and drawn with the same target cards, so the two screens
 * cannot drift into saying one thing two ways.
 *
 * Read-only on purpose. It applies nothing, and the version is not editable
 * from here: a bare version dropdown is the footgun this replaces, since it
 * would leave a mods folder full of jars built for the version before.
 */
export default function ReplanModal({
  instance,
  onClose,
}: {
  instance: Instance;
  onClose: () => void;
}) {
  const [resolution, setResolution] = useState<PlanResolution | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [chosen, setChosen] = useState(0);

  useEffect(() => {
    let dropped = false;
    api
      .resolveInstance(instance.id)
      .then((found) => !dropped && setResolution(found))
      .catch((e) => !dropped && setError(errorMessage(e)));
    return () => {
      dropped = true;
    };
  }, [instance.id]);

  /** Where the instance already is, so one card can be read as "you are here". */
  const isHere = (option: PlanTarget) =>
    option.loader === instance.loader &&
    option.gameVersion === instance.mcVersion;

  const target: PlanTarget | undefined = resolution?.targets[chosen];
  const blockers = (target?.fits ?? []).filter((f) => f.fit === "missing");

  return (
    <Modal
      title="Where else could this run?"
      subtitle={`${instance.name} — ${
        instance.loader === "vanilla"
          ? "no mod loader"
          : `${loaderLabel(instance.loader)} ${instance.loaderVersion}`
      }, Minecraft ${instance.mcVersion}`}
      onClose={onClose}
      width={560}
      footer={
        <button className="btn" onClick={onClose}>
          Close
        </button>
      }
    >
      {error && <p className={styles.error}>{error}</p>}

      {!resolution && !error && (
        <div className={styles.loading}>
          <Loader2 size={16} className="spin" />
          <span>Reading what every mod here publishes…</span>
        </div>
      )}

      {resolution && (
        <>
          {resolution.targets.length > 0 && (
            <p className={styles.reads}>
              Nothing here changes this instance. It is what the mods you
              already have would allow, read from what each one actually
              publishes.
            </p>
          )}

          {resolution.targets.length === 0 ? (
            <p className={styles.note}>
              {resolution.unresolved.length === 0
                ? "There are no mods in this instance, so nothing here pins it to one Minecraft version or one loader."
                : "None of this instance's mods could be checked, so there is nothing to rank. The list below says why for each one."}
            </p>
          ) : (
            <>
              <div className={planStyles.targets}>
                {resolution.targets.map((option, index) => (
                  <button
                    key={`${option.loader}:${option.gameVersion}`}
                    className={clsx(
                      planStyles.target,
                      index === chosen && planStyles.targetChosen,
                      option.missing > 0 && planStyles.targetIncomplete,
                    )}
                    onClick={() => setChosen(index)}
                  >
                    <span className={planStyles.targetHead}>
                      <span className={planStyles.targetLoader}>
                        {loaderLabel(option.loader)}
                      </span>
                      <span className={planStyles.targetVersion}>
                        {option.gameVersion}
                      </span>
                      {index === 0 && (
                        <span className={planStyles.best}>best</span>
                      )}
                      {isHere(option) && (
                        <span className={styles.here}>installed</span>
                      )}
                    </span>

                    <span
                      className={clsx(
                        planStyles.coverage,
                        option.missing === 0 && planStyles.coverageFull,
                      )}
                    >
                      {option.missing === 0
                        ? `All ${option.fits.length} mods run here`
                        : `${option.fits.length - option.missing} of ${option.fits.length} — ${option.missing} left out`}
                    </span>

                    {(option.connector > 0 || option.near > 0) && (
                      <span className={planStyles.compromises}>
                        {option.connector > 0 && (
                          <span
                            className={clsx(
                              planStyles.fit,
                              planStyles.connector,
                            )}
                          >
                            <Blocks size={10} />
                            {option.connector} via Connector
                          </span>
                        )}
                        {option.near > 0 && (
                          <span
                            className={clsx(
                              planStyles.fit,
                              planStyles.nearVersion,
                            )}
                          >
                            {option.near} other version
                          </span>
                        )}
                      </span>
                    )}
                  </button>
                ))}
              </div>

              {target && target.connector > 0 && (
                <p className={planStyles.soft}>
                  <Blocks size={12} />
                  <span>
                    {target.connector} mod{target.connector === 1 ? "" : "s"}{" "}
                    here {target.connector === 1 ? "has" : "have"} no{" "}
                    {loaderLabel(target.loader)} build, so{" "}
                    {target.connector === 1 ? "it runs" : "they run"} on Sinytra
                    Connector. That works, but it is a compatibility layer — a
                    target without it is the safer move.
                  </span>
                </p>
              )}

              {target && target.near > 0 && (
                <p className={planStyles.soft}>
                  <AlertTriangle size={12} />
                  <span>
                    {target.near} mod{target.near === 1 ? "" : "s"} would use a
                    build from a neighbouring version. Usually fine on Fabric,
                    less reliable on Forge and NeoForge.
                  </span>
                </p>
              )}

              {blockers.length > 0 && (
                <section className={planStyles.blockers}>
                  <h3 className={planStyles.blockTitle}>
                    Would be left behind
                  </h3>
                  {blockers.map((blocker) => (
                    <div
                      key={`${blocker.provider}:${blocker.projectId}`}
                      className={planStyles.blocker}
                    >
                      <span className={planStyles.blockText}>
                        <span className={planStyles.blockName}>
                          {blocker.name}
                        </span>
                        <span className={planStyles.blockWhere}>
                          {blocker.elsewhere && blocker.elsewhere.length > 0
                            ? `Runs on ${blocker.elsewhere.join(", ")}`
                            : "No build anywhere"}
                        </span>
                      </span>
                    </div>
                  ))}
                </section>
              )}
            </>
          )}

          {/* The honest half of the answer. A mod with no project id was never
              looked at, and a count that quietly left it out would say a move
              is clean when half the folder went unchecked. */}
          {resolution.unresolved.length > 0 && (
            <section className={planStyles.blockers}>
              <h3 className={planStyles.blockTitle}>Could not be checked</h3>
              {resolution.unresolved.map((mod, index) => (
                <div
                  key={`${mod.provider}:${mod.projectId}:${mod.name}:${index}`}
                  className={planStyles.blocker}
                >
                  <span className={planStyles.blockText}>
                    <span className={planStyles.blockName}>{mod.name}</span>
                    <span className={planStyles.blockWhere}>{mod.reason}</span>
                  </span>
                </div>
              ))}
            </section>
          )}

          {resolution.targets.length > 0 &&
            resolution.unresolved.length === 0 && (
              <p className={styles.note}>
                Every mod in this instance was checked.
              </p>
            )}

          {resolution.warnings.map((warning) => (
            <p key={warning} className={planStyles.warning}>
              <AlertTriangle size={12} />
              <span>{warning}</span>
            </p>
          ))}
        </>
      )}
    </Modal>
  );
}

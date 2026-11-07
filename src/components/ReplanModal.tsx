import { useEffect, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Blocks, Loader2 } from "lucide-react";

import Modal from "./Modal";
import StatusNote from "./StatusNote";
import { api, errorMessage } from "../lib/api";
import { titleCase } from "../lib/format";
import { sentence } from "../lib/share";
import type { StatusLine } from "../lib/share";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type {
  Instance,
  PlanResolution,
  PlanTarget,
  RetargetReport,
} from "../lib/types";

import planStyles from "../views/PlanView.module.css";
import styles from "./ReplanModal.module.css";

/** Loaders have fixed capitalisation ("NeoForge"), so never title-case them. */
function loaderLabel(id: string): string {
  return LOADERS.find((l) => l.id === id)?.label ?? titleCase(id);
}

/** Which instance a re-target lands on. `""` leaves the choice to Geode. */
type LandOn = "" | "here" | "copy";

/** What a finished re-target amounts to, in one line. */
function retargetLine(report: RetargetReport): StatusLine {
  // The preview said "using its 1.21.3 build" about some of these, and the
  // install did exactly that — so the count says so rather than rounding every
  // jar off to the target's own version.
  // A mod carried across on a mismatched build is kept out of this count: the
  // preview rated a neighbouring patch as usable, and nothing rated these.
  // Counting them together would dress one person's override up as the
  // resolver's judgement.
  const near = report.repointed.filter(
    (one) => one.viaVersion && !one.ignoredGameVersion,
  );
  const versions = [...new Set(near.map((one) => one.viaVersion))];
  const anyway = report.repointed.filter((one) => one.ignoredGameVersion);
  const moved =
    `${report.repointed.length} mod${report.repointed.length === 1 ? "" : "s"} now built for ${loaderLabel(report.loader)} ${report.mcVersion}` +
    (near.length > 0
      ? `, ${near.length} of them on its ${versions.join(" and ")} build`
      : "") +
    (anyway.length > 0
      ? `, ${anyway.length} carried over on a build for another version because you asked Geode to ignore it`
      : "");
  const left = [
    report.stranded.length > 0 && `${report.stranded.length} turned off`,
    report.unchecked.length > 0 && `${report.unchecked.length} not checked`,
  ].filter(Boolean) as string[];
  return {
    // A carried mod warns for the same reason a stranded one does: there is
    // something here the person has to look at, and a tick saying it all went
    // fine would be the screen deciding that for them.
    tone: report.stranded.length > 0 || anyway.length > 0 ? "warn" : "ok",
    title: report.appliedToCopy ? `Applied to ${report.instanceName}` : "Target changed",
    detail: [
      report.copiedBecause && `A copy was made because ${report.copiedBecause}.`,
      sentence(left.length > 0 ? `${moved}, ${left.join(" and ")}` : moved),
    ]
      .filter(Boolean)
      .join(" "),
  };
}

/**
 * What moving this instance to another loader or Minecraft version would cost,
 * and the one place that move is made.
 *
 * The same answer the Plans view gives a versionless plan, asked of something
 * already installed — and drawn with the same target cards, so the two screens
 * cannot drift into saying one thing two ways.
 *
 * Looking is free: the ranking is read from what every mod here publishes, and
 * nothing moves until a target is applied. Applying is the end of that reading
 * rather than a way past it, which is why there is no bare version dropdown
 * anywhere — one would leave a mods folder full of jars built for the version
 * before and say nothing about it. A move applied to this instance cannot be
 * undone, so a copy is Geode's own answer whenever there is anything to lose,
 * and the report says which happened and why.
 */
export default function ReplanModal({
  instance,
  onApplied,
  onClose,
}: {
  instance: Instance;
  /**
   * A re-target landed. The report names the instance it landed on, so the
   * screen behind this dialog can put its own copy of the pair back in step.
   */
  onApplied: (report: RetargetReport) => void;
  onClose: () => void;
}) {
  const navigate = useStore((s) => s.navigate);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const refreshInstances = useStore((s) => s.refreshInstances);

  const [resolution, setResolution] = useState<PlanResolution | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [chosen, setChosen] = useState(0);
  const [landOn, setLandOn] = useState<LandOn>("");
  const [applying, setApplying] = useState(false);
  const [moved, setMoved] = useState<RetargetReport | null>(null);

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

  /**
   * Move this instance — or a copy of it — to the chosen target. Nothing is
   * staged and nothing is reversible: it downloads files and renames jars, so
   * the report it comes back with is the only account of what happened.
   */
  async function apply() {
    if (!target) return;
    setApplying(true);
    try {
      const report = await api.retargetInstance(
        instance.id,
        target.loader,
        target.gameVersion,
        landOn === "" ? null : landOn === "copy",
      );
      setMoved(report);
      onApplied(report);
      await refreshInstances();
      toast({ kind: "success", title: retargetLine(report).title });
    } catch (e) {
      reportError("Could not change the target", e);
    } finally {
      setApplying(false);
    }
  }

  return (
    <Modal
      title={moved ? "What happened" : "Where else could this run?"}
      subtitle={`${instance.name} — ${
        instance.loader === "vanilla"
          ? "no mod loader"
          : `${loaderLabel(instance.loader)} ${instance.loaderVersion}`
      }, Minecraft ${instance.mcVersion}`}
      onClose={onClose}
      width={560}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Close
          </button>
          {moved
            ? moved.instanceId !== instance.id && (
                <button
                  className="btn btn-primary"
                  onClick={() => {
                    onClose();
                    navigate({ view: "instance", id: moved.instanceId });
                  }}
                >
                  Open {moved.instanceName}
                </button>
              )
            : target && (
                <button
                  className="btn btn-primary"
                  onClick={apply}
                  disabled={applying || instance.running}
                  title={instance.running ? "Stop the game first" : undefined}
                >
                  {applying && <Loader2 size={14} className="spin" />}
                  Apply {loaderLabel(target.loader)} {target.gameVersion}
                </button>
              )}
        </>
      }
    >
      {error && <p className={styles.error}>{error}</p>}

      {!resolution && !error && (
        <div className={styles.loading}>
          <Loader2 size={16} className="spin" />
          <span>Reading what every mod here publishes…</span>
        </div>
      )}

      {/* What the move did, in place of the ranking that led to it: the files
          have moved, so a ranking read before they did is no longer an answer
          about this instance. */}
      {moved && (
        <div className={styles.result}>
          <StatusNote line={retargetLine(moved)} />

          {moved.connector && moved.connector.length > 0 && (
            <p className={styles.note}>
              Installed for the Fabric mods that need it:{" "}
              {moved.connector.join(", ")}.
            </p>
          )}

          {/* Its own section, above the two below it, because it is the only
              one naming mods that are switched *on* and may not load. Neither
              re-pointed quietly nor left behind: a file did land, and it is
              not a build for this target. */}
          {moved.repointed.some((one) => one.ignoredGameVersion) && (
            <section className={planStyles.blockers}>
              <h3 className={planStyles.blockTitle}>Carried over anyway</h3>
              {moved.repointed
                .filter((one) => one.ignoredGameVersion)
                .map((one) => (
                  <div key={one.fileName} className={planStyles.blocker}>
                    <span className={planStyles.blockText}>
                      <span className={planStyles.blockName}>{one.name}</span>
                      <span
                        className={clsx(planStyles.blockWhere, styles.wraps)}
                      >
                        Nothing is published for {loaderLabel(moved.loader)}{" "}
                        {moved.mcVersion}, so its{" "}
                        {one.viaVersion
                          ? `${one.viaVersion} build`
                          : "newest build"}{" "}
                        was installed because this mod ignores the Minecraft
                        version. It is on, and whether it runs is not something
                        Geode can tell you — if the game stops starting, this is
                        the first thing to turn off.
                      </span>
                    </span>
                  </div>
                ))}
            </section>
          )}

          {moved.stranded.length > 0 && (
            <section className={planStyles.blockers}>
              {/* The same words the preview used, in the past tense: what it
                  said would be left behind is what was. */}
              <h3 className={planStyles.blockTitle}>Left behind</h3>
              {moved.stranded.map((one) => (
                <div key={one.fileName} className={planStyles.blocker}>
                  <span className={planStyles.blockText}>
                    <span className={planStyles.blockName}>{one.name}</span>
                    <span
                      className={clsx(planStyles.blockWhere, styles.wraps)}
                    >
                      {sentence(one.reason)}{" "}
                      {one.off
                        ? "Turned off; the jar is still in your mods folder."
                        : "It could not be turned off — move it out of the mods folder yourself."}
                    </span>
                  </span>
                </div>
              ))}
            </section>
          )}

          {moved.unchecked.length > 0 && (
            <section className={planStyles.blockers}>
              <h3 className={planStyles.blockTitle}>Could not be checked</h3>
              {moved.unchecked.map((one) => (
                <div key={one.fileName} className={planStyles.blocker}>
                  <span className={planStyles.blockText}>
                    <span className={planStyles.blockName}>{one.fileName}</span>
                    <span
                      className={clsx(planStyles.blockWhere, styles.wraps)}
                    >
                      {sentence(one.reason)} Left exactly as it was.
                    </span>
                  </span>
                </div>
              ))}
            </section>
          )}
        </div>
      )}

      {resolution && !moved && (
        <>
          {resolution.targets.length > 0 && (
            <p className={styles.reads}>
              Read from what the mods you already have actually publish, best
              first. Nothing moves until you apply one, and a move applied to
              this instance cannot be undone.
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

          {/* The action at the end of the reading, below everything that was
              read. A mod with no build for the new pair is turned off rather
              than deleted, and a jar nothing can be looked up for is left
              alone — the report says what happened to each. */}
          {target && (
            <section className={styles.apply}>
              <p className={styles.note}>
                Applying {loaderLabel(target.loader)} {target.gameVersion}{" "}
                re-points every mod this instance tracks at a build for that
                pair. A mod with no build for it is turned off, never deleted
                &mdash; unless you have told that mod to ignore its Minecraft
                version, and then it is carried over on the newest build its
                author has published. A jar you added by hand is left alone.
                The report says what happened to each.
              </p>
              <div>
                <label className="label" htmlFor="replan-land-on">
                  Apply to
                </label>
                <select
                  id="replan-land-on"
                  className="select"
                  value={landOn}
                  onChange={(e) => setLandOn(e.target.value as LandOn)}
                >
                  <option value="">A copy if there is anything to lose</option>
                  <option value="here">This instance</option>
                  <option value="copy">A copy</option>
                </select>
                <p className="hint">
                  Minecraft upgrades a world the first time a newer version
                  opens it and never puts it back, so a copy is the default for
                  an instance with worlds in it.
                </p>
                {/* Said in full on the one choice that cannot be undone, and
                    only on it: "nothing puts that back" in a hint under a
                    dropdown is a sentence somebody reads after. What it costs
                    is a world, so the words are the cost and the way to keep
                    one.

                    Worded for what this screen actually knows, which is not
                    whether there are any worlds — nothing here reads an
                    instance's `saves`, and a dialog that fetched a world list
                    to decide which sentence to print would be reading the disk
                    to choose wording. It headed itself "the worlds in
                    Survival" over an instance that had none. Nor is the target
                    necessarily newer than the instance: a re-target goes
                    either way, and each direction costs a world something
                    different. */}
                {landOn === "here" && (
                  <StatusNote
                    line={{
                      tone: "warn",
                      title: `Applying this to ${instance.name} itself cannot be undone`,
                      detail: `Any world in this instance is opened at ${loaderLabel(target.loader)} ${target.gameVersion} from now on, and it is the only copy there is. That is one-way either way: a world's data format is upgraded the first time a newer Minecraft opens it and no version puts it back, and a Minecraft older than the one a world was last saved by may not open it at all. Back up the worlds you care about from the Worlds tab first if you want to be able to play them at ${instance.mcVersion} again — or apply this to a copy, which leaves this instance exactly as it is.`,
                    }}
                  />
                )}
              </div>
            </section>
          )}
        </>
      )}
    </Modal>
  );
}

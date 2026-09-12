import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import {
  AlertTriangle,
  ArrowLeft,
  Blocks,
  Hammer,
  Loader2,
  Package,
  Plus,
  RefreshCw,
  Trash2,
} from "lucide-react";

import PlanSourcePanel from "../components/PlanSourcePanel";
import { api } from "../lib/api";
import { titleCase } from "../lib/format";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type {
  Fit,
  ModFit,
  Plan,
  PlanMod,
  PlanResolution,
  PlanTarget,
} from "../lib/types";

import styles from "./PlanView.module.css";

/** Loaders have fixed capitalisation ("NeoForge"), so never title-case them. */
function loaderLabel(id: string): string {
  return LOADERS.find((l) => l.id === id)?.label ?? titleCase(id);
}

/**
 * What each verdict is called on screen. A native fit says nothing — it is the
 * normal case, and a chip on every row would drown the ones that matter.
 */
const FIT_LABEL: Record<Fit, string> = {
  native: "",
  nearVersion: "other version",
  connector: "via Connector",
  connectorNear: "Connector, other version",
  missing: "no build",
};

const modKey = (mod: { provider: string; projectId: string }) =>
  `${mod.provider}:${mod.projectId}`;

export default function PlanView({ id }: { id: string }) {
  const navigate = useStore((s) => s.navigate);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const refreshInstances = useStore((s) => s.refreshInstances);

  const [plan, setPlan] = useState<Plan | null>(null);
  const [resolution, setResolution] = useState<PlanResolution | null>(null);
  const [resolving, setResolving] = useState(false);
  const [chosen, setChosen] = useState(0);
  const [adding, setAdding] = useState(false);
  const [building, setBuilding] = useState(false);
  const [name, setName] = useState("");

  // Guards against a slow resolve landing after a newer one.
  const resolveSeq = useRef(0);

  const resolve = useCallback(async () => {
    const seq = ++resolveSeq.current;
    setResolving(true);
    try {
      const found = await api.resolvePlan(id);
      if (seq !== resolveSeq.current) return;
      setResolution(found);
      setChosen(0);
    } catch (e) {
      if (seq === resolveSeq.current) reportError("Could not work out a version", e);
    } finally {
      if (seq === resolveSeq.current) setResolving(false);
    }
  }, [id, reportError]);

  useEffect(() => {
    let cancelled = false;
    api
      .getPlan(id)
      .then((loaded) => {
        if (cancelled) return;
        setPlan(loaded);
        setName(loaded.name);
        // Answering the plan's whole question on open is the point of the
        // view; the backend caches per project, so reopening is nearly free.
        if (loaded.mods.length > 0) void resolve();
      })
      .catch((e) => !cancelled && reportError("Could not open that plan", e));
    return () => {
      cancelled = true;
    };
  }, [id, resolve, reportError]);

  const commit = useCallback(
    async (next: Plan, reResolve: boolean) => {
      setPlan(next);
      try {
        setPlan(await api.savePlan(next));
        if (reResolve) {
          if (next.mods.length > 0) await resolve();
          else setResolution(null);
        }
      } catch (e) {
        reportError("Could not save the plan", e);
      }
    },
    [resolve, reportError],
  );

  const addMods = (incoming: PlanMod[]) => {
    if (!plan) return;
    const present = new Set(plan.mods.map(modKey));
    const fresh = incoming.filter((mod) => !present.has(modKey(mod)));
    if (fresh.length === 0) return;
    void commit({ ...plan, mods: [...plan.mods, ...fresh] }, true);
  };

  const removeMod = (mod: PlanMod) => {
    if (!plan) return;
    void commit(
      { ...plan, mods: plan.mods.filter((m) => modKey(m) !== modKey(mod)) },
      true,
    );
  };

  const present = useMemo(
    () => new Set((plan?.mods ?? []).map(modKey)),
    [plan],
  );

  const target: PlanTarget | undefined = resolution?.targets[chosen];

  /** This target's verdict for each mod, keyed so rows can look themselves up. */
  const verdicts = useMemo(() => {
    const map = new Map<string, ModFit>();
    for (const fit of target?.fits ?? []) map.set(modKey(fit), fit);
    return map;
  }, [target]);

  async function build() {
    if (!plan || !target) return;
    setBuilding(true);
    try {
      const report = await api.buildPlan(
        plan.id,
        target.loader,
        target.gameVersion,
        plan.name,
      );
      await refreshInstances();
      toast({
        kind: "success",
        title: `Built ${report.instanceName}`,
        body: `${loaderLabel(target.loader)} ${target.gameVersion} · ${report.installed.length} files installed.`,
      });
      if (report.skipped.length > 0) {
        toast({
          kind: "error",
          title: `${report.skipped.length} could not be installed`,
          body: report.skipped.join("; "),
        });
      }
      navigate({ view: "instance", id: report.instanceId });
    } catch (e) {
      await refreshInstances();
      reportError("Could not build the plan", e);
    } finally {
      setBuilding(false);
    }
  }

  if (!plan) {
    return (
      <div className={styles.loading}>
        <Loader2 size={20} className="spin" />
      </div>
    );
  }

  const blockers = (target?.fits ?? []).filter((f) => f.fit === "missing");

  return (
    <>
      <header className={styles.header}>
        <button
          className={styles.back}
          onClick={() => navigate({ view: "plans" })}
          aria-label="Back to plans"
        >
          <ArrowLeft size={17} />
        </button>

        <input
          className={styles.name}
          value={name}
          onChange={(e) => setName(e.target.value)}
          onBlur={() => name.trim() && name !== plan.name && commit({ ...plan, name }, false)}
          onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
          aria-label="Plan name"
        />

        <span className={styles.count}>
          {plan.mods.length} mod{plan.mods.length === 1 ? "" : "s"}
        </span>

        <div className={styles.headActions}>
          <button
            className="btn"
            onClick={() => void resolve()}
            disabled={resolving || plan.mods.length === 0}
            title="Check which loader and version can run this"
          >
            {resolving ? (
              <Loader2 size={14} className="spin" />
            ) : (
              <RefreshCw size={14} />
            )}
            Re-check
          </button>
          <button
            className={clsx("btn", !adding && "btn-primary")}
            onClick={() => setAdding((open) => !open)}
          >
            <Plus size={14} />
            Add mods
          </button>
        </div>
      </header>

      <div className={styles.body}>
        <div className={styles.list}>
          {plan.mods.length === 0 ? (
            <div className={styles.empty}>
              <Package size={34} className={styles.emptyIcon} />
              <p>Nothing in this plan yet.</p>
              <p className={styles.emptyHint}>
                Add mods by searching, or copy them wholesale out of an instance
                or a published modpack. Versions come later — Geode picks those
                once it knows the whole list.
              </p>
              <button className="btn btn-primary" onClick={() => setAdding(true)}>
                <Plus size={14} />
                Add mods
              </button>
            </div>
          ) : (
            plan.mods.map((mod) => {
              const verdict = verdicts.get(modKey(mod));
              const fit = verdict?.fit ?? "native";
              return (
                <div
                  key={modKey(mod)}
                  className={clsx(
                    styles.row,
                    fit === "missing" && styles.rowMissing,
                  )}
                >
                  <span className={styles.icon}>
                    {mod.iconUrl ? (
                      <img src={mod.iconUrl} alt="" loading="lazy" />
                    ) : (
                      <Package size={15} />
                    )}
                  </span>

                  <span className={styles.rowText}>
                    <span className={styles.rowName}>{mod.name}</span>
                    {verdict && fit === "missing" && (
                      <span className={styles.rowWhere}>
                        {verdict.elsewhere && verdict.elsewhere.length > 0
                          ? `Runs on ${verdict.elsewhere.join(", ")}`
                          : "No build anywhere"}
                      </span>
                    )}
                    {verdict?.viaVersion && fit !== "missing" && (
                      <span className={styles.rowWhere}>
                        Using its {verdict.viaVersion} build
                      </span>
                    )}
                  </span>

                  <span className={clsx("badge", `badge-${mod.provider}`)}>
                    {mod.provider === "modrinth" ? "MR" : "CF"}
                  </span>

                  {verdict && fit !== "native" && (
                    <span className={clsx(styles.fit, styles[fit])}>
                      {(fit === "connector" || fit === "connectorNear") && (
                        <Blocks size={10} />
                      )}
                      {fit === "missing" && <AlertTriangle size={10} />}
                      {FIT_LABEL[fit]}
                    </span>
                  )}

                  <button
                    className={styles.remove}
                    onClick={() => removeMod(mod)}
                    title={`Remove ${mod.name}`}
                    aria-label={`Remove ${mod.name}`}
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              );
            })
          )}
        </div>

        {adding ? (
          <PlanSourcePanel
            present={present}
            onAdd={addMods}
            onClose={() => setAdding(false)}
          />
        ) : (
          <aside className={styles.rail}>
            <h2 className={styles.railTitle}>Where this can run</h2>

            {plan.mods.length === 0 ? (
              <p className={styles.railNote}>
                Add some mods and Geode will work out which loader and Minecraft
                version can run all of them.
              </p>
            ) : resolving && !resolution ? (
              <div className={styles.railSkeletons}>
                {Array.from({ length: 3 }, (_, i) => (
                  <div key={i} className="skeleton" style={{ height: 74 }} />
                ))}
              </div>
            ) : !resolution || resolution.targets.length === 0 ? (
              <p className={styles.railNote}>
                No loader and version combination runs any of these mods. Check
                the list below for mods with no usable build.
              </p>
            ) : (
              <>
                <div className={styles.targets}>
                  {resolution.targets.map((option, index) => (
                    <button
                      key={`${option.loader}:${option.gameVersion}`}
                      className={clsx(
                        styles.target,
                        index === chosen && styles.targetChosen,
                        option.missing > 0 && styles.targetIncomplete,
                      )}
                      onClick={() => setChosen(index)}
                    >
                      <span className={styles.targetHead}>
                        <span className={styles.targetLoader}>
                          {loaderLabel(option.loader)}
                        </span>
                        <span className={styles.targetVersion}>
                          {option.gameVersion}
                        </span>
                        {index === 0 && (
                          <span className={styles.best}>best</span>
                        )}
                      </span>

                      <span
                        className={clsx(
                          styles.coverage,
                          option.missing === 0 && styles.coverageFull,
                        )}
                      >
                        {option.missing === 0
                          ? `All ${option.fits.length} mods run here`
                          : `${option.fits.length - option.missing} of ${option.fits.length} — ${option.missing} left out`}
                      </span>

                      {(option.connector > 0 || option.near > 0) && (
                        <span className={styles.compromises}>
                          {option.connector > 0 && (
                            <span className={clsx(styles.fit, styles.connector)}>
                              <Blocks size={10} />
                              {option.connector} via Connector
                            </span>
                          )}
                          {option.near > 0 && (
                            <span className={clsx(styles.fit, styles.nearVersion)}>
                              {option.near} other version
                            </span>
                          )}
                        </span>
                      )}
                    </button>
                  ))}
                </div>

                {target && target.connector > 0 && (
                  <p className={styles.soft}>
                    <Blocks size={12} />
                    <span>
                      {target.connector} mod{target.connector === 1 ? "" : "s"}{" "}
                      here {target.connector === 1 ? "has" : "have"} no{" "}
                      {loaderLabel(target.loader)} build, so{" "}
                      {target.connector === 1 ? "it runs" : "they run"} on
                      Sinytra Connector. That works, but it is a compatibility
                      layer — a target without it is the safer pack.
                    </span>
                  </p>
                )}

                {target && target.near > 0 && (
                  <p className={styles.soft}>
                    <AlertTriangle size={12} />
                    <span>
                      {target.near} mod{target.near === 1 ? "" : "s"} would use a
                      build from a neighbouring version. Usually fine on Fabric,
                      less reliable on Forge and NeoForge.
                    </span>
                  </p>
                )}

                {blockers.length > 0 && (
                  <section className={styles.blockers}>
                    <h3 className={styles.blockTitle}>
                      Remove to use this target
                    </h3>
                    {blockers.map((blocker) => (
                      <div key={modKey(blocker)} className={styles.blocker}>
                        <span className={styles.blockText}>
                          <span className={styles.blockName}>{blocker.name}</span>
                          <span className={styles.blockWhere}>
                            {blocker.elsewhere && blocker.elsewhere.length > 0
                              ? `Runs on ${blocker.elsewhere.join(", ")}`
                              : "No build anywhere"}
                          </span>
                        </span>
                        <button
                          className="btn btn-sm"
                          onClick={() =>
                            removeMod({
                              provider: blocker.provider,
                              projectId: blocker.projectId,
                              name: blocker.name,
                            })
                          }
                        >
                          Remove
                        </button>
                      </div>
                    ))}
                  </section>
                )}

                {resolution.unresolved.length > 0 && (
                  <section className={styles.blockers}>
                    <h3 className={styles.blockTitle}>Could not be checked</h3>
                    {resolution.unresolved.map((mod) => (
                      <div key={modKey(mod)} className={styles.blocker}>
                        <span className={styles.blockText}>
                          <span className={styles.blockName}>{mod.name}</span>
                          <span className={styles.blockWhere}>{mod.reason}</span>
                        </span>
                      </div>
                    ))}
                  </section>
                )}

                {resolution.warnings.map((warning) => (
                  <p key={warning} className={styles.warning}>
                    <AlertTriangle size={12} />
                    <span>{warning}</span>
                  </p>
                ))}
              </>
            )}

            {target && (
              <button
                className={clsx("btn btn-primary", styles.build)}
                onClick={build}
                disabled={building}
                title={`Create a ${loaderLabel(target.loader)} ${target.gameVersion} instance and install the plan`}
              >
                {building ? (
                  <Loader2 size={14} className="spin" />
                ) : (
                  <Hammer size={14} />
                )}
                Build on {loaderLabel(target.loader)} {target.gameVersion}
              </button>
            )}
          </aside>
        )}
      </div>
    </>
  );
}

import { useCallback, useEffect, useState } from "react";
import { ClipboardList, Loader2, Package, Plus, Trash2 } from "lucide-react";

import { api } from "../lib/api";
import { relativeTime } from "../lib/format";
import { useStore } from "../lib/store";
import type { Plan } from "../lib/types";

import styles from "./Plans.module.css";

export default function Plans() {
  const navigate = useStore((s) => s.navigate);
  const reportError = useStore((s) => s.reportError);

  const [plans, setPlans] = useState<Plan[] | null>(null);
  const [creating, setCreating] = useState(false);

  const load = useCallback(async () => {
    try {
      setPlans(await api.listPlans());
    } catch (e) {
      reportError("Could not load plans", e);
      setPlans([]);
    }
  }, [reportError]);

  useEffect(() => {
    void load();
  }, [load]);

  async function create() {
    setCreating(true);
    try {
      const plan = await api.createPlan(`Plan ${(plans?.length ?? 0) + 1}`);
      navigate({ view: "plan", id: plan.id });
    } catch (e) {
      reportError("Could not create a plan", e);
    } finally {
      setCreating(false);
    }
  }

  async function remove(plan: Plan) {
    try {
      await api.deletePlan(plan.id);
      await load();
    } catch (e) {
      reportError(`Could not delete ${plan.name}`, e);
    }
  }

  return (
    <>
      <header className={styles.header}>
        <h1 className={styles.heading}>Plans</h1>
        <button className="btn btn-primary" onClick={create} disabled={creating}>
          {creating ? <Loader2 size={14} className="spin" /> : <Plus size={14} />}
          New plan
        </button>
      </header>

      <div className={styles.scroll}>
        {plans === null ? (
          <div className={styles.grid}>
            {Array.from({ length: 3 }, (_, i) => (
              <div key={i} className="skeleton" style={{ height: 116 }} />
            ))}
          </div>
        ) : plans.length === 0 ? (
          <div className={styles.empty}>
            <ClipboardList size={40} className={styles.emptyIcon} />
            <h2>Plan a pack before you build it</h2>
            <p>
              A plan is a list of mods with no Minecraft version and no loader
              attached. Collect what you want to play with, and Geode works out
              afterwards which loader and version can actually run all of it —
              preferring Fabric, then NeoForge, then Forge.
            </p>
            <button className="btn btn-primary" onClick={create} disabled={creating}>
              <Plus size={14} />
              Start a plan
            </button>
          </div>
        ) : (
          <div className={styles.grid}>
            {plans.map((plan) => (
              <article key={plan.id} className={styles.card}>
                <button
                  className={styles.cardBody}
                  onClick={() => navigate({ view: "plan", id: plan.id })}
                >
                  <h3 className={styles.cardName}>{plan.name}</h3>
                  <p className={styles.cardMeta}>
                    <Package size={11} />
                    {plan.mods.length} mod{plan.mods.length === 1 ? "" : "s"}
                    <span className={styles.dot} />
                    edited {relativeTime(plan.updatedAt)}
                  </p>
                  {plan.notes.trim() && (
                    <p className={styles.cardNotes}>{plan.notes}</p>
                  )}
                </button>
                <button
                  className={styles.remove}
                  onClick={() => remove(plan)}
                  title={`Delete ${plan.name}`}
                  aria-label={`Delete ${plan.name}`}
                >
                  <Trash2 size={14} />
                </button>
              </article>
            ))}
          </div>
        )}
      </div>
    </>
  );
}

import { useEffect, useState } from "react";
import {
  AlertTriangle,
  Loader2,
  MinusCircle,
  PlusCircle,
  RefreshCw,
  ShieldCheck,
} from "lucide-react";

import Modal from "./Modal";
import { api, errorMessage } from "../lib/api";
import { useStore } from "../lib/store";
import type { Instance, Loader, PackUpdate, PackUpdatePlan } from "../lib/types";
import { LOADERS } from "../lib/types";

import styles from "./PackUpdateModal.module.css";

/** Long lists are read as a count and a sample, not scrolled through. */
const SHOWN = 12;

/** What the rest of the app calls a loader, rather than its id. */
function loaderLabel(id: Loader): string {
  return LOADERS.find((l) => l.id === id)?.label ?? id;
}

/**
 * Updating an instance to a newer version of its pack, as a preview the player
 * says yes to rather than a report afterwards.
 *
 * The same order as the export dialog, and the same reason for it: nothing
 * destructive happens until the person has read what it would do. What arrives
 * is the pack's business; what *goes* and what is *kept* is theirs, so those
 * two are what this leads with.
 *
 * A pack version that moved to another Minecraft version or another loader is
 * refused rather than offered — the backend refuses it too, so this is where
 * the reason is read and not where the decision is made.
 */
export default function PackUpdateModal({
  instance,
  update,
  onApplied,
  onClose,
}: {
  instance: Instance;
  update: PackUpdate;
  /**
   * An update ran. Not "it worked": a run that could not fetch part of the
   * pack leaves the instance on the version it had, so what is on offer is
   * asked again rather than assumed to be gone.
   */
  onApplied: () => void;
  onClose: () => void;
}) {
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const refreshInstances = useStore((s) => s.refreshInstances);

  const [plan, setPlan] = useState<PackUpdatePlan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [updating, setUpdating] = useState(false);

  useEffect(() => {
    let dropped = false;
    api
      .planPackUpdate(instance.id, update.versionId)
      .then((p) => !dropped && setPlan(p))
      .catch((e) => !dropped && setError(errorMessage(e)));
    return () => {
      dropped = true;
    };
  }, [instance.id, update.versionId]);

  async function apply() {
    setUpdating(true);
    try {
      const outcome = await api.applyPackUpdate(instance.id, update.versionId);
      await refreshInstances();
      onApplied();
      toast({
        kind: outcome.problems.length > 0 ? "info" : "success",
        title: `Updated to ${plan?.toVersion ?? update.versionNumber}`,
        body:
          outcome.problems.length > 0
            ? outcome.problems.join("\n")
            : undefined,
      });
      onClose();
    } catch (e) {
      reportError("The pack was not updated", e);
    } finally {
      setUpdating(false);
    }
  }

  return (
    <Modal
      title="Update this pack"
      subtitle={`${update.currentVersion || "the version installed"} → ${update.versionNumber}`}
      onClose={onClose}
      width={560}
      footer={
        <>
          <button className="btn" onClick={onClose} disabled={updating}>
            {plan?.refused ? "Close" : "Cancel"}
          </button>
          {!plan?.refused && (
            <button
              className="btn btn-primary"
              onClick={apply}
              disabled={updating || !plan}
            >
              {updating && <Loader2 size={14} className="spin" />}
              Update
            </button>
          )}
        </>
      }
    >
      {error && <p className={styles.error}>{error}</p>}

      {!plan && !error && (
        <div className={styles.loading}>
          <Loader2 size={16} className="spin" />
          <span>Reading what this version carries…</span>
        </div>
      )}

      {plan && (
        <div className={styles.body}>
          {plan.refused ? (
            <p className={styles.refused}>
              <AlertTriangle size={15} />
              <span>{plan.refused}</span>
            </p>
          ) : (
            <p className={styles.keeps}>
              <ShieldCheck size={15} />
              <span>
                Your worlds, your options and your own config files stay exactly
                as they are
                {plan.takenOver.length === 0
                  ? ", and nothing of yours is deleted."
                  : `, but this version of the pack takes over ${
                      plan.takenOver.length
                    } file${
                      plan.takenOver.length === 1 ? "" : "s"
                    } of yours — the ones under “Replaced or turned off” below.`}
              </span>
            </p>
          )}

          <dl className={styles.facts}>
            <div>
              <dt>Minecraft</dt>
              <dd>
                {plan.mcVersion}
                {plan.mcVersion !== plan.fromMcVersion && (
                  <span className={styles.was}> was {plan.fromMcVersion}</span>
                )}
              </dd>
            </div>
            <div>
              <dt>Loader</dt>
              <dd>
                {loaderLabel(plan.loader)} {plan.loaderVersion}
                {plan.loader !== plan.fromLoader && (
                  <span className={styles.was}>
                    {" "}
                    was {loaderLabel(plan.fromLoader)}
                  </span>
                )}
              </dd>
            </div>
          </dl>

          <Section
            icon={<PlusCircle size={14} />}
            label="Arrives"
            items={plan.arriving}
            empty="Nothing new — this version changes the builds it ships."
          />
          <Section
            icon={<MinusCircle size={14} />}
            label="Goes"
            items={plan.departing}
            empty={
              plan.nothingRecorded
                ? "Nothing. Geode has no record of which of this instance's files came from its pack, so this update adds and replaces but deletes nothing — none of your mods can go by mistake. The next update will know."
                : "Nothing — this version drops none of the pack's files."
            }
          />
          <Section
            icon={<RefreshCw size={14} />}
            label="Replaced or turned off"
            items={plan.takenOver}
            empty="Nothing of yours — this version ships none of the mods you added yourself."
          />
          <Section
            icon={<ShieldCheck size={14} />}
            label="Stays yours"
            items={plan.keeping}
            empty="Nothing here is outside the pack."
          />
          {plan.stayingOff.length > 0 && (
            <Section
              icon={<ShieldCheck size={14} />}
              label="Stays turned off"
              items={plan.stayingOff}
              empty=""
            />
          )}
        </div>
      )}
    </Modal>
  );
}

function Section({
  icon,
  label,
  items,
  empty,
}: {
  icon: React.ReactNode;
  label: string;
  items: string[];
  empty: string;
}) {
  return (
    <section className={styles.section}>
      <h3 className={styles.heading}>
        {icon}
        {label}
        <span className={styles.count}>{items.length}</span>
      </h3>
      {items.length === 0 ? (
        <p className={styles.muted}>{empty}</p>
      ) : (
        <p className={styles.items}>
          {items.slice(0, SHOWN).join(", ")}
          {items.length > SHOWN && ` and ${items.length - SHOWN} more`}
        </p>
      )}
    </section>
  );
}

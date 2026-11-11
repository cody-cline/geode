import { useCallback, useEffect, useState } from "react";
import clsx from "clsx";
import { ClipboardList, Loader2, Package, Plus, Trash2 } from "lucide-react";

import Modal from "../components/Modal";
import { api } from "../lib/api";
import { relativeTime } from "../lib/format";
import { actionHint } from "../lib/shortcuts";
import { useActionShortcut } from "../lib/useActionShortcut";
import { useStore } from "../lib/store";
import type { Plan } from "../lib/types";

import styles from "./Plans.module.css";

export default function Plans() {
  const navigate = useStore((s) => s.navigate);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  const [plans, setPlans] = useState<Plan[] | null>(null);
  const [creating, setCreating] = useState(false);
  /** The dialog state the other two screens have; Plans makes one outright. */
  /** The plan the delete dialog is asking about, if it is open. */
  const [asking, setAsking] = useState<Plan | null>(null);

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


  // Plans has no dialog to open: the button makes one and navigates into it,
  // so the key does exactly what the button does rather than a lighter version.
  useActionShortcut("newThing", (e) => {
    e.preventDefault();
    if (!creating) void create();
  });

  async function remove(plan: Plan) {
    // The question has been answered, so the dialog goes before the work
    // starts — that also means the button cannot be pressed twice.
    setAsking(null);
    try {
      await api.deletePlan(plan.id);
      await load();
      toast({ kind: "success", title: `Deleted ${plan.name}` });
    } catch (e) {
      reportError(`Could not delete ${plan.name}`, e);
    }
  }

  return (
    <>
      <header className={styles.header}>
        <div>
          <h1 className={styles.heading}>Plans</h1>
          {/* The one sentence that says what a plan is lived only in the empty
              state, so it disappeared the moment somebody made one and was
              never readable again. It is here now, which makes this the
              screen's one explanation: the empty state said the same thing a
              second time, in longer words, just below it. */}
          <p className={styles.lede}>
            A list of mods with no Minecraft version and no loader attached.
            Geode works out afterwards which loader and version can run all of
            them.
          </p>
        </div>
        {/* Quiet while the page below is offering the same thing, the way
            Library and Servers are: one primary per screen, and the one in
            the middle of the page is the one with the explanation attached.
            `!== 0` rather than `> 0` so the loading page, which offers
            nothing, keeps the only button it has as the primary. */}
        <button
          className={clsx("btn", plans?.length !== 0 && "btn-primary")}
          onClick={create}
          disabled={creating}
        >
          {creating ? <Loader2 size={14} className="spin" /> : <Plus size={14} />}
          New plan
          <kbd className={styles.newKey}>{actionHint("newThing")}</kbd>
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
          // What the subtitle above does not already say, and nothing more.
          // The explanation was here a second time, in longer words, and it
          // is in the header now. The button was taken away with it, which
          // left this page describing one — "New plan, at the top of this
          // screen, starts one" — a sentence that only existed because there
          // was nothing here to press. Library and Servers resolve the same
          // two-primaries problem the other way round, and that is the one
          // copied here: the button is on the page it belongs to and the
          // header's goes quiet.
          <div className={styles.empty}>
            <ClipboardList size={40} className={styles.emptyIcon} />
            <h2>Plan a pack before you build it</h2>
            <p>
              Add mods to it as you come across them, and build it into an
              instance or a server once the list is settled.
            </p>
            <button
              className="btn btn-primary"
              onClick={create}
              disabled={creating}
            >
              Create your first plan
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
                  onClick={() => setAsking(plan)}
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

      {/* A plan is an evening's collecting and there is nothing anywhere that
          could put it back, so the trash icon asks rather than acts — and it
          says how many mods are on the one it is asking about, because the
          cards are small and a press can land on the wrong name. */}
      {asking && (
        <Modal
          title={`Delete ${asking.name}?`}
          subtitle={
            asking.mods.length === 0
              ? "This removes the plan. It cannot be undone."
              : `This removes the plan and the ${asking.mods.length} mod${
                  asking.mods.length === 1 ? "" : "s"
                } collected on it. It cannot be undone.`
          }
          onClose={() => setAsking(null)}
          width={420}
          footer={
            <>
              <button className="btn" onClick={() => setAsking(null)}>
                Cancel
              </button>
              <button
                className="btn btn-danger"
                onClick={() => void remove(asking)}
              >
                Delete plan
              </button>
            </>
          }
        >
          <p className={styles.note}>
            A plan is only the list, so nothing was downloaded for it and no
            files are removed. Any instance or server you have already built
            from it has its own copy of the mods and is untouched.
          </p>
        </Modal>
      )}
    </>
  );
}

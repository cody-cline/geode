import { useEffect, useState } from "react";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { AlertTriangle, Loader2, PackageOpen } from "lucide-react";

import Modal from "./Modal";
import { api, errorMessage } from "../lib/api";
import { fileSize } from "../lib/format";
import {
  EXPORT_FORMATS,
  authorDefault,
  copiedByReason,
  exportSummary,
  groupHint,
  groupLabel,
  groupNote,
  safeFileStem,
  whyLabel,
} from "../lib/share";
import { useStore } from "../lib/store";
import { GROUPS } from "../lib/types";
import type { ExportPlan, Group, GroupTally, Instance, PackFormat } from "../lib/types";

import styles from "./ExportModal.module.css";

const FORMATS: PackFormat[] = ["geodepack", "mrpack", "curseforge", "prism"];

/**
 * What a pack of an instance carries unless the sharer says otherwise: their
 * mods, settings and packs, and not their worlds, screenshots or logs. The
 * same six the backend's `Selection::default` carries, because that is what
 * every export did before anybody was asked.
 */
const BY_DEFAULT: Group[] = ["mods", "config", "resourcePacks", "shaderPacks", "dataPacks", "other"];

/**
 * Exporting an instance, as a preview the sharer drives rather than a report
 * afterwards.
 *
 * The order matters and is the reason this is one dialog and not four buttons.
 * The format is picked first because it changes what everything below means:
 * which of the instance's groups can travel as links rather than copies, and
 * what happens to each mod. Then the groups, with what is in each one and what
 * this format would do with it. Then what the export would actually do with
 * every mod, from a dry run of the very pass the export makes — so a mod shown
 * here as a copy is a copy, and a sharer about to hand somebody else's jar to a
 * third party knows before they press the button rather than after.
 */
export default function ExportModal({
  instance,
  onClose,
  startWith = "geodepack",
}: {
  instance: Instance;
  onClose: () => void;
  /** Which format to open on, so a Share menu can name one. */
  startWith?: PackFormat;
}) {
  const accounts = useStore((s) => s.accounts);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  const [format, setFormat] = useState<PackFormat>(startWith);
  const [chosen, setChosen] = useState<Group[]>(BY_DEFAULT);
  const [tallies, setTallies] = useState<GroupTally[] | null>(null);
  // One entry per format *and* selection looked at: a group left out travels
  // in no channel, so what the pack would do with each file and how many of
  // them there are is the chosen groups' own answer. The plan costs a hash of
  // every content file and a request per platform, so going back to a pair
  // already seen must not ask again.
  const [plans, setPlans] = useState<Record<string, ExportPlan>>({});
  const [planning, setPlanning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [author, setAuthor] = useState(() => authorDefault(accounts));
  const [exporting, setExporting] = useState(false);

  useEffect(() => {
    let dropped = false;
    api
      .exportGroups(instance.id)
      .then((groups) => !dropped && setTallies(groups))
      .catch((e) => !dropped && setError(errorMessage(e)));
    return () => {
      dropped = true;
    };
  }, [instance.id]);

  // Sorted, so ticking a group and unticking it again is the answer already in
  // hand rather than a third question.
  const planKey = `${format}:${[...chosen].sort().join(",")}`;

  useEffect(() => {
    if (plans[planKey]) return;
    let dropped = false;
    setPlanning(true);
    setError(null);
    api
      .exportPlan(instance.id, format, chosen)
      .then((plan) => !dropped && setPlans((seen) => ({ ...seen, [planKey]: plan })))
      .catch((e) => !dropped && setError(errorMessage(e)))
      .finally(() => !dropped && setPlanning(false));
    return () => {
      dropped = true;
    };
    // `plans` is what this writes; re-running on it would ask twice. `format`
    // and `chosen` are what `planKey` is made of.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [instance.id, planKey]);

  const plan = plans[planKey];
  const tallyOf = (group: Group) => tallies?.find((t) => t.group === group);
  // What a format can do with a group is a fact about the format and not about
  // what is ticked, so any plan of this format answers it — which keeps the
  // rows on screen while a newly ticked selection is being asked about.
  const carries = plan ?? Object.entries(plans).find(([key]) => key.startsWith(`${format}:`))?.[1];
  const carryOf = (group: Group) => carries?.carries.find((c) => c.group === group);

  function toggle(group: Group) {
    setChosen((groups) =>
      groups.includes(group) ? groups.filter((g) => g !== group) : [...groups, group],
    );
  }

  // The format is never read back off the file name: the save dialog does not
  // report which filter was chosen, and on Linux it does not add the extension
  // either, so "ForPrism" typed under the Modrinth filter would come back with
  // no extension at all. Two of the four save under a name the format does not
  // own — a CurseForge pack and a Prism instance are both plain .zips — and the
  // four differ in what they can say, so guessing wrong silently changes what
  // the file contains.
  async function run() {
    const { ext, filter, name } = EXPORT_FORMATS[format];
    const picked = await saveDialog({
      defaultPath: `${safeFileStem(instance.name)}.${ext}`,
      filters: [{ name: filter, extensions: [ext] }],
    });
    if (!picked) return;
    const dest = picked.toLowerCase().endsWith(`.${ext}`) ? picked : `${picked}.${ext}`;

    setExporting(true);
    try {
      const report = await write(dest);
      toast({ kind: "success", title: `Exported ${name}`, body: exportSummary(format, report) });
      onClose();
    } catch (e) {
      reportError("Export failed", e);
    } finally {
      setExporting(false);
    }
  }

  function write(dest: string) {
    switch (format) {
      case "mrpack":
        return api.exportModpack(instance.id, dest, chosen);
      case "geodepack":
        return api.exportGeodepack(instance.id, dest, chosen);
      case "curseforge":
        // The box's own value, and never `null` for an empty one: `null` means
        // nobody answered, which the backend stands the signed-in account's
        // name in for — so an emptied box would credit the pack to the very
        // name the sharer just deleted, under a placeholder promising
        // "Anonymous". An empty string is an answer, and the writer turns it
        // into the "Anonymous" a reader shows for a pack whose author it
        // cannot find.
        return api.exportCurseforgePack(instance.id, dest, author.trim(), chosen);
      case "prism":
        return api.exportPrismInstance(instance.id, dest, chosen);
    }
  }

  return (
    <Modal
      title={`Export ${instance.name}`}
      subtitle="Pick a format, choose what travels, and see what each mod will do before anything is written."
      onClose={onClose}
      width={660}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn btn-primary" onClick={run} disabled={exporting || planning}>
            {exporting ? <Loader2 size={14} className="spin" /> : <PackageOpen size={14} />}
            Export
          </button>
        </>
      }
    >
      <section>
        <h3 className={styles.heading}>Format</h3>
        <div className={styles.formats}>
          {FORMATS.map((candidate) => (
            <button
              key={candidate}
              type="button"
              className={styles.format}
              aria-pressed={candidate === format}
              onClick={() => setFormat(candidate)}
            >
              <span className={styles.formatName}>{EXPORT_FORMATS[candidate].filter}</span>
              <span className={styles.formatBlurb}>{EXPORT_FORMATS[candidate].blurb}</span>
            </button>
          ))}
        </div>
      </section>

      <section>
        <h3 className={styles.heading}>What travels</h3>
        <div className={styles.groups}>
          {GROUPS.map((group) => {
            const tally = tallyOf(group);
            const carry = carryOf(group);
            const empty = tally?.files === 0;
            return (
              <label key={group} className={styles.group} data-empty={empty || undefined}>
                <input
                  type="checkbox"
                  checked={chosen.includes(group)}
                  disabled={empty}
                  onChange={() => toggle(group)}
                />
                <span className={styles.groupBody}>
                  <span className={styles.groupTop}>
                    <strong>{groupLabel(group)}</strong>
                    <span className={styles.groupSize}>
                      {tally == null
                        ? "…"
                        : tally.files === 0
                          ? "nothing here"
                          : `${tally.files} ${tally.files === 1 ? "file" : "files"} · ${fileSize(tally.bytes)}`}
                    </span>
                  </span>
                  {groupHint(group) && <span className={styles.groupWhy}>{groupHint(group)}</span>}
                  {carry && (
                    <span className={styles.groupWhy}>
                      {groupNote(format, group, carry.canReference, carries?.sparesBytes ?? true)}
                    </span>
                  )}
                </span>
              </label>
            );
          })}
        </div>
      </section>

      {format === "curseforge" && (
        <section>
          <h3 className={styles.heading}>Author</h3>
          <input
            className="input"
            value={author}
            placeholder="Anonymous"
            onChange={(e) => setAuthor(e.target.value)}
          />
          <p className="hint">
            A CurseForge pack has to credit somebody and an instance has nowhere to keep a name, so
            the export asks. This travels inside the file.
          </p>
        </section>
      )}

      <section>
        <h3 className={styles.heading}>Your mods and packs</h3>
        {planning && (
          <p className={styles.pending}>
            <Loader2 size={14} className="spin" />
            Asking the platforms what they have…
          </p>
        )}
        {error && (
          <p className={styles.error}>
            <AlertTriangle size={14} />
            {error}
          </p>
        )}
        {plan && (
          <>
            <p className={styles.summary}>{exportSummary(format, plan.report)}</p>
            {copiedByReason(plan.content).map(({ why, files }) => (
              <div key={why} className={styles.reason}>
                <span className={styles.reasonWhy}>
                  {whyLabel(why, format)} — {files.length}
                </span>
                <span className={styles.reasonFiles}>
                  {files.map((file) => file.fileName).join(", ")}
                </span>
              </div>
            ))}
          </>
        )}
      </section>
    </Modal>
  );
}

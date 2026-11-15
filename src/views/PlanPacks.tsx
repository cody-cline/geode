import { useCallback, useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import {
  AlertTriangle,
  ArrowLeft,
  Check,
  ClipboardList,
  Download,
  Info,
  Loader2,
  Package,
  Plus,
  Search,
} from "lucide-react";

import InstanceIcon from "../components/InstanceIcon";
import PlanSourceTabs from "../components/PlanSourceTabs";
import type { PlanSource } from "../components/PlanSourceTabs";
import ProjectPanel from "../components/ProjectPanel";
import { api, errorMessage } from "../lib/api";
import { compactNumber, providerLabel } from "../lib/format";
import { modKey, planModFromProject } from "../lib/plan";
import { useBackHandler } from "../lib/useBackHandler";
import { renderProviderBody } from "../lib/richtext";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type { Instance, PlanMod, Project } from "../lib/types";

import browse from "./Browse.module.css";
import styles from "./PlanPacks.module.css";

/** A pack you already have, or one published online. Both open the same way. */
type OpenPack =
  | { kind: "instance"; instance: Instance }
  | { kind: "project"; project: Project };

type Tab = "installed" | "online";

/**
 * Taking a whole mod list into a plan.
 *
 * The mods come across without the version the pack was built for — that is
 * the point of copying into a plan rather than installing the pack, which
 * would pin every decision the planner exists to make.
 */
export default function PlanPacks({
  name,
  inPlan,
  onAdd,
  onDone,
  source,
  onSource,
}: {
  name: string;
  inPlan: Set<string>;
  onAdd: (mods: PlanMod[]) => void;
  onDone?: () => void;
  source: PlanSource;
  onSource: (next: PlanSource) => void;
}) {
  const instances = useStore((s) => s.instances);
  const instancesError = useStore((s) => s.instancesError);
  const refreshInstances = useStore((s) => s.refreshInstances);
  const reportError = useStore((s) => s.reportError);

  const [tab, setTab] = useState<Tab>("installed");
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [packs, setPacks] = useState<Project[] | null>(null);
  /**
   * Why the search did not answer, when it did not.
   *
   * Without it `setPacks([])` lands on "No modpacks found for “Create”", which
   * names the user's own query back at them: a provider being down reads as
   * nothing of that name existing on Modrinth. Browse's search keeps the same
   * state for the same reason.
   */
  const [searchFailed, setSearchFailed] = useState<string | null>(null);
  const [open, setOpen] = useState<OpenPack | null>(null);
  const [mods, setMods] = useState<PlanMod[] | null>(null);
  /**
   * Why the open pack's contents could not be read, when they could not.
   *
   * `mods` at `[]` heads the list "0 mods" and then says nothing in the pack
   * could be identified as a project — which is a verdict on a manifest, and
   * is what this screen says when a pack really does list only loose files.
   * Nothing read a manifest here.
   */
  const [modsFailed, setModsFailed] = useState<string | null>(null);
  /**
   * Counted up by "Try again" below. The read is keyed on which pack is open,
   * and asking for the same pack a second time is not a different pack, so
   * there is nothing in `open` for a retry to change.
   */
  const [attempt, setAttempt] = useState(0);
  const [description, setDescription] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);

  // A pack lists its contents by identity alone, so opening one of them means
  // fetching the project behind it before the panel has anything to show.
  const [selected, setSelected] = useState<Project | null>(null);
  const [opening, setOpening] = useState<string | null>(null);

  // An opened pack is a place of its own: back returns to the list of packs.
  // The mod panel registers later and so closes first, which gives the whole
  // chain — panel, pack, adding, screen — one step at a time.
  useBackHandler(Boolean(open), () => setOpen(null));

  const openMod = useCallback(
    async (mod: PlanMod) => {
      setOpening(modKey(mod));
      try {
        setSelected(await api.getProject(mod.provider, mod.projectId));
      } catch (e) {
        reportError(`Could not open ${mod.name}`, e);
      } finally {
        setOpening(null);
      }
    },
    [reportError],
  );

  useEffect(() => {
    const timer = setTimeout(() => setDebounced(query), 280);
    return () => clearTimeout(timer);
  }, [query]);

  const searchPacks = useCallback(async () => {
    setPacks(null);
    try {
      const results = await api.searchProjects({
        query: debounced,
        kind: "modpack",
        gameVersion: null,
        loader: null,
        categories: [],
        sort: debounced ? "relevance" : "downloads",
        offset: 0,
        limit: 30,
      });
      setPacks(results.hits);
      setSearchFailed(null);
    } catch (e) {
      reportError("Could not search modpacks", e);
      setPacks([]);
      setSearchFailed(errorMessage(e));
    }
  }, [debounced, reportError]);

  useEffect(() => {
    if (tab !== "online" || open) return;
    void searchPacks();
  }, [tab, open, searchPacks]);

  // Opening a pack loads the two things the list needs: what is in it, and
  // enough of its identity to be recognisable at the top.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setMods(null);
    setModsFailed(null);
    setDescription(null);
    setExpanded(false);

    if (open.kind === "instance") {
      const instance = open.instance;
      api
        .listContent(instance.id, "mod")
        .then((items) => {
          if (cancelled) return;
          setMods(
            items
              .filter((item) => item.projectId && item.provider)
              .map((item) => ({
                provider: item.provider!,
                projectId: item.projectId!,
                name: item.name,
                slug: item.slug ?? null,
                iconUrl: item.iconUrl ?? null,
              })),
          );
        })
        .catch((e) => {
          if (cancelled) return;
          reportError("Could not read that instance", e);
          setMods([]);
          setModsFailed(errorMessage(e));
        });
      setDescription(instance.pack?.summary ?? instance.notes ?? "");
      return () => {
        cancelled = true;
      };
    }

    const project = open.project;
    api
      .packContents(project.provider, project.id)
      .then((found) => !cancelled && setMods(found))
      .catch((e) => {
        if (cancelled) return;
        reportError(`Could not read ${project.name}`, e);
        setMods([]);
        setModsFailed(errorMessage(e));
      });
    api
      .getProjectDescription(project.provider, project.id)
      .then((body) => !cancelled && setDescription(body))
      .catch(() => !cancelled && setDescription(""));

    return () => {
      cancelled = true;
    };
  }, [open, attempt, reportError]);

  const missing = useMemo(
    () => (mods ?? []).filter((mod) => !inPlan.has(modKey(mod))),
    [mods, inPlan],
  );

  const banner =
    open?.kind === "project"
      ? (open.project.bannerUrl ?? open.project.iconUrl ?? null)
      : (open?.instance.pack?.bannerUrl ?? open?.instance.pack?.iconUrl ?? null);

  const title =
    open?.kind === "project" ? open.project.name : (open?.instance.name ?? "");

  const subtitle =
    open?.kind === "project"
      ? open.project.author
        ? `by ${open.project.author}`
        : ""
      : open
        ? `Minecraft ${open.instance.mcVersion}${
            open.instance.loader !== "vanilla"
              ? ` · ${LOADERS.find((l) => l.id === open.instance.loader)?.label}`
              : ""
          }`
        : "";

  return (
    <>
      <header className={browse.header}>
        {open ? (
          <button className="btn btn-sm" onClick={() => setOpen(null)}>
            <ArrowLeft size={14} />
            All packs
          </button>
        ) : tab === "online" ? (
          <div className={browse.searchWrap}>
            <Search size={15} className={browse.searchIcon} />
            <input
              className={clsx("input", browse.search)}
              placeholder="Search modpacks"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              autoFocus
            />
          </div>
        ) : (
          <span className={styles.lead}>
            Copy the mod list out of something you already have.
          </span>
        )}

        <div className={browse.target}>
          <span className={browse.targetLabel}>Adding to</span>
          <span className={browse.lockedTarget}>
            <ClipboardList size={14} />
            {name}
          </span>
          <button className="btn btn-sm btn-primary" onClick={onDone}>
            Done
          </button>
        </div>
      </header>

      <div className={browse.tabs}>
        <PlanSourceTabs source={source} onSource={onSource} />

        {!open && (
          <div className={styles.where}>
            <button
              className={clsx(browse.tab, tab === "installed" && browse.tabActive)}
              onClick={() => setTab("installed")}
            >
              Installed
            </button>
            <button
              className={clsx(browse.tab, tab === "online" && browse.tabActive)}
              onClick={() => setTab("online")}
            >
              Online
            </button>
          </div>
        )}

        <div style={{ flex: 1 }} />

        {open && missing.length > 0 && (
          <button className="btn btn-sm btn-primary" onClick={() => onAdd(missing)}>
            <Plus size={13} />
            Add all {missing.length}
          </button>
        )}
      </div>

      <div className={styles.body}>
        {open ? (
          <div className={styles.detail}>
            {/* The pack's own identity, at the head of its own list. */}
            <div className={styles.banner}>
              {banner ? (
                <img src={banner} alt="" />
              ) : (
                <span className={styles.bannerFallback} />
              )}
              <span className={styles.bannerScrim} />
              <div className={styles.bannerText}>
                <h2 className={styles.bannerName}>{title}</h2>
                {subtitle && <p className={styles.bannerMeta}>{subtitle}</p>}
              </div>
            </div>

            {description === null ? (
              <div className="skeleton" style={{ height: 54 }} />
            ) : description.trim() === "" ? null : open.kind === "project" ? (
              <>
                <div
                  className={clsx(
                    "prose",
                    styles.description,
                    !expanded && styles.clamped,
                  )}
                  // Written by third-party authors, so it is rendered only
                  // after being normalised and stripped of anything executable.
                  dangerouslySetInnerHTML={{
                    __html: renderProviderBody(description),
                  }}
                />
                <button
                  className={styles.more}
                  onClick={() => setExpanded((was) => !was)}
                >
                  {expanded ? "Show less" : "Show more"}
                </button>
              </>
            ) : (
              <p className={styles.plainDescription}>{description}</p>
            )}

            <div className={styles.modsHead}>
              <h3 className={styles.modsTitle}>
                {/* No count when nothing was counted. "0 mods" is a number
                    somebody can act on, and a failed read did not produce
                    one. */}
                {mods === null
                  ? "Reading the pack…"
                  : modsFailed
                    ? "Could not read the pack"
                    : `${mods.length} mod${mods.length === 1 ? "" : "s"}`}
              </h3>
              {mods !== null && missing.length === 0 && mods.length > 0 && (
                <span className={styles.allIn}>
                  <Check size={13} />
                  All already on the plan
                </span>
              )}
            </div>

            {mods === null ? (
              <div className={styles.rows}>
                {Array.from({ length: 6 }, (_, i) => (
                  <div key={i} className="skeleton" style={{ height: 52 }} />
                ))}
              </div>
            ) : modsFailed ? (
              /* Before the line below, which is a verdict on what a pack
                 lists — the one it gets when a pack really does hold only
                 loose files. Nothing read a manifest here, so nothing is
                 entitled to that conclusion. Back is the only other way out
                 of this screen and it leads away from the pack, so the retry
                 belongs here. */
              <div className={styles.note}>
                <p>
                  The mods in this pack could not be read, so this is not a
                  pack with nothing to copy. Your plan is untouched.
                </p>
                <p className="hint">{modsFailed}</p>
                <button
                  className="btn"
                  style={{ marginTop: 12 }}
                  onClick={() => setAttempt((n) => n + 1)}
                >
                  Try again
                </button>
              </div>
            ) : mods.length === 0 ? (
              <p className={styles.note}>
                Nothing here could be identified as a project, so there is
                nothing to copy.
              </p>
            ) : (
              <div className={styles.rows}>
                {mods.map((mod) => {
                  const added = inPlan.has(modKey(mod));
                  const busy = opening === modKey(mod);
                  return (
                    <div key={modKey(mod)} className={styles.modRow}>
                      {/* Opening is a press of its own rather than a press on
                          the row, because the row also holds Add — and a
                          `<button>` wrapped round another one is invalid, while
                          `role="button"` on the row would make the whole of its
                          contents presentational and hide Add from a screen
                          reader. So the row stays a plain container that draws
                          the border, and its two actions are two buttons beside
                          each other: the same split the plan cards and the log
                          list already use. */}
                      <button
                        className={styles.modOpen}
                        onClick={() => void openMod(mod)}
                        title={`Open ${mod.name}`}
                      >
                        <span className={styles.modIcon}>
                          {mod.iconUrl ? (
                            <img src={mod.iconUrl} alt="" loading="lazy" />
                          ) : (
                            <Package size={15} />
                          )}
                        </span>
                        <span className={styles.modName}>{mod.name}</span>
                        <span className={clsx("badge", `badge-${mod.provider}`)}>
                          {providerLabel(mod.provider)}
                        </span>
                        {busy && <Loader2 size={13} className="spin" />}
                      </button>
                      {added ? (
                        <span className={styles.added}>
                          <Check size={13} />
                          Added
                        </span>
                      ) : (
                        <button
                          className="btn btn-sm"
                          onClick={() => onAdd([mod])}
                        >
                          <Plus size={13} />
                          Add
                        </button>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        ) : tab === "installed" ? (
          instancesError && instances.length === 0 ? (
            /* The store keeps why the list could not be read, and this tab is
               one of the screens that would otherwise report it as having
               nothing in it. */
            <div className={browse.empty}>
              <AlertTriangle size={30} className={browse.emptyIcon} />
              <p>
                Your instances could not be read, so this is not an empty
                list. Nothing has been removed.
              </p>
              <p className={browse.emptyHint}>{instancesError}</p>
              <button
                className="btn"
                style={{ marginTop: 14 }}
                onClick={() => void refreshInstances()}
              >
                Try again
              </button>
            </div>
          ) : instances.length === 0 ? (
            <div className={browse.empty}>
              <Info size={30} className={browse.emptyIcon} />
              <p>You have no instances to copy from yet.</p>
            </div>
          ) : (
            <div className={styles.rows}>
              {instances.map((instance) => (
                <button
                  key={instance.id}
                  className={styles.packRow}
                  onClick={() => setOpen({ kind: "instance", instance })}
                >
                  <InstanceIcon instance={instance} size={44} />
                  <span className={styles.packText}>
                    <span className={styles.packName}>{instance.name}</span>
                    <span className={styles.packMeta}>
                      Minecraft {instance.mcVersion}
                      {instance.loader !== "vanilla" &&
                        ` · ${LOADERS.find((l) => l.id === instance.loader)?.label}`}
                      {instance.modCount > 0 &&
                        ` · ${instance.modCount} mod${instance.modCount === 1 ? "" : "s"}`}
                    </span>
                  </span>
                  <span className={styles.openHint}>Open</span>
                </button>
              ))}
            </div>
          )
        ) : packs === null ? (
          <div className={styles.rows}>
            {Array.from({ length: 6 }, (_, i) => (
              <div key={i} className="skeleton" style={{ height: 76 }} />
            ))}
          </div>
        ) : searchFailed ? (
          /* Browse's words, because it is the same failure on the other search
             surface: not "No modpacks found", which names the query back at
             somebody as if the provider had answered. It carries the retry
             that Browse leaves out, though — there the rail re-runs the search
             on any filter change, and here the search box is the only live
             control, so the way back would be to retype the query. */
          <div className={browse.empty}>
            <AlertTriangle size={30} className={browse.emptyIcon} />
            <p>The search failed, so there is nothing here to read.</p>
            <p className={browse.emptyHint}>{searchFailed}</p>
            <button
              className="btn"
              style={{ marginTop: 14 }}
              onClick={() => void searchPacks()}
            >
              Try again
            </button>
          </div>
        ) : packs.length === 0 ? (
          <div className={browse.empty}>
            <Info size={30} className={browse.emptyIcon} />
            <p>No modpacks found{debounced && <> for “{debounced}”</>}.</p>
          </div>
        ) : (
          <div className={styles.rows}>
            {packs.map((pack) => (
              <button
                key={`${pack.provider}:${pack.id}`}
                className={styles.packRow}
                onClick={() => setOpen({ kind: "project", project: pack })}
              >
                <span className={styles.packIcon}>
                  {pack.iconUrl ? (
                    <img src={pack.iconUrl} alt="" loading="lazy" />
                  ) : (
                    <Package size={20} />
                  )}
                </span>
                <span className={styles.packText}>
                  <span className={styles.packName}>{pack.name}</span>
                  <span className={styles.packSummary}>{pack.summary}</span>
                  <span className={styles.packMeta}>
                    <Download size={11} />
                    {compactNumber(pack.downloads)}
                    {pack.author && ` · by ${pack.author}`}
                  </span>
                </span>
                <span className={styles.openHint}>Open</span>
              </button>
            ))}
          </div>
        )}
      </div>

      {selected && (
        <ProjectPanel
          project={selected}
          onClose={() => setSelected(null)}
          // Stays open after adding: the panel re-renders as "already on the
          // plan", which is the confirmation.
          onRequestInstall={(project) => onAdd([planModFromProject(project)])}
          planMode
          added={inPlan.has(`${selected.provider}:${selected.id}`)}
        />
      )}
    </>
  );
}

import { useCallback, useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import { Check, Package, Plus, Search, X } from "lucide-react";

import { api } from "../lib/api";
import { modKey, planModFromProject } from "../lib/plan";
import { useStore } from "../lib/store";
import type { PlanMod, Project } from "../lib/types";

import styles from "./PlanSourcePanel.module.css";

type Mode = "instance" | "pack";

const MODES: { id: Mode; label: string }[] = [
  { id: "instance", label: "An instance" },
  { id: "pack", label: "A modpack" },
];

/**
 * Copying a whole existing mod list into a plan.
 *
 * Searching for individual mods is the browse view's job — this is for taking
 * a set wholesale. Everything it hands back is versionless: a pack built for
 * 1.20.1 Forge contributes its mods, not its target, so the planner stays free
 * to re-aim the lot.
 */
export default function PlanSourcePanel({
  present,
  onAdd,
  onClose,
}: {
  /** Keys already in the plan, so rows can show as added. */
  present: Set<string>;
  onAdd: (mods: PlanMod[]) => void;
  onClose: () => void;
}) {
  const instances = useStore((s) => s.instances);
  const reportError = useStore((s) => s.reportError);

  const [mode, setMode] = useState<Mode>("instance");
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [loading, setLoading] = useState(false);
  const [found, setFound] = useState<PlanMod[]>([]);

  // Pack mode has two steps: find a pack, then read what is inside it.
  const [packs, setPacks] = useState<Project[]>([]);
  const [openPack, setOpenPack] = useState<Project | null>(null);

  const [instanceId, setInstanceId] = useState("");

  useEffect(() => {
    const timer = setTimeout(() => setDebounced(query), 280);
    return () => clearTimeout(timer);
  }, [query]);

  const loadInstance = useCallback(
    async (id: string) => {
      setInstanceId(id);
      if (!id) {
        setFound([]);
        return;
      }
      setLoading(true);
      try {
        const items = await api.listContent(id, "mod");
        // Dropping the file, the build and the loader it was installed for is
        // the point: what survives is which mods.
        setFound(
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
      } catch (e) {
        reportError("Could not read that instance", e);
        setFound([]);
      } finally {
        setLoading(false);
      }
    },
    [reportError],
  );

  const searchPacks = useCallback(async () => {
    setLoading(true);
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
    } catch (e) {
      reportError("Could not search modpacks", e);
    } finally {
      setLoading(false);
    }
  }, [debounced, reportError]);

  useEffect(() => {
    if (mode !== "pack" || openPack) return;
    void searchPacks();
  }, [mode, openPack, searchPacks]);

  // The first instance loads on open, since that is the default source.
  useEffect(() => {
    if (mode === "instance" && !instanceId && instances.length > 0) {
      void loadInstance(instances[0].id);
    }
  }, [mode, instanceId, instances, loadInstance]);

  function switchMode(next: Mode) {
    setMode(next);
    setQuery("");
    setOpenPack(null);
    setFound([]);
    setPacks([]);
    if (next === "instance") {
      void loadInstance(instanceId || instances[0]?.id || "");
    }
  }

  async function readPack(project: Project) {
    setOpenPack(project);
    setLoading(true);
    try {
      setFound(await api.packContents(project.provider, project.id));
    } catch (e) {
      reportError(`Could not read ${project.name}`, e);
      setFound([]);
      setOpenPack(null);
    } finally {
      setLoading(false);
    }
  }

  const missing = useMemo(
    () => found.filter((mod) => !present.has(modKey(mod))),
    [found, present],
  );

  const showingPackList = mode === "pack" && !openPack;

  return (
    <aside className={styles.panel}>
      <header className={styles.head}>
        <div className={styles.modes}>
          {MODES.map((m) => (
            <button
              key={m.id}
              className={clsx(styles.mode, mode === m.id && styles.modeActive)}
              onClick={() => switchMode(m.id)}
            >
              {m.label}
            </button>
          ))}
        </div>
        <button
          className="btn btn-ghost btn-icon btn-sm"
          onClick={onClose}
          aria-label="Close"
        >
          <X size={16} />
        </button>
      </header>

      <div className={styles.controls}>
        {mode === "instance" ? (
          <select
            className="select"
            value={instanceId}
            onChange={(e) => void loadInstance(e.target.value)}
            aria-label="Copy mods from"
          >
            {instances.length === 0 && <option value="">No instances yet</option>}
            {instances.map((instance) => (
              <option key={instance.id} value={instance.id}>
                {instance.name} — {instance.mcVersion}
              </option>
            ))}
          </select>
        ) : openPack ? (
          <button className={styles.crumb} onClick={() => setOpenPack(null)}>
            <X size={12} />
            {openPack.name}
          </button>
        ) : (
          <div className={styles.searchWrap}>
            <Search size={14} className={styles.searchIcon} />
            <input
              className={clsx("input", styles.search)}
              placeholder="Search modpacks"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              autoFocus
            />
          </div>
        )}

        {!showingPackList && missing.length > 0 && (
          <button
            className="btn btn-sm btn-primary"
            onClick={() => onAdd(missing)}
            title={`Add all ${missing.length} to the plan`}
          >
            <Plus size={13} />
            Add {missing.length}
          </button>
        )}
      </div>

      <div className={styles.list}>
        {loading ? (
          Array.from({ length: 6 }, (_, i) => (
            <div key={i} className="skeleton" style={{ height: 44 }} />
          ))
        ) : showingPackList ? (
          packs.length === 0 ? (
            <p className={styles.note}>No modpacks found.</p>
          ) : (
            packs.map((pack) => (
              <button
                key={modKey(planModFromProject(pack))}
                className={styles.row}
                onClick={() => void readPack(pack)}
              >
                <span className={styles.icon}>
                  {pack.iconUrl ? (
                    <img src={pack.iconUrl} alt="" loading="lazy" />
                  ) : (
                    <Package size={14} />
                  )}
                </span>
                <span className={styles.rowText}>
                  <span className={styles.rowName}>{pack.name}</span>
                  <span className={styles.rowMeta}>{pack.author}</span>
                </span>
                <span className={styles.open}>Open</span>
              </button>
            ))
          )
        ) : found.length === 0 ? (
          <p className={styles.note}>
            {mode === "instance"
              ? instances.length === 0
                ? "You have no instances to copy from yet."
                : "Nothing here came from Modrinth or CurseForge, so there is no project to copy."
              : "This pack lists no mods we can identify."}
          </p>
        ) : (
          found.map((mod) => {
            const added = present.has(modKey(mod));
            return (
              <div key={modKey(mod)} className={styles.row}>
                <span className={styles.icon}>
                  {mod.iconUrl ? (
                    <img src={mod.iconUrl} alt="" loading="lazy" />
                  ) : (
                    <Package size={14} />
                  )}
                </span>
                <span className={styles.rowText}>
                  <span className={styles.rowName}>{mod.name}</span>
                </span>
                {added ? (
                  <span className={styles.added}>
                    <Check size={13} />
                    Added
                  </span>
                ) : (
                  <button
                    className="btn btn-sm"
                    onClick={() => onAdd([mod])}
                    aria-label={`Add ${mod.name}`}
                  >
                    <Plus size={13} />
                  </button>
                )}
              </div>
            );
          })
        )}
      </div>

      <p className={styles.footNote}>
        {mode === "instance"
          ? "Copied without their versions — the plan re-targets them itself."
          : openPack
            ? `${openPack.name}'s mods, stripped of the version it was built for.`
            : "Open a pack to see what is in it."}
      </p>
    </aside>
  );
}

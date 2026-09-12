import { useCallback, useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import { Check, Package, Plus, Search, X } from "lucide-react";

import { api } from "../lib/api";
import { useStore } from "../lib/store";
import type { PlanMod, Project } from "../lib/types";

import styles from "./PlanSourcePanel.module.css";

type Mode = "search" | "instance" | "pack";

const MODES: { id: Mode; label: string }[] = [
  { id: "search", label: "Search" },
  { id: "instance", label: "An instance" },
  { id: "pack", label: "A modpack" },
];

function toPlanMod(project: Project): PlanMod {
  return {
    provider: project.provider,
    projectId: project.id,
    name: project.name,
    slug: project.slug,
    iconUrl: project.iconUrl ?? null,
  };
}

/**
 * The side-by-side source for adding mods to a plan.
 *
 * Everything it hands back is versionless — that is the whole point of copying
 * *into* a plan. A pack built for 1.20.1 Forge contributes its mods, not its
 * version, so the planner stays free to re-target the lot.
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

  const [mode, setMode] = useState<Mode>("search");
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [loading, setLoading] = useState(false);
  const [found, setFound] = useState<PlanMod[]>([]);

  // Pack mode has two steps: pick a pack, then read what is in it.
  const [packs, setPacks] = useState<Project[]>([]);
  const [openPack, setOpenPack] = useState<Project | null>(null);

  // Instance mode: which instance's mods are being shown.
  const [instanceId, setInstanceId] = useState("");

  useEffect(() => {
    const timer = setTimeout(() => setDebounced(query), 280);
    return () => clearTimeout(timer);
  }, [query]);

  const key = (mod: PlanMod) => `${mod.provider}:${mod.projectId}`;

  // Mods are searched with no version or loader filter at all. Narrowing here
  // would quietly bake a target into a plan that is meant not to have one.
  const runSearch = useCallback(async () => {
    setLoading(true);
    try {
      const results = await api.searchProjects({
        query: debounced,
        kind: mode === "pack" ? "modpack" : "mod",
        gameVersion: null,
        loader: null,
        categories: [],
        sort: debounced ? "relevance" : "downloads",
        offset: 0,
        limit: 30,
      });
      if (mode === "pack") {
        setPacks(results.hits);
        setFound([]);
      } else {
        setFound(results.hits.map(toPlanMod));
        setPacks([]);
      }
    } catch (e) {
      reportError("Search failed", e);
    } finally {
      setLoading(false);
    }
  }, [debounced, mode, reportError]);

  useEffect(() => {
    if (mode === "instance") return;
    setOpenPack(null);
    void runSearch();
  }, [runSearch, mode]);

  // An instance's mods become plan mods by dropping everything version-shaped:
  // the file, the build, the loader it happened to be installed for.
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

  // Switching source is an explicit act, so it loads explicitly. Driving this
  // from an effect keyed on the selected instance meant the first pick fetched
  // twice, and coming back to a source you had already chosen showed nothing.
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
    () => found.filter((mod) => !present.has(key(mod))),
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
              placeholder={mode === "pack" ? "Search modpacks" : "Search mods"}
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
                key={`${pack.provider}:${pack.id}`}
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
              : "Nothing found."}
          </p>
        ) : (
          found.map((mod) => {
            const added = present.has(key(mod));
            return (
              <div key={key(mod)} className={styles.row}>
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

      {mode === "instance" && (
        <p className={styles.footNote}>
          Copied without their versions — the plan re-targets them itself.
        </p>
      )}
      {mode === "pack" && openPack && (
        <p className={styles.footNote}>
          {openPack.name}&apos;s mods, stripped of the version it was built for.
        </p>
      )}
    </aside>
  );
}

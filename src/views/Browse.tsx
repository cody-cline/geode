import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Download, Info, Loader2, Search } from "lucide-react";

import InstallTargetModal from "../components/InstallTargetModal";
import ModpackInstallModal from "../components/ModpackInstallModal";
import InstanceIcon from "../components/InstanceIcon";
import ProjectCard from "../components/ProjectCard";
import ProjectPanel from "../components/ProjectPanel";
import { api } from "../lib/api";
import { titleCase } from "../lib/format";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type { Instance, Project, ProjectKind, Provider, SortBy } from "../lib/types";

import styles from "./Browse.module.css";

const KINDS: { id: ProjectKind; label: string }[] = [
  { id: "mod", label: "Mods" },
  { id: "modpack", label: "Modpacks" },
  { id: "resourcepack", label: "Resource Packs" },
  { id: "shader", label: "Shaders" },
  { id: "datapack", label: "Data Packs" },
];

const SORTS: { id: SortBy; label: string }[] = [
  { id: "relevance", label: "Relevance" },
  { id: "downloads", label: "Downloads" },
  { id: "follows", label: "Followers" },
  { id: "updated", label: "Recently updated" },
  { id: "newest", label: "Newest" },
];

const PAGE_SIZE = 30;

/** Loaders have fixed capitalisation ("NeoForge"), so never title-case them. */
function loaderLabel(id: string): string {
  return LOADERS.find((l) => l.id === id)?.label ?? titleCase(id);
}

export default function Browse({
  lockedInstance,
  onDone,
}: {
  /**
   * Set when browsing from inside an instance. The destination is then implied
   * by context, so results are scoped to it and installs skip the picker —
   * asking again would only re-confirm a choice already made by being here.
   */
  lockedInstance?: Instance;
  onDone?: () => void;
} = {}) {
  const instances = useStore((s) => s.instances);
  const config = useStore((s) => s.config);
  const navigate = useStore((s) => s.navigate);
  const reportError = useStore((s) => s.reportError);
  const toast = useStore((s) => s.toast);
  const refreshInstances = useStore((s) => s.refreshInstances);

  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [kind, setKind] = useState<ProjectKind>("mod");
  const [sort, setSort] = useState<SortBy>("relevance");
  const [sources, setSources] = useState<Provider[]>(["modrinth", "curseforge"]);
  const [categories, setCategories] = useState<string[]>([]);
  const [available, setAvailable] = useState<string[]>([]);
  const [targetId, setTargetId] = useState<string>("");

  const [hits, setHits] = useState<Project[]>([]);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [exhausted, setExhausted] = useState(false);
  const [selected, setSelected] = useState<Project | null>(null);
  const [installed, setInstalled] = useState<Set<string>>(new Set());
  const [page, setPage] = useState(0);

  // An install is never implicit: the button opens one of these, and the pack
  // route creates an instance rather than writing into one.
  const [askTarget, setAskTarget] = useState<{
    project: Project;
    versionId?: string;
  } | null>(null);
  const [packInstall, setPackInstall] = useState<{
    project: Project;
    versionId?: string;
  } | null>(null);
  const [installing, setInstalling] = useState<string | null>(null);

  // Guards against an older, slower request overwriting a newer one.
  const requestSeq = useRef(0);

  const target = lockedInstance ?? instances.find((i) => i.id === targetId);

  // `sources` records what the user wants; `usableSources` is what we can
  // actually query. Keeping them apart means CurseForge stays selected — and
  // comes straight back — once a key is added, without ever showing a ticked
  // box for a provider that is switched off.
  // Memoised because it feeds a useCallback dependency list — a fresh array
  // every render would restart the search on every render.
  const hasCurseforgeKey = config?.hasCurseforgeKey ?? false;
  const providers = useMemo(() => {
    const availableSources: Provider[] = hasCurseforgeKey
      ? ["modrinth", "curseforge"]
      : ["modrinth"];
    return sources.filter((s) => availableSources.includes(s));
  }, [sources, hasCurseforgeKey]);

  // Knowing what the target already has lets cards say "Added" instead of
  // offering an install that would be a no-op.
  const refreshInstalled = useCallback(async () => {
    if (!target) {
      setInstalled(new Set());
      return;
    }
    try {
      const lists = await Promise.all(
        (["mod", "resourcepack", "shaderpack", "datapack"] as const).map((k) =>
          api.listContent(target.id, k),
        ),
      );
      setInstalled(
        new Set(
          lists
            .flat()
            .filter((item) => item.projectId)
            .map((item) => `${item.provider}:${item.projectId}`),
        ),
      );
    } catch {
      setInstalled(new Set());
    }
  }, [target]);

  useEffect(() => {
    void refreshInstalled();
  }, [refreshInstalled]);

  const isInstalled = (project: Project) =>
    installed.has(`${project.provider}:${project.id}`) ||
    project.alsoOn.some((ref) => installed.has(`${ref.provider}:${ref.id}`));

  function requestInstall(project: Project, versionId?: string) {
    // A pack always creates its own instance, even from inside one.
    if (project.kind === "modpack") {
      setPackInstall({ project, versionId });
      return;
    }
    if (lockedInstance) {
      void installDirect(lockedInstance, project, versionId);
      return;
    }
    setAskTarget({ project, versionId });
  }

  async function installDirect(
    instance: Instance,
    project: Project,
    versionId?: string,
  ) {
    setInstalling(project.id);
    try {
      const report = await api.installContent(
        instance.id,
        project.provider,
        project.id,
        versionId,
      );
      await Promise.all([refreshInstalled(), refreshInstances()]);

      const extras = report.installed.length - 1;
      toast({
        kind: "success",
        title: `Added ${project.name} to ${instance.name}`,
        body:
          extras > 0
            ? `Plus ${extras} required ${extras === 1 ? "dependency" : "dependencies"}.`
            : undefined,
      });
      if (report.skipped.length > 0) {
        toast({
          kind: "error",
          title: "Some dependencies were skipped",
          body: report.skipped.join("; "),
        });
      }
    } catch (e) {
      reportError(`Could not install ${project.name}`, e);
    } finally {
      setInstalling(null);
    }
  }

  async function installInto(instance: Instance) {
    if (!askTarget) return;
    const { project, versionId } = askTarget;
    setAskTarget(null);
    await installDirect(instance, project, versionId);
  }

  useEffect(() => {
    if (!lockedInstance && !targetId && instances.length > 0) {
      setTargetId(instances[0].id);
    }
  }, [instances, targetId, lockedInstance]);

  useEffect(() => {
    const timer = setTimeout(() => setDebounced(query), 280);
    return () => clearTimeout(timer);
  }, [query]);

  useEffect(() => {
    api
      .getCategories(kind)
      .then(setAvailable)
      .catch(() => setAvailable([]));
    setCategories([]);
  }, [kind]);

  // Scoping results to the target instance is what makes the install button
  // trustworthy: everything listed is known to fit.
  const gameVersion = target?.mcVersion ?? null;
  const loader =
    target && target.loader !== "vanilla" ? target.loader : null;

  const runSearch = useCallback(
    async (page: number) => {
      const offset = page * PAGE_SIZE;
      const seq = ++requestSeq.current;
      if (page === 0) setLoading(true);
      else setLoadingMore(true);

      try {
        const results = await api.searchProjects({
          query: debounced,
          kind,
          gameVersion,
          loader,
          categories,
          sort,
          offset,
          limit: PAGE_SIZE,
          providers: providers.length > 0 ? providers : ["modrinth"],
        });
        if (seq !== requestSeq.current) return;

        setWarnings(results.warnings);
        setExhausted(results.hits.length < PAGE_SIZE / 2);
        setHits((prev) => {
          if (page === 0) return results.hits;
          // Merging pages can re-surface a project the merge already folded in.
          const seen = new Set(prev.map((p) => `${p.provider}:${p.id}`));
          return [
            ...prev,
            ...results.hits.filter((p) => !seen.has(`${p.provider}:${p.id}`)),
          ];
        });
      } catch (e) {
        if (seq === requestSeq.current) reportError("Search failed", e);
      } finally {
        if (seq === requestSeq.current) {
          setLoading(false);
          setLoadingMore(false);
        }
      }
    },
    [debounced, kind, gameVersion, loader, categories, sort, providers, reportError],
  );

  useEffect(() => {
    setPage(0);
    void runSearch(0);
  }, [runSearch]);

  const toggleSource = (provider: Provider) =>
    setSources((prev) =>
      prev.includes(provider)
        ? prev.filter((p) => p !== provider)
        : [...prev, provider],
    );

  const toggleCategory = (category: string) =>
    setCategories((prev) =>
      prev.includes(category)
        ? prev.filter((c) => c !== category)
        : [...prev, category],
    );

  const kindLabel = useMemo(
    () => KINDS.find((k) => k.id === kind)?.label.toLowerCase() ?? "results",
    [kind],
  );

  return (
    <>
      <header className={styles.header}>
        <div className={styles.searchWrap}>
          <Search size={15} className={styles.searchIcon} />
          <input
            className={clsx("input", styles.search)}
            placeholder={`Search ${kindLabel}`}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            autoFocus
          />
        </div>

        <div className={styles.target}>
          {lockedInstance ? (
            <>
              <span className={styles.targetLabel}>Adding to</span>
              <span className={styles.lockedTarget}>
                <InstanceIcon instance={lockedInstance} size={20} />
                {lockedInstance.name}
              </span>
              <button className="btn btn-sm" onClick={onDone}>
                Done
              </button>
            </>
          ) : instances.length === 0 ? (
            <>
              <span className={styles.targetLabel}>Compatible with</span>
              <button
                className="btn btn-sm"
                onClick={() => navigate({ view: "library" })}
              >
                Create an instance
              </button>
            </>
          ) : (
            <>
            <span className={styles.targetLabel}>Compatible with</span>
            <select
              className="select"
              style={{ width: 190 }}
              value={targetId}
              onChange={(e) => setTargetId(e.target.value)}
              aria-label="Filter by instance compatibility"
            >
              {instances.map((i) => (
                <option key={i.id} value={i.id}>
                  {i.name} · {i.mcVersion}
                </option>
              ))}
            </select>
            </>
          )}
        </div>
      </header>

      <div className={styles.tabs}>
        {KINDS.map((k) => (
          <button
            key={k.id}
            className={clsx(styles.tab, kind === k.id && styles.tabActive)}
            onClick={() => setKind(k.id)}
          >
            {k.label}
          </button>
        ))}
        <div style={{ flex: 1 }} />
        <select
          className="select"
          style={{ width: 175 }}
          value={sort}
          onChange={(e) => setSort(e.target.value as SortBy)}
          aria-label="Sort results"
        >
          {SORTS.map((s) => (
            <option key={s.id} value={s.id}>
              {s.label}
            </option>
          ))}
        </select>
      </div>

      <div className={styles.body}>
        <aside className={styles.filters}>
          <section className={styles.filterSection}>
            <h3 className={styles.filterTitle}>Source</h3>
            <label className={styles.check}>
              <input
                type="checkbox"
                checked={providers.includes("modrinth")}
                onChange={() => toggleSource("modrinth")}
              />
              <span className="badge badge-modrinth">Modrinth</span>
            </label>
            <label className={clsx(styles.check, !config?.hasCurseforgeKey && styles.checkOff)}>
              <input
                type="checkbox"
                checked={providers.includes("curseforge")}
                disabled={!config?.hasCurseforgeKey}
                onChange={() => toggleSource("curseforge")}
              />
              <span className="badge badge-curseforge">CurseForge</span>
            </label>
            {!config?.hasCurseforgeKey && (
              <p className={styles.filterNote}>
                Needs your own API key.{" "}
                <button
                  className={styles.linkBtn}
                  onClick={() => navigate({ view: "settings", tab: "sources" })}
                >
                  Add it
                </button>
              </p>
            )}
          </section>

          {target && (
            <section className={styles.filterSection}>
              <h3 className={styles.filterTitle}>Compatibility</h3>
              <p className={styles.compat}>
                Filtering to what works with{" "}
                <strong>Minecraft {target.mcVersion}</strong>
                {loader && (
                  <>
                    {" "}
                    on <strong>{loaderLabel(loader)}</strong>
                  </>
                )}
                .
              </p>
            </section>
          )}

          {available.length > 0 && (
            <section className={styles.filterSection}>
              <h3 className={styles.filterTitle}>Categories</h3>
              <div className={styles.categories}>
                {available.map((category) => (
                  <button
                    key={category}
                    className={clsx(
                      styles.categoryChip,
                      categories.includes(category) && styles.categoryActive,
                    )}
                    onClick={() => toggleCategory(category)}
                  >
                    {titleCase(category)}
                  </button>
                ))}
              </div>
            </section>
          )}
        </aside>

        <div className={styles.results}>
          {warnings.map((warning) => (
            <div key={warning} className={styles.warning}>
              <AlertTriangle size={14} />
              {warning}
            </div>
          ))}

          {loading ? (
            <div className={styles.grid}>
              {Array.from({ length: 8 }, (_, i) => (
                <div key={i} className="skeleton" style={{ height: 116 }} />
              ))}
            </div>
          ) : hits.length === 0 ? (
            <div className={styles.empty}>
              <Info size={30} className={styles.emptyIcon} />
              <p>
                Nothing found
                {debounced && <> for “{debounced}”</>}
                {target && <> that fits {target.name}</>}.
              </p>
              <p className={styles.emptyHint}>
                Try a different search, widen the sources, or clear the category
                filters.
              </p>
            </div>
          ) : (
            <>
              <div className={styles.grid}>
                {hits.map((project) => (
                  <ProjectCard
                    key={`${project.provider}:${project.id}`}
                    project={project}
                    installed={isInstalled(project)}
                    busy={installing === project.id}
                    directInstall={Boolean(lockedInstance)}
                    onOpen={() => setSelected(project)}
                    onRequestInstall={requestInstall}
                  />
                ))}
              </div>

              {!exhausted && (
                <button
                  className={clsx("btn", styles.loadMore)}
                  onClick={() => {
                    const next = page + 1;
                    setPage(next);
                    void runSearch(next);
                  }}
                  disabled={loadingMore}
                >
                  {loadingMore ? (
                    <Loader2 size={14} className="spin" />
                  ) : (
                    <Download size={14} />
                  )}
                  Load more
                </button>
              )}
            </>
          )}
        </div>
      </div>

      {selected && (
        <ProjectPanel
          project={selected}
          target={target}
          onClose={() => setSelected(null)}
          onRequestInstall={requestInstall}
          directInstall={Boolean(lockedInstance)}
        />
      )}

      {askTarget && (
        <InstallTargetModal
          project={askTarget.project}
          preselect={targetId}
          installedIn={
            target && isInstalled(askTarget.project)
              ? new Set([target.id])
              : undefined
          }
          busy={installing !== null}
          onPick={installInto}
          onClose={() => setAskTarget(null)}
        />
      )}

      {packInstall && (
        <ModpackInstallModal
          project={packInstall.project}
          versionId={packInstall.versionId}
          onClose={() => setPackInstall(null)}
        />
      )}
    </>
  );
}

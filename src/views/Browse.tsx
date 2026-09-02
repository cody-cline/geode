import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Download, Info, Loader2, Search } from "lucide-react";

import InstallTargetModal from "../components/InstallTargetModal";
import InstanceIcon from "../components/InstanceIcon";
import ProjectCard from "../components/ProjectCard";
import ProjectPanel from "../components/ProjectPanel";
import { api } from "../lib/api";
import { titleCase } from "../lib/format";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type {
  ConnectorSupport,
  Instance,
  Loader,
  Project,
  ProjectKind,
  Provider,
  SortBy,
} from "../lib/types";

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
  /** Last instance installed into, used to preselect the picker. */
  const [lastTargetId, setLastTargetId] = useState<string>("");
  const [filterVersion, setFilterVersion] = useState("");
  const [filterLoader, setFilterLoader] = useState("");
  const [gameVersions, setGameVersions] = useState<string[]>([]);
  const [includeFabric, setIncludeFabric] = useState(false);
  const [connector, setConnector] = useState<ConnectorSupport | null>(null);

  const [hits, setHits] = useState<Project[]>([]);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [exhausted, setExhausted] = useState(false);
  const [selected, setSelected] = useState<Project | null>(null);
  const [installed, setInstalled] = useState<Set<string>>(new Set());
  const [page, setPage] = useState(0);

  // Mods ask where they should go; packs do not, because they answer that
  // themselves by building the instance.
  const [askTarget, setAskTarget] = useState<{
    project: Project;
    versionId?: string;
  } | null>(null);
  const [installing, setInstalling] = useState<string | null>(null);

  const progressMap = useStore((s) => s.progress);

  /**
   * The live job for a row, if any. Content installs are keyed
   * `install:<instance>:<project>` and packs `pack:<project>`, so a row can
   * find its own without knowing which instance was chosen.
   */
  const progressFor = (project: Project) => {
    if (project.kind === "modpack") return progressMap[`pack:${project.id}`];
    const suffix = `:${project.id}`;
    for (const [key, value] of Object.entries(progressMap)) {
      if (key.startsWith("install:") && key.endsWith(suffix)) return value;
    }
    return undefined;
  };

  // Guards against an older, slower request overwriting a newer one.
  const requestSeq = useRef(0);

  const target = lockedInstance;

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

  const contentKeys = useCallback(async (instanceId: string) => {
    const lists = await Promise.all(
      (["mod", "resourcepack", "shaderpack", "datapack"] as const).map((k) =>
        api.listContent(instanceId, k),
      ),
    );
    return new Set(
      lists
        .flat()
        .filter((item) => item.projectId)
        .map((item) => `${item.provider}:${item.projectId}`),
    );
  }, []);

  // Cards only claim "Added" when there is one unambiguous instance to be
  // added to — in global browse a mod may be in one instance and not another,
  // and a single badge cannot honestly say which.
  const refreshInstalled = useCallback(async () => {
    if (!lockedInstance) {
      setInstalled(new Set());
      return;
    }
    try {
      setInstalled(await contentKeys(lockedInstance.id));
    } catch {
      setInstalled(new Set());
    }
  }, [lockedInstance, contentKeys]);

  useEffect(() => {
    void refreshInstalled();
  }, [refreshInstalled]);

  /**
   * A modpack defines an instance, so a second install would only produce a
   * duplicate. If one already exists for this project, the row plays it.
   */
  const packInstance = (project: Project): Instance | undefined => {
    if (project.kind !== "modpack") return undefined;
    const keys = new Set([
      `${project.provider}:${project.id}`,
      ...project.alsoOn.map((r) => `${r.provider}:${r.id}`),
    ]);
    return instances.find(
      (i) =>
        i.pack?.provider &&
        i.pack?.projectId &&
        keys.has(`${i.pack.provider}:${i.pack.projectId}`),
    );
  };

  const isInstalled = (project: Project) =>
    installed.has(`${project.provider}:${project.id}`) ||
    project.alsoOn.some((ref) => installed.has(`${ref.provider}:${ref.id}`));

  // Which instances already have the project the picker is open for.
  const [installedIn, setInstalledIn] = useState<Set<string>>(new Set());
  useEffect(() => {
    if (!askTarget) return;
    let cancelled = false;
    const key = `${askTarget.project.provider}:${askTarget.project.id}`;
    const alt = askTarget.project.alsoOn.map((r) => `${r.provider}:${r.id}`);

    Promise.all(
      instances.map(async (i) => {
        try {
          const keys = await contentKeys(i.id);
          return keys.has(key) || alt.some((a) => keys.has(a)) ? i.id : null;
        } catch {
          return null;
        }
      }),
    ).then((ids) => {
      if (!cancelled) setInstalledIn(new Set(ids.filter(Boolean) as string[]));
    });

    return () => {
      cancelled = true;
    };
  }, [askTarget, instances, contentKeys]);

  function requestInstall(project: Project, versionId?: string) {
    // A pack already carries its own name, version and loader, so there is
    // nothing to ask — it creates its own instance straight away, even when
    // invoked from inside another one.
    if (project.kind === "modpack") {
      const existing = packInstance(project);
      if (existing) {
        navigate({ view: "instance", id: existing.id });
        return;
      }
      void installPack(project, versionId);
      return;
    }
    if (lockedInstance) {
      void installDirect(lockedInstance, project, versionId);
      return;
    }
    setAskTarget({ project, versionId });
  }

  async function installPack(project: Project, versionId?: string) {
    setInstalling(project.id);
    try {
      const instance = await api.installModpack(
        project.provider,
        project.id,
        versionId,
      );
      await refreshInstances();
      toast({
        kind: "success",
        title: `Installed ${instance.name}`,
        body: `Minecraft ${instance.mcVersion}${
          instance.loader !== "vanilla" ? ` · ${instance.loader}` : ""
        }`,
      });
      // Deliberately stays put: installing a pack is often one of several
      // things you are doing while browsing, and jumping away loses your
      // place in the results.
    } catch (e) {
      // A partial import still leaves a usable instance, so refresh either way.
      await refreshInstances();
      reportError(`Could not install ${project.name}`, e);
    } finally {
      setInstalling(null);
    }
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
        project.viaConnector,
      );
      setLastTargetId(instance.id);
      await Promise.all([refreshInstalled(), refreshInstances()]);

      const extras = report.installed.length - 1;
      toast({
        kind: "success",
        title: `Added ${project.name} to ${instance.name}`,
        body: project.viaConnector
          ? `Running on Sinytra Connector${extras > 0 ? `, plus ${extras} more` : ""}.`
          : extras > 0
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

  /**
   * Build an instance for the mod, then install into it.
   *
   * The two steps are separate backend calls, so a failure partway leaves the
   * new instance behind. That is deliberate — an empty instance of the right
   * version is a usable starting point, and deleting it would throw away work
   * the user asked for.
   */
  async function createAndInstall(
    name: string,
    gameVersion: string,
    loader: Loader,
  ) {
    if (!askTarget) return;
    const { project, versionId } = askTarget;
    setInstalling(project.id);

    try {
      let loaderVersion = "";
      if (loader !== "vanilla") {
        const builds = await api.listLoaderVersions(loader, gameVersion);
        if (builds.length === 0) {
          throw new Error(
            `No ${loader} build exists for Minecraft ${gameVersion}.`,
          );
        }
        loaderVersion = builds[0];
      }

      const instance = await api.createInstance(
        name,
        gameVersion,
        loader,
        loaderVersion,
      );
      await refreshInstances();
      setAskTarget(null);
      await installDirect(instance, project, versionId);
    } catch (e) {
      reportError(`Could not create an instance for ${project.name}`, e);
    } finally {
      setInstalling(null);
    }
  }

  useEffect(() => {
    const timer = setTimeout(() => setDebounced(query), 280);
    return () => clearTimeout(timer);
  }, [query]);

  useEffect(() => {
    if (lockedInstance) return;
    api
      .listGameVersions(config?.showSnapshots ?? false)
      .then((list) => setGameVersions(list.map((v) => v.id)))
      .catch(() => setGameVersions([]));
  }, [lockedInstance, config?.showSnapshots]);

  useEffect(() => {
    api
      .getCategories(kind)
      .then(setAvailable)
      .catch(() => setAvailable([]));
    setCategories([]);
  }, [kind]);

  // Only the in-instance view scopes automatically — there the destination is
  // fixed, so narrowing is helpful. Global browse shows everything unless the
  // user asks otherwise, because hiding a mod that would have worked is worse
  // than showing one that will not.
  const gameVersion = lockedInstance
    ? lockedInstance.mcVersion
    : filterVersion || null;
  const loader = lockedInstance
    ? lockedInstance.loader !== "vanilla"
      ? lockedInstance.loader
      : null
    : filterLoader || null;

  // Whether Fabric mods can be offered for the current target. The answer
  // comes from the files Connector publishes, so it needs no local version
  // table and stays right as Connector gains or drops support.
  useEffect(() => {
    const version = lockedInstance?.mcVersion ?? filterVersion;
    const activeLoader = lockedInstance?.loader ?? filterLoader;
    if (!version || !activeLoader) {
      setConnector(null);
      return;
    }
    let cancelled = false;
    api
      .connectorSupport(version, activeLoader)
      .then((s) => !cancelled && setConnector(s))
      .catch(() => !cancelled && setConnector(null));
    return () => {
      cancelled = true;
    };
  }, [lockedInstance, filterVersion, filterLoader]);

  const connectorOffered = kind === "mod" && Boolean(connector?.supported);

  // Turning the target off should not leave the option silently applied.
  useEffect(() => {
    if (!connectorOffered && includeFabric) setIncludeFabric(false);
  }, [connectorOffered, includeFabric]);

  const useFabric = connectorOffered && includeFabric;

  const runSearch = useCallback(
    async (page: number) => {
      const offset = page * PAGE_SIZE;
      const seq = ++requestSeq.current;
      if (page === 0) setLoading(true);
      else setLoadingMore(true);

      try {
        const base = {
          query: debounced,
          kind,
          gameVersion,
          categories,
          sort,
          offset,
          limit: PAGE_SIZE,
          providers: (providers.length > 0
            ? providers
            : (["modrinth"] as Provider[])) as Provider[],
        };

        // Two queries when Connector is in play: the target's own loader, then
        // Fabric. Native builds win, so anything only the second query found is
        // what Connector actually adds.
        const [native, fabric] = await Promise.all([
          api.searchProjects({ ...base, loader }),
          useFabric
            ? api.searchProjects({ ...base, loader: "fabric" })
            : Promise.resolve(null),
        ]);
        if (seq !== requestSeq.current) return;

        const nativeKeys = new Set(
          native.hits.flatMap((p) => [
            `${p.provider}:${p.id}`,
            ...p.alsoOn.map((r) => `${r.provider}:${r.id}`),
          ]),
        );
        const viaConnector = (fabric?.hits ?? [])
          .filter((p) => !nativeKeys.has(`${p.provider}:${p.id}`))
          .map((p) => ({ ...p, viaConnector: true }));
        const results = {
          // Interleave by each hit's rank in its own result set so Connector
          // mods land among equally-relevant native ones instead of below them.
          hits: [...native.hits, ...viaConnector]
            .map((p, i) => ({ p, rank: i < native.hits.length ? i : i - native.hits.length }))
            .sort((a, b) => a.rank - b.rank)
            .map((e) => e.p),
          warnings: [
            ...native.warnings,
            ...(fabric?.warnings ?? []).filter(
              (w) => !native.warnings.includes(w),
            ),
          ],
        };

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
    [
      debounced,
      kind,
      gameVersion,
      loader,
      categories,
      sort,
      providers,
      useFabric,
      reportError,
    ],
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

  // A modpack builds its own instance rather than going into one, so it is not
  // a thing you can add to the profile you are already in.
  const kinds = useMemo(
    () => (lockedInstance ? KINDS.filter((k) => k.id !== "modpack") : KINDS),
    [lockedInstance],
  );

  useEffect(() => {
    if (lockedInstance && kind === "modpack") setKind("mod");
  }, [lockedInstance, kind]);

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
          ) : (
            <>
              <span className={styles.targetLabel}>Filter</span>
              <select
                className="select"
                style={{ width: 150 }}
                value={filterVersion}
                onChange={(e) => setFilterVersion(e.target.value)}
                aria-label="Filter by Minecraft version"
              >
                <option value="">Any version</option>
                {gameVersions.map((v) => (
                  <option key={v} value={v}>
                    {v}
                  </option>
                ))}
              </select>
              {(kind === "mod" || kind === "modpack") && (
                <select
                  className="select"
                  style={{ width: 130 }}
                  value={filterLoader}
                  onChange={(e) => setFilterLoader(e.target.value)}
                  aria-label="Filter by mod loader"
                >
                  <option value="">Any loader</option>
                  {LOADERS.filter((l) => l.id !== "vanilla").map((l) => (
                    <option key={l.id} value={l.id}>
                      {l.label}
                    </option>
                  ))}
                </select>
              )}
            </>
          )}
        </div>
      </header>

      <div className={styles.tabs}>
        {kinds.map((k) => (
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

          {lockedInstance && (
            <section className={styles.filterSection}>
              <h3 className={styles.filterTitle}>Compatibility</h3>
              <p className={styles.compat}>
                Filtering to what works with{" "}
                <strong>Minecraft {lockedInstance.mcVersion}</strong>
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

          {connectorOffered && (
            <section className={styles.filterSection}>
              <h3 className={styles.filterTitle}>Compatibility layer</h3>
              <label className={styles.check}>
                <input
                  type="checkbox"
                  checked={includeFabric}
                  onChange={(e) => setIncludeFabric(e.target.checked)}
                />
                <span className={styles.connectorLabel}>Include Fabric mods</span>
              </label>
              <p className={styles.filterNote}>
                Mixes Fabric-only mods into the list, run through Sinytra
                Connector. Installing one also adds Connector, Forgified Fabric
                API and Connector Extras.
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
            <div className={styles.list}>
              {Array.from({ length: 7 }, (_, i) => (
                <div key={i} className="skeleton" style={{ height: 94 }} />
              ))}
            </div>
          ) : hits.length === 0 ? (
            <div className={styles.empty}>
              <Info size={30} className={styles.emptyIcon} />
              <p>
                Nothing found
                {debounced && <> for “{debounced}”</>}
                {lockedInstance && <> that fits {lockedInstance.name}</>}.
              </p>
              <p className={styles.emptyHint}>
                Try a different search, widen the sources, or clear the category
                filters.
              </p>
            </div>
          ) : (
            <>
              <div className={styles.list}>
                {hits.map((project) => (
                  <ProjectCard
                    key={`${project.provider}:${project.id}`}
                    project={project}
                    installed={isInstalled(project)}
                    installedInstance={packInstance(project)}
                    busy={installing === project.id}
                    progress={progressFor(project)}
                    directInstall={Boolean(lockedInstance)}
                    onOpen={() => setSelected(project)}
                    onRequestInstall={requestInstall}
                    viaConnector={project.viaConnector}
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
          installedInstance={packInstance(selected)}
          onClose={() => setSelected(null)}
          onRequestInstall={requestInstall}
          directInstall={Boolean(lockedInstance)}
        />
      )}

      {askTarget && (
        <InstallTargetModal
          project={askTarget.project}
          preselect={lastTargetId}
          installedIn={installedIn}
          busy={installing !== null}
          onPick={installInto}
          onCreateAndInstall={createAndInstall}
          onClose={() => setAskTarget(null)}
        />
      )}

    </>
  );
}

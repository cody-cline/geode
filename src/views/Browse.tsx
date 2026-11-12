import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { FocusEvent, KeyboardEvent } from "react";
import clsx from "clsx";
import {
  AlertTriangle,
  Check,
  ClipboardList,
  Download,
  Info,
  Loader2,
  Search,
  Server as ServerIcon,
} from "lucide-react";

import InstallTargetModal from "../components/InstallTargetModal";
import PackServerModal from "../components/PackServerModal";
import PlanSourceTabs from "../components/PlanSourceTabs";
import type { PlanSource } from "../components/PlanSourceTabs";
import ServerTargetModal from "../components/ServerTargetModal";
import InstanceIcon from "../components/InstanceIcon";
import ProjectCard from "../components/ProjectCard";
import ProjectPanel from "../components/ProjectPanel";
import { api, errorMessage } from "../lib/api";
import { titleCase } from "../lib/format";
import { rovingKey } from "../lib/rovingFocus";
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
  { id: "plugin", label: "Plugins" },
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

/** The arrows inside the category chips, by the step each one moves. */
const CHIP_STEP: Record<string, number> = {
  ArrowLeft: -1,
  ArrowUp: -1,
  ArrowRight: 1,
  ArrowDown: 1,
};

/** Loaders have fixed capitalisation ("NeoForge"), so never title-case them. */
function loaderLabel(id: string): string {
  return LOADERS.find((l) => l.id === id)?.label ?? titleCase(id);
}

/** A hit's own identity: what it is listed as, and what its row is called. */
function rowId(p: Project): string {
  return `${p.provider}:${p.id}`;
}

/** Every identity a hit answers to, so cross-provider listings match. */
function projectKeys(p: Project): string[] {
  return [rowId(p), ...p.alsoOn.map((r) => `${r.provider}:${r.id}`)];
}

/**
 * Browsing to fill a plan rather than to install.
 *
 * Nothing is downloaded and nothing needs a destination, so the whole
 * install apparatus — the picker, progress, the Connector option — stands down
 * and the action becomes "add to the list".
 */
export interface PlanBrowse {
  name: string;
  /** `provider:projectId` for everything already on the list. */
  inPlan: Set<string>;
  onAdd: (projects: Project[]) => void;
  /** Which source the plan is adding from; packs are a whole other view. */
  source: PlanSource;
  onSource: (next: PlanSource) => void;
}

/**
 * Browsing to fill a server.
 *
 * Like an instance the destination is fixed, but what lands where depends on
 * the folder rather than the project: a Paper server takes plugins, a Fabric
 * one takes mods, and a hybrid takes both in different places.
 */
export interface ServerBrowse {
  name: string;
  /** `provider:projectId` already installed in this folder. */
  installed: Set<string>;
  /**
   * Which folder an install lands in. Datapacks belong to the world rather
   * than the loader, so every server that hosts one takes them.
   */
  kind: "plugin" | "mod" | "datapack";
  /** The Minecraft version the server runs, so results are scoped to it. */
  mcVersion: string;
  /**
   * The loader its mods are built for, from the backend rather than guessed
   * at here — `null` when the folder is not loader-specific.
   *
   * Plugins deliberately have none: a Bukkit plugin runs on Paper, Spigot and
   * Purpur alike, and narrowing to one of those hides the rest.
   */
  loader: string | null;
  onInstall: (project: Project) => void;
}

export default function Browse({
  lockedInstance,
  onDone,
  plan,
  server,
}: {
  /**
   * Set when browsing from inside an instance. The destination is then implied
   * by context, so results are scoped to it and installs skip the picker —
   * asking again would only re-confirm a choice already made by being here.
   */
  lockedInstance?: Instance;
  onDone?: () => void;
  plan?: PlanBrowse;
  server?: ServerBrowse;
} = {}) {
  const instances = useStore((s) => s.instances);
  const config = useStore((s) => s.config);
  const navigate = useStore((s) => s.navigate);
  const reportError = useStore((s) => s.reportError);
  const toast = useStore((s) => s.toast);
  const refreshInstances = useStore((s) => s.refreshInstances);
  const refreshServers = useStore((s) => s.refreshServers);

  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [kind, setKind] = useState<ProjectKind>("mod");
  const [sort, setSort] = useState<SortBy>("relevance");
  // SpigotMC is selected from the start so it comes back on its own when the
  // Plugins tab is opened, the way CurseForge does once a key is added. It is
  // filtered out of every other tab below.
  const [sources, setSources] = useState<Provider[]>([
    "modrinth",
    "curseforge",
    "spiget",
  ]);
  const [categories, setCategories] = useState<string[]>([]);
  const [available, setAvailable] = useState<string[]>([]);
  /**
   * The chip carrying the rail's one tab stop, held as a category name rather
   * than a position: the list is replaced wholesale when the kind tab changes,
   * and twenty-one names are not twenty-one of anything else.
   */
  const [activeChip, setActiveChip] = useState("");
  /** Last instance installed into, used to preselect the picker. */
  const [lastTargetId, setLastTargetId] = useState<string>("");
  const [filterVersion, setFilterVersion] = useState("");
  const [filterLoader, setFilterLoader] = useState("");
  const [gameVersions, setGameVersions] = useState<string[]>([]);
  const [includeFabric, setIncludeFabric] = useState(false);
  const [connector, setConnector] = useState<ConnectorSupport | null>(null);
  /**
   * Show mods whose author has not listed this instance's Minecraft version,
   * and install whatever is picked from that list with its version ignored.
   *
   * Searching is the real barrier, not the Install button: the search is
   * filtered to the instance's version, so the mod a person wants to override
   * for is the one mod they cannot find. Dropping the filter here and
   * recording the choice on what gets installed are the same decision, so they
   * are the same box.
   */
  const [ignoreGameVersion, setIgnoreGameVersion] = useState(false);

  const [hits, setHits] = useState<Project[]>([]);
  const [warnings, setWarnings] = useState<string[]>([]);
  /**
   * Why the newest search did not answer, when it did not.
   *
   * Separate from `warnings`, which a search that *worked* returns to say a
   * provider was skipped. This one means there is no answer at all, and the
   * screen has to stop presenting the absence of results as a result.
   */
  const [failed, setFailed] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [exhausted, setExhausted] = useState(false);
  const [selected, setSelected] = useState<Project | null>(null);
  const [installed, setInstalled] = useState<Set<string>>(new Set());
  const [page, setPage] = useState(0);
  /**
   * The row carrying the results grid's one tab stop, held as a row's id
   * rather than its position.
   *
   * The list grows under the keyboard: "Load more" appends a page, and the
   * merge that folds an already-seen project out of it means the page you get
   * is not the page you asked for. A position would be a tab stop moving under
   * somebody who moved nothing; an id is still the same row afterwards, and is
   * simply not found when a new search has replaced the lot.
   */
  const [activeRow, setActiveRow] = useState("");

  // Mods ask where they should go; packs do not, because they answer that
  // themselves by building the instance.
  const [askTarget, setAskTarget] = useState<{
    project: Project;
    versionId?: string;
  } | null>(null);
  /** A plugin found in the global view, waiting to be told which server. */
  const [askServer, setAskServer] = useState<Project | null>(null);
  /** A pack being hosted rather than played, waiting on a software choice. */
  const [askPackServer, setAskPackServer] = useState<{
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

  /** Browsing on nobody's behalf: the whole catalogue, with a page of its own. */
  const globalBrowse = !lockedInstance && !plan && !server;

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
    // SpigotMC indexes plugins and nothing else, so it is offered only where
    // plugins are what you are looking for — asking it for mods finds nothing.
    if (kind === "plugin") availableSources.push("spiget");
    return sources.filter((s) => availableSources.includes(s));
  }, [sources, hasCurseforgeKey, kind]);

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

  // For a plan, "installed" means "already on the list" — and it counts under
  // any identity the project answers to, so the CurseForge twin of a Modrinth
  // entry does not offer itself a second time.
  const isInstalled = (project: Project) =>
    plan
      ? projectKeys(project).some((key) => plan.inPlan.has(key))
      : server
        ? projectKeys(project).some((key) => server.installed.has(key))
        : installed.has(`${project.provider}:${project.id}`) ||
          project.alsoOn.some((ref) => installed.has(`${ref.provider}:${ref.id}`));

  /**
   * Which instances hold each project, under every identity a project answers
   * to.
   *
   * An instance's installed content is not in the store — `Instance` carries a
   * mod count and the pack's files, but no project ids — so knowing costs four
   * `listContent` calls per instance. They are paid once per instance list,
   * here, and read by every row and by the picker; asked per card it would be
   * the same four calls again for every instance, times however many hits the
   * search returned.
   */
  const [heldBy, setHeldBy] = useState<Map<string, Instance[]>>(new Map());

  useEffect(() => {
    // Only global browse has anything to work out: where the destination is
    // fixed, what it already holds is `installed` above, and nothing else asks.
    if (!globalBrowse) {
      setHeldBy(new Map());
      return;
    }
    let cancelled = false;
    void Promise.all(
      instances.map(
        async (i) =>
          [i, await contentKeys(i.id).catch(() => new Set<string>())] as const,
      ),
    ).then((pairs) => {
      if (cancelled) return;
      const map = new Map<string, Instance[]>();
      for (const [instance, keys] of pairs) {
        for (const key of keys) {
          const held = map.get(key);
          if (held) held.push(instance);
          else map.set(key, [instance]);
        }
      }
      setHeldBy(map);
    });
    return () => {
      cancelled = true;
    };
  }, [globalBrowse, instances, contentKeys]);

  /**
   * The instances a hit is already in, deduplicated: a project installed from
   * both providers into one instance answers to two keys and is still one
   * instance.
   *
   * What the row says about this goes *beside* its install button and never
   * instead of it, for the same reason `installed` stays empty here — a mod
   * being in one instance is no argument against putting it in another.
   */
  const heldInstances = (project: Project): Instance[] => {
    const seen = new Set<string>();
    const held: Instance[] = [];
    for (const key of projectKeys(project)) {
      for (const instance of heldBy.get(key) ?? []) {
        if (seen.has(instance.id)) continue;
        seen.add(instance.id);
        held.push(instance);
      }
    }
    return held;
  };

  // Which instances already have the project the picker is open for, read off
  // the map the rows read, so opening it asks the backend nothing new.
  const installedIn = new Set(
    askTarget ? heldInstances(askTarget.project).map((i) => i.id) : [],
  );

  function requestInstall(project: Project, versionId?: string) {
    // Adding to a plan is the whole transaction: no version to choose, no
    // instance to choose, nothing to download.
    if (plan) {
      plan.onAdd([project]);
      return;
    }
    if (server) {
      server.onInstall(project);
      return;
    }
    // Keyed on what is being browsed, not on the project's own type: Modrinth
    // returns plenty of multi-platform plugins typed as mods, and asking the
    // project would misroute exactly those.
    if (kind === "plugin") {
      // The panel goes before the dialog arrives, every time one does. Two
      // overlays mounted together both hear Escape — the panel listens on the
      // document — so one press dismissed the pair instead of the top layer.
      // It is also what lets the panel's focus trap put the keyboard back
      // where it came from rather than behind the dialog that replaced it.
      setSelected(null);
      setAskServer(project);
      return;
    }
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
    // Closes the panel first, for the reason above.
    setSelected(null);
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

  async function hostPack(project: Project, kind: string, versionId?: string) {
    setInstalling(project.id);
    try {
      const report = await api.createServerFromModpack(
        project.provider,
        project.id,
        kind,
        versionId,
      );
      await refreshServers();
      toast({
        kind: "success",
        title: `Built ${report.serverName}`,
        body: `${report.installed.length} ${
          report.installed.length === 1 ? "mod" : "mods"
        } from the pack.`,
      });
      if (report.skipped.length > 0) {
        toast({
          kind: "error",
          title: "Some of the pack was skipped",
          body: report.skipped.join("; "),
        });
      }
      // Unlike a pack install, this does navigate: a fresh server has
      // properties to set and a world to generate before it is worth
      // anything, and all of that lives on its page.
      navigate({ view: "server", id: report.serverId });
    } catch (e) {
      // A half-built server is still a server, so let the list catch up.
      await refreshServers();
      reportError(`Could not host ${project.name}`, e);
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
        // What the list was widened by travels with what was installed from
        // it, so the choice is recorded against the mod rather than lost with
        // the search that found it.
        useAnyVersion,
      );
      setLastTargetId(instance.id);
      await Promise.all([refreshInstalled(), refreshInstances()]);

      const extras = report.installed.length - 1;
      toast({
        kind: "success",
        title: `Added ${project.name} to ${instance.name}`,
        // Every one of these can be true at once, so none of them is allowed
        // to hide the others: a Connector install with the box ticked used to
        // stop mentioning that a whole runtime had arrived.
        //
        // The override reads off `ignoredGameVersion` — what the install
        // actually did — and not off the box, which is a filter over the whole
        // search. A mod that fits perfectly gets no override recorded and so
        // must not be told it has one.
        body:
          [
            report.ignoredGameVersion &&
              "Its Minecraft version is ignored from now on, so it may not run. Undo that on its row in Content.",
            project.viaConnector && "Running on Sinytra Connector.",
            extras > 0 &&
              `Plus ${extras} required ${extras === 1 ? "dependency" : "dependencies"}.`,
          ]
            .filter(Boolean)
            .join(" ") || undefined,
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
  // A server scopes automatically for the same reason an instance does, and it
  // needs its own branch: without one it fell through to global browse, where
  // the filters it never shows are empty — so a Forge server was asking for
  // every mod of every loader and every version, and being given them.
  // Offered only where it can mean something: one instance to install into,
  // and mods, which is the only kind the override covers.
  const anyVersionOffered = Boolean(lockedInstance) && !plan && !server && kind === "mod";
  const useAnyVersion = anyVersionOffered && ignoreGameVersion;

  const gameVersion = lockedInstance
    ? // Dropped, so the mods whose authors have not listed this version are in
      // the list at all. The loader is still asked for: a Fabric jar in a
      // Forge folder does nothing, so widening that would only hand somebody a
      // mod that cannot work.
      useAnyVersion
      ? null
      : lockedInstance.mcVersion
    : server
      ? server.mcVersion
      : filterVersion || null;
  const loader = lockedInstance
    ? lockedInstance.loader !== "vanilla"
      ? lockedInstance.loader
      : null
    : server
      ? server.loader
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

  // Not offered for a plan: whether a mod needs Connector is the planner's
  // decision once it knows the whole list, not a search filter.
  const connectorOffered =
    !plan && !server && kind === "mod" && Boolean(connector?.supported);

  // Turning the target off should not leave the option silently applied.
  useEffect(() => {
    if (!connectorOffered && includeFabric) setIncludeFabric(false);
  }, [connectorOffered, includeFabric]);

  // The same, for the same reason: a box left ticked after the thing it
  // applies to has gone would keep widening a search nobody asked it to.
  useEffect(() => {
    if (!anyVersionOffered && ignoreGameVersion) setIgnoreGameVersion(false);
  }, [anyVersionOffered, ignoreGameVersion]);

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

        // A project can be listed on both providers, so match on every
        // identity a hit carries rather than just its own.
        const nativeKeys = new Set(native.hits.flatMap(projectKeys));
        const fabricKeys = new Set((fabric?.hits ?? []).flatMap(projectKeys));

        // Found by both queries: it has a build for either loader, and the row
        // offers the choice. Found only by the Fabric one: Connector is the
        // only way to run it here.
        const nativeHits = native.hits.map((p) =>
          fabric && projectKeys(p).some((k) => fabricKeys.has(k))
            ? { ...p, fabricAlso: true }
            : p,
        );
        const viaConnector = (fabric?.hits ?? [])
          .filter((p) => !projectKeys(p).some((k) => nativeKeys.has(k)))
          .map((p) => ({ ...p, viaConnector: true }));
        const results = {
          // Interleave by each hit's rank in its own result set so Connector
          // mods land among equally-relevant native ones instead of below them.
          hits: [...nativeHits, ...viaConnector]
            .map((p, i) => ({ p, rank: i < nativeHits.length ? i : i - nativeHits.length }))
            .sort((a, b) => a.rank - b.rank)
            .map((e) => e.p),
          warnings: [
            ...native.warnings,
            ...(fabric?.warnings ?? []).filter(
              (w) => !native.warnings.includes(w),
            ),
          ],
        };

        setFailed(null);
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
        if (seq === requestSeq.current) {
          reportError("Search failed", e);
          setFailed(errorMessage(e));
          // A first page that failed leaves nothing known about this query, and
          // what was on screen belongs to the last one. Keeping it would hand
          // somebody the previous search's results under the words they just
          // typed. A later page is more of a query that did work, so its rows
          // stay and only the strip above them says the rest did not arrive.
          if (page === 0) {
            setHits([]);
            setWarnings([]);
          }
        }
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

  // The choice lives on the hit, so everything downstream — the install call,
  // the panel, the picker dialog — carries it without being told separately.
  const chooseFabric = (project: Project, fabric: boolean) =>
    setHits((prev) =>
      prev.map((p) =>
        p.provider === project.provider && p.id === project.id
          ? { ...p, viaConnector: fabric }
          : p,
      ),
    );

  // A modpack builds its own instance rather than going into one, so it is not
  // a thing you can add to the profile you are already in.
  const kinds = useMemo(
    () =>
      plan
        ? // A plan is a list of mods, so there is nothing else to browse for.
          KINDS.filter((k) => k.id === "mod")
        : server
          ? // The folder decides: a plugin search for plugins/, mods for mods/.
            KINDS.filter((k) => k.id === server.kind)
          : lockedInstance
            ? // An instance has no plugins folder, and a pack is not something
              // you add to one.
              KINDS.filter((k) => k.id !== "modpack" && k.id !== "plugin")
            : KINDS,
    [lockedInstance, plan, server],
  );

  useEffect(() => {
    if (lockedInstance && kind === "modpack") setKind("mod");
    if (plan && kind !== "mod") setKind("mod");
    if (server && kind !== server.kind) setKind(server.kind);
  }, [lockedInstance, plan, server, kind]);

  const kindLabel = useMemo(
    () => KINDS.find((k) => k.id === kind)?.label.toLowerCase() ?? "results",
    [kind],
  );

  /**
   * Which tab carries the strip's one tab stop. Normally the kind in force —
   * but the effect above corrects `kind` a render *after* a server or a plan
   * narrows the list it may appear in, and a strip where no tab is reachable
   * is a strip the keyboard cannot get into at all.
   */
  const kindStop = kinds.some((k) => k.id === kind) ? kind : kinds[0]?.id;

  /**
   * Which chip holds the rail's tab stop. The one the keyboard was last on,
   * else the first — the kind tab replaces every chip, and a group where no
   * chip is reachable is a group the keyboard cannot get into at all.
   */
  const chipStop = available.includes(activeChip) ? activeChip : available[0];

  /**
   * Which row holds the grid's tab stop. The one the keyboard was last on,
   * else the first — a new search replaces every row, and a grid where no row
   * is reachable is a grid the keyboard cannot get into at all.
   */
  const stopRow = hits.some((p) => rowId(p) === activeRow)
    ? activeRow
    : hits.length > 0
      ? rowId(hits[0])
      : "";

  /** Keeps the tab stop where the keyboard actually is, however it got there. */
  function rememberRow(e: FocusEvent<HTMLElement>) {
    const row = e.target.closest<HTMLElement>("[data-row]");
    if (row?.dataset.row) setActiveRow(row.dataset.row);
  }

  /**
   * The arrow keys inside the category chips, which is the other half of what
   * `role="toolbar"` promises: one tab stop for the whole group, the arrows
   * walking between the chips from there, and the toggling done by Space or
   * Enter — which a `<button>` answers itself, so nothing here touches them.
   * Twenty-one filters as twenty-one tab stops is the same bill the results
   * grid stopped paying, charged one panel to the left.
   *
   * Not `rovingKey`, and not for the reason the grid is not: that helper's
   * contract is that the option the arrows land on is *chosen* by landing on
   * it, which is right for one-of-N and ruinous here — these are independent
   * toggles, so a walk across the rail would switch on every filter it passed
   * and re-run the search for each. `aria-pressed` stays for the same reason:
   * each chip is on or off by itself, and nothing in the group is selected.
   *
   * Every arrow walks the one line, wrapping at both ends. A chip cloud is a
   * line in reading order however many rows it wraps onto — unlike the results
   * grid, where a row is a place and `rovingKey`'s own geometry pass reads each
   * option's `top` to find one. Here the rows are an accident of the rail's
   * width, so Down meaning "the next chip" is the only thing it can mean that
   * is the same at every window size.
   *
   * The chips are read off the DOM at the moment of the press, so a kind tab
   * swapping the whole list needs no bookkeeping.
   */
  function moveInCategories(e: KeyboardEvent<HTMLElement>) {
    const step = CHIP_STEP[e.key];
    if (!step) return;
    if (!(e.target instanceof HTMLElement)) return;

    const chips = [
      ...e.currentTarget.querySelectorAll<HTMLElement>("[data-chip]"),
    ];
    const at = chips.indexOf(e.target);
    if (at < 0) return;

    e.preventDefault();
    chips[(at + step + chips.length) % chips.length]?.focus();
  }

  /**
   * The arrow keys inside the results, which is the contract `role="grid"`
   * promises: Up and Down between rows, Left and Right between the controls of
   * the row you are on. Twenty results carry sixty-odd controls, and as plain
   * tab stops there was no way across them and no way out of the middle.
   *
   * Not `rovingKey`, deliberately. That helper is the radio group's contract —
   * the option the arrows land on is *chosen* by landing on it — and a search
   * result is not chosen by being looked at. Nor can its geometry pass be
   * borrowed: it reads each option's `top` to find the rows, and a row's own
   * controls disagree there by design, the name sitting on the first line of a
   * text block while the install button is centred against the whole of it.
   *
   * Everything below is read off the DOM at the moment of the press — which
   * row it came from, which row is next, what each one holds — so a page of
   * results arriving mid-session needs no bookkeeping. The new rows are simply
   * there to be walked onto.
   */
  function moveInResults(e: KeyboardEvent<HTMLElement>) {
    const across = e.key === "ArrowLeft" || e.key === "ArrowRight";
    const down = e.key === "ArrowDown";
    if (!across && !down && e.key !== "ArrowUp") return;

    const target = e.target;
    if (!(target instanceof HTMLElement)) return;
    const from = target.closest("[data-row]");
    if (!from) return;

    // A disabled control is skipped rather than landed on, so a blocked mod —
    // whose install cannot be pressed — is a row of one control.
    const controls = (row: Element) => [
      ...row.querySelectorAll<HTMLElement>("button:not(:disabled)"),
    ];
    const here = controls(from);
    const at = here.indexOf(target);
    if (at < 0) return;

    // Swallowed whether or not it moves anything, so an arrow at the edge of
    // the grid does not scroll the results out from under the row it was
    // pointed at.
    e.preventDefault();

    if (across) {
      const step = e.key === "ArrowRight" ? 1 : -1;
      here[(at + step + here.length) % here.length]?.focus();
      return;
    }

    // Up and Down stop at the first and last row rather than wrapping, which
    // is what `rovingFocus` does for a grid and for the same reason: this list
    // is twenty rows and growing, and being thrown from one end of it to the
    // other is a move somebody has to go looking for their focus after.
    //
    // They move by *cell* rather than by how many controls along you are: a
    // row that offers a loader to choose between carries two more buttons than
    // its neighbours, all of them in the same cell as the name, so counting
    // controls put Down from one install button on the next row's loader
    // chooser. Landing on the first control of the cell underneath is the
    // column holding, which is what a grid's Down means.
    const row = down ? from.nextElementSibling : from.previousElementSibling;
    if (!row) return;
    const cells = (of: Element) => [...of.querySelectorAll("[role=gridcell]")];
    const cell = target.closest("[role=gridcell]");
    const column = cell ? Math.max(0, cells(from).indexOf(cell)) : 0;
    const landing = cells(row)[column];
    const first = landing?.querySelector<HTMLElement>("button:not(:disabled)");
    // A cell can have nothing to land on — a blocked mod's install is disabled
    // — so the row's first control takes the press rather than nothing at all.
    (first ?? controls(row)[0])?.focus();
  }

  return (
    <>
      <header className={styles.header}>
        <div className={styles.headerMain}>
          {/* Its own screen gets its own heading, the way Library, Plans and
              Servers do. Not when it is standing in for a destination's "add
              something" screen, though: the pill on the right is the heading
              there, and it names the instance, plan or server you came from. */}
          {globalBrowse && (
            <div className={styles.headerText}>
              <h1 className={styles.heading}>Browse</h1>
              <p className={styles.lede}>
                Mods, plugins, packs and shaders from Modrinth, CurseForge and
                SpigotMC, searched at once and listed together.
              </p>
            </div>
          )}
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
        </div>

        <div className={styles.target}>
          {server ? (
            <>
              <span className={styles.targetLabel}>Adding to</span>
              {/* The name is capped at 240px, so the long ones are cut off.
                  The title carries the whole of it, and the ellipsis says
                  there is more — on the name rather than on the pill, which is
                  a flex container and so cannot ellipsise its own text. */}
              <span className={styles.lockedTarget} title={server.name}>
                <ServerIcon size={14} />
                <span className={styles.lockedName}>{server.name}</span>
              </span>
              <button className="btn btn-sm btn-primary" onClick={onDone}>
                Done
              </button>
            </>
          ) : plan ? (
            <>
              <span className={styles.targetLabel}>Adding to</span>
              <span className={styles.lockedTarget} title={plan.name}>
                <ClipboardList size={14} />
                <span className={styles.lockedName}>{plan.name}</span>
              </span>
              <button className="btn btn-sm btn-primary" onClick={onDone}>
                Done
              </button>
            </>
          ) : lockedInstance ? (
            <>
              <span className={styles.targetLabel}>Adding to</span>
              <span className={styles.lockedTarget} title={lockedInstance.name}>
                <InstanceIcon instance={lockedInstance} size={20} />
                <span className={styles.lockedName}>{lockedInstance.name}</span>
              </span>
              <button className="btn btn-sm" onClick={onDone}>
                Done
              </button>
            </>
          ) : null}
        </div>
      </header>

      {/* A strip that swaps the list below it is a tab set, and said so to
          nobody: no role, no selected state, no name — in the same row as
          PlanSourceTabs, which announces its own. Only the kind strip is a tab
          set, though. For a plan the row holds PlanSourceTabs instead, which
          names and marks its own group, so the roles go on neither the row nor
          the results while it is there. */}
      <div
        className={styles.tabs}
        role={plan ? undefined : "tablist"}
        aria-label={plan ? undefined : "What to browse"}
        onKeyDown={
          plan
            ? undefined
            : (e) => {
                const next = rovingKey(
                  e,
                  kinds.map((k) => k.id),
                );
                if (next) setKind(next);
              }
        }
      >
        {plan ? (
          <PlanSourceTabs source={plan.source} onSource={plan.onSource} />
        ) : (
          kinds.map((k) => (
            <button
              key={k.id}
              className={clsx(styles.tab, kind === k.id && styles.tabActive)}
              onClick={() => setKind(k.id)}
              role="tab"
              aria-selected={kind === k.id}
              id={`browse-tab-${k.id}`}
              aria-controls="browse-results"
              // One tab stop for the whole strip, with the arrows moving
              // between the kinds from there.
              tabIndex={k.id === kindStop ? 0 : -1}
              data-roving={k.id}
            >
              {k.label}
            </button>
          ))
        )}
      </div>

      <div className={styles.body}>
        <aside className={styles.filters}>
          {/* Sort and the two narrowing selects used to sit above the results,
              in two rows that had nothing else in them, while the rail below
              held the rest of the same job — so what the list shows was set
              from three places at once and the plan's explanation of what the
              selects do was six hundred pixels from the selects. One place,
              and the row above the first result is the search and the kind. */}
          <section className={styles.filterSection}>
            <h3 className={styles.filterTitle}>Sort</h3>
            <select
              className={clsx("select", styles.filterSelect)}
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
          </section>

          {!lockedInstance && !server && (
            <section className={styles.filterSection}>
              <h3 className={styles.filterTitle}>Narrow</h3>
              <select
                className={clsx("select", styles.filterSelect)}
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
              {(Boolean(plan) || kind === "mod" || kind === "modpack") && (
                <select
                  className={clsx("select", styles.filterSelect)}
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
              {plan && (
                /* These narrow the search, not the plan. A plan has no version
                   of its own and picking one here does not give it one. */
                <p className={styles.filterNote}>
                  A plan has no Minecraft version, so this only narrows what you
                  are looking at. Geode works out which loader and version can
                  run the finished list.
                </p>
              )}
            </section>
          )}

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
            {config?.hasCurseforgeKey && (
              <label className={styles.check}>
                <input
                  type="checkbox"
                  checked={providers.includes("curseforge")}
                  onChange={() => toggleSource("curseforge")}
                />
                <span className="badge badge-curseforge">CurseForge</span>
              </label>
            )}
            {kind === "plugin" && (
              <>
                <label className={styles.check}>
                  <input
                    type="checkbox"
                    checked={providers.includes("spiget")}
                    onChange={() => toggleSource("spiget")}
                  />
                  <span className="badge badge-spiget">SpigotMC</span>
                </label>
                {/* Said here rather than discovered on a failed install. */}
                <p className={styles.filterNote}>
                  Lists each plugin&apos;s current file only, with no per-version
                  compatibility. Some are hosted elsewhere or sold — those open
                  their page instead of installing.
                </p>
              </>
            )}
          </section>

          {lockedInstance && (
            <section className={styles.filterSection}>
              <h3 className={styles.filterTitle}>Compatibility</h3>
              {/* The claim changes with the box, because the box changes what
                  was asked. Saying "filtering to what works with Minecraft
                  1.21.4" over a list that was not filtered by version is the
                  screen claiming a check nothing ran. */}
              <p className={styles.compat}>
                {useAnyVersion ? (
                  <>
                    Showing everything for{" "}
                    <strong>{loaderLabel(loader ?? lockedInstance.loader)}</strong>
                    , whatever Minecraft version its author listed.
                  </>
                ) : (
                  <>
                    Filtering to what works with{" "}
                    <strong>Minecraft {lockedInstance.mcVersion}</strong>
                    {loader && (
                      <>
                        {" "}
                        on <strong>{loaderLabel(loader)}</strong>
                      </>
                    )}
                    .
                  </>
                )}
              </p>
              {anyVersionOffered && (
                <>
                  <label className={styles.check}>
                    <input
                      type="checkbox"
                      checked={ignoreGameVersion}
                      onChange={(e) => setIgnoreGameVersion(e.target.checked)}
                    />
                    <span className={styles.connectorLabel}>
                      Ignore the Minecraft version
                    </span>
                  </label>
                  <p className={styles.filterNote}>
                    Also shows mods whose authors have not listed Minecraft{" "}
                    {lockedInstance.mcVersion}. A jar usually runs on a little
                    newer than it says — one that does not can stop the game
                    starting, and nothing here has read it. Anything you install
                    from this list keeps ignoring the version, on its own row in
                    Content, until you turn that off. The loader is still
                    matched: a Fabric mod cannot be made to run on Forge this
                    way.
                  </p>
                </>
              )}
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
              {/* Several can be on at once, so these are toggles rather than a
                  radio group — `aria-pressed` each, and a toolbar rather than
                  a tab set or a radio group, which is the one of the three
                  roles whose keyboard contract does not choose what it lands
                  on. See `moveInCategories`.
                  All of them, no longer the first ten with the rest behind a
                  "more": a cut made at ten of an alphabetical list makes the
                  alphabet into a ranking, which is how Cursed came to be the
                  second thing in this rail and Optimization was not in it at
                  all. Nothing upstream publishes an order worth cutting by —
                  Modrinth's `/tag/category` gives a name and a project type —
                  and the thing that made twenty-one chips expensive was
                  twenty-one tab stops, which is what the toolbar above
                  settles. What is left is height, in the last section of a
                  rail that already scrolls, so what it pushes below the fold
                  is itself. */}
              <div
                className={styles.categories}
                role="toolbar"
                aria-label="Filter by category"
                onKeyDown={moveInCategories}
                // Where the keyboard is, recorded as it arrives, so the chip
                // it is on is the chip Tab comes back to.
                onFocus={(e) => {
                  const chip = e.target.dataset.chip;
                  if (chip) setActiveChip(chip);
                }}
              >
                {/* The tick is the second channel: on and off were an accent
                    tint and nothing else, which is the one difference a screen
                    reader cannot read out and some eyes cannot see. */}
                {available.map((category) => {
                  const on = categories.includes(category);
                  return (
                    <button
                      key={category}
                      className={clsx(styles.categoryChip, on && styles.categoryActive)}
                      aria-pressed={on}
                      onClick={() => toggleCategory(category)}
                      tabIndex={category === chipStop ? 0 : -1}
                      data-chip={category}
                    >
                      {titleCase(category)}
                      {on && <Check size={11} aria-hidden="true" />}
                    </button>
                  );
                })}
              </div>
            </section>
          )}
        </aside>

        <div
          className={styles.results}
          id="browse-results"
          role={plan ? undefined : "tabpanel"}
          aria-labelledby={plan ? undefined : `browse-tab-${kindStop}`}
          // Focusable because it is the thing the strip switches and it scrolls:
          // while it is loading, empty, or showing rows whose first stop is a
          // long way down, Tab out of the strip would otherwise skip the
          // results entirely and the keyboard could not scroll them.
          tabIndex={plan ? undefined : 0}
        >
          {warnings.map((warning) => (
            <div key={warning} className={styles.warning}>
              <AlertTriangle size={14} />
              {warning}
            </div>
          ))}

          {/* Rows on screen and a failure both: the rows are this query's
              earlier pages, so they stand, and only the part that did not
              arrive is reported. With no rows the strip would be saying it
              above nothing, and the branch below says it instead. */}
          {failed && hits.length > 0 && (
            <div className={styles.failed}>
              <AlertTriangle size={14} />
              Could not load more results. {failed}
            </div>
          )}

          {loading ? (
            <div className={styles.list}>
              {Array.from({ length: 7 }, (_, i) => (
                <div key={i} className="skeleton" style={{ height: 94 }} />
              ))}
            </div>
          ) : failed ? (
            /* Not "Nothing found": nothing was looked through. The three
               suggestions that state offers — search differently, widen the
               sources, clear the filters — are all about a search that ran,
               and following any of them would have been wasted on a search
               that did not. */
            <div className={styles.empty}>
              <AlertTriangle size={30} className={styles.emptyIcon} />
              <p>The search failed, so there is nothing here to read.</p>
              <p className={styles.emptyHint}>{failed}</p>
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
              {/* A list whose items each carry controls of their own is a
                  grid, and `listbox` would be the wrong borrowing: its options
                  are things you pick one of, and these are rows you act on
                  without the list ever holding a selection. The role is only
                  half of it — a grid that does not answer its arrows is worse
                  than an unnamed list, because it has promised keys that do
                  nothing — so `moveInResults` is the other half. */}
              <div
                className={styles.list}
                role="grid"
                aria-label={`${kindLabel} found`}
                onKeyDown={moveInResults}
                onFocus={rememberRow}
              >
                {hits.map((project) => (
                  <ProjectCard
                    key={rowId(project)}
                    project={project}
                    rowKey={rowId(project)}
                    tabStop={rowId(project) === stopRow}
                    installed={isInstalled(project)}
                    inInstances={heldInstances(project).map((i) => i.name)}
                    installedInstance={packInstance(project)}
                    busy={installing === project.id}
                    progress={progressFor(project)}
                    directInstall={Boolean(lockedInstance) || Boolean(server)}
                    actionLabel={plan ? "Add" : undefined}
                    onOpen={() => setSelected(project)}
                    onRequestInstall={requestInstall}
                    viaConnector={project.viaConnector}
                    fabricAlso={project.fabricAlso}
                    nativeLoaderLabel={loaderLabel(loader ?? "")}
                    onChooseFabric={(fabric) => chooseFabric(project, fabric)}
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
          // Hosting is a global act, so it is not offered while browsing on
          // behalf of one instance, one plan, or one existing server.
          onRequestServer={
            lockedInstance || plan || server
              ? undefined
              : (project, versionId) => {
                  setSelected(null);
                  setAskPackServer({ project, versionId });
                }
          }
          directInstall={Boolean(lockedInstance)}
          planMode={Boolean(plan)}
          added={isInstalled(selected)}
        />
      )}

      {askPackServer && (
        <PackServerModal
          project={askPackServer.project}
          versionId={askPackServer.versionId}
          onClose={() => setAskPackServer(null)}
          onChoose={(kind) => {
            const { project, versionId } = askPackServer;
            setAskPackServer(null);
            void hostPack(project, kind, versionId);
          }}
        />
      )}

      {askServer && (
        <ServerTargetModal
          project={askServer}
          busy={installing !== null}
          onClose={() => setAskServer(null)}
          onPick={(target) => {
            const project = askServer;
            setAskServer(null);
            setInstalling(project.id);
            void api
              .installServerContent(
                target.id,
                project.provider,
                project.id,
                "plugin",
              )
              .then((report) => {
                toast({
                  kind: "success",
                  title: `Added ${project.name} to ${target.name}`,
                  body:
                    report.skipped.length > 0
                      ? report.skipped.join("; ")
                      : "Restart the server to load it.",
                });
              })
              .catch((e) => reportError(`Could not install ${project.name}`, e))
              .finally(() => setInstalling(null));
          }}
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

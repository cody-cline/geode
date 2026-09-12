export type Loader = "vanilla" | "fabric" | "quilt" | "neoforge" | "forge";

export const LOADERS: { id: Loader; label: string }[] = [
  { id: "vanilla", label: "Vanilla" },
  { id: "fabric", label: "Fabric" },
  { id: "quilt", label: "Quilt" },
  { id: "neoforge", label: "NeoForge" },
  { id: "forge", label: "Forge" },
];

export type Provider = "modrinth" | "curseforge";
export type ProjectKind = "mod" | "modpack" | "resourcepack" | "shader" | "datapack";
export type ContentKind = "mod" | "resourcepack" | "shaderpack" | "datapack";

export interface GameVersion {
  id: string;
  kind: string;
  releaseTime: string;
}

export interface InstanceOverrides {
  memoryMb?: number | null;
  javaArgs?: string | null;
  javaPath?: string | null;
  width?: number | null;
  height?: number | null;
  fullscreen?: boolean | null;
  preLaunchCommand?: string | null;
  postExitCommand?: string | null;
  quickPlayServer?: string | null;
}

export interface PackSource {
  name: string;
  version: string;
  provider?: string | null;
  projectId?: string | null;
  slug?: string | null;
  iconUrl?: string | null;
  bannerUrl?: string | null;
  summary?: string | null;
  pageUrl?: string | null;
  versionId?: string | null;
}

export interface LinkReport {
  identified: number;
  unknown: number;
}

export interface Instance {
  id: string;
  name: string;
  mcVersion: string;
  loader: Loader;
  loaderVersion: string;
  icon?: string | null;
  group?: string | null;
  notes: string;
  createdAt: number;
  lastPlayed?: number | null;
  playSeconds: number;
  overrides: InstanceOverrides;
  pack?: PackSource | null;
  /** Added by the backend view layer. */
  running: boolean;
  modCount: number;
}

export interface ContentItem {
  fileName: string;
  kind: ContentKind;
  name: string;
  version: string;
  enabled: boolean;
  size: number;
  provider?: Provider;
  projectId?: string;
  versionId?: string;
  iconUrl?: string;
  slug?: string;
  dependency: boolean;
  fromDisk: boolean;
  /** File names of installed mods this one needs. */
  requires: string[];
  /** File names of installed mods that need this one. */
  requiredBy: string[];
  /** Arrived as a dependency, and nothing installed still needs it. */
  unused: boolean;
  /** The jar stated its own dependencies, rather than us inferring them. */
  declaresDeps: boolean;
  /** Mod ids this file answers to. Stable where display names are not. */
  modIds: string[];
}

export interface ProviderRef {
  provider: Provider;
  id: string;
  pageUrl: string;
}

// ---------------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------------

/** One mod in a plan. Carries no version — that is the point of a plan. */
export interface PlanMod {
  provider: Provider;
  projectId: string;
  name: string;
  slug?: string | null;
  iconUrl?: string | null;
}

export interface Plan {
  id: string;
  name: string;
  notes: string;
  createdAt: number;
  updatedAt: number;
  mods: PlanMod[];
}

/**
 * How well one mod fits a candidate target.
 *
 * `native` is a file built for exactly this pair. The middle three are
 * compromises, in roughly ascending order of risk. `missing` is a hard
 * conflict: nothing this mod publishes runs here.
 */
export type Fit =
  | "native"
  | "nearVersion"
  | "connector"
  | "connectorNear"
  | "missing";

export interface ModFit {
  provider: Provider;
  projectId: string;
  name: string;
  fit: Fit;
  /** The game version whose build would be used, when not the target's own. */
  viaVersion?: string;
  /** Where this mod can run instead, when it cannot run here. */
  elsewhere?: string[];
}

/** One candidate answer: a loader, a game version, and what it costs. */
export interface PlanTarget {
  loader: string;
  gameVersion: string;
  fits: ModFit[];
  missing: number;
  connector: number;
  near: number;
  native: number;
  cost: number;
}

export interface UnresolvedMod {
  provider: Provider;
  projectId: string;
  name: string;
  reason: string;
}

export interface PlanResolution {
  /** Candidate targets, best first. */
  targets: PlanTarget[];
  unresolved: UnresolvedMod[];
  warnings: string[];
}

export interface BuildReport {
  instanceId: string;
  instanceName: string;
  installed: string[];
  skipped: string[];
}

export interface Project {
  provider: Provider;
  id: string;
  slug: string;
  name: string;
  summary: string;
  iconUrl?: string | null;
  downloads: number;
  follows: number;
  author: string;
  kind: ProjectKind;
  categories: string[];
  loaders: string[];
  gameVersions: string[];
  updated?: string | null;
  pageUrl: string;
  bannerUrl?: string | null;
  distributionAllowed: boolean;
  /**
   * Set client-side, not by the API: install the Fabric build and run it
   * through Sinytra Connector. Always true for a hit only the Fabric search
   * found; on a [`fabricAlso`] project it is the choice the row is showing.
   */
  viaConnector?: boolean;
  /**
   * Both searches turned this up, so it has a build for the target's own
   * loader *and* a Fabric one. The row lets you pick between them.
   */
  fabricAlso?: boolean;
  alsoOn: ProviderRef[];
}

export interface VersionFile {
  url: string;
  filename: string;
  sha1?: string | null;
  size?: number | null;
  primary: boolean;
}

export interface ProjectVersion {
  provider: Provider;
  id: string;
  projectId: string;
  name: string;
  versionNumber: string;
  gameVersions: string[];
  loaders: string[];
  releaseType: string;
  published?: string | null;
  downloads: number;
  files: VersionFile[];
  dependencies: { projectId?: string; versionId?: string; kind: string }[];
  blockedReason?: string | null;
}

export interface SearchResults {
  hits: Project[];
  warnings: string[];
}

export type SortBy = "relevance" | "downloads" | "follows" | "newest" | "updated";

export interface SearchQuery {
  query: string;
  kind: ProjectKind;
  gameVersion?: string | null;
  loader?: string | null;
  categories: string[];
  sort: SortBy;
  offset: number;
  limit: number;
  providers?: Provider[] | null;
}

export interface Account {
  id: string;
  username: string;
  kind: "microsoft" | "offline" | "imported";
  active: boolean;
  needsReauth: boolean;
  /** Which launcher an imported session came from. */
  sourceLabel: string | null;
}

/** A live session found in another launcher. Carries no token. */
export interface ImportCandidate {
  username: string;
  uuid: string;
  sourceLabel: string;
  sourcePath: string;
}

export interface ImportScan {
  candidates: ImportCandidate[];
  /** Why a launcher we found could not be imported from. */
  notes: string[];
}

export interface DeviceCodeStart {
  deviceCode: string;
  userCode: string;
  verificationUri: string;
  expiresIn: number;
  interval: number;
  message: string;
}

export interface Config {
  memoryMb: number;
  javaArgs: string;
  javaPath: string | null;
  clientId: string | null;
  theme: string;
  accent: string;
  hideOnLaunch: boolean;
  concurrentDownloads: number;
  showSnapshots: boolean;
  hasCurseforgeKey: boolean;
  /** True when the key came from GEODE_CURSEFORGE_KEY at build time. */
  curseforgeKeyFromBuild: boolean;
}

export interface ConnectorSupport {
  supported: boolean;
  /** Game versions Connector covers for this loader, for explaining a no. */
  versionsForLoader: string[];
  /** Slugs installed alongside a Fabric mod. */
  stack: string[];
}

export interface KeyCheck {
  ok: boolean;
  status: number;
  message: string;
  /** Which key was tested — typed, saved in Settings, or compiled in. */
  source: string;
  /** Safe shape summary (prefix + length); never the key itself. */
  fingerprint: string;
}

export interface JavaInstall {
  path: string;
  major: number;
  version: string;
  vendor: string;
  managed: boolean;
}

export interface ProgressEvent {
  id: string;
  stage: string;
  detail: string;
  current: number;
  total: number;
  done: boolean;
  error: string | null;
}

export interface LogEntry {
  instanceId: string;
  level: string;
  message: string;
  logger?: string;
  thread?: string;
}

export interface ExitEvent {
  instanceId: string;
  code: number | null;
  success: boolean;
  playedSeconds: number;
}

export interface UpdateCandidate {
  fileName: string;
  name: string;
  provider: Provider;
  projectId: string;
  currentVersion: string;
  newVersionId: string;
  newVersionNumber: string;
  kind: ContentKind;
}

export interface InstallReport {
  installed: string[];
  skipped: string[];
}

export interface PackPreview {
  name: string;
  version: string;
  summary: string | null;
  mcVersion: string;
  loader: Loader;
  loaderVersion: string;
  fileCount: number;
  format: string;
  needsCurseforgeKey: boolean;
}

export interface AppInfo {
  version: string;
  dataDir: string;
  platform: string;
  hasClientId: boolean;
}

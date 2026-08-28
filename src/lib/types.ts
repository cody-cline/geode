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
}

export interface ProviderRef {
  provider: Provider;
  id: string;
  pageUrl: string;
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
  distributionAllowed: boolean;
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

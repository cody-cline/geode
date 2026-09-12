export type Loader = "vanilla" | "fabric" | "quilt" | "neoforge" | "forge";

export const LOADERS: { id: Loader; label: string }[] = [
  { id: "vanilla", label: "Vanilla" },
  { id: "fabric", label: "Fabric" },
  { id: "quilt", label: "Quilt" },
  { id: "neoforge", label: "NeoForge" },
  { id: "forge", label: "Forge" },
];

/** `spiget` is SpigotMC, read through the Spiget index. Plugins only. */
export type Provider = "modrinth" | "curseforge" | "spiget";
export type ProjectKind =
  | "mod"
  | "modpack"
  | "resourcepack"
  | "shader"
  | "datapack"
  | "plugin";
/** `plugin` only ever applies to a server — instances have no plugins folder. */
export type ContentKind =
  | "mod"
  | "resourcepack"
  | "shaderpack"
  | "datapack"
  | "plugin";

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
  /** File name of an uploaded image, inside the instance's media folder. */
  icon?: string | null;
  banner?: string | null;
  /**
   * Absolute paths to those images, resolved by the backend — `null` when none
   * is set or the file has gone. Pass through `convertFileSrc` to display.
   */
  iconPath: string | null;
  bannerPath: string | null;
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
// Servers
// ---------------------------------------------------------------------------

/** Provenance for one installed plugin or mod. */
export interface ContentRecord {
  fileName: string;
  kind: ContentKind;
  provider: string;
  projectId: string;
  versionId: string;
  name: string;
  versionNumber: string;
  slug?: string | null;
  iconUrl?: string | null;
  dependency: boolean;
  requires?: string[] | null;
}

export interface ServerSettings {
  memoryMb: number;
  javaPath: string | null;
  javaArgs: string | null;
  autoRestart: boolean;
  /** How many rotated logs to keep. `null` keeps everything. */
  keepLogs: number | null;
  /** How many backups to keep. `null` keeps every one ever taken. */
  keepBackups: number | null;
}

/**
 * What a schedule does when it fires. Adjacently tagged, matching the backend:
 * `{ kind: "backup" }` or `{ kind: "command", value: "say hello" }`.
 */
export type ScheduleAction =
  | { kind: "backup" }
  | { kind: "command"; value: string };

export interface Schedule {
  id: string;
  action: ScheduleAction;
  /** Zero means never, so a half-filled form stays harmless. */
  everyMinutes: number;
  enabled: boolean;
  lastRun: number | null;
}

/** One of a server's logs — the live one, or a rotated archive. */
export interface LogFile {
  name: string;
  size: number;
  modified: number;
  /** The log being written right now, which is never pruned or deleted. */
  current: boolean;
}

/** One backup of a server's files. Its name is also its timestamp. */
export interface BackupInfo {
  name: string;
  size: number;
  createdAt: number;
}

export interface Server {
  id: string;
  name: string;
  /** File name of an uploaded image, inside the server's media folder. */
  icon?: string | null;
  banner?: string | null;
  /** Absolute paths to those images, resolved by the backend. */
  iconPath: string | null;
  bannerPath: string | null;
  /**
   * The modpack this server was built from, if it was. The same record an
   * instance keeps, so a server made from a pack can show its artwork and link
   * back to its page.
   */
  pack?: PackSource | null;
  /** `paper`, `fabric`, `mohist`, … */
  kind: string;
  mcVersion: string;
  build: string;
  jar: string;
  javaMajor: number | null;
  createdAt: number;
  lastStarted: number | null;
  eulaAccepted: boolean;
  settings: ServerSettings;
  content: ContentRecord[];
  /** Backups and commands this server runs on a timer. */
  schedules: Schedule[];
  notes: string;
  /** Added by the backend view layer. */
  running: boolean;
  /**
   * Capabilities of this server rather than of its kind — a Fabric server with
   * Cardboard installed takes Bukkit plugins, which `ServerKindInfo` cannot say.
   */
  takesPlugins: boolean;
  takesMods: boolean;
  canAddCardboard: boolean;
  /**
   * Whether the software is in place and the server can start. Not the same as
   * having a jar: a modern Forge server has none and launches from an argument
   * file instead.
   */
  installed: boolean;
  /** The software's proper name — "NeoForge" rather than the id's "neoforge". */
  kindName: string;
  /**
   * Whether it hosts a world. A proxy does not, so it has nowhere to put
   * datapacks.
   */
  hasWorld: boolean;
}

/**
 * Whether Git is on this machine, and what Geode would do about it.
 *
 * Only Spigot needs it: it is compiled from source rather than downloaded.
 */
export interface GitStatus {
  present: boolean;
  version?: string | null;
  /** True when this is Geode's own copy rather than the machine's. */
  portable: boolean;
  /** One sentence describing what confirming would do. */
  plan: string;
  /** The exact command Geode would run, shown before it runs. */
  command?: string | null;
  /** Whether Geode can finish the job itself once confirmed. */
  automatic: boolean;
  /** What to do by hand, always offered as a way out. */
  manual: string;
}

/** One choice in the "what kind of server" picker. */
export interface ServerKindInfo {
  id: string;
  name: string;
  takesPlugins: boolean;
  takesMods: boolean;
  isProxy: boolean;
}

/** One line of a server's own `server.properties`. */
export interface ServerProperty {
  key: string;
  value: string;
}

export interface ServerLine {
  serverId: string;
  line: string;
  /** `err` is the JVM talking, not the game. */
  stream: "out" | "err";
}

export interface ServerExit {
  serverId: string;
  code: number | null;
  success: boolean;
  ranSeconds: number;
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

/**
 * The same, for a plan built as a server. `skipped` carries the client-only
 * mods that were deliberately left out, not only things that failed.
 */
export interface ServerBuildReport {
  serverId: string;
  serverName: string;
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
   * `required`, `optional` or `unsupported`. Only Modrinth publishes these, so
   * elsewhere they are null and nothing should be concluded from that.
   */
  clientSide?: string | null;
  serverSide?: string | null;
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

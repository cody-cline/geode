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
/**
 * `plugin` only ever applies to a server — instances have no plugins folder.
 * Neither does `clientmod`: a server's Client mods, which it never loads and
 * its players get as ordinary mods.
 */
export type ContentKind =
  | "mod"
  | "resourcepack"
  | "shaderpack"
  | "datapack"
  | "plugin"
  | "clientmod";

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
  /** The server this was joined from, when it was. */
  joined?: JoinLink | null;
  /** A joined instance keeps its server's server-side mods turned off. */
  serverSideModsOff?: boolean;
  /** The server revision this joined instance last installed completely. */
  joinedRevision?: string | null;
  /** The joined server's id on the admin's machine, kept apart from its password. */
  joinedServerId?: string | null;
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
  /**
   * Who put this here. A server's own mods can be turned off but not deleted —
   * sharing runs one way, so removing one would only leave the next sync to
   * bring it back. Absent means it is yours.
   */
  origin?: "user" | "server";
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
  /**
   * Which side a mod or client mod belongs on: its folder, then what the jar
   * declares, then the listing. A file from a server's pack carries the
   * server's answer, or none.
   */
  side?: "client" | "server" | "both";
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
  /**
   * Whether the command probe rides along on this server's JVM.
   *
   * On by default. Off means the console has no idea what commands a mod or
   * plugin registered, which is the price of Geode putting nothing of its own
   * inside the server process.
   */
  readCommands: boolean;
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
  /**
   * The loader its mods are built for — `forge` for a Mohist server, not
   * `mohist`. `null` when it takes no mods at all.
   */
  modLoader?: string | null;
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

/**
 * One command a server has, and what may follow it.
 *
 * Read from the server itself rather than guessed at, so a mod's commands and
 * their subcommands appear exactly as vanilla's do.
 */
export interface CommandNode {
  name: string;
  /** Fixed words that may come next. */
  children: CommandNode[];
  /** Argument names that may come next — shown, never completed. */
  args: string[];
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
 * The same, for a plan built as a server. Client-only mods are installed into
 * the server's Client mods, so `skipped` is only what failed.
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
  sha512?: string | null;
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
  /** How players reach this machine for shared servers' packs. */
  share: ShareSettings;
}

// ---------------------------------------------------------------------------
// Sharing a server with players
// ---------------------------------------------------------------------------

export interface ShareSettings {
  /** The one port every shared server's pack is served on. */
  port: number;
  /** `auto` opens the port on the router over UPnP; `manual` leaves it alone. */
  mapping: "auto" | "manual";
  /** Used in links instead of the detected address: a domain, a tunnel's host:port, or an https:// URL. */
  publicAddress: string | null;
}

export type PortMappingStatus =
  | { state: "off" }
  | { state: "searching" }
  | {
      state: "mapped";
      externalIp: string;
      port: number;
      /** `public` should work; the others mean players cannot reach this machine despite the mapping. */
      reachability: "public" | "cgnat" | "doubleNat" | "noIpv4";
    }
  | {
      state: "failed";
      /** `permanentOnly`: the router only grants forwards that never expire, which Geode will not create because a crash would leave the port open for good. */
      reason: "noGateway" | "refused" | "portInUse" | "permanentOnly" | "other";
      detail?: string;
    };

/** A listener that could not start, being tried again on its own. */
export interface ShareRetry {
  /** Attempts that have failed so far, the first one included. */
  attempts: number;
  /** Roughly how long until the next attempt. */
  nextInSeconds: number;
  /** True once it has been failing for about a minute, which is when the admin is told. */
  needsYou: boolean;
}

export interface ShareStatus {
  /** Which router setup the admin chose. Not the same as `mapping.state`: a mapper that never started because nothing listens is `off` in either mode. */
  mode: "auto" | "manual";
  /** With manual router setup, the public address the router reported when asked (nothing is changed on the router), or null if it could not be learned. */
  detected: { ip: string; reachability: "public" | "cgnat" | "doubleNat" | "noIpv4" } | null;
  /** With manual router setup, true while the router is still being asked for the first time, so a null `detected` means nothing yet. */
  asking: boolean;
  /** True while the pack server is listening, which is while any server is shared. */
  listening: boolean;
  port: number;
  /** Why it could not start listening, e.g. the port is already taken. */
  listenError: string | null;
  /** While something is shared and nothing listens: how trying again is going. Null when nothing is being tried. */
  retry: ShareRetry | null;
  mapping: PortMappingStatus;
  /** The address links use: the public address if one is set, else the router's external IP, else null. */
  address: string | null;
  /** The certificate fingerprint players' Geode pins, as hex. */
  fingerprint: string;
}

/**
 * The pack server going down and coming back, sent once each rather than once
 * an attempt: the Share tab's own line follows `geode://share-status`.
 */
export interface ShareListenNotice {
  /** True on the recovery; false when retrying has gone on long enough to need the admin. */
  listening: boolean;
  port: number;
  /** Attempts that had failed when this was sent. */
  attempts: number;
  /** Why it could not bind; null on the recovery. */
  error: string | null;
}

/**
 * A server whose pack would not build for a player. The player is told only to
 * ask their admin, so this is the only way the admin hears of it.
 */
export interface SharePackFailed {
  serverId: string;
  serverName: string;
  error: string;
}

export interface ServerShareView {
  /** The server's password, `GEODE-XXXXX-XXXXX`, for reading out or typing. */
  code: string;
  /** The `geode://join` link, or null while no address is known to put in it. */
  link: string | null;
  includeConfig: boolean;
  /** False while sharing is stopped; the password is kept. */
  shared: boolean;
  /** Why this server cannot be shared, when it cannot (a proxy, an unsupported loader). */
  unsupported: string | null;
}

/** What a server says it is sharing, shown before a player imports. */
export interface PackInfo {
  name: string;
  mcVersion: string;
  loader: string;
  loaderVersion: string;
  mods: number;
  /** Of `mods`, how many only the server needs. */
  serverMods: number;
  serverKind: string;
  /** Changes exactly when what a player receives changes; empty from an older Geode. */
  revision: string;
  /** The server's id on the admin's machine; empty from an older Geode. */
  serverId: string;
}

/** A parsed join link. Opaque to the UI: take it from a preview and pass it back to `joinServer`. */
export interface JoinLink {
  address: { kind: "direct"; host: string; port: number } | { kind: "https"; url: string };
  code: string;
  pin?: string;
}

export type JoinInput = { link: string } | { address: string; code: string };

export interface ExistingInstance {
  id: string;
  name: string;
  passwordChanged: boolean;
  certificateChanged: boolean;
}

export interface JoinPreview {
  info: PackInfo;
  address: string;
  /** `pinned`: matched the link. `domain`: checked by its certificate authority. `firstUse`: typed by hand, trusted now and pinned from here on. */
  verified: "pinned" | "domain" | "firstUse";
  link: JoinLink;
  /** An instance already joined from this server: offer to open it rather than make a second. `passwordChanged` means it was recognised by the server rather than by its password, so opening it saves the new one; `certificateChanged` means this link is for a different certificate than the instance holds. */
  existing: ExistingInstance | null;
}

/** A join that got as far as creating the instance. Anything that went wrong after that is a problem to show, not a failure to retry — retrying would make a second instance. */
export interface JoinOutcome {
  instance: Instance;
  problems: string[];
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
  /** The folder it went into, which on a server may be Client mods rather than the one asked for. */
  kind: ContentKind;
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

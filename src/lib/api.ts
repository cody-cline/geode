import { invoke } from "@tauri-apps/api/core";
import type {
  Account,
  AppInfo,
  BackupInfo,
  BuildReport,
  CommandNode,
  Config,
  ConnectorSupport,
  ContentItem,
  ContentKind,
  DeviceCodeStart,
  Disconnected,
  DriveShare,
  DriveStatus,
  ExportReport,
  GameVersion,
  GitStatus,
  Forbidden,
  ImportScan,
  InstallReport,
  Instance,
  InstanceOverrides,
  JavaInstall,
  JoinInput,
  JoinLink,
  JoinOutcome,
  JoinPreview,
  KeyCheck,
  LinkReport,
  LogFile,
  PackArrival,
  PackImport,
  PackInfo,
  PackLinkView,
  PackPreview,
  Plan,
  PlanMod,
  PlanResolution,
  Project,
  ProjectKind,
  ProjectVersion,
  Provider,
  Schedule,
  SearchQuery,
  SearchResults,
  Server,
  ServerBuildReport,
  ServerKindInfo,
  ServerProperty,
  ServerSettings,
  ServerShareView,
  ShareSettings,
  ShareStatus,
  StagedPack,
  UpdateCandidate,
} from "./types";

/** Backend errors arrive as plain strings; normalise them for the toast layer. */
export function errorMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

export const api = {
  // Config
  getConfig: () => invoke<Config>("get_config"),
  setConfig: (patch: Partial<Record<string, unknown>>) =>
    invoke<Config>("set_config", { patch }),
  setCurseforgeKey: (key: string | null) =>
    invoke<boolean>("set_curseforge_key", { key }),
  testCurseforgeKey: (key?: string) =>
    invoke<KeyCheck>("test_curseforge_key", { key: key ?? null }),
  appInfo: () => invoke<AppInfo>("app_info"),
  systemMemoryMb: () => invoke<number>("system_memory_mb"),

  // Versions
  listGameVersions: (includeSnapshots: boolean) =>
    invoke<GameVersion[]>("list_game_versions", { includeSnapshots }),
  listLoaderVersions: (loader: string, gameVersion: string) =>
    invoke<string[]>("list_loader_versions", { loader, gameVersion }),

  // Instances
  listInstances: () => invoke<Instance[]>("list_instances"),
  getInstance: (id: string) => invoke<Instance>("get_instance", { id }),
  createInstance: (
    name: string,
    gameVersion: string,
    loader: string,
    loaderVersion: string,
  ) =>
    invoke<Instance>("create_instance", {
      name,
      gameVersion,
      loader,
      loaderVersion,
    }),
  updateInstance: (
    id: string,
    patch: {
      name?: string;
      group?: string | null;
      notes?: string;
      gameVersion?: string;
      loader?: string;
      loaderVersion?: string;
      overrides?: InstanceOverrides;
    },
  ) => invoke<Instance>("update_instance", { id, patch }),
  deleteInstance: (id: string) => invoke<void>("delete_instance", { id }),
  duplicateInstance: (id: string, name: string) =>
    invoke<Instance>("duplicate_instance", { id, name }),
  instanceFolder: (id: string) => invoke<string>("instance_folder", { id }),
  /**
   * Set or clear an instance's icon or banner. `source` is a path on disk; the
   * backend copies it into the instance's own media folder. `null` clears it.
   */
  setInstanceImage: (
    id: string,
    slot: "icon" | "banner",
    source: string | null,
  ) => invoke<Instance>("set_instance_image", { id, slot, source }),
  setServerImage: (
    id: string,
    slot: "icon" | "banner",
    source: string | null,
  ) => invoke<Server>("set_server_image", { id, slot, source }),

  // Content
  listContent: (id: string, kind: ContentKind) =>
    invoke<ContentItem[]>("list_content", { id, kind }),
  setContentEnabled: (
    id: string,
    kind: ContentKind,
    file: string,
    enabled: boolean,
  ) => invoke<string>("set_content_enabled", { id, kind, file, enabled }),
  /** Turn a joined instance's server-side mods on or off; resolves to how many changed. */
  setServerSideMods: (id: string, on: boolean) =>
    invoke<number>("set_server_side_mods", { id, on }),
  removeContent: (id: string, kind: ContentKind, file: string) =>
    invoke<void>("remove_content", { id, kind, file }),
  addLocalContent: (id: string, kind: ContentKind, files: string[]) =>
    invoke<number>("add_local_content", { id, kind, files }),
  installContent: (
    id: string,
    provider: Provider,
    projectId: string,
    versionId?: string,
    viaConnector?: boolean,
  ) =>
    invoke<InstallReport>("install_content", {
      id,
      provider,
      projectId,
      versionId: versionId ?? null,
      viaConnector: viaConnector ?? null,
    }),
  connectorSupport: (gameVersion: string, loader: string) =>
    invoke<ConnectorSupport>("connector_support", { gameVersion, loader }),
  installModpack: (
    provider: Provider,
    projectId: string,
    versionId?: string,
    name?: string,
  ) =>
    invoke<Instance>("install_modpack", {
      provider,
      projectId,
      versionId: versionId ?? null,
      name: name ?? null,
    }),
  linkUntrackedContent: (id: string) =>
    invoke<LinkReport>("link_untracked_content", { id }),
  checkContentUpdates: (id: string) =>
    invoke<UpdateCandidate[]>("check_content_updates", { id }),
  applyContentUpdate: (
    id: string,
    provider: Provider,
    projectId: string,
    versionId: string,
  ) =>
    invoke<InstallReport>("apply_content_update", {
      id,
      provider,
      projectId,
      versionId,
    }),

  // Browse
  searchProjects: (query: SearchQuery) =>
    invoke<SearchResults>("search_projects", { query }),
  getProject: (provider: Provider, id: string) =>
    invoke<Project>("get_project", { provider, id }),
  getProjectDescription: (provider: Provider, id: string) =>
    invoke<string>("get_project_description", { provider, id }),
  getProjectVersions: (
    provider: Provider,
    id: string,
    gameVersion?: string | null,
    loader?: string | null,
  ) =>
    invoke<ProjectVersion[]>("get_project_versions", {
      provider,
      id,
      gameVersion: gameVersion ?? null,
      loader: loader ?? null,
    }),
  getCategories: (kind: ProjectKind) =>
    invoke<string[]>("get_categories", { kind }),

  // Plans
  listPlans: () => invoke<Plan[]>("list_plans"),
  getPlan: (id: string) => invoke<Plan>("get_plan", { id }),
  createPlan: (name: string) => invoke<Plan>("create_plan", { name }),
  savePlan: (plan: Plan) => invoke<Plan>("save_plan", { plan }),
  deletePlan: (id: string) => invoke<void>("delete_plan", { id }),
  resolvePlan: (id: string) => invoke<PlanResolution>("resolve_plan", { id }),
  buildPlan: (
    id: string,
    loader: string,
    gameVersion: string,
    name?: string,
  ) =>
    invoke<BuildReport>("build_plan", {
      id,
      loader,
      gameVersion,
      name: name ?? null,
    }),
  /**
   * Build a plan as a server instead of an instance. Refused, with the reason,
   * for targets that cannot be one — the rule lives in the backend so it
   * cannot drift.
   */
  createServerFromPlan: (
    id: string,
    loader: string,
    gameVersion: string,
    name?: string,
  ) =>
    invoke<ServerBuildReport>("create_server_from_plan", {
      id,
      loader,
      gameVersion,
      name: name ?? null,
    }),
  /**
   * Which server software could run this modpack. Answered from the published
   * version's loader rather than by downloading it, so the choice can be
   * offered before committing to the download.
   */
  serverKindsForPack: (
    provider: Provider,
    projectId: string,
    versionId?: string,
  ) =>
    invoke<ServerKindInfo[]>("server_kinds_for_pack", {
      provider,
      projectId,
      versionId: versionId ?? null,
    }),
  /** Install a modpack as a server rather than as an instance. */
  createServerFromModpack: (
    provider: Provider,
    projectId: string,
    kind: string,
    versionId?: string,
    name?: string,
  ) =>
    invoke<ServerBuildReport>("create_server_from_modpack", {
      provider,
      projectId,
      versionId: versionId ?? null,
      kind,
      name: name ?? null,
    }),
  packContents: (
    provider: Provider,
    projectId: string,
    versionId?: string,
  ) =>
    invoke<PlanMod[]>("pack_contents", {
      provider,
      projectId,
      versionId: versionId ?? null,
    }),

  // Servers
  serverKinds: () => invoke<ServerKindInfo[]>("server_kinds"),
  /**
   * Every command this server has, as the server itself reported them.
   *
   * Covers vanilla, mods and plugins alike. Empty until the server has run
   * once, since nothing outside a running server knows a mod's commands.
   */
  serverCommands: (id: string) => invoke<CommandNode[]>("server_commands", { id }),
  // Log files. An instance's and a server's are the same files in different
  // folders, so the two sets of bindings are deliberately identical in shape.
  listInstanceLogs: (id: string) => invoke<LogFile[]>("list_instance_logs", { id }),
  readInstanceLog: (id: string, name: string) =>
    invoke<string>("read_instance_log", { id, name }),
  deleteInstanceLog: (id: string, name: string) =>
    invoke<void>("delete_instance_log", { id, name }),
  /**
   * A pasteable report: what is installed, the crash report if there is one,
   * and the log — with a prompt on top asking for the cause rather than a
   * guess. `live` is the console's own text, used only when no log file has
   * anything in it.
   */
  instanceReport: (id: string, logName?: string, live?: string) =>
    invoke<string>("instance_report", {
      id,
      logName: logName ?? null,
      live: live ?? null,
    }),
  serverReport: (id: string, logName?: string, live?: string) =>
    invoke<string>("server_report", {
      id,
      logName: logName ?? null,
      live: live ?? null,
    }),
  /** Whether compiling Spigot is possible here, and what would make it so. */
  gitStatus: () => invoke<GitStatus>("git_status"),
  /** Only ever after the user has read `plan` and said yes. */
  installGit: () => invoke<GitStatus>("install_git"),
  serverVersions: (kind: string) =>
    invoke<string[]>("server_versions", { kind }),
  listServers: () => invoke<Server[]>("list_servers"),
  getServer: (id: string) => invoke<Server>("get_server", { id }),
  createServer: (name: string, kind: string, version: string) =>
    invoke<Server>("create_server", { name, kind, version }),
  deleteServer: (id: string) => invoke<void>("delete_server", { id }),
  updateServer: (
    id: string,
    patch: {
      name?: string;
      notes?: string;
      settings?: ServerSettings;
      schedules?: Schedule[];
    },
  ) => invoke<Server>("update_server", { id, patch }),
  acceptServerEula: (id: string) =>
    invoke<Server>("accept_server_eula", { id }),
  /** `moved` names the mods moved to Client mods before the server launched. */
  startServer: (id: string) =>
    invoke<{ pid: number; moved: string[] }>("start_server", { id }),
  stopServer: (id: string) => invoke<boolean>("stop_server", { id }),
  killServer: (id: string) => invoke<boolean>("kill_server", { id }),
  sendServerCommand: (id: string, command: string) =>
    invoke<boolean>("send_server_command", { id, command }),
  runningServers: () => invoke<string[]>("running_servers"),
  serverFolder: (id: string) => invoke<string>("server_folder", { id }),
  listServerContent: (id: string, kind: ContentKind) =>
    invoke<ContentItem[]>("list_server_content", { id, kind }),
  installServerContent: (
    id: string,
    provider: Provider,
    projectId: string,
    kind: ContentKind,
    versionId?: string,
  ) =>
    invoke<InstallReport>("install_server_content", {
      id,
      provider,
      projectId,
      versionId: versionId ?? null,
      kind,
    }),
  removeServerContent: (id: string, kind: ContentKind, file: string) =>
    invoke<void>("remove_server_content", { id, kind, file }),
  setServerContentEnabled: (
    id: string,
    kind: ContentKind,
    file: string,
    enabled: boolean,
  ) =>
    invoke<string>("set_server_content_enabled", { id, kind, file, enabled }),
  /** Move a mod between a server's Mods and Client mods. Refused while it runs. */
  moveServerMod: (id: string, file: string, toClient: boolean) =>
    invoke<void>("move_server_mod", { id, file, toClient }),
  /**
   * Install Cardboard, so a Fabric server can load Bukkit plugins. Refused,
   * with the reason, where it would do nothing.
   */
  addCardboard: (id: string) => invoke<Server>("add_cardboard", { id }),
  addLocalServerContent: (id: string, kind: ContentKind, files: string[]) =>
    invoke<number>("add_local_server_content", { id, kind, files }),
  checkServerContentUpdates: (id: string) =>
    invoke<UpdateCandidate[]>("check_server_content_updates", { id }),
  linkUntrackedServerContent: (id: string) =>
    invoke<LinkReport>("link_untracked_server_content", { id }),
  applyServerContentUpdate: (
    id: string,
    provider: Provider,
    projectId: string,
    versionId: string,
    kind: ContentKind,
  ) =>
    invoke<InstallReport>("apply_server_content_update", {
      id,
      provider,
      projectId,
      versionId,
      kind,
    }),

  listServerLogs: (id: string) =>
    invoke<LogFile[]>("list_server_logs", { id }),
  readServerLog: (id: string, name: string) =>
    invoke<string>("read_server_log", { id, name }),
  deleteServerLog: (id: string, name: string) =>
    invoke<void>("delete_server_log", { id, name }),

  listServerBackups: (id: string) =>
    invoke<BackupInfo[]>("list_server_backups", { id }),
  createServerBackup: (id: string) =>
    invoke<BackupInfo>("create_server_backup", { id }),
  deleteServerBackup: (id: string, name: string) =>
    invoke<void>("delete_server_backup", { id, name }),
  restoreServerBackup: (id: string, name: string) =>
    invoke<void>("restore_server_backup", { id, name }),

  readServerProperties: (id: string) =>
    invoke<ServerProperty[]>("read_server_properties", { id }),
  /** Only the settings passed are changed; the rest of the file is left alone. */
  writeServerProperties: (id: string, properties: ServerProperty[]) =>
    invoke<void>("write_server_properties", { id, properties }),

  // Accounts
  listAccounts: () => invoke<Account[]>("list_accounts"),
  beginMicrosoftLogin: () => invoke<DeviceCodeStart>("begin_microsoft_login"),
  completeMicrosoftLogin: (start: DeviceCodeStart) =>
    invoke<Account>("complete_microsoft_login", { start }),
  findImportableAccounts: () => invoke<ImportScan>("find_importable_accounts"),
  importAccount: (uuid: string) => invoke<Account>("import_account", { uuid }),
  addOfflineAccount: (username: string) =>
    invoke<Account>("add_offline_account", { username }),
  removeAccount: (id: string) => invoke<Account[]>("remove_account", { id }),
  setActiveAccount: (id: string) =>
    invoke<Account[]>("set_active_account", { id }),

  // Java
  detectJava: () => invoke<JavaInstall[]>("detect_java"),
  installJava: (major: number) =>
    invoke<JavaInstall>("install_java", { major }),

  // Launch
  /** `notices` are for after the game is on its way: what an update from the server changed, or why it could not be checked for. */
  launchInstance: (id: string, accountId?: string) =>
    invoke<{ pid: number; instanceId: string; notices: string[] }>("launch_instance", {
      id,
      accountId: accountId ?? null,
    }),
  prepareInstance: (id: string) => invoke<void>("prepare_instance", { id }),
  stopInstance: (id: string) => invoke<boolean>("stop_instance", { id }),
  runningInstances: () => invoke<string[]>("running_instances"),
  cancelTask: (id: string) => invoke<boolean>("cancel_task", { id }),

  // Modpacks
  previewModpack: (path: string) =>
    invoke<PackPreview>("preview_modpack", { path }),
  /** Makes the instance and reports what it could not fetch beside it. Progress id `import:<instance id>`. */
  importModpack: (path: string, name?: string) =>
    invoke<PackImport>("import_modpack", { path, name: name ?? null }),
  /** All four return what the export did with the instance's content, so the UI can say it. */
  exportModpack: (id: string, dest: string, includeSaves: boolean) =>
    invoke<ExportReport>("export_modpack", { id, dest, includeSaves }),
  exportGeodepack: (id: string, dest: string, includeSaves: boolean) =>
    invoke<ExportReport>("export_geodepack", { id, dest, includeSaves }),
  /**
   * `author` is required by the CurseForge format and an instance has nowhere
   * to keep it, so the export flow asks. Null leaves it to the backend, which
   * uses the signed-in account's name.
   */
  exportCurseforgePack: (
    id: string,
    dest: string,
    author: string | null,
    includeSaves: boolean,
  ) => invoke<ExportReport>("export_curseforge_pack", { id, dest, author, includeSaves }),
  /**
   * A zip Prism opens as an instance of its own. Every jar travels inside it —
   * the format has no way to say "download this" — so `referenced` counts the
   * mods the recipient's Prism will know the name and update path of, not what
   * the zip left out.
   */
  exportPrismInstance: (id: string, dest: string, includeSaves: boolean) =>
    invoke<ExportReport>("export_prism_instance", { id, dest, includeSaves }),

  // Google Drive. Connecting opens the user's browser and blocks until they
  // come back, the way completeMicrosoftLogin does; cancel it with
  // cancelTask("drive:connect").
  driveStatus: () => invoke<DriveStatus>("drive_status"),
  connectDrive: () => invoke<DriveStatus>("connect_drive"),
  /** Forgets the connection whatever Google answers, and says whether Google accepted the revoke. */
  disconnectDrive: () => invoke<Disconnected>("disconnect_drive"),
  /** Separate from setConfig so a secret is never part of a routine settings round-trip. */
  setGoogleClientSecret: (secret: string | null) =>
    invoke<boolean>("set_google_client_secret", { secret }),
  /** Export the pack and report what travelled, before anything is published. Progress id `export:<id>`. */
  stageDrivePack: (id: string, includeSaves: boolean) =>
    invoke<StagedPack>("stage_drive_pack", { id, includeSaves }),
  /** Upload a staged pack and make it readable by anyone with the link. Progress id `drive-upload:<stage>`. */
  uploadDrivePack: (stage: string, name: string) =>
    invoke<DriveShare>("upload_drive_pack", { stage, name }),
  /** The strict parse, in Rust, before anything is fetched. */
  previewPackLink: (link: string) => invoke<PackLinkView>("preview_pack_link", { link }),
  /** Download the pack a link names, verify its hash, and read what is in it. Progress id `pack-link` — fixed, because there is one dialog at a time and the webview has to be able to cancel an id it never saw. */
  fetchPackLink: (link: string) => invoke<PackArrival>("fetch_pack_link", { link }),
  importPackStage: (stage: string, name?: string) =>
    invoke<PackImport>("import_pack_stage", { stage, name: name ?? null }),

  // Sharing a server with players. Status changes also arrive as the
  // `geode://share-status` event with a ShareStatus payload.
  getShareStatus: () => invoke<ShareStatus>("get_share_status"),
  setShareSettings: (settings: ShareSettings) =>
    invoke<ShareStatus>("set_share_settings", { settings }),
  getServerShare: (id: string) =>
    invoke<ServerShareView | null>("get_server_share", { id }),
  shareServer: (id: string) => invoke<ServerShareView>("share_server", { id }),
  unshareServer: (id: string) => invoke<void>("unshare_server", { id }),
  setServerShareConfig: (id: string, includeConfig: boolean) =>
    invoke<ServerShareView>("set_server_share_config", { id, includeConfig }),
  /** Call this when the admin takes the link, so Geode can tell them later if the address it points at has changed. */
  shareLinkCopied: (id: string) =>
    invoke<ServerShareView>("share_link_copied", { id }),
  /** How this server appears in players' multiplayer lists. An empty name means the server's own. */
  setServerListing: (id: string, name: string, address: string) =>
    invoke<ServerShareView>("set_server_listing", { id, name, address }),
  /** The mods this server does not allow: the whole list, replacing what was there. Client-side only — a rule, not a wall. */
  setServerForbidden: (id: string, forbidden: Forbidden[]) =>
    invoke<ServerShareView>("set_server_forbidden", { id, forbidden }),
  rotateServerShareCode: (id: string) =>
    invoke<ServerShareView>("rotate_server_share_code", { id }),

  // Joining a shared server. A geode:// link opened while Geode runs arrives
  // as the `geode://join-link` event (payload: the link string); one that
  // started Geode is waiting in takePendingJoin.
  previewJoin: (input: JoinInput) => invoke<JoinPreview>("preview_join", { input }),
  /** `info` is what the player approved; the import is refused if the downloaded pack describes something else. */
  joinServer: (link: JoinLink, info: PackInfo, serverSideMods: boolean, name?: string) =>
    invoke<JoinOutcome>("join_server", { link, info, serverSideMods, name: name ?? null }),
  /** Save a link with a replaced password onto the instance already joined from that server. */
  relinkJoinedInstance: (id: string, link: JoinLink) =>
    invoke<Instance>("relink_joined_instance", { id, link }),
  takePendingJoin: () => invoke<string | null>("take_pending_join"),
};

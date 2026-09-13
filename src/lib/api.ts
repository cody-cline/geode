import { invoke } from "@tauri-apps/api/core";
import type {
  Account,
  AppInfo,
  BackupInfo,
  BuildReport,
  Config,
  ConnectorSupport,
  ContentItem,
  ContentKind,
  DeviceCodeStart,
  GameVersion,
  ImportScan,
  InstallReport,
  Instance,
  InstanceOverrides,
  JavaInstall,
  KeyCheck,
  LinkReport,
  LogFile,
  PackPreview,
  Plan,
  PlanMod,
  PlanResolution,
  Project,
  ProjectKind,
  ProjectVersion,
  Provider,
  SearchQuery,
  SearchResults,
  Schedule,
  Server,
  ServerBuildReport,
  CommandNode,
  GitStatus,
  ServerKindInfo,
  ServerProperty,
  ServerSettings,
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
  startServer: (id: string) => invoke<number>("start_server", { id }),
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
  launchInstance: (id: string, accountId?: string) =>
    invoke<{ pid: number; instanceId: string }>("launch_instance", {
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
  importModpack: (path: string, name?: string) =>
    invoke<Instance>("import_modpack", { path, name: name ?? null }),
  exportModpack: (id: string, dest: string, includeSaves: boolean) =>
    invoke<string>("export_modpack", { id, dest, includeSaves }),
  exportGeodepack: (id: string, dest: string, includeSaves: boolean) =>
    invoke<string>("export_geodepack", { id, dest, includeSaves }),
};

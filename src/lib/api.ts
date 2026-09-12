import { invoke } from "@tauri-apps/api/core";
import type {
  Account,
  AppInfo,
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
  Server,
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
      icon?: string | null;
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
  serverVersions: (kind: string) =>
    invoke<string[]>("server_versions", { kind }),
  listServers: () => invoke<Server[]>("list_servers"),
  getServer: (id: string) => invoke<Server>("get_server", { id }),
  createServer: (name: string, kind: string, version: string) =>
    invoke<Server>("create_server", { name, kind, version }),
  deleteServer: (id: string) => invoke<void>("delete_server", { id }),
  updateServer: (
    id: string,
    patch: { name?: string; notes?: string; settings?: ServerSettings },
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
};

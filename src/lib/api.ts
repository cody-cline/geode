import { invoke } from "@tauri-apps/api/core";
import type {
  Account,
  AppInfo,
  Config,
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
  Project,
  ProjectKind,
  ProjectVersion,
  Provider,
  SearchQuery,
  SearchResults,
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
  ) =>
    invoke<InstallReport>("install_content", {
      id,
      provider,
      projectId,
      versionId: versionId ?? null,
    }),
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

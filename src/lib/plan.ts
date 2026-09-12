import type { PlanMod, Project } from "./types";

/** Identity for a plan entry — the same key the backend de-duplicates on. */
export function modKey(mod: { provider: string; projectId: string }): string {
  return `${mod.provider}:${mod.projectId}`;
}

/**
 * A search hit as a plan entry.
 *
 * Deliberately drops everything version-shaped. A plan records *which* mods,
 * and pinning a version here would quietly give it the one thing it is defined
 * by not having.
 */
export function planModFromProject(project: Project): PlanMod {
  return {
    provider: project.provider,
    projectId: project.id,
    name: project.name,
    slug: project.slug,
    iconUrl: project.iconUrl ?? null,
  };
}

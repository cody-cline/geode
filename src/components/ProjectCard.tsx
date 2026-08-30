import clsx from "clsx";
import {
  Check,
  Download,
  Loader2,
  Package,
  Play,
  Plus,
  ShieldAlert,
} from "lucide-react";

import { launchInstance } from "../lib/actions";
import { compactNumber, relativeTime, titleCase } from "../lib/format";
import type { Instance, Project } from "../lib/types";

import styles from "./ProjectCard.module.css";

export default function ProjectCard({
  project,
  installed,
  installedInstance,
  busy,
  directInstall,
  onOpen,
  onRequestInstall,
}: {
  project: Project;
  installed?: boolean;
  /**
   * For modpacks: the instance this pack already built. A pack defines an
   * instance, so installing it twice would just make a duplicate — the row
   * offers to play the one that exists instead.
   */
  installedInstance?: Instance;
  busy?: boolean;
  /** True when the destination is already fixed, so no dialog follows. */
  directInstall?: boolean;
  onOpen: () => void;
  /** Asks the parent where this should go — the card never decides. */
  onRequestInstall: (project: Project) => void;
}) {
  const isModpack = project.kind === "modpack";
  const blocked = !project.distributionAllowed;

  return (
    <article className={styles.row} onClick={onOpen}>
      <div className={styles.icon}>
        {project.iconUrl ? (
          <img src={project.iconUrl} alt="" loading="lazy" />
        ) : (
          <span className={styles.iconFallback}>{project.name.slice(0, 2)}</span>
        )}
      </div>

      <div className={styles.text}>
        <div className={styles.titleRow}>
          <h3 className={styles.name}>{project.name}</h3>
          {project.author && (
            <span className={styles.author}>by {project.author}</span>
          )}
          <span className={clsx("badge", `badge-${project.provider}`)}>
            {project.provider === "modrinth" ? "Modrinth" : "CurseForge"}
          </span>
          {project.alsoOn.map((ref) => (
            <span
              key={ref.provider}
              className={clsx("badge", `badge-${ref.provider}`)}
              title="Also available here"
            >
              {ref.provider === "modrinth" ? "Modrinth" : "CurseForge"}
            </span>
          ))}
          {blocked && (
            <span
              className="badge badge-warn"
              title="The author disabled third-party downloads"
            >
              <ShieldAlert size={10} />
              Manual
            </span>
          )}
        </div>

        <p className={styles.summary}>{project.summary}</p>

        <div className={styles.metaRow}>
          <span className={styles.stat}>
            <Download size={12} />
            {compactNumber(project.downloads)}
          </span>
          {project.updated && (
            <span className={styles.stat}>
              Updated {relativeTime(project.updated)}
            </span>
          )}
          {project.categories.slice(0, 3).map((category) => (
            <span key={category} className={styles.category}>
              {titleCase(category)}
            </span>
          ))}
        </div>
      </div>

      <div className={styles.action}>
        {installedInstance ? (
          <>
            <button
              className="btn btn-sm btn-primary"
              onClick={(e) => {
                e.stopPropagation();
                void launchInstance(installedInstance);
              }}
              disabled={installedInstance.running}
              title={`Play ${installedInstance.name}`}
            >
              <Play size={12} fill="currentColor" />
              {installedInstance.running ? "Running" : "Play"}
            </button>
            <span className={styles.installedNote}>Installed</span>
          </>
        ) : isModpack ? (
          <button
            className="btn btn-sm btn-primary"
            onClick={(e) => {
              e.stopPropagation();
              onRequestInstall(project);
            }}
            disabled={busy || blocked}
            title={
              blocked
                ? "The author disabled third-party downloads — open the project page"
                : "Create a new instance from this pack"
            }
          >
            {busy ? <Loader2 size={12} className="spin" /> : <Package size={13} />}
            Install
          </button>
        ) : installed ? (
          <span className={styles.installed}>
            <Check size={13} />
            Added
          </span>
        ) : (
          <button
            className="btn btn-sm btn-primary"
            onClick={(e) => {
              e.stopPropagation();
              onRequestInstall(project);
            }}
            disabled={busy || blocked}
            title={
              blocked
                ? "The author disabled third-party downloads — open the project page"
                : directInstall
                  ? undefined
                  : "Choose an instance to install into"
            }
          >
            {busy ? <Loader2 size={12} className="spin" /> : <Plus size={13} />}
            {directInstall ? "Install" : "Install…"}
          </button>
        )}
      </div>
    </article>
  );
}

import clsx from "clsx";
import { Check, Download, Loader2, Package, Plus, ShieldAlert } from "lucide-react";

import { compactNumber, relativeTime } from "../lib/format";
import type { Project } from "../lib/types";

import styles from "./ProjectCard.module.css";

export default function ProjectCard({
  project,
  installed,
  busy,
  onOpen,
  onRequestInstall,
}: {
  project: Project;
  installed?: boolean;
  busy?: boolean;
  onOpen: () => void;
  /** Asks the parent where this should go — the card never decides. */
  onRequestInstall: (project: Project) => void;
}) {
  const isModpack = project.kind === "modpack";
  const blocked = !project.distributionAllowed;
  const showInstalled = installed;

  return (
    <article className={styles.card} onClick={onOpen}>
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
          {project.author && <span className={styles.author}>by {project.author}</span>}
        </div>

        <p className={styles.summary}>{project.summary}</p>

        <div className={styles.metaRow}>
          <span className={styles.stat}>
            <Download size={11} />
            {compactNumber(project.downloads)}
          </span>
          {project.updated && (
            <span className={styles.stat}>Updated {relativeTime(project.updated)}</span>
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
            <span className="badge badge-warn" title="The author disabled third-party downloads">
              <ShieldAlert size={10} />
              Manual
            </span>
          )}
        </div>
      </div>

      <div className={styles.action}>
        {isModpack ? (
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
            Install pack
          </button>
        ) : showInstalled ? (
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
                : "Choose an instance to install into"
            }
          >
            {busy ? <Loader2 size={12} className="spin" /> : <Plus size={13} />}
            Install…
          </button>
        )}
      </div>
    </article>
  );
}

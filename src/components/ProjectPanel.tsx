import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import {
  Download,
  ExternalLink,
  Loader2,
  Package,
  Play,
  Plus,
  ShieldAlert,
  X,
} from "lucide-react";

import { api } from "../lib/api";
import { renderProviderBody } from "../lib/richtext";
import { launchInstance } from "../lib/actions";
import { useStore } from "../lib/store";
import { compactNumber, fileSize, relativeTime, titleCase } from "../lib/format";
import type { Instance, Project, ProjectVersion, Provider } from "../lib/types";

import styles from "./ProjectPanel.module.css";

export default function ProjectPanel({
  project,
  target,
  onClose,
  onRequestInstall,
  directInstall,
  installedInstance,
  planMode,
  added,
}: {
  project: Project;
  /** Only used to scope the version list; never an install destination. */
  target?: Instance;
  onClose: () => void;
  onRequestInstall: (project: Project, versionId?: string) => void;
  /** True when the destination is already fixed, so no dialog follows. */
  directInstall?: boolean;
  /** For modpacks: the instance this pack already built, if any. */
  installedInstance?: Instance;
  /** Filling a plan: the mod is added whole, without picking a version. */
  planMode?: boolean;
  /** Already on the plan's list. */
  added?: boolean;
}) {

  // A merged card can be installed from either platform.
  const availableSources: { provider: Provider; id: string; pageUrl: string }[] = [
    { provider: project.provider, id: project.id, pageUrl: project.pageUrl },
    ...project.alsoOn,
  ];
  const [source, setSource] = useState(availableSources[0]);

  const [description, setDescription] = useState<string | null>(null);
  const [versions, setVersions] = useState<ProjectVersion[] | null>(null);
  const [tab, setTab] = useState<"about" | "versions">("about");
  const navigate = useStore((st) => st.navigate);

  useEffect(() => {
    let cancelled = false;
    setDescription(null);
    setVersions(null);

    api
      .getProjectDescription(source.provider, source.id)
      .then((body) => !cancelled && setDescription(body))
      .catch(() => !cancelled && setDescription(""));

    const loader =
      project.kind === "mod" && target && target.loader !== "vanilla"
        ? target.loader
        : null;

    api
      .getProjectVersions(source.provider, source.id, target?.mcVersion ?? null, loader)
      .then((list) => !cancelled && setVersions(list))
      .catch(() => !cancelled && setVersions([]));

    return () => {
      cancelled = true;
    };
  }, [source, target, project.kind]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  const blocked = !project.distributionAllowed;
  const isModpack = project.kind === "modpack";

  return (
    <div className={styles.backdrop} onMouseDown={onClose}>
      <aside
        className={styles.panel}
        onMouseDown={(e) => e.stopPropagation()}
        role="dialog"
        aria-label={project.name}
      >
        <header className={styles.head}>
          <div className={styles.icon}>
            {project.iconUrl ? (
              <img src={project.iconUrl} alt="" />
            ) : (
              <span>{project.name.slice(0, 2)}</span>
            )}
          </div>
          <div className={styles.headText}>
            <h2 className={styles.name}>{project.name}</h2>
            <p className={styles.byline}>
              {project.author && <>by {project.author} · </>}
              {compactNumber(project.downloads)} downloads
            </p>
          </div>
          <button className="btn btn-ghost btn-icon btn-sm" onClick={onClose} aria-label="Close">
            <X size={16} />
          </button>
        </header>

        <p className={styles.summary}>{project.summary}</p>

        {availableSources.length > 1 && (
          <div className={styles.sourceSwitch}>
            <span className={styles.sourceLabel}>Source</span>
            {availableSources.map((s) => (
              <button
                key={s.provider}
                className={clsx(
                  styles.sourceChip,
                  source.provider === s.provider && styles.sourceActive,
                )}
                onClick={() => setSource(s)}
              >
                {s.provider === "modrinth" ? "Modrinth" : "CurseForge"}
              </button>
            ))}
          </div>
        )}

        <div className={styles.actions}>
          {blocked ? (
            <div className={styles.blocked}>
              <ShieldAlert size={15} />
              <div>
                <strong>Manual download only.</strong> The author disabled
                third-party downloads for this project. Get the file from the
                project page and drop it into the instance's mods folder.
              </div>
            </div>
          ) : installedInstance ? (
            <>
              <button
                className="btn btn-primary"
                onClick={() => void launchInstance(installedInstance)}
                disabled={installedInstance.running}
              >
                <Play size={15} fill="currentColor" />
                {installedInstance.running ? "Running" : "Play"}
              </button>
              <button
                className="btn"
                onClick={() => {
                  navigate({ view: "instance", id: installedInstance.id });
                  onClose();
                }}
              >
                Open instance
              </button>
            </>
          ) : planMode ? (
            added ? (
              <span className={styles.installedTag}>Already on the plan</span>
            ) : (
              <button
                className="btn btn-primary"
                onClick={() => onRequestInstall(project)}
              >
                <Plus size={15} />
                Add to plan
              </button>
            )
          ) : (
            <button
              className="btn btn-primary"
              onClick={() => onRequestInstall(project)}
            >
              {isModpack ? <Package size={15} /> : <Plus size={15} />}
              {isModpack
                ? "Install as new instance"
                : directInstall
                  ? "Install"
                  : "Install\u2026"}
            </button>
          )}
          <button className="btn" onClick={() => void openUrl(source.pageUrl)}>
            <ExternalLink size={14} />
            Open page
          </button>
        </div>

        <div className={styles.tabs}>
          <button
            className={clsx(styles.tab, tab === "about" && styles.tabActive)}
            onClick={() => setTab("about")}
          >
            About
          </button>
          <button
            className={clsx(styles.tab, tab === "versions" && styles.tabActive)}
            onClick={() => setTab("versions")}
          >
            Versions{versions ? ` (${versions.length})` : ""}
          </button>
        </div>

        <div className={styles.content}>
          {tab === "about" ? (
            <>
              {project.categories.length > 0 && (
                <div className={styles.chips}>
                  {project.categories.map((c) => (
                    <span key={c} className="badge">
                      {titleCase(c)}
                    </span>
                  ))}
                </div>
              )}
              {description === null ? (
                <div className={styles.loading}>
                  <Loader2 size={16} className="spin" />
                </div>
              ) : description === "" ? (
                <p className={styles.muted}>No description available.</p>
              ) : (
                // Written by third-party mod authors, so it is rendered only
                // after being normalised to HTML and stripped of anything
                // executable.
                <div
                  className="prose"
                  dangerouslySetInnerHTML={{ __html: renderProviderBody(description) }}
                />
              )}
            </>
          ) : versions === null ? (
            <div className={styles.loading}>
              <Loader2 size={16} className="spin" />
            </div>
          ) : versions.length === 0 ? (
            <p className={styles.muted}>
              No versions
              {target && (
                <>
                  {" "}
                  for Minecraft {target.mcVersion}
                  {target.loader !== "vanilla" && ` on ${titleCase(target.loader)}`}
                </>
              )}
              .
            </p>
          ) : (
            <ul className={styles.versionList}>
              {versions.map((version) => {
                const file = version.files.find((f) => f.primary) ?? version.files[0];
                return (
                  <li key={version.id} className={styles.version}>
                    <div className={styles.versionText}>
                      <div className={styles.versionTitle}>
                        <span className={styles.versionName}>
                          {version.versionNumber}
                        </span>
                        <span
                          className={clsx(
                            "badge",
                            version.releaseType === "release" && "badge-accent",
                          )}
                        >
                          {version.releaseType}
                        </span>
                      </div>
                      <div className={styles.versionMeta}>
                        {version.gameVersions.slice(0, 4).join(", ")}
                        {version.gameVersions.length > 4 && " …"}
                        {version.published && ` · ${relativeTime(version.published)}`}
                        {file?.size ? ` · ${fileSize(file.size)}` : ""}
                        {version.downloads > 0 && (
                          <>
                            {" · "}
                            <Download size={10} style={{ display: "inline" }} />{" "}
                            {compactNumber(version.downloads)}
                          </>
                        )}
                      </div>
                      {version.blockedReason && (
                        <p className={styles.versionBlocked}>{version.blockedReason}</p>
                      )}
                    </div>
                    {isModpack && installedInstance ? (
                      <span className={styles.installedTag}>Installed</span>
                    ) : planMode ? null : ( // A plan takes the mod, not one of its builds.
                      <button
                        className="btn btn-sm"
                        onClick={() => onRequestInstall(project, version.id)}
                        disabled={Boolean(version.blockedReason)}
                      >
                        {isModpack
                          ? "Use this"
                          : directInstall
                            ? "Install"
                            : "Install\u2026"}
                      </button>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </aside>
    </div>
  );
}

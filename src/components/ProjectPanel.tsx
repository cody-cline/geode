import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import {
  Download,
  ExternalLink,
  Loader2,
  Package,
  Play,
  Plus,
  Server,
  ShieldAlert,
  X,
} from "lucide-react";

import { api } from "../lib/api";
import { renderProviderBody } from "../lib/richtext";
import { useBackHandler } from "../lib/useBackHandler";
import { launchInstance } from "../lib/actions";
import { useStore } from "../lib/store";
import {
  compactNumber,
  fileSize,
  providerLabel,
  relativeTime,
  titleCase,
} from "../lib/format";
import type { Instance, Project, ProjectVersion, Provider } from "../lib/types";

import styles from "./ProjectPanel.module.css";

export default function ProjectPanel({
  project,
  target,
  onClose,
  onRequestInstall,
  onRequestServer,
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
  /** Modpacks only: host it rather than play it. Absent where that can't apply. */
  onRequestServer?: (project: Project, versionId?: string) => void;
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

  // Back closes the panel before it changes screen — it is the thing in front
  // of you, so it is what the gesture is aimed at.
  useBackHandler(true, onClose);

  const blocked = !project.distributionAllowed;
  const isModpack = project.kind === "modpack";

  // Portalled to the body rather than rendered in place. A custom-styled
  // scrollbar is painted with its own scroller, above overlapping positioned
  // siblings, so the results list's scrollbar drew straight over this panel
  // however high its z-index went. Leaving the subtree is what settles it.
  return createPortal(
    // Primary press only — see Modal. This backdrop is portalled over the whole
    // window, so a back-button mousedown anywhere would otherwise close the
    // panel and let the mouseup pop a page behind it.
    <div
      className={styles.backdrop}
      onMouseDown={(e) => {
        if (e.button === 0) onClose();
      }}
    >
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
                {providerLabel(s.provider)}
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
          {/* Offered even once the pack is installed: playing it and hosting
              it are separate things to want, and wanting both is normal. */}
          {isModpack && !blocked && onRequestServer && (
            <button className="btn" onClick={() => onRequestServer(project)}>
              <Server size={14} />
              Host as server…
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
                      // A file whose author turned third-party downloads off is
                      // still worth offering: the core looks for that author's
                      // own Modrinth release of the identical bytes, installs
                      // it when the hashes match, and says why not when there
                      // is none. A pack is the exception — it is fetched as a
                      // file rather than found by hash.
                      <button
                        className="btn btn-sm"
                        onClick={() => onRequestInstall(project, version.id)}
                        disabled={isModpack && Boolean(version.blockedReason)}
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
    </div>,
    document.body,
  );
}

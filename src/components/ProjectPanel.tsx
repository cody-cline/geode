import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import {
  Check,
  Download,
  ExternalLink,
  Loader2,
  Plus,
  ShieldAlert,
  X,
} from "lucide-react";

import { api } from "../lib/api";
import { compactNumber, fileSize, relativeTime, titleCase } from "../lib/format";
import { useStore } from "../lib/store";
import type { Instance, Project, ProjectVersion, Provider } from "../lib/types";

import styles from "./ProjectPanel.module.css";

export default function ProjectPanel({
  project,
  target,
  onClose,
  onInstalled,
}: {
  project: Project;
  target?: Instance;
  onClose: () => void;
  onInstalled?: () => void;
}) {
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  // A merged card can be installed from either platform.
  const availableSources: { provider: Provider; id: string; pageUrl: string }[] = [
    { provider: project.provider, id: project.id, pageUrl: project.pageUrl },
    ...project.alsoOn,
  ];
  const [source, setSource] = useState(availableSources[0]);

  const [description, setDescription] = useState<string | null>(null);
  const [versions, setVersions] = useState<ProjectVersion[] | null>(null);
  const [installing, setInstalling] = useState<string | null>(null);
  const [installedIds, setInstalledIds] = useState<Set<string>>(new Set());
  const [tab, setTab] = useState<"about" | "versions">("about");

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

  async function install(version?: ProjectVersion) {
    if (!target) return;
    setInstalling(version?.id ?? "latest");
    try {
      const report = await api.installContent(
        target.id,
        source.provider,
        source.id,
        version?.id,
      );
      if (version) setInstalledIds((prev) => new Set(prev).add(version.id));
      onInstalled?.();

      const extras = report.installed.length - 1;
      toast({
        kind: "success",
        title: `Added ${project.name}`,
        body: extras > 0 ? `Plus ${extras} required.` : undefined,
      });
      if (report.skipped.length > 0) {
        toast({
          kind: "error",
          title: "Some dependencies were skipped",
          body: report.skipped.join("; "),
        });
      }
    } catch (e) {
      reportError(`Could not install ${project.name}`, e);
    } finally {
      setInstalling(null);
    }
  }

  const blocked = !project.distributionAllowed;

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
          ) : (
            <button
              className="btn btn-primary"
              onClick={() => void install()}
              disabled={!target || installing !== null}
            >
              {installing === "latest" ? (
                <Loader2 size={14} className="spin" />
              ) : (
                <Plus size={15} />
              )}
              {target ? `Install into ${target.name}` : "Pick an instance first"}
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
                // Provider-supplied HTML/Markdown. It comes from a third party,
                // so scripts and frames are stripped before it reaches the DOM.
                <div
                  className="prose"
                  dangerouslySetInnerHTML={{ __html: sanitize(description) }}
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
                const isInstalled = installedIds.has(version.id);
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
                    {isInstalled ? (
                      <span className={styles.installedTag}>
                        <Check size={13} /> Added
                      </span>
                    ) : (
                      <button
                        className="btn btn-sm"
                        onClick={() => void install(version)}
                        disabled={
                          !target ||
                          installing !== null ||
                          Boolean(version.blockedReason)
                        }
                      >
                        {installing === version.id ? (
                          <Loader2 size={12} className="spin" />
                        ) : (
                          "Install"
                        )}
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

/**
 * Strip anything executable or framed out of provider-supplied markup.
 *
 * The panel renders HTML written by third-party mod authors, so it is treated
 * as untrusted: parse it detached from the document, drop script/style/iframe
 * and friends, and remove every `on*` handler and `javascript:` URL.
 */
function sanitize(html: string): string {
  const doc = new DOMParser().parseFromString(html, "text/html");

  doc
    .querySelectorAll("script, style, iframe, object, embed, form, link, meta, base")
    .forEach((el) => el.remove());

  doc.querySelectorAll("*").forEach((el) => {
    for (const attr of [...el.attributes]) {
      const name = attr.name.toLowerCase();
      const value = attr.value.trim().toLowerCase();
      if (name.startsWith("on")) {
        el.removeAttribute(attr.name);
      } else if (
        (name === "href" || name === "src" || name === "xlink:href") &&
        (value.startsWith("javascript:") || value.startsWith("data:text/html"))
      ) {
        el.removeAttribute(attr.name);
      }
    }
    if (el.tagName === "A") {
      el.setAttribute("target", "_blank");
      el.setAttribute("rel", "noreferrer noopener");
    }
  });

  return doc.body.innerHTML;
}

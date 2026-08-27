import { useState } from "react";
import clsx from "clsx";
import { Check, Download, Loader2, Plus, ShieldAlert } from "lucide-react";

import { api } from "../lib/api";
import { compactNumber, relativeTime } from "../lib/format";
import { useStore } from "../lib/store";
import type { Instance, Project } from "../lib/types";

import styles from "./ProjectCard.module.css";

export default function ProjectCard({
  project,
  target,
  installed,
  onOpen,
  onInstalled,
}: {
  project: Project;
  target?: Instance;
  installed?: boolean;
  onOpen: () => void;
  onInstalled?: () => void;
}) {
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);

  const isModpack = project.kind === "modpack";
  const blocked = !project.distributionAllowed;
  const showInstalled = installed || done;

  async function install(e: React.MouseEvent) {
    e.stopPropagation();
    if (!target) return;

    setBusy(true);
    try {
      const report = await api.installContent(
        target.id,
        project.provider,
        project.id,
      );
      setDone(true);
      onInstalled?.();

      const extras = report.installed.length - 1;
      toast({
        kind: "success",
        title: `Added ${project.name}`,
        body:
          extras > 0
            ? `Plus ${extras} required ${extras === 1 ? "dependency" : "dependencies"}.`
            : undefined,
      });
      if (report.skipped.length > 0) {
        toast({
          kind: "error",
          title: "Some dependencies were skipped",
          body: report.skipped.join("; "),
        });
      }
    } catch (err) {
      reportError(`Could not install ${project.name}`, err);
    } finally {
      setBusy(false);
    }
  }

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
          <span className={styles.packHint}>Open</span>
        ) : showInstalled ? (
          <span className={styles.installed}>
            <Check size={13} />
            Added
          </span>
        ) : (
          <button
            className="btn btn-sm btn-primary"
            onClick={install}
            disabled={!target || busy || blocked}
            title={
              blocked
                ? "The author disabled third-party downloads — open the project page"
                : !target
                  ? "Pick an instance to install into"
                  : `Install into ${target.name}`
            }
          >
            {busy ? <Loader2 size={12} className="spin" /> : <Plus size={13} />}
            Install
          </button>
        )}
      </div>
    </article>
  );
}

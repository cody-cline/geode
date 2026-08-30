import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Download, ExternalLink, Loader2, Users } from "lucide-react";

import { api } from "../../lib/api";
import { compactNumber, relativeTime, titleCase } from "../../lib/format";
import { renderProviderBody } from "../../lib/richtext";
import type { Instance, Project } from "../../lib/types";

import styles from "./PackTab.module.css";

/**
 * The pack's own project page, shown inside the instance it built.
 *
 * An instance made from a pack is still that pack, so its description,
 * categories and stats belong here rather than only behind a link to a browser.
 * What `PackSource` stored at install time renders immediately; the live
 * project fills in the rest when it arrives, so this is useful offline too.
 */
export default function PackTab({ instance }: { instance: Instance }) {
  const pack = instance.pack;
  const provider = pack?.provider;
  const projectId = pack?.projectId;

  const [project, setProject] = useState<Project | null>(null);
  const [description, setDescription] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!provider || !projectId) return;
    let cancelled = false;

    const source = provider === "curseforge" ? "curseforge" : "modrinth";

    api
      .getProject(source, projectId)
      .then((p) => !cancelled && setProject(p))
      .catch(() => !cancelled && setFailed(true));

    api
      .getProjectDescription(source, projectId)
      .then((body) => !cancelled && setDescription(body))
      .catch(() => !cancelled && setDescription(""));

    return () => {
      cancelled = true;
    };
  }, [provider, projectId]);

  if (!pack) return null;

  const banner = pack.bannerUrl ?? project?.bannerUrl ?? null;
  const summary = pack.summary ?? project?.summary ?? "";

  return (
    <div className={styles.scroll}>
      <div className={styles.page}>
        {banner && (
          <div className={styles.banner}>
            <img src={banner} alt="" />
          </div>
        )}

        <div className={styles.head}>
          {(pack.iconUrl ?? project?.iconUrl) && (
            <img
              className={styles.icon}
              src={pack.iconUrl ?? project?.iconUrl ?? ""}
              alt=""
            />
          )}
          <div className={styles.headText}>
            <h2 className={styles.name}>{pack.name}</h2>
            <p className={styles.byline}>
              {project?.author && <>by {project.author} · </>}
              version {pack.version}
              {pack.provider && ` · ${titleCase(pack.provider)}`}
            </p>
          </div>
          {pack.pageUrl && (
            <button className="btn" onClick={() => void openUrl(pack.pageUrl!)}>
              <ExternalLink size={14} />
              Open page
            </button>
          )}
        </div>

        {summary && <p className={styles.summary}>{summary}</p>}

        <div className={styles.stats}>
          {project && (
            <>
              <span className={styles.stat}>
                <Download size={13} />
                {compactNumber(project.downloads)} downloads
              </span>
              {project.follows > 0 && (
                <span className={styles.stat}>
                  <Users size={13} />
                  {compactNumber(project.follows)}
                </span>
              )}
              {project.updated && (
                <span className={styles.stat}>
                  Updated {relativeTime(project.updated)}
                </span>
              )}
            </>
          )}
        </div>

        {project && project.categories.length > 0 && (
          <div className={styles.chips}>
            {project.categories.map((c) => (
              <span key={c} className="badge">
                {titleCase(c)}
              </span>
            ))}
          </div>
        )}

        <hr className="divider" />

        {description === null && !failed ? (
          <div className={styles.loading}>
            <Loader2 size={16} className="spin" />
          </div>
        ) : description ? (
          <div
            className="prose"
            dangerouslySetInnerHTML={{ __html: renderProviderBody(description) }}
          />
        ) : (
          <p className={styles.muted}>
            {failed
              ? "Could not reach the project page. The details above are what was saved when this pack was installed."
              : "This pack has no description."}
          </p>
        )}
      </div>
    </div>
  );
}

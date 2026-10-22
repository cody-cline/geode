import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  ArrowUpCircle,
  Download,
  ExternalLink,
  Loader2,
  RefreshCw,
  Users,
} from "lucide-react";

import PackUpdateModal from "../../components/PackUpdateModal";
import { api } from "../../lib/api";
import { compactNumber, relativeTime, titleCase } from "../../lib/format";
import { renderProviderBody } from "../../lib/richtext";
import { useStore } from "../../lib/store";
import type { Instance, PackUpdate, Project } from "../../lib/types";

import styles from "./PackTab.module.css";

/**
 * The pack's own project page, shown inside the instance it built.
 *
 * An instance made from a pack is still that pack, so its description,
 * categories and stats belong here rather than only behind a link to a browser.
 * What `PackSource` stored at install time renders immediately; the live
 * project fills in the rest when it arrives, so this is useful offline too.
 *
 * It is also where a newer version of the pack is noticed, because this is the
 * page that says which version the instance is on.
 */
export default function PackTab({ instance }: { instance: Instance }) {
  const pack = instance.pack;
  const provider = pack?.provider;
  const projectId = pack?.projectId;

  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  const [project, setProject] = useState<Project | null>(null);
  const [description, setDescription] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [checking, setChecking] = useState(false);
  const [update, setUpdate] = useState<PackUpdate | null>(null);
  const [reviewing, setReviewing] = useState(false);
  const [asked, setAsked] = useState(0);

  // Asked when the page opens, because this is the page that shows a version
  // number and a stale one beside "Check" is the thing nobody clicks. A
  // failure is left to the button: the page is readable offline on purpose.
  //
  // And asked again after an update has run, rather than the offer being
  // cleared: a run that could not fetch part of the pack leaves the instance
  // on the version it had, and that offer is still the one to take.
  useEffect(() => {
    if (!provider || !projectId) return;
    let dropped = false;
    api
      .checkPackUpdate(instance.id)
      .then((found) => !dropped && setUpdate(found))
      .catch(() => {});
    return () => {
      dropped = true;
    };
  }, [instance.id, provider, projectId, asked]);

  async function checkForUpdate() {
    setChecking(true);
    try {
      const found = await api.checkPackUpdate(instance.id);
      setUpdate(found);
      toast({
        kind: found ? "info" : "success",
        title: found
          ? `${found.versionNumber} is available`
          : "This pack is up to date",
      });
    } catch (e) {
      reportError("Could not check for a newer version", e);
    } finally {
      setChecking(false);
    }
  }

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
          {provider && projectId && (
            <button
              className="btn"
              onClick={checkForUpdate}
              disabled={checking}
              title="Ask the platform whether this pack has a newer version"
            >
              {checking ? (
                <Loader2 size={14} className="spin" />
              ) : (
                <RefreshCw size={14} />
              )}
              Check for updates
            </button>
          )}
          {pack.pageUrl && (
            <button className="btn" onClick={() => void openUrl(pack.pageUrl!)}>
              <ExternalLink size={14} />
              Open page
            </button>
          )}
        </div>

        {summary && <p className={styles.summary}>{summary}</p>}

        {update && (
          <div className={styles.update}>
            <ArrowUpCircle size={15} />
            <span>
              Version {update.versionNumber} is available
              {update.releaseType !== "release" && ` (${update.releaseType})`}
            </span>
            <button
              className="btn btn-sm btn-primary"
              onClick={() => setReviewing(true)}
            >
              Review update
            </button>
          </div>
        )}

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

      {reviewing && update && (
        <PackUpdateModal
          instance={instance}
          update={update}
          onApplied={() => setAsked((n) => n + 1)}
          onClose={() => setReviewing(false)}
        />
      )}
    </div>
  );
}

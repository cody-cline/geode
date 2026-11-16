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

import { api, errorMessage } from "../lib/api";
import { rovingKey } from "../lib/rovingFocus";
import { renderProviderBody } from "../lib/richtext";
import { useBackHandler } from "../lib/useBackHandler";
import { useFocusTrap } from "../lib/useFocusTrap";
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
  /**
   * Why the description could not be fetched, when it could not.
   *
   * `""` is the sentinel for a provider that answered with an empty body, and
   * the catch used to set it too — so a fetch that failed rendered "No
   * description available." as the first thing anybody read about the mod,
   * About being this panel's default tab.
   */
  const [descriptionFailed, setDescriptionFailed] = useState<string | null>(
    null,
  );
  const [versions, setVersions] = useState<ProjectVersion[] | null>(null);
  /**
   * Why the version list could not be fetched, when it could not.
   *
   * `[]` rendered "No versions for Minecraft 1.21.1 on Fabric" under a tab
   * labelled "Versions (0)" — two agreeing, specific claims about the mod, out
   * of a request that never got an answer. This panel is the main route to
   * deciding whether a mod will work, and "none for your setup" is the one
   * answer that makes somebody stop looking.
   */
  const [versionsFailed, setVersionsFailed] = useState<string | null>(null);
  const [descriptionAsked, setDescriptionAsked] = useState(0);
  const [versionsAsked, setVersionsAsked] = useState(0);
  const [tab, setTab] = useState<"about" | "versions">("about");
  const navigate = useStore((st) => st.navigate);

  // Two reads, two effects. They were one, and a Try again for either would
  // then have thrown the other's answer away and fetched it over again.
  useEffect(() => {
    let cancelled = false;
    setDescription(null);
    setDescriptionFailed(null);

    api
      .getProjectDescription(source.provider, source.id)
      .then((body) => !cancelled && setDescription(body))
      .catch((e) => !cancelled && setDescriptionFailed(errorMessage(e)));

    return () => {
      cancelled = true;
    };
  }, [source, descriptionAsked]);

  useEffect(() => {
    let cancelled = false;
    setVersions(null);
    setVersionsFailed(null);

    const loader =
      project.kind === "mod" && target && target.loader !== "vanilla"
        ? target.loader
        : null;

    api
      .getProjectVersions(source.provider, source.id, target?.mcVersion ?? null, loader)
      .then((list) => !cancelled && setVersions(list))
      .catch((e) => !cancelled && setVersionsFailed(errorMessage(e)));

    return () => {
      cancelled = true;
    };
  }, [source, target, project.kind, versionsAsked]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Back closes the panel before it changes screen — it is the thing in front
  // of you, so it is what the gesture is aimed at. An overlay for the same
  // reason a dialog is: it sits over the list rather than in place of it.
  useBackHandler(true, onClose, "overlay");

  // And the keyboard belongs to it for the same reason. Escape above was the
  // only part of that this panel had: Tab still walked the filter rail and the
  // result grid behind it, pressing controls the panel was covering.
  const panel = useFocusTrap<HTMLElement>();

  const blocked = !project.distributionAllowed;
  const isModpack = project.kind === "modpack";

  /**
   * The one sentence every build in the list is carrying, when there is one
   * and no banner is already carrying it.
   *
   * A platform that refuses third-party downloads refuses them for the whole
   * project, so the reason is built from a single constant and lands on every
   * row identically — dozens of copies of one sentence down one list. Said
   * once above the list instead.
   *
   * Only when `blocked` is false, because when it is true the banner at the
   * top of the panel is already saying it in more words. That is the
   * merged-card case: a project on both platforms is canonically its Modrinth
   * entry, whose downloads are never author-blocked, so no banner renders —
   * and then the CurseForge chip refetches CurseForge's own builds, every one
   * of them refused. Two or more rows, because a lone row is not a
   * repetition, and a reason specific to one build (SpigotMC's premium and
   * externally hosted resources) belongs on the build it is about.
   */
  const sharedBlockedReason =
    !blocked && versions && versions.length > 1 && versions[0].blockedReason
      ? versions.every((v) => v.blockedReason === versions[0].blockedReason)
        ? versions[0].blockedReason
        : null
      : null;

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
        ref={panel}
        className={styles.panel}
        onMouseDown={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={project.name}
        tabIndex={-1}
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
            {/* One of them, never several, so the chips are a radio group
                rather than a row of buttons that each happen to look pressed.
                The label stays outside it — a group holds radios and nothing
                else — and names it from there, since a `span` names nothing
                on its own. */}
            <span className={styles.sourceLabel} id="project-source-label">
              Source
            </span>
            <div
              className={styles.sourceChips}
              role="radiogroup"
              aria-labelledby="project-source-label"
              onKeyDown={(e) => {
                const next = rovingKey(
                  e,
                  availableSources.map((s) => s.provider),
                );
                const picked = availableSources.find((s) => s.provider === next);
                if (picked) setSource(picked);
              }}
            >
              {availableSources.map((s) => (
                <button
                  key={s.provider}
                  className={clsx(
                    styles.sourceChip,
                    source.provider === s.provider && styles.sourceActive,
                  )}
                  onClick={() => setSource(s)}
                  role="radio"
                  aria-checked={source.provider === s.provider}
                  tabIndex={source.provider === s.provider ? 0 : -1}
                  data-roving={s.provider}
                >
                  {providerLabel(s.provider)}
                </button>
              ))}
            </div>
          </div>
        )}

        <div className={styles.actions}>
          {blocked ? (
            <div className={styles.blocked}>
              <ShieldAlert size={15} />
              {/* Two different facts, and the old text told the pack's one about
                  both. A mod whose author turned third-party downloads off is
                  still installable here: the core looks for that author's own
                  release of the identical bytes and installs it when the hashes
                  match, which is what the comment on the Versions button below
                  describes and what `blocked.test.mjs` exists to keep. So this
                  banner used to deny, in the slot read first, the thing the
                  same panel does one tab across. A pack really is manual —
                  it is fetched as a file rather than found by its contents.
                  "the instance's mods folder" went with it: the panel opens
                  from Browse with no instance chosen, so there was no folder
                  the sentence could mean. */}
              {isModpack ? (
                <div>
                  <strong>Manual download only.</strong> The author disabled
                  third-party downloads, and a pack is fetched as a file rather
                  than matched by its contents, so there is nothing Geode can
                  put in its place. Download it from the project page and bring
                  the .zip in from Library.
                </div>
              ) : (
                <div>
                  <strong>Not downloadable directly.</strong> The author disabled
                  third-party downloads, so Geode looks for the same file
                  published by that author elsewhere and installs that when the
                  contents match. Pick a build under Versions to try it — if
                  nothing matches, it says so.
                </div>
              )}
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
              {descriptionFailed ? (
                /* Before the empty-body branch, and borrowing the sentence the
                   installed-pack panel already uses for the same situation:
                   nothing arrived, so whether this project has a description
                   is not something this panel found out. */
                <div className={styles.muted}>
                  <p>
                    Could not reach the project page, so the description is not
                    shown. Not that there is none — the summary above is what
                    the search result carried.
                  </p>
                  <p className="hint">{descriptionFailed}</p>
                  <button
                    className="btn btn-sm"
                    style={{ marginTop: 10 }}
                    onClick={() => setDescriptionAsked((n) => n + 1)}
                  >
                    Try again
                  </button>
                </div>
              ) : description === null ? (
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
          ) : versionsFailed ? (
            /* Before the empty branch, which names the Minecraft version and
               the loader and so reads as an answer about this mod and this
               instance. The count beside the tab label stays off for the same
               reason — "Versions (0)" is the same claim in two characters. */
            <div className={styles.muted}>
              <p>
                Could not read the versions {project.name} publishes. Not that
                there are none for this setup — the list was not read.
              </p>
              <p className="hint">{versionsFailed}</p>
              <button
                className="btn btn-sm"
                style={{ marginTop: 10 }}
                onClick={() => setVersionsAsked((n) => n + 1)}
              >
                Try again
              </button>
            </div>
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
            <>
              {/* The reason said once above the list when every build carries
                  the same one and no banner is already carrying it. That is
                  the merged-card shape: a project on both platforms is
                  canonically its Modrinth entry, whose downloads are never
                  author-blocked, so `blocked` is false and no banner renders
                  — then clicking the CurseForge chip refetches CurseForge's
                  own builds and every row has the identical sentence on it.
                  One sentence for the list is the same repair the banner case
                  gets, applied where there is no banner to make it. */}
              {sharedBlockedReason && (
                <p className={styles.versionBlocked} style={{ marginBottom: 10 }}>
                  {sharedBlockedReason}
                </p>
              )}
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
                      {/* Dropped only where it is CurseForge's constant and
                          the banner at the top is already saying it. That one
                          is built per file from a single string, so an author
                          who turned third-party downloads off puts the same
                          sentence on every row, dozens of times down one list
                          — and it tells the reader to drop the jar in "the
                          mods folder", which the banner dropped for the reason
                          given up there, and to do by hand what the banner
                          says the core does by itself.

                          Keyed on the provider, not on the banner, because the
                          banner is up for a premium or externally hosted
                          SpigotMC resource too, and that reason is specific to
                          the resource and prints on the single row it has.
                          Nothing is repeated there, and it is the only text on
                          screen saying to buy or fetch the jar — the banner
                          offers to match the file elsewhere, which for a paid
                          resource there is no way to do. */}
                      {version.blockedReason &&
                        !sharedBlockedReason &&
                        !(blocked && source.provider === "curseforge") && (
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
            </>
          )}
        </div>
      </aside>
    </div>,
    document.body,
  );
}

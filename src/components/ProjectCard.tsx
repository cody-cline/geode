import clsx from "clsx";
import {
  Blocks,
  Check,
  Download,
  Loader2,
  Package,
  Play,
  Plus,
  ShieldAlert,
} from "lucide-react";

import { launchInstance } from "../lib/actions";
import {
  compactNumber,
  providerLabel,
  relativeTime,
  titleCase,
} from "../lib/format";
import type { Instance, ProgressEvent, Project } from "../lib/types";

import styles from "./ProjectCard.module.css";

export default function ProjectCard({
  project,
  rowKey,
  tabStop,
  installed,
  inInstances,
  installedInstance,
  busy,
  progress,
  directInstall,
  actionLabel,
  viaConnector,
  fabricAlso,
  nativeLoaderLabel,
  onChooseFabric,
  onOpen,
  onRequestInstall,
}: {
  project: Project;
  /**
   * This row's identity inside the results grid, written out as `data-row` so
   * the grid's key handler can find the row a press came from and the row it
   * should move to. The list it sits in owns what that string is.
   */
  rowKey: string;
  /**
   * This row carries the grid's one tab stop.
   *
   * A results page is twenty of these and each has two to four controls, so
   * left as ordinary tab stops they are sixty-odd presses with no way out.
   * Every control here is therefore `tabIndex={-1}` and reached with the arrow
   * keys; the exception is the name, which is the row's first control and
   * takes the tab stop on whichever row the keyboard was last on.
   */
  tabStop: boolean;
  installed?: boolean;
  /**
   * Names of the instances that already hold this project, when the row is not
   * being browsed on one instance's behalf.
   *
   * It cannot replace the button the way `installed` above does: that one has
   * a single destination behind it and can say "Added", while this is a mod
   * that is in one instance and not another, and installing it into a second
   * is a perfectly ordinary thing to want. So it changes what the button
   * *says* instead — "Install elsewhere…", which only means anything about a
   * mod that is already somewhere — and the row states where before offering
   * it rather than after.
   */
  inInstances?: string[];
  /**
   * For modpacks: the instance this pack already built. A pack defines an
   * instance, so installing it twice would just make a duplicate — the row
   * offers to play the one that exists instead.
   */
  installedInstance?: Instance;
  busy?: boolean;
  /** Live install progress for this project, when one is running. */
  progress?: ProgressEvent;
  /** True when the destination is already fixed, so no dialog follows. */
  directInstall?: boolean;
  /** Overrides the button's wording, e.g. "Add" when filling a plan. */
  actionLabel?: string;
  /** This is a Fabric mod being offered on a (Neo)Forge target. */
  viaConnector?: boolean;
  /** It builds for both loaders, so the row offers the choice. */
  fabricAlso?: boolean;
  /** Display name of the instance's own loader, e.g. "NeoForge". */
  nativeLoaderLabel?: string;
  onChooseFabric?: (fabric: boolean) => void;
  onOpen: () => void;
  /** Asks the parent where this should go — the card never decides. */
  onRequestInstall: (project: Project) => void;
}) {
  const isModpack = project.kind === "modpack";
  const blocked = !project.distributionAllowed;
  // The instances this is already in, which only global browse can know: where
  // the destination is fixed, what it holds is `installed` above instead.
  const heldIn = inInstances ?? [];

  // While installing, the row's job is to report the install. The blurb and
  // stats are not going anywhere, so they yield to the bar.
  const installing = Boolean(progress) && !progress?.done;
  const pct =
    progress && progress.total > 0
      ? Math.min(100, Math.round((progress.current / progress.total) * 100))
      : null;

  // Capped at three, and joined into one string here rather than rendered a
  // span each. Inside a single run a separator is only ever between two names,
  // so it has no edge of its own to be stranded at; `.categories` keeps that
  // run on one line and truncates it as a whole.
  const categories = project.categories.slice(0, 3).map(titleCase);

  // What the count beside the provider badge stands for. Said in words as well
  // as drawn as a count, because "+1" is a glance, not a sentence, and a
  // screen reader reading out "plus one" has been told nothing.
  const alsoOn = `Also on ${project.alsoOn
    .map((ref) => providerLabel(ref.provider))
    .join(" and ")}`;

  return (
    // A row of the results grid. Its two cells are the thing itself — artwork,
    // name, summary, stats — and what you can do about it, which is the same
    // two on every row whether or not that row offers a loader to choose
    // between, so a screen reader counting columns counts the same number all
    // the way down.
    <article className={styles.row} onClick={onOpen} role="row" data-row={rowKey}>
      <div className={styles.main} role="gridcell">
        <div className={styles.icon}>
          {project.iconUrl ? (
            <img src={project.iconUrl} alt="" loading="lazy" />
          ) : (
            <span className={styles.iconFallback}>{project.name.slice(0, 2)}</span>
          )}
        </div>

        <div className={styles.text}>
          <div className={styles.titleRow}>
            {/* The name, who wrote it and where it is published travel
                together: a provider badge that wrapped away from them landed
                on a line of its own between the title and the summary,
                belonging to neither. What gives inside this group is the
                *second* provider, not the name — see `.alsoOn` below. */}
            <span className={styles.nameGroup}>
              {/* The row's keyboard route in, and the row's one tab stop. The
                  library and server cards wrap their whole text block in a
                  `<button>`, and that shape cannot be borrowed here: this row
                  already holds controls of its own — the loader chooser
                  further along this very line, and the install button in the
                  column at the end — and a button inside a button is neither
                  valid nor operable. `role="button"` on the row is the same
                  trap by another route, since a button's content is
                  presentational to a screen reader and would swallow both. So
                  the name is the control: named by the project, opening what
                  clicking the row opens, with every other control left as its
                  sibling. */}
              <h3 className={styles.name}>
                <button
                  type="button"
                  className={styles.nameBtn}
                  tabIndex={tabStop ? 0 : -1}
                  onClick={(e) => {
                    e.stopPropagation();
                    onOpen();
                  }}
                >
                  {project.name}
                </button>
              </h3>
              {project.author && (
                <span className={styles.author}>by {project.author}</span>
              )}
              <span className={clsx("badge", `badge-${project.provider}`)}>
                {providerLabel(project.provider)}
              </span>
              {/* Where this would be installed from is a glance-level fact and
                  keeps its badge. That the same mod is *also* published
                  somewhere else is not: it changes nothing about this row, and
                  it was being paid for out of the only thing on the line that
                  could give. "CurseForge" spelled out is 79px where a count is
                  27, and at 940 those 52px were coming off the name and the
                  author — "Cloth Confi… by she…", so that the second badge
                  could be written in full. So the rest are a count, with the
                  names on the tooltip and in the panel behind the row. */}
              {project.alsoOn.length > 0 && (
                <span
                  className={clsx("badge", styles.alsoOn)}
                  role="img"
                  title={alsoOn}
                  aria-label={alsoOn}
                >
                  +{project.alsoOn.length}
                </span>
              )}
            </span>
            {fabricAlso ? (
              <span
                className={styles.loaderPick}
                role="group"
                aria-label="Build to install"
                onClick={(e) => e.stopPropagation()}
              >
                <button
                  type="button"
                  className={clsx(
                    styles.loaderOpt,
                    !viaConnector && styles.loaderOptNative,
                  )}
                  aria-pressed={!viaConnector}
                  tabIndex={-1}
                  onClick={() => onChooseFabric?.(false)}
                  title={`Install the ${nativeLoaderLabel} build`}
                >
                  {nativeLoaderLabel}
                </button>
                <button
                  type="button"
                  className={clsx(
                    styles.loaderOpt,
                    viaConnector && styles.loaderOptFabric,
                  )}
                  aria-pressed={Boolean(viaConnector)}
                  tabIndex={-1}
                  onClick={() => onChooseFabric?.(true)}
                  title="Install the Fabric build instead and run it through Sinytra Connector — useful when another mod expects the Fabric one"
                >
                  <Blocks size={10} />
                  Fabric
                </button>
              </span>
            ) : (
              viaConnector && (
                <span
                  className={clsx("badge", styles.connectorBadge)}
                  title="A Fabric mod. Installing it adds Sinytra Connector, Forgified Fabric API and Connector Extras so it can run on this loader."
                >
                  <Blocks size={10} />
                  via Connector
                </span>
              )
            )}
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

          {installing ? (
            <div className={styles.progress}>
              <div className={styles.progressHead}>
                <span className={styles.progressStage}>
                  {progress?.error ? "Failed" : (progress?.stage ?? "Working")}
                </span>
                <span className={styles.progressPct}>
                  {pct !== null ? `${pct}%` : ""}
                </span>
              </div>
              <div className={styles.track}>
                <div
                  className={pct === null ? styles.barIndeterminate : styles.bar}
                  style={pct === null ? undefined : { width: `${pct}%` }}
                  data-error={progress?.error ? "true" : undefined}
                />
              </div>
              <p className={styles.progressDetail}>
                {progress?.error ?? progress?.detail}
              </p>
            </div>
          ) : (
            <>
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
                {categories.length > 0 && (
                  <span
                    className={styles.categories}
                    title={categories.join(", ")}
                  >
                    {categories.join(" · ")}
                  </span>
                )}
              </div>
            </>
          )}
        </div>
      </div>

      <div className={styles.action} role="gridcell">
        {/* Before whatever the row offers, not after it, and never instead of
            it. The app already knows this, and it used to be the quietest type
            on the row, below the button, wrapping to two ragged lines — read
            as a footnote to the action rather than as the answer it is. One
            instance is named; several are counted, because the column is
            narrow and a list of names would not fit, so they are on the
            tooltip. "In Showcase" rather than "Already in Showcase" now that
            it sits above the button: what made "Already" necessary was being
            *under* one, where "In Showcase" read as where the button would put
            it. Above a button that says "elsewhere", the tick and the colour
            are the tense. */}
        {heldIn.length > 0 && (
          <span className={styles.heldIn} title={heldIn.join(", ")}>
            <Check size={12} />
            <span className={styles.heldName}>
              {heldIn.length === 1
                ? `In ${heldIn[0]}`
                : `In ${heldIn.length} instances`}
            </span>
          </span>
        )}
        {installedInstance ? (
          <>
            {/* The one filled button in the list, and the reason the others
                are not: this row is a pack you already have, so the thing to
                do with it is play it, and that is worth being the loudest
                thing on the screen because it is true of one row in twenty. */}
            <button
              className="btn btn-sm btn-primary"
              tabIndex={-1}
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
            className={clsx("btn btn-sm", styles.install)}
            tabIndex={-1}
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
            // The accent wash is the row's offer of something new. A row that
            // is already in an instance is not making that offer — it is
            // offering a second copy, which is a real thing to want and a
            // quieter one — so it keeps the plain button and leaves the wash
            // to the nineteen rows in twenty that have something to add.
            className={clsx(
              "btn btn-sm",
              heldIn.length === 0 ? styles.install : styles.elsewhere,
            )}
            tabIndex={-1}
            onClick={(e) => {
              e.stopPropagation();
              onRequestInstall(project);
            }}
            disabled={busy || blocked}
            title={
              blocked
                ? "The author disabled third-party downloads — open the project page"
                : actionLabel
                  ? "Add to the plan"
                  : viaConnector
                    ? "Installs the Fabric build, plus Sinytra Connector, Forgified Fabric API and Connector Extras"
                    : heldIn.length > 0
                      ? "Choose another instance to install into"
                      : directInstall
                        ? undefined
                        : "Choose an instance to install into"
            }
          >
            {/* The plus is what gives on a held row, and it is the only thing
                on this button that can. It draws the word "Install" a second
                time, which is the word already doing the least work here —
                what this button says that the others do not is "elsewhere".
                Twenty pixels, and the column is sized by this button, so they
                come back to the title line beside it: with them and the
                tighter padding in the stylesheet's 1100 block, "Cloth Config
                API by shedaniel" still renders whole at 940, which the gaps on
                that line were tuned for once already. The spinner is state
                rather than decoration, so it stays. */}
            {busy ? (
              <Loader2 size={12} className="spin" />
            ) : heldIn.length > 0 ? null : (
              <Plus size={13} />
            )}
            {actionLabel ??
              (heldIn.length > 0
                ? "Install elsewhere…"
                : directInstall
                  ? "Install"
                  : "Install…")}
          </button>
        )}
      </div>
    </article>
  );
}

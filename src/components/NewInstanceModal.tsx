import { useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import { FileArchive, FolderOpen, Loader2, Search, Users } from "lucide-react";

import Modal from "./Modal";
import { api, errorMessage } from "../lib/api";
import { rovingKey } from "../lib/rovingFocus";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type { FolderInstance, GameVersion, Loader, PackPreview } from "../lib/types";

import styles from "./NewInstanceModal.module.css";

/**
 * What each loader *is*, in one line.
 *
 * The first decision anyone makes in this app was five bare proper nouns, in a
 * product that explains what a plan is, what a proxy is and what Java 17 is
 * for. Somebody who has never modded Minecraft cannot pick between Quilt and
 * NeoForge from the names, and picking wrong means every mod they then find is
 * the wrong one.
 *
 * Descriptions of the software, never claims about what it supports: which
 * loader has a build for which Minecraft version is read from the upstream at
 * runtime — the picker below already says "No Fabric builds for 1.21.4" when
 * that is the answer — and a sentence here asserting it would be the same
 * hardcoding this codebase refuses everywhere else.
 *
 * `Record<Loader, string>` rather than a list, so a loader added to `LOADERS`
 * and forgotten here fails the typecheck instead of rendering a blank row.
 */
const LOADER_BLURB: Record<Loader, string> = {
  vanilla: "Minecraft as it comes, with no mod loader at all.",
  fabric: "Light and quick to update. Most new mods are built for it.",
  quilt: "A fork of Fabric. It runs Fabric's mods as well as its own.",
  neoforge: "A fork of Forge, and the newer of the two.",
  forge: "The older line, and what many long-running packs are built on.",
};

/**
 * What each kind of version is called, in the words a player would use.
 *
 * `version.kind` is Mojang's own `type` field, passed through
 * `list_game_versions` untouched, and two of the four values it takes are
 * machine tokens: a badge beside a version from 2010 read `old_beta`,
 * underscore and all.
 *
 * Keyed by `string` rather than by a union, because the set of types is
 * Mojang's to extend and the row below falls back to the raw value — a fifth
 * kind then looks wrong rather than disappearing, which is the right way
 * round for a badge whose whole job is to say this is not a release.
 */
const VERSION_KIND_LABEL: Record<string, string> = {
  snapshot: "snapshot",
  old_beta: "beta",
  old_alpha: "alpha",
};

export default function NewInstanceModal({ onClose }: { onClose: () => void }) {
  const navigate = useStore((s) => s.navigate);
  const refresh = useStore((s) => s.refreshInstances);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const config = useStore((s) => s.config);
  const openJoin = useStore((s) => s.openJoin);

  const [name, setName] = useState("");
  const [nameTouched, setNameTouched] = useState(false);
  /**
   * Minecraft's versions, or null while that is not known.
   *
   * It used to start at `[]` and the catch left it there, so the one branch
   * the list below had — "No versions match" — was what stood here both before
   * the manifest arrived and forever after it failed to. With `filter` still
   * at its initial value that read `No versions match “”`, a sentence with
   * empty quotation marks that is false twice over: nothing was searched for,
   * and Minecraft's versions were never in question.
   */
  const [versions, setVersions] = useState<GameVersion[] | null>(null);
  /** Why that list could not be read, when it could not. */
  const [versionsProblem, setVersionsProblem] = useState<string | null>(null);
  /** Bumped to ask again; the read is keyed on the snapshot toggle alone. */
  const [versionsAsked, setVersionsAsked] = useState(0);
  const [showSnapshots, setShowSnapshots] = useState(config?.showSnapshots ?? false);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<string>("");
  const [loader, setLoader] = useState<Loader>("fabric");
  const [loaderVersions, setLoaderVersions] = useState<string[]>([]);
  /**
   * Why the loader's builds could not be read, when they could not.
   *
   * An empty `loaderVersions` renders "No Fabric builds for 1.21.1", which is
   * a claim about what FabricMC publishes — and the first rule this codebase
   * holds itself to is that support is derived at runtime from the upstream,
   * so a derivation that failed must not be reported as the upstream's answer.
   */
  const [loaderProblem, setLoaderProblem] = useState<string | null>(null);
  /**
   * Bumped to ask for the loader's builds again.
   *
   * The read is keyed on the loader and the Minecraft version, and pressing
   * the loader that is already chosen is not a change, so nothing else here
   * re-runs it for the pair that failed.
   */
  const [loaderAsked, setLoaderAsked] = useState(0);
  const [loaderVersion, setLoaderVersion] = useState("");
  const [loadingLoader, setLoadingLoader] = useState(false);
  const [creating, setCreating] = useState(false);
  // The instances found inside a folder that was not itself one, waiting to be
  // picked from.
  const [inside, setInside] = useState<FolderInstance[] | null>(null);

  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // Back to null rather than leaving the last answer on screen: once
    // snapshots are toggled, the list that is up is the answer to the other
    // question, and a refetch that fails would leave it standing as this one.
    setVersions(null);
    setVersionsProblem(null);
    api
      .listGameVersions(showSnapshots)
      .then((v) => {
        setVersions(v);
        // Default to the newest stable so the common case is one click.
        setSelected((current) => {
          if (current && v.some((x) => x.id === current)) return current;
          return v.find((x) => x.kind === "release")?.id ?? v[0]?.id ?? "";
        });
      })
      .catch((e) => {
        reportError("Could not load Minecraft versions", e);
        setVersionsProblem(errorMessage(e));
      });
  }, [showSnapshots, versionsAsked, reportError]);

  // Loader builds depend on the chosen game version, so refetch on either.
  useEffect(() => {
    if (loader === "vanilla" || !selected) {
      setLoaderVersions([]);
      setLoaderVersion("");
      return;
    }
    let cancelled = false;
    setLoadingLoader(true);
    setLoaderVersions([]);
    setLoaderVersion("");
    setLoaderProblem(null);

    api
      .listLoaderVersions(loader, selected)
      .then((list) => {
        if (cancelled) return;
        setLoaderVersions(list);
        setLoaderVersion(list[0] ?? "");
      })
      .catch((e) => {
        if (!cancelled) setLoaderProblem(errorMessage(e));
      })
      .finally(() => {
        if (!cancelled) setLoadingLoader(false);
      });

    return () => {
      cancelled = true;
    };
  }, [loader, selected, loaderAsked]);

  const filtered = useMemo(() => {
    if (!versions) return [];
    const q = filter.trim().toLowerCase();
    return q ? versions.filter((v) => v.id.toLowerCase().includes(q)) : versions;
  }, [versions, filter]);

  const virtualizer = useVirtualizer({
    count: filtered.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => 32,
    overscan: 8,
  });

  // Suggest a name from the selection until the user types their own.
  const effectiveName =
    name.trim() ||
    (selected
      ? loader === "vanilla"
        ? selected
        : `${selected} ${LOADERS.find((l) => l.id === loader)?.label}`
      : "");

  const loaderReady =
    loader === "vanilla" || (!loadingLoader && loaderVersion !== "");
  const canCreate = Boolean(selected) && effectiveName !== "" && loaderReady && !creating;

  async function create() {
    if (!canCreate) return;
    setCreating(true);
    try {
      const instance = await api.createInstance(
        effectiveName,
        selected,
        loader,
        loaderVersion,
      );
      await refresh();
      toast({ kind: "success", title: `Created ${instance.name}` });
      navigate({ view: "instance", id: instance.id });
      onClose();
    } catch (e) {
      reportError("Could not create the instance", e);
    } finally {
      setCreating(false);
    }
  }

  async function importPack() {
    const picked = await openDialog({
      multiple: false,
      filters: [
        { name: "Modpack or instance", extensions: ["geodepack", "mrpack", "zip"] },
      ],
    });
    if (typeof picked !== "string") return;
    await importFrom(picked);
  }

  // A Prism or MultiMC instance folder, read where the user points at it:
  // Geode never goes looking through another launcher's data directory.
  async function importFolder() {
    const picked = await openDialog({ multiple: false, directory: true });
    if (typeof picked !== "string") return;
    // The folder a person thinks of as "my instances" is the one holding all
    // of them, so that is what they point at. When it is not itself an
    // instance but holds some, the instances are offered to pick from — ten of
    // them is a worse thing to refuse than to list, and which one they want is
    // theirs to say, so none is imported here.
    const found = await api.instancesInFolder(picked).catch(() => []);
    if (found.length > 0) {
      setInside(found);
      return;
    }
    await importFrom(picked);
  }

  async function importFrom(picked: string) {
    let preview: PackPreview;
    try {
      preview = await api.previewModpack(picked);
    } catch (e) {
      reportError("That is not a modpack or an instance", e);
      return;
    }

    // `!config?.hasCurseforgeKey` refused on a settings read that failed
    // exactly as it refused on a build with no key, and said the second thing
    // either way — which is a claim about which Geode this is, made without
    // having read the file that would say.
    if (preview.needsCurseforgeKey) {
      if (!config) {
        reportError(
          "Cannot tell whether this pack can be imported",
          "This pack's mods come from CurseForge, and Geode could not read its settings to see whether it can reach CurseForge. Settings says what went wrong.",
        );
        return;
      }
      if (!config.hasCurseforgeKey) {
        reportError(
          "CurseForge packs are not available",
          "This pack's mods come from CurseForge, which this copy of Geode cannot reach.",
        );
        return;
      }
    }

    setCreating(true);
    try {
      const { instance, problems } = await api.importModpack(picked);
      await refresh();
      // An instance was made either way, so it is opened either way; what the
      // import could not fetch is reported beside it rather than in place of
      // it, because importing again would only make a second one.
      if (problems.length === 0) {
        toast({
          kind: "success",
          title: `Imported ${instance.name}`,
          body: `${preview.fileCount} files, Minecraft ${preview.mcVersion}.`,
        });
      } else {
        reportError(`Imported ${instance.name}, with problems`, problems.join(" "));
      }
      navigate({ view: "instance", id: instance.id });
      onClose();
    } catch (e) {
      // Nothing was made: an import that gets as far as an instance comes back
      // as an outcome, problems and all.
      await refresh();
      reportError("Could not import that pack", e);
    } finally {
      setCreating(false);
    }
  }

  // Which of a folder's instances to bring across is the person's choice, so
  // the dialog becomes that list until they make it or go back.
  if (inside) {
    return (
      <Modal
        title="Pick an instance"
        subtitle={`That folder is not an instance itself, but it holds ${
          inside.length === 1 ? "one" : inside.length
        }.`}
        onClose={onClose}
        width={520}
        footer={
          <>
            <button
              className="btn btn-ghost"
              onClick={() => setInside(null)}
              disabled={creating}
            >
              Back
            </button>
            <div style={{ flex: 1 }} />
            <button className="btn" onClick={onClose}>
              Cancel
            </button>
          </>
        }
      >
        <div className={styles.foundList}>
          {inside.map((found) => (
            <button
              key={found.path}
              className={styles.foundRow}
              disabled={creating}
              onClick={() => importFrom(found.path)}
            >
              <span className={styles.foundName}>{found.name}</span>
              <span className={styles.foundMeta}>
                {found.mcVersion}
                {found.loader !== "vanilla" &&
                  ` · ${LOADERS.find((l) => l.id === found.loader)?.label}`}
              </span>
              <span className={styles.foundHint}>Import</span>
            </button>
          ))}
        </div>
      </Modal>
    );
  }

  return (
    <Modal
      title="New instance"
      subtitle="Pick a Minecraft version and a mod loader."
      onClose={onClose}
      width={620}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn btn-primary" onClick={create} disabled={!canCreate}>
            {creating && <Loader2 size={14} className="spin" />}
            Create
          </button>
        </>
      }
    >
      {/* The three other ways in, and they are not footer buttons. A footer
          holds what acts on the form above it — Cancel and Create — while these
          three throw that form away and build the instance out of something
          that already exists. In the same row as Create they read as five
          peers, and "Join a server" standing beside it in a dialog called New
          instance read as a mistake.

          So they come to the top instead, in a box of their own, and before the
          form rather than after it: somebody who has a pack on disk should find
          that out before picking a Minecraft version they will not use. A box,
          small buttons and a line saying what they are — a route, then the
          form. */}
      <div className={styles.otherWays}>
        <p className={styles.otherWaysLead}>
          Already have a pack, an instance or a server? Start from that instead.
          Each of these brings its own Minecraft version and loader, so nothing
          below applies.
        </p>
        <div className={styles.otherWaysRow}>
          <button
            className="btn btn-sm"
            onClick={importPack}
            disabled={creating}
          >
            <FileArchive size={14} />
            Import modpack
          </button>
          {/* An instance folder rather than a file: a Prism or MultiMC
              instance is a directory, and a zipped one goes through the
              button beside this. */}
          <button
            className="btn btn-sm"
            onClick={importFolder}
            disabled={creating}
            title="Import a Prism or MultiMC instance folder"
          >
            <FolderOpen size={14} />
            Import folder
          </button>
          {/* A joined server becomes an instance like any other, so this is
              where someone looking to add one would go looking. */}
          <button
            className="btn btn-sm"
            onClick={() => {
              onClose();
              openJoin();
            }}
            disabled={creating}
          >
            <Users size={14} />
            Join a server
          </button>
        </div>
      </div>

      <div>
        <label className="label" htmlFor="instance-name">
          Name
        </label>
        <input
          id="instance-name"
          className="input"
          value={nameTouched ? name : effectiveName}
          placeholder="My instance"
          onChange={(e) => {
            setNameTouched(true);
            setName(e.target.value);
          }}
          onFocus={() => {
            if (!nameTouched) {
              setNameTouched(true);
              setName(effectiveName);
            }
          }}
        />
      </div>

      <div>
        <span className="label" id="loader-label">
          Mod loader
        </span>
        {/* Five alternatives, one of which is in force — which is a radio
            group, and was five separate toggle buttons: four off, one on, and
            nothing saying that picking one unpicks the rest. The group is
            named from the `span`, which is not a `<label>` and so names
            nothing on its own. */}
        <div
          className={styles.loaderRow}
          role="radiogroup"
          aria-labelledby="loader-label"
          onKeyDown={(e) => {
            const next = rovingKey(
              e,
              LOADERS.map((l) => l.id),
            );
            if (next) setLoader(next);
          }}
        >
          {LOADERS.map((l) => (
            <button
              key={l.id}
              className={clsx(styles.loaderChip, loader === l.id && styles.loaderActive)}
              onClick={() => setLoader(l.id)}
              role="radio"
              aria-checked={loader === l.id}
              // One tab stop for the group, as a native radio row has: Tab
              // reaches the chosen loader, and the arrows move from there.
              tabIndex={loader === l.id ? 0 : -1}
              data-roving={l.id}
            >
              <span className={styles.loaderName}>{l.label}</span>
              <span className={styles.loaderBlurb}>{LOADER_BLURB[l.id]}</span>
            </button>
          ))}
        </div>

        {loader !== "vanilla" && (
          <div className={styles.loaderVersion}>
            {/* Named, because the grid above now has enough structure that an
                unlabelled select under it reads as belonging to nothing. */}
            <label className="label" htmlFor="loader-version">
              {LOADERS.find((l) => l.id === loader)?.label} version
            </label>
            {loadingLoader ? (
              <span className={styles.loaderHint}>
                <Loader2 size={13} className="spin" /> Looking up {loader} builds…
              </span>
            ) : loaderProblem ? (
              /* Before the "no builds" branch, which answers for the upstream.
                 Nothing else here re-runs this read — the loader and the
                 Minecraft version are what it is keyed on, and both are
                 already where they need to be — so it carries its own way
                 back. */
              <div>
                <span className={styles.loaderWarn}>
                  Could not read which{" "}
                  {LOADERS.find((l) => l.id === loader)?.label} builds exist for{" "}
                  {selected || "this version"}. Not that there are none.
                </span>
                <p className="hint">{loaderProblem}</p>
                <button
                  className="btn btn-sm"
                  style={{ marginTop: 8 }}
                  onClick={() => setLoaderAsked((n) => n + 1)}
                >
                  Try again
                </button>
              </div>
            ) : loaderVersions.length === 0 ? (
              <span className={styles.loaderWarn}>
                No {LOADERS.find((l) => l.id === loader)?.label} builds for{" "}
                {selected || "this version"}.
              </span>
            ) : (
              <select
                id="loader-version"
                className="select"
                value={loaderVersion}
                onChange={(e) => setLoaderVersion(e.target.value)}
              >
                {loaderVersions.map((v, i) => (
                  <option key={v} value={v}>
                    {v}
                    {i === 0 ? "  (latest)" : ""}
                  </option>
                ))}
              </select>
            )}
          </div>
        )}
      </div>

      <div>
        <div className={styles.versionHead}>
          <span className="label" style={{ margin: 0 }}>
            Minecraft version
          </span>
          {/* Named after the three badges it turns on, rather than after the
              one of them most people are here for: this switch asks for every
              version Mojang publishes, so ticking it also brings back the
              alphas and betas from 2010, which "Show snapshots" alone gave no
              warning of. */}
          <label className={styles.snapshotToggle}>
            <input
              type="checkbox"
              checked={showSnapshots}
              onChange={(e) => setShowSnapshots(e.target.checked)}
            />
            Show snapshots, betas and alphas
          </label>
        </div>

        <div className={styles.searchWrap}>
          <Search size={14} className={styles.searchIcon} />
          <input
            className={clsx("input", styles.searchInput)}
            data-enter="field"
            placeholder="Filter versions"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            // Claimed and spent. `Modal` finishes a dialog on Enter unless the
            // field it was pressed in takes the key, and a filter is the one
            // field here where finishing is the wrong answer — somebody
            // narrowing a list of versions has not decided yet, and Create
            // would make the instance out from under them.
            onKeyDown={(e) => {
              if (e.key === "Enter") e.preventDefault();
            }}
          />
        </div>

        <div ref={listRef} className={styles.versionList}>
          {versions === null ? (
            versionsProblem ? (
              /* Both of these come before "No versions match", which was the
                 only thing this box ever said and so said it while the
                 manifest was still coming and again when it never did. The
                 filter is empty at that point, so it read as a search for
                 nothing that found nothing. */
              <div className={styles.noResults}>
                <p>
                  Could not read Minecraft&apos;s versions. Not that there are
                  none to pick from — the list was not read, so what Mojang
                  publishes is unknown here.
                </p>
                <p className="hint">{versionsProblem}</p>
                <button
                  className="btn btn-sm"
                  style={{ marginTop: 10 }}
                  onClick={() => setVersionsAsked((n) => n + 1)}
                >
                  Try again
                </button>
              </div>
            ) : (
              <div style={{ display: "grid", gap: 6, padding: 8 }}>
                {Array.from({ length: 6 }, (_, i) => (
                  <div key={i} className="skeleton" style={{ height: 26 }} />
                ))}
              </div>
            )
          ) : filtered.length === 0 ? (
            <p className={styles.noResults}>No versions match “{filter}”.</p>
          ) : (
            <div
              style={{ height: virtualizer.getTotalSize(), position: "relative" }}
            >
              {virtualizer.getVirtualItems().map((row) => {
                const version = filtered[row.index];
                return (
                  <button
                    key={version.id}
                    className={clsx(
                      styles.versionRow,
                      selected === version.id && styles.versionActive,
                    )}
                    style={{
                      position: "absolute",
                      top: 0,
                      left: 0,
                      width: "100%",
                      height: row.size,
                      transform: `translateY(${row.start}px)`,
                    }}
                    onClick={() => setSelected(version.id)}
                  >
                    <span className={styles.versionId}>{version.id}</span>
                    {version.kind !== "release" && (
                      <span className="badge">
                        {VERSION_KIND_LABEL[version.kind] ?? version.kind}
                      </span>
                    )}
                    <span className={styles.versionDate}>
                      {version.releaseTime.slice(0, 10)}
                    </span>
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </Modal>
  );
}

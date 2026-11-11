import { useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import { GitBranch, Globe, Loader2, Server as ServerIcon } from "lucide-react";

import Modal from "./Modal";
import { api } from "../lib/api";
import { rovingKey } from "../lib/rovingFocus";
import { useStore } from "../lib/store";
import type { GitStatus, Server, ServerKindInfo } from "../lib/types";

import styles from "./NewServerModal.module.css";

/**
 * The choices a picker can actually tell apart, read off the kind rather than
 * written down here.
 *
 * Fourteen names in one grid is fourteen decisions; what separates them is
 * what the software runs, and `ServerKindInfo` already says. A list of names
 * in this file would be a second copy of a fact that belongs to the software
 * — the thing `ServerKind` exists to hold — and a kind added to the backend
 * would quietly land in the wrong place or nowhere at all.
 */
type Group = "plugins" | "plain" | "mods" | "both" | "proxy";

function groupOf(kind: ServerKindInfo): Group {
  // Asked first, because a proxy takes plugins too and would otherwise read as
  // a server that happens to be tagged.
  if (kind.isProxy) return "proxy";
  if (kind.takesPlugins && kind.takesMods) return "both";
  if (kind.takesPlugins) return "plugins";
  if (kind.takesMods) return "mods";
  return "plain";
}

/**
 * What each group of server software does for you, in the words of somebody
 * who has not hosted one before.
 *
 * Two of these used to say "Bukkit", which is the one insider word on a screen
 * that is otherwise careful not to use any — and it is a lineage, not a
 * capability: somebody who knows it already recognises Paper and Spigot by
 * name, and somebody who does not learns nothing they can act on. A plugin is
 * a plugin. The distinction that matters here is plugins against mods, and
 * that is the distinction these say.
 */
const GROUP_LABEL: Record<Group, string> = {
  plugins: "Runs plugins",
  plain: "Runs no plugins or mods",
  mods: "Runs mods",
  both: "Runs plugins and mods side by side",
  proxy: "Not a server",
};

/**
 * The thing worth knowing about a whole group, said once above its tiles
 * rather than repeated on each of them.
 *
 * Two of these are older than the other two, and which two says a lot: the
 * group to walk past and the group that is not a server at all were explained,
 * and the two somebody actually has to choose between — "Runs plugins" and
 * "Runs mods", eight of the fourteen tiles — were four names apiece and a
 * sentence each about speed. That is the one distinction a newcomer most needs
 * and least knows, and it is not about the software: it is about who has to
 * install something.
 *
 * Both of the new claims come off `ServerKind` rather than from here.
 * `takes_plugins` is loading Bukkit-style plugins out of `plugins/`, which
 * happens inside the server's own process and touches nothing of a player's;
 * `takes_mods` is loading mods out of `mods/`, the kind of thing that has to
 * match at both ends. "Usually" is doing real work in the second one — a
 * server-only mod exists — and saying it flatly would be a promise about
 * somebody else's jar.
 */
const GROUP_NOTE: Record<Group, string | null> = {
  plugins:
    "A plugin runs on the server and nowhere else. Players join with Minecraft exactly as it comes and install nothing, which is why most of what a small server wants — land claims, teleports, moderation — is a plugin.",
  // "Runs no plugins or mods" has nothing left to explain.
  plain: null,
  mods:
    "A mod usually has to be in every player's game as well as on the server, so everyone needs the same list. Sharing the server hands that list to anyone joining through Geode; anybody else assembles it themselves.",
  // The caveat that applies to all three, and the reason not to reach for one
  // by default.
  both:
    "These bolt the two worlds together, and both sides say so: a mod or a plugin that works alone can still break here. Pick one only if you need both at once.",
  // Said before the tile is pressed rather than after. A proxy looks like
  // every other choice here and is the one that cannot be played on.
  proxy:
    "A proxy hosts no world of its own. It stands in front of servers you already have and passes players between them.",
};

/**
 * The kind the dialog opens on, and the one tile marked as the usual answer.
 *
 * One constant for both, because the marker and the preselection disagreeing
 * would be worse than having neither. Paper was already preselected and its
 * blurb already opens "The usual choice" — but a recommendation that is three
 * words inside one of fourteen bodies of copy is not one anybody scanning
 * fourteen tiles will find, so the tile carries a marker as well. What Paper
 * *is* still comes from the kind; this only says which tile to look at first,
 * and "usually" is the whole claim — it is not the only good answer here.
 */
const USUAL = "paper";

export default function NewServerModal({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: (server: Server) => void;
}) {
  const reportError = useStore((s) => s.reportError);
  const refreshServers = useStore((s) => s.refreshServers);

  const [kinds, setKinds] = useState<ServerKindInfo[] | null>(null);
  const [kind, setKind] = useState(USUAL);
  const [versions, setVersions] = useState<string[] | null>(null);
  const [version, setVersion] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);

  // Spigot is the one choice that needs something of the machine. Asked when
  // it is picked, rather than discovered ten minutes into a compile.
  const [git, setGit] = useState<GitStatus | null>(null);
  const [gettingGit, setGettingGit] = useState(false);

  useEffect(() => {
    if (kind !== "spigot" || git?.present) return;
    let cancelled = false;
    api
      .gitStatus()
      .then((status) => !cancelled && setGit(status))
      .catch(() => !cancelled && setGit(null));
    return () => {
      cancelled = true;
    };
  }, [kind, git?.present]);

  async function getGit() {
    setGettingGit(true);
    try {
      setGit(await api.installGit());
    } catch (e) {
      reportError("Could not install Git", e);
      // Whatever happened, the question is still what is on the machine now.
      await api.gitStatus().then(setGit).catch(() => {});
    } finally {
      setGettingGit(false);
    }
  }

  const gitMissing = kind === "spigot" && git !== null && !git.present;

  useEffect(() => {
    api
      .serverKinds()
      .then(setKinds)
      .catch((e) => reportError("Could not list server types", e));
  }, [reportError]);

  const chosen = useMemo(
    () => kinds?.find((k) => k.id === kind),
    [kinds, kind],
  );

  // Groups in the order their first member arrives, so the order a picker
  // should offer them stays `ServerKind::ALL`'s decision rather than becoming
  // a second one here.
  const groups = useMemo(() => {
    const out: { key: Group; kinds: ServerKindInfo[] }[] = [];
    for (const option of kinds ?? []) {
      const key = groupOf(option);
      const group = out.find((g) => g.key === key);
      if (group) group.kinds.push(option);
      else out.push({ key, kinds: [option] });
    }
    return out;
  }, [kinds]);

  /**
   * Every kind in the order the screen offers them, flattened back out of the
   * groups.
   *
   * The groups are how a person reads this list and the arrow keys are how the
   * keyboard crosses it, and those are not the same shape: stopping at the end
   * of "Runs mods" would strand somebody four tiles from the proxies with no
   * key that reaches them. So the keyboard walks all fourteen and the headings
   * are announced on the way past.
   */
  const order = useMemo(
    () => groups.flatMap((g) => g.kinds.map((k) => k.id)),
    [groups],
  );
  // The group is one tab stop. It lands on the chosen tile, or on the first
  // one when the chosen kind is not in the list yet — without that fallback a
  // list still loading has no stop at all and Tab goes straight past it.
  const stop = order.includes(kind) ? kind : order[0];

  // Versions belong to the software, not to Minecraft: Velocity's are its own.
  useEffect(() => {
    let cancelled = false;
    setVersions(null);
    api
      .serverVersions(kind)
      .then((list) => {
        if (cancelled) return;
        setVersions(list);
        // Nothing preselected, and the newest least of all. This took
        // `list[0]`, which is the newest the upstream publishes — and on the
        // day a version ships that is the one version nobody's friends are on
        // yet, so the dialog answered the question it could not possibly know
        // the answer to. It is also the only choice here that cannot be taken
        // back: `ServerPatch` carries no version, nothing in Geode moves a
        // server from one to another, and Minecraft upgrades a world the first
        // time a newer version opens it and never puts it back. So it is
        // asked. Create is already disabled while this is empty.
        setVersion("");
      })
      .catch(() => !cancelled && setVersions([]));
    return () => {
      cancelled = true;
    };
  }, [kind]);

  const suggested = chosen ? `${chosen.name} ${version}`.trim() : "";

  async function create() {
    setBusy(true);
    try {
      const server = await api.createServer(name.trim() || suggested, kind, version);
      await refreshServers();
      onCreated(server);
    } catch (e) {
      // The folder survives a failed download, so refresh either way.
      await refreshServers();
      reportError("Could not create the server", e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      title="New server"
      subtitle="Pick the software, then the version it runs."
      onClose={onClose}
      width={520}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn btn-primary"
            onClick={create}
            disabled={busy || !version || gitMissing}
          >
            {busy ? (
              <Loader2 size={14} className="spin" />
            ) : (
              <ServerIcon size={14} />
            )}
            Create server
          </button>
        </>
      }
    >
      {kinds === null ? (
        <div className={styles.loading}>
          <Loader2 size={16} className="spin" />
        </div>
      ) : (
        <>
          {/* A choice of one from fourteen, in five labelled groups — which is
              what `<select>` with `<optgroup>` is, so it carries those roles
              rather than a flat radio group's: a radiogroup owns radios and
              has nowhere to put "Runs Bukkit-style plugins". Each tile reads
              out its own sentence as part of its name, which is the whole
              reason the sentences are there. */}
          <div
            className={styles.groups}
            role="listbox"
            aria-label="Server software"
            onKeyDown={(e) => {
              const next = rovingKey(e, order);
              if (next) setKind(next);
            }}
          >
            {groups.map((group) => (
              <div
                key={group.key}
                role="presentation"
                className={clsx(group.key === "proxy" && styles.apart)}
              >
                <p className={styles.groupLabel} id={`kinds-${group.key}`}>
                  {GROUP_LABEL[group.key]}
                </p>
                {GROUP_NOTE[group.key] && (
                  <p className={styles.groupNote}>{GROUP_NOTE[group.key]}</p>
                )}
                {/* The group wraps the tiles alone. Its heading and its note
                    sit outside it and name it by id, so the listbox owns
                    options and groups of options and nothing else. */}
                <div
                  className={styles.kinds}
                  role="group"
                  aria-labelledby={`kinds-${group.key}`}
                >
                  {group.kinds.map((option) => (
                    <button
                      key={option.id}
                      role="option"
                      aria-selected={kind === option.id}
                      data-roving={option.id}
                      tabIndex={stop === option.id ? 0 : -1}
                      className={clsx(
                        styles.kind,
                        kind === option.id && styles.kindActive,
                      )}
                      onClick={() => setKind(option.id)}
                    >
                      <span className={styles.kindHead}>
                        <span className={styles.kindName}>{option.name}</span>
                        {/* Part of the tile's name to a screen reader as well,
                            which is the same bargain the blurb already makes:
                            this is the one option the list is steering toward
                            and that should not be a thing only sighted
                            scanning can find. */}
                        {option.id === USUAL && (
                          <span className="badge badge-accent">
                            Usually this one
                          </span>
                        )}
                      </span>
                      <span className={styles.kindBlurb}>{option.blurb}</span>
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </div>

            <div className={styles.row}>
              <div className={styles.field}>
                <label className="label" htmlFor="server-version">
                  {chosen?.isProxy ? "Version" : "Minecraft version"}
                </label>
                {versions === null ? (
                  <div className="skeleton" style={{ height: 34 }} />
                ) : versions.length === 0 ? (
                  <p className={styles.note}>No versions available.</p>
                ) : (
                  <select
                    id="server-version"
                    className="select"
                    value={version}
                    onChange={(e) => setVersion(e.target.value)}
                  >
                    <option value="">Choose a version</option>
                    {versions.map((v) => (
                      <option key={v} value={v}>
                        {v}
                      </option>
                    ))}
                  </select>
                )}
              </div>
              <div className={styles.field}>
                <label className="label" htmlFor="server-name">
                  Name
                </label>
                <input
                  id="server-name"
                  className="input"
                  value={name}
                  placeholder={suggested}
                  onChange={(e) => setName(e.target.value)}
                />
              </div>
            </div>

            {/* Under the row rather than under the select, because the row is
                two fields wide and a four-line hint inside one of them leaves
                the other hanging at the top of it. Only for a server anyone
                plays on: a proxy's version is the proxy software's own, which
                is what the label above already says, and no player has to
                match it. */}
            {chosen && !chosen.isProxy && (
              <p className={styles.note}>
                Everyone who joins has to be on this same version, and nothing
                moves a server to another one later. Pick what your friends
                already play rather than the newest.
              </p>
            )}

            {/* The promise the Servers screen makes is "a server your friends
                can join", and until now the whole subject of how a friend
                outside the house reaches it lived in Settings → Server
                sharing — a pane about Geode's *pack* port, written for
                somebody who already knows what a tunnel is, and no reason for
                a first-time host to ever open. They found out when a friend
                said it would not connect.

                So it is said here, where the server is made, and said about
                the port that actually matters. `portmap.rs` opens
                `ShareConfig::port` (25590), which is the port packs come
                down; the game is on the server's own `server-port` and
                nothing in Geode opens it. Not a warning and not a promise
                either — `portmap.rs` is explicit that a router saying yes is
                not players connecting, so this says what has to be true and
                who has to do it. */}
            <div className={styles.needs}>
              <Globe size={15} />
              <div className={styles.needsBody}>
                <strong>
                  Friends outside your own network need a port opened.
                </strong>
                <p>
                  Anyone on this network can join as soon as the server is
                  running. Anyone further away needs Minecraft&apos;s port
                  forwarded to this computer on your router, or handed to a
                  tunnel such as playit.gg — the port Geode opens on your
                  router carries packs, not the game. Nothing here can test it
                  from outside, so the first real check is a friend connecting.
                </p>
              </div>
            </div>

            {/* What the software runs is the heading above the tile now, so it
                is not repeated here. The EULA is: a proxy writes no eula.txt
                and is never asked for one (`ServerKind::needs_eula`), and this
                claimed otherwise for the two kinds it is not true of. */}
            {chosen && !chosen.isProxy && (
              <p className={styles.note}>
                You will be asked to accept Mojang&apos;s EULA before it starts
                for the first time.
              </p>
            )}

            {/* These two take noticeably longer than a download, and saying so
                beforehand is the difference between waiting and wondering. */}
            {chosen?.id === "spigot" && (
              <p className={styles.note}>
                Spigot is compiled here on your machine — its licence does not
                allow anyone to hand out a finished jar. The first build takes
                several minutes; later ones reuse the same work and are faster.
              </p>
            )}
            {gitMissing && (
              <div className={styles.needs}>
                <GitBranch size={15} />
                <div className={styles.needsBody}>
                  <strong>Compiling Spigot needs Git, and it is not here.</strong>
                  <p>{git.plan}</p>
                  {/* Shown in full before it runs, because on Linux and macOS
                      this installs software outside Geode. */}
                  {git.command && <code className={styles.command}>{git.command}</code>}
                  <div className={styles.needsActions}>
                    {git.automatic && (
                      <button
                        className="btn btn-primary btn-sm"
                        onClick={getGit}
                        disabled={gettingGit}
                      >
                        {gettingGit && <Loader2 size={13} className="spin" />}
                        {gettingGit ? "Installing…" : "Install Git"}
                      </button>
                    )}
                    <button
                      className="btn btn-sm"
                      onClick={() => void api.gitStatus().then(setGit)}
                      disabled={gettingGit}
                    >
                      Check again
                    </button>
                  </div>
                  <p className={styles.needsAside}>
                    {git.manual} Or pick Paper, which runs every Spigot plugin
                    and installs without building anything.
                  </p>
                </div>
              </div>
            )}
            {(chosen?.id === "forge" ||
              chosen?.id === "neoforge" ||
              chosen?.id === "quilt") && (
              <p className={styles.note}>
                {chosen.name} runs its official installer to assemble the
                server and fetch its libraries, which takes a minute or two.
              </p>
            )}
        </>
      )}
    </Modal>
  );
}

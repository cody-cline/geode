import { useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import clsx from "clsx";
import {
  AlertTriangle,
  Cpu,
  Loader2,
  Play,
  Server as ServerIcon,
  Search,
  Square,
  Download,
} from "lucide-react";

import InstanceIcon from "../components/InstanceIcon";
import NewServerModal from "../components/NewServerModal";
import { actionHint } from "../lib/shortcuts";
import { useActionShortcut } from "../lib/useActionShortcut";
import TransferServerModal from "../components/TransferServerModal";
import { api } from "../lib/api";
import { hashHue, relativeTime } from "../lib/format";
import { OLD_ADDRESS_LABEL, linksPointAtOldAddress } from "../lib/share";
import { term } from "../lib/glossary";
import { useStore } from "../lib/store";
import type { Server } from "../lib/types";

import { cardKeys, pressLaunch } from "./Library";
// The Library's stylesheet, deliberately. A server is a profile with a console
// attached, and the two lists should not be able to drift apart — which is
// the same reason the grid's keyboard is imported from there on the line
// above rather than written again here.
import styles from "./Library.module.css";

type Sort = "recent" | "name" | "kind";

export default function Servers() {
  const servers = useStore((s) => s.servers);
  const loaded = useStore((s) => s.serversLoaded);
  const loadError = useStore((s) => s.serversError);
  const refreshServers = useStore((s) => s.refreshServers);
  const navigate = useStore((s) => s.navigate);

  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<Sort>("recent");
  const [creating, setCreating] = useState(false);

  // Which card the keyboard is on, so Tab comes back to the one it left
  // rather than to the top of the grid. Checked against the list below rather
  // than trusted, because it outlives the search it was set under.
  const [keyed, setKeyed] = useState<string | null>(null);
  const searchBox = useRef<HTMLInputElement>(null);

  useActionShortcut("newThing", (e) => {
    e.preventDefault();
    setCreating(true);
  });

  /**
   * Ctrl+F reaches this screen's search box, as it does Library's and every
   * filter box inside an instance or a server.
   *
   * Only once there is something to search: the box is not rendered until
   * there is a server, and a key that focused nothing would cost the webview's
   * own find bar and give nothing back.
   */
  useActionShortcut("focusFilter", (e) => {
    const box = searchBox.current;
    if (!box) return;
    e.preventDefault();
    box.focus();
    box.select();
  });

  useActionShortcut("launch", pressLaunch);
  const [bringing, setBringing] = useState(false);

  useEffect(() => {
    void refreshServers();
  }, [refreshServers]);

  const shown = useMemo(() => {
    const q = search.trim().toLowerCase();
    const filtered = q
      ? servers.filter(
          (s) =>
            s.name.toLowerCase().includes(q) ||
            s.kindName.toLowerCase().includes(q) ||
            s.mcVersion.includes(q),
        )
      : servers;

    const sorted = [...filtered];
    if (sort === "name") {
      sorted.sort((a, b) => a.name.localeCompare(b.name));
    } else if (sort === "kind") {
      sorted.sort(
        (a, b) =>
          a.kindName.localeCompare(b.kindName) || a.name.localeCompare(b.name),
      );
    }
    // "recent" is the backend's own ordering, so leave it alone.
    return sorted;
  }, [servers, search, sort]);

  // The one card that answers Tab.
  const stop = shown.some((s) => s.id === keyed) ? keyed : shown[0]?.id;

  return (
    <>
      <header className={styles.header}>
        <div>
          <h1 className={styles.heading}>Servers</h1>
          {/* What a server is, kept on screen instead of only in the empty
              state — the same thing Plans and Library do, and out of the
              same one copy the empty state below reads. Only the first
              sentence: the rest of what that state says is about making one,
              which is not what somebody looking at a list of them needs. */}
          <p className={styles.lede}>{term("Server").short}</p>
        </div>
        <div className={styles.headerTools}>
          {/* Nothing to search or sort until there is a server, and a live
              control over an empty page is a control that cannot work. */}
          {servers.length > 0 && (
            <>
              <div className={styles.searchWrap}>
                <Search size={14} className={styles.searchIcon} />
                <input
                  ref={searchBox}
                  className={clsx("input", styles.search)}
                  placeholder="Search servers"
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                />
                {/* The binding, printed in the box it reaches — the same hint
                    in the same corner as Library's and as the four filter
                    boxes deeper in. */}
                <kbd className={styles.searchKey}>{actionHint("focusFilter")}</kbd>
              </div>
              <select
                className="select"
                style={{ width: 140 }}
                value={sort}
                onChange={(e) => setSort(e.target.value as Sort)}
                aria-label="Sort servers"
              >
                <option value="recent">Recently run</option>
                <option value="name">Name</option>
                <option value="kind">Software</option>
              </select>
            </>
          )}
          {/* Beside New server rather than buried in Settings: bringing a
              server over from another machine is one of the two ways a server
              appears in this list.

              "From a host" was what it said, which is a word this app uses for
              a machine running `geoded` and the rest of the world uses for a
              company that rents you one. Somebody who has never heard of the
              first reads the second and presses it expecting to be sold
              something. */}
          <button
            className="btn"
            onClick={() => setBringing(true)}
            title="Move or copy a server off another computer that is running Geode's headless host"
          >
            <Download size={14} />
            From another machine
          </button>
          <button
            className={clsx("btn", servers.length > 0 && "btn-primary")}
            onClick={() => setCreating(true)}
          >
            New server
            <kbd className={styles.newKey}>{actionHint("newThing")}</kbd>
          </button>
        </div>
      </header>

      <div className={styles.scroll}>
        {!loaded ? (
          <div className={styles.grid}>
            {Array.from({ length: 6 }, (_, i) => (
              <div key={i} className="skeleton" style={{ height: 148 }} />
            ))}
          </div>
        ) : loadError && servers.length === 0 ? (
          /* The same order as Library, for the same reason: an unread list is
             not an empty one. */
          <div className={styles.empty}>
            <AlertTriangle size={40} className={styles.emptyIcon} />
            <h2>Could not read your servers</h2>
            <p>
              Which is not the same as having none. Nothing has been changed or
              removed — the list could not be read, so what is in it is unknown.
            </p>
            <p className={styles.why}>{loadError}</p>
            <button className="btn" onClick={() => void refreshServers()}>
              Try again
            </button>
          </div>
        ) : servers.length === 0 ? (
          <div className={styles.empty}>
            <ServerIcon size={40} className={styles.emptyIcon} />
            <h2>No servers yet</h2>
            {/* All of it for somebody who has never hosted a server, because
                that is who is reading an empty one. It opened for them and
                finished for an administrator — "the hybrids that take both,
                and the proxies that sit in front of them" — which reads as
                evidence the screen is not for you. The taxonomy is already
                explained a tile at a time inside the dialog this leads to,
                where there is room to say what each kind is and where nobody
                meets it before they have asked for it.

                The first sentence used to be "A server your friends can join,
                running on this machine", and nothing anywhere said that a
                friend outside the house cannot reach it until the network is
                dealt with — which they found out when somebody said it would
                not connect. It is one clause now rather than a lecture: an
                empty screen is where a promise is made, and the dialog this
                button opens is where the two ways of keeping it belong. */}
            <p>
              {term("Server").short} {term("Server").more} Creating one asks
              which software to run and says what each kind is for, so there is
              nothing to look up first.
            </p>
            <button className="btn btn-primary" onClick={() => setCreating(true)}>
              Create your first server
            </button>
          </div>
        ) : shown.length === 0 ? (
          <p className={styles.noResults}>Nothing matches “{search}”.</p>
        ) : (
          /* The same grid Library declares and the keyboard that goes with
             it — see `cardKeys` there. The two lists are one shape and now
             answer one set of keys. */
          <div
            className={styles.grid}
            role="grid"
            aria-label="Servers"
            onKeyDown={(e) => cardKeys(e, (id) => navigate({ view: "server", id }))}
            onFocus={(e) => {
              const card = (e.target as HTMLElement).closest<HTMLElement>(
                "[data-row]",
              );
              if (card?.dataset.row) setKeyed(card.dataset.row);
            }}
          >
            {shown.map((server) => (
              <ServerCard
                key={server.id}
                server={server}
                stop={server.id === stop}
                onOpen={() => navigate({ view: "server", id: server.id })}
              />
            ))}
          </div>
        )}
      </div>

      {creating && (
        <NewServerModal
          onClose={() => setCreating(false)}
          onCreated={(server) => {
            setCreating(false);
            navigate({ view: "server", id: server.id });
          }}
        />
      )}

      {bringing && (
        <TransferServerModal direction="pull" onClose={() => setBringing(false)} />
      )}
    </>
  );
}

function ServerCard({
  server,
  stop,
  onOpen,
}: {
  server: Server;
  stop: boolean;
  onOpen: () => void;
}) {
  const refreshServers = useStore((s) => s.refreshServers);
  const reportError = useStore((s) => s.reportError);
  // Two job ids, because a start can be two jobs. `server:` is the start's
  // own, and the one the installs done from this server's screens report
  // under. Before it, a start that finds the software gone fetches it again
  // under `server-install:` (`install_if_the_software_is_gone` in
  // `server/run.rs`), which is the bigger of the two by far and had nothing
  // reading its id: the card went on printing when the server was last
  // started, with Start still inviting another press, through a 49 MB
  // download. They are sequential rather than concurrent, so the only moment
  // both are in the map is the second a finished one lingers there, and the
  // start's own is the live one by then.
  const progress = useStore(
    (s) =>
      s.progress[`server:${server.id}`] ??
      s.progress[`server-install:${server.id}`],
  );
  const shareStatus = useStore((s) => s.shareStatus);

  const [busy, setBusy] = useState(false);
  const hue = hashHue(server.id);
  const installing = Boolean(progress) && !server.running;

  async function toggle() {
    if (!server.running) {
      // Starting may need the EULA accepted, which is a question rather than a
      // step, so it is asked inside the server's own view.
      onOpen();
      return;
    }
    setBusy(true);
    try {
      await api.stopServer(server.id);
      await refreshServers();
    } catch (e) {
      reportError(`Could not stop ${server.name}`, e);
    } finally {
      setBusy(false);
    }
  }

  const contentCount = server.contentCount;

  return (
    <article
      className={clsx(styles.card, server.running && styles.cardRunning)}
      role="row"
      data-row={server.id}
      tabIndex={stop ? 0 : -1}
    >
      {/* An uploaded banner if there is one, otherwise the generated wash —
          the same branch an instance without artwork takes, which keeps one
          silhouette across the grid. A span rather than a button for the
          reason the library card's is one. */}
      <span className={styles.art} onClick={onOpen} aria-hidden="true">
        {server.bannerPath ? (
          <img src={convertFileSrc(server.bannerPath)} alt="" loading="lazy" />
        ) : (
          <span
            className={styles.artFallback}
            style={{
              background: `linear-gradient(135deg, hsl(${hue} 46% 34%), hsl(${(hue + 55) % 360} 44% 20%))`,
            }}
          />
        )}
        <span className={styles.artScrim} />
      </span>

      {/* The card's two cells — what the thing is, and what you can do
          about it — which is the same two on every card in the grid.
          `role` goes on a wrapper rather than on the button inside it,
          because putting it on the button would take the button's own
          role away; the same place `ProjectCard` puts its two. */}
      <div className={styles.bodyCell} role="gridcell">
        <button className={styles.cardBody} onClick={onOpen} tabIndex={-1}>
          <span className={styles.iconRing}>
            <InstanceIcon
              instance={{
                id: server.id,
                name: server.name,
                iconPath: server.iconPath,
              }}
              size={46}
            />
          </span>
          {/* Spans for the same reason as the library card: this is inside a
              button, where block elements are not allowed. */}
          <span className={styles.cardText}>
            <span className={styles.cardName}>{server.name}</span>
            <span className={styles.cardMeta}>
              {server.mcVersion} · {server.kindName}
            </span>
            <span className={styles.cardStats}>
              {contentCount > 0 && (
                <span className={styles.stat}>
                  {contentCount} item{contentCount === 1 ? "" : "s"}
                </span>
              )}
              <span className={styles.stat}>
                <Cpu size={11} />
                {server.settings.memoryMb} MB
              </span>
              {/* Among the server's other facts rather than over its artwork, so
                  the grid keeps one silhouette. Clicking it opens the server,
                  which is where the link to send instead is. */}
              {linksPointAtOldAddress(shareStatus, server.lastLinkAddress) && (
                <span
                  className="badge badge-warn"
                  role="img"
                  title={OLD_ADDRESS_LABEL}
                  aria-label={OLD_ADDRESS_LABEL}
                >
                  <AlertTriangle size={11} aria-hidden="true" />
                  Old address
                </span>
              )}
            </span>
          </span>
        </button>
      </div>

      <div className={styles.cardFoot} role="gridcell">
        <span className={styles.lastPlayed}>
          {server.running
            ? "Running"
            : installing
              ? (progress?.stage ?? "Installing…")
              : !server.installed
                ? "Not installed"
                : server.lastStarted
                  ? /* "Last started", the words the server's own header uses
                       for this fact — a server is started where an instance
                       is played, but the shape is one shape. The card said
                       "Ran 3 days ago" to the header's "Last started 3 days
                       ago".

                       Words alone, as the library card's footer is. A clock
                       sat here and on none of this slot's other four states,
                       so the text moved sideways as a server was installed or
                       started; and it sat on the server card and not on the
                       instance card beside it in the sidebar, where the same
                       slot says the same kind of thing at the same height.
                       Dropping it settles both — and the instance card
                       already spends a clock on the playtime above, which is
                       one per card. */
                    `Last started ${relativeTime(server.lastStarted)}`
                  : "Never started"}
        </span>

        {server.running ? (
          <button
            className="btn btn-sm btn-danger"
            onClick={() => void toggle()}
            disabled={busy}
            tabIndex={-1}
          >
            {busy ? (
              <Loader2 size={12} className="spin" />
            ) : (
              <Square size={12} fill="currentColor" />
            )}
            Stop
          </button>
        ) : (
          <button
            className="btn btn-sm btn-primary"
            onClick={() => void toggle()}
            disabled={busy || installing || !server.installed}
            tabIndex={-1}
            data-launch
            title={
              server.installed
                ? undefined
                : "This server has no software installed yet"
            }
          >
            {busy || installing ? (
              <Loader2 size={12} className="spin" />
            ) : (
              <Play size={12} fill="currentColor" />
            )}
            Start
            {/* The key, printed in the button it presses — and only while the
                keyboard is in this card, for the reason the library card's
                Play button gives. */}
            <kbd className={styles.launchKey}>{actionHint("launch")}</kbd>
          </button>
        )}
      </div>
    </article>
  );
}

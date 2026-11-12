import { useEffect, useMemo, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import clsx from "clsx";
import {
  AlertTriangle,
  Clock,
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
import { useStore } from "../lib/store";
import type { Server } from "../lib/types";

// The Library's stylesheet, deliberately. A server is a profile with a console
// attached, and the two lists should not be able to drift apart.
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

  useActionShortcut("newThing", (e) => {
    e.preventDefault();
    setCreating(true);
  });
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

  return (
    <>
      <header className={styles.header}>
        <h1 className={styles.heading}>Servers</h1>
        <div className={styles.headerTools}>
          {/* Nothing to search or sort until there is a server, and a live
              control over an empty page is a control that cannot work. */}
          {servers.length > 0 && (
            <>
              <div className={styles.searchWrap}>
                <Search size={14} className={styles.searchIcon} />
                <input
                  className={clsx("input", styles.search)}
                  placeholder="Search servers"
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                />
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
              A server running on this machine: anyone on your own network can
              join it, and friends further away once its port is open to the
              internet. Geode downloads the software, sets it up, and gives you
              a console to start and stop it from. Creating one asks which
              software to run and says what each kind is for, so there is
              nothing to look up first.
            </p>
            <button className="btn btn-primary" onClick={() => setCreating(true)}>
              Create your first server
            </button>
          </div>
        ) : shown.length === 0 ? (
          <p className={styles.noResults}>Nothing matches “{search}”.</p>
        ) : (
          <div className={styles.grid}>
            {shown.map((server) => (
              <ServerCard
                key={server.id}
                server={server}
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
  onOpen,
}: {
  server: Server;
  onOpen: () => void;
}) {
  const refreshServers = useStore((s) => s.refreshServers);
  const reportError = useStore((s) => s.reportError);
  const progress = useStore((s) => s.progress[`server:${server.id}`]);
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

  const contentCount = server.content.length;

  return (
    <article className={clsx(styles.card, server.running && styles.cardRunning)}>
      {/* An uploaded banner if there is one, otherwise the generated wash —
          the same branch an instance without artwork takes, which keeps one
          silhouette across the grid. */}
      <button
        className={styles.art}
        onClick={onOpen}
        tabIndex={-1}
        aria-hidden="true"
      >
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
      </button>

      <button className={styles.cardBody} onClick={onOpen}>
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

      <div className={styles.cardFoot}>
        <span className={styles.lastPlayed}>
          {server.running ? (
            "Running"
          ) : installing ? (
            (progress?.stage ?? "Installing…")
          ) : !server.installed ? (
            "Not installed"
          ) : server.lastStarted ? (
            // "Last started", the words the server's own header uses for this
            // fact — a server is started where an instance is played, but the
            // shape is one shape. The card said "Ran 3 days ago" to the
            // header's "Last started 3 days ago".
            <>
              <Clock size={11} /> Last started {relativeTime(server.lastStarted)}
            </>
          ) : (
            "Never started"
          )}
        </span>

        {server.running ? (
          <button className="btn btn-sm btn-danger" onClick={() => void toggle()} disabled={busy}>
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
          </button>
        )}
      </div>
    </article>
  );
}

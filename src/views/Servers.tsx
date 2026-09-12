import { useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import {
  Clock,
  Cpu,
  Loader2,
  Play,
  Server as ServerIcon,
  Search,
  Square,
} from "lucide-react";

import InstanceIcon from "../components/InstanceIcon";
import NewServerModal from "../components/NewServerModal";
import { api } from "../lib/api";
import { hashHue, relativeTime } from "../lib/format";
import { useStore } from "../lib/store";
import type { Server } from "../lib/types";

// The Library's stylesheet, deliberately. A server is a profile with a console
// attached, and the two lists should not be able to drift apart.
import styles from "./Library.module.css";

type Sort = "recent" | "name" | "kind";

export default function Servers() {
  const servers = useStore((s) => s.servers);
  const loaded = useStore((s) => s.serversLoaded);
  const refreshServers = useStore((s) => s.refreshServers);
  const navigate = useStore((s) => s.navigate);

  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<Sort>("recent");
  const [creating, setCreating] = useState(false);

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
          <button className="btn btn-primary" onClick={() => setCreating(true)}>
            New server
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
        ) : servers.length === 0 ? (
          <div className={styles.empty}>
            <ServerIcon size={40} className={styles.emptyIcon} />
            <h2>No servers yet</h2>
            <p>
              Geode sets up Paper, Purpur, Folia, Spigot, Fabric, Quilt, Forge,
              NeoForge, the hybrids and the proxies — finding the build, putting
              it in place, and giving you a console to run it from.
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
      {/* A server has no artwork of its own yet, so it always takes the
          generated wash — the same branch an instance without a banner uses,
          which keeps one silhouette across the grid. */}
      <button
        className={styles.art}
        onClick={onOpen}
        tabIndex={-1}
        aria-hidden="true"
      >
        <span
          className={styles.artFallback}
          style={{
            background: `linear-gradient(135deg, hsl(${hue} 46% 34%), hsl(${(hue + 55) % 360} 44% 20%))`,
          }}
        />
        <span className={styles.artScrim} />
      </button>

      <button className={styles.cardBody} onClick={onOpen}>
        <span className={styles.iconRing}>
          <InstanceIcon
            instance={{ id: server.id, name: server.name }}
            size={46}
          />
        </span>
        <div className={styles.cardText}>
          <h3 className={styles.cardName}>{server.name}</h3>
          <p className={styles.cardMeta}>
            {server.mcVersion} · {server.kindName}
          </p>
          <div className={styles.cardStats}>
            {contentCount > 0 && (
              <span className={styles.stat}>
                {contentCount} item{contentCount === 1 ? "" : "s"}
              </span>
            )}
            <span className={styles.stat}>
              <Cpu size={11} />
              {server.settings.memoryMb} MB
            </span>
          </div>
        </div>
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
            <>
              <Clock size={11} /> Ran {relativeTime(server.lastStarted)}
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

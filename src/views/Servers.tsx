import { useEffect, useState } from "react";
import clsx from "clsx";
import {
  Loader2,
  Play,
  Plus,
  Server as ServerIcon,
  Square,
  Trash2,
} from "lucide-react";

import NewServerModal from "../components/NewServerModal";
import { api } from "../lib/api";
import { relativeTime } from "../lib/format";
import { useStore } from "../lib/store";
import type { Server } from "../lib/types";

import styles from "./Servers.module.css";

export default function Servers() {
  const servers = useStore((s) => s.servers);
  const loaded = useStore((s) => s.serversLoaded);
  const refreshServers = useStore((s) => s.refreshServers);
  const navigate = useStore((s) => s.navigate);
  const reportError = useStore((s) => s.reportError);

  const [creating, setCreating] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    void refreshServers();
  }, [refreshServers]);

  async function remove(server: Server) {
    try {
      await api.deleteServer(server.id);
      await refreshServers();
    } catch (e) {
      reportError(`Could not delete ${server.name}`, e);
    }
  }

  async function toggle(server: Server) {
    setBusy(server.id);
    try {
      if (server.running) {
        await api.stopServer(server.id);
      } else {
        // Starting needs the EULA, which is a question rather than a step, so
        // it is asked inside the server's own view.
        navigate({ view: "server", id: server.id });
        return;
      }
      await refreshServers();
    } catch (e) {
      reportError(`Could not stop ${server.name}`, e);
    } finally {
      setBusy(null);
    }
  }

  return (
    <>
      <header className={styles.header}>
        <h1 className={styles.heading}>Servers</h1>
        <button className="btn btn-primary" onClick={() => setCreating(true)}>
          <Plus size={14} />
          New server
        </button>
      </header>

      <div className={styles.scroll}>
        {!loaded ? (
          <div className={styles.grid}>
            {Array.from({ length: 3 }, (_, i) => (
              <div key={i} className="skeleton" style={{ height: 108 }} />
            ))}
          </div>
        ) : servers.length === 0 ? (
          <div className={styles.empty}>
            <ServerIcon size={40} className={styles.emptyIcon} />
            <h2>No servers yet</h2>
            <p>
              Geode can set up Paper, Purpur, Folia, Fabric, Vanilla, Mohist and
              the proxies — finding the build, putting the jar in place, and
              giving you a console to run it from.
            </p>
            <button className="btn btn-primary" onClick={() => setCreating(true)}>
              <Plus size={14} />
              Create a server
            </button>
          </div>
        ) : (
          <div className={styles.grid}>
            {servers.map((server) => (
              <article
                key={server.id}
                className={clsx(styles.card, server.running && styles.cardRunning)}
              >
                <button
                  className={styles.body}
                  onClick={() => navigate({ view: "server", id: server.id })}
                >
                  <span className={styles.name}>{server.name}</span>
                  <span className={styles.meta}>
                    {server.kind} {server.mcVersion}
                    {server.build && ` · build ${server.build}`}
                  </span>
                  <span className={styles.state}>
                    {server.running
                      ? "Running"
                      : server.lastStarted
                        ? `Last run ${relativeTime(server.lastStarted)}`
                        : "Never started"}
                  </span>
                </button>

                <div className={styles.foot}>
                  <button
                    className={clsx(
                      "btn btn-sm",
                      server.running ? "btn-danger" : "btn-primary",
                    )}
                    onClick={() => void toggle(server)}
                    disabled={busy === server.id}
                  >
                    {busy === server.id ? (
                      <Loader2 size={12} className="spin" />
                    ) : server.running ? (
                      <Square size={12} fill="currentColor" />
                    ) : (
                      <Play size={12} fill="currentColor" />
                    )}
                    {server.running ? "Stop" : "Start"}
                  </button>
                  <button
                    className={styles.remove}
                    onClick={() => void remove(server)}
                    disabled={server.running}
                    title={
                      server.running
                        ? "Stop it before deleting"
                        : `Delete ${server.name}`
                    }
                    aria-label={`Delete ${server.name}`}
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              </article>
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

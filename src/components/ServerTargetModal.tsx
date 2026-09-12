import { useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Check, Loader2, Server as ServerIcon } from "lucide-react";

import Modal from "./Modal";
import { api } from "../lib/api";
import { useStore } from "../lib/store";
import type { Project, Server, ServerKindInfo } from "../lib/types";

import styles from "./ServerTargetModal.module.css";

/**
 * Where a plugin should go.
 *
 * Servers that cannot load plugins are shown greyed rather than hidden: a
 * Fabric server missing from the list looks like a bug, whereas one that says
 * why explains the whole model in a line.
 */
export default function ServerTargetModal({
  project,
  busy,
  onPick,
  onClose,
}: {
  project: Project;
  busy: boolean;
  onPick: (server: Server) => void;
  onClose: () => void;
}) {
  const servers = useStore((s) => s.servers);
  const [kinds, setKinds] = useState<ServerKindInfo[] | null>(null);
  const [selected, setSelected] = useState<string>("");

  useEffect(() => {
    api
      .serverKinds()
      .then(setKinds)
      .catch(() => setKinds([]));
  }, []);

  const takesPlugins = useMemo(() => {
    const byId = new Map((kinds ?? []).map((k) => [k.id, k]));
    // Before the list arrives, assume yes rather than greying everything out
    // for a moment.
    return (server: Server) => byId.get(server.kind)?.takesPlugins ?? true;
  }, [kinds]);

  // Somewhere sensible to start: the first server that can actually take it.
  const ordered = useMemo(
    () =>
      [...servers].sort(
        (a, b) => Number(takesPlugins(b)) - Number(takesPlugins(a)),
      ),
    [servers, takesPlugins],
  );

  useEffect(() => {
    if (!selected) {
      const first = ordered.find(takesPlugins) ?? ordered[0];
      if (first) setSelected(first.id);
    }
  }, [ordered, selected, takesPlugins]);

  const target = servers.find((s) => s.id === selected);
  const usable = target ? takesPlugins(target) : false;

  return (
    <Modal
      title={`Install ${project.name}`}
      subtitle="Choose which server it goes on."
      onClose={onClose}
      width={460}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn btn-primary"
            onClick={() => target && onPick(target)}
            disabled={!target || !usable || busy}
          >
            {busy && <Loader2 size={14} className="spin" />}
            {target ? `Install into ${target.name}` : "Install"}
          </button>
        </>
      }
    >
      {servers.length === 0 ? (
        <p className={styles.note}>
          You have no servers yet. Make one from the Servers section and its
          plugins folder will be waiting.
        </p>
      ) : (
        <div className={styles.list}>
          {ordered.map((server) => {
            const ok = takesPlugins(server);
            return (
              <button
                key={server.id}
                className={clsx(
                  styles.row,
                  !ok && styles.unusable,
                  selected === server.id && styles.selected,
                )}
                onClick={() => setSelected(server.id)}
                onDoubleClick={() => ok && onPick(server)}
              >
                <ServerIcon size={17} className={styles.icon} />
                <span className={styles.meta}>
                  <span className={styles.name}>{server.name}</span>
                  <span className={styles.detail}>
                    {server.kind} {server.mcVersion}
                  </span>
                </span>

                {!ok && (
                  <span className={styles.warn}>
                    <AlertTriangle size={12} />
                    loads mods, not plugins
                  </span>
                )}
                {selected === server.id && ok && (
                  <Check size={15} className={styles.check} />
                )}
              </button>
            );
          })}
        </div>
      )}
    </Modal>
  );
}

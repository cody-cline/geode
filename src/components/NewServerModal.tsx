import { useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import { Blocks, Loader2, Puzzle, Server as ServerIcon } from "lucide-react";

import Modal from "./Modal";
import { api } from "../lib/api";
import { useStore } from "../lib/store";
import type { Server, ServerKindInfo } from "../lib/types";

import styles from "./NewServerModal.module.css";

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
  const [kind, setKind] = useState("paper");
  const [versions, setVersions] = useState<string[] | null>(null);
  const [version, setVersion] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);

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

  // Versions belong to the software, not to Minecraft: Velocity's are its own.
  useEffect(() => {
    let cancelled = false;
    setVersions(null);
    api
      .serverVersions(kind)
      .then((list) => {
        if (cancelled) return;
        setVersions(list);
        setVersion(list[0] ?? "");
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
            disabled={busy || !version}
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
          <div className={styles.kinds}>
            {kinds.map((option) => (
              <button
                key={option.id}
                className={clsx(
                  styles.kind,
                  kind === option.id && styles.kindActive,
                )}
                onClick={() => setKind(option.id)}
              >
                <span className={styles.kindName}>{option.name}</span>
                <span className={styles.kindTags}>
                  {option.takesPlugins && (
                    <span className={styles.tag}>
                      <Puzzle size={9} />
                      plugins
                    </span>
                  )}
                  {option.takesMods && (
                    <span className={styles.tag}>
                      <Blocks size={9} />
                      mods
                    </span>
                  )}
                  {option.isProxy && <span className={styles.tag}>proxy</span>}
                </span>
              </button>
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

            <p className={styles.note}>
              {chosen?.takesPlugins && chosen?.takesMods
                ? "Runs Bukkit plugins and mods side by side."
                : chosen?.takesPlugins
                  ? "Runs Bukkit-style plugins."
                  : chosen?.takesMods
                    ? "Runs mods."
                    : "Runs no plugins or mods."}{" "}
              You will be asked to accept Mojang&apos;s EULA before it starts
              for the first time.
            </p>

            {/* These two take noticeably longer than a download, and saying so
                beforehand is the difference between waiting and wondering. */}
            {chosen?.id === "spigot" && (
              <p className={styles.note}>
                Spigot is compiled here on your machine — its licence does not
                allow anyone to hand out a finished jar. The first build takes
                several minutes; later ones reuse the same work and are faster.
              </p>
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

import { useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import {
  Blocks,
  GitBranch,
  Loader2,
  Puzzle,
  Server as ServerIcon,
} from "lucide-react";

import Modal from "./Modal";
import { api } from "../lib/api";
import { useStore } from "../lib/store";
import type { GitStatus, Server, ServerKindInfo } from "../lib/types";

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

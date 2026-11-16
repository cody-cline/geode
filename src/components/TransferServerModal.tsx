import { useCallback, useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import {
  AlertTriangle,
  ArrowRight,
  Check,
  Copy,
  Loader2,
  Server as ServerIcon,
  Unplug,
} from "lucide-react";

import Modal from "./Modal";
import { api, errorMessage } from "../lib/api";
import { fileSize } from "../lib/format";
import { useStore } from "../lib/store";
import type {
  RemoteHost,
  RemoteServer,
  Server,
  TransferMeasure,
  TransferMode,
} from "../lib/types";

import styles from "./TransferServerModal.module.css";

/**
 * Move or copy a whole server between this machine and a host.
 *
 * One dialog for both directions, because the decision is the same either way
 * and so is everything that has to be said before it: which host, whether the
 * original stays, how much is about to cross the network, and what happens to
 * the server's sharing. Only where the server is picked differs — pushing
 * starts from a server that is already open, pulling starts from the host's
 * own list.
 *
 * Nothing here deletes anything. A move's delete is the backend's last step
 * and happens only once the other side has checked every file it received.
 */
export default function TransferServerModal({
  direction,
  server,
  onClose,
}: {
  /** `push`: this machine to a host. `pull`: a host to this machine. */
  direction: "push" | "pull";
  /** The server being sent. Only for a push; a pull picks one off the host. */
  server?: Server;
  onClose: () => void;
}) {
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const refreshServers = useStore((s) => s.refreshServers);
  const navigate = useStore((s) => s.navigate);

  const [hosts, setHosts] = useState<RemoteHost[] | null>(null);
  /**
   * Why `hosts.json` could not be read, when it could not.
   *
   * The empty-list copy below sends somebody to Settings → Server hosts to
   * add their first one. Over a failed read that is both false and a dead
   * end: the screen it offers reads the same file, so it can only repeat the
   * claim.
   */
  const [hostsFailed, setHostsFailed] = useState<string | null>(null);
  const [host, setHost] = useState("");
  const [mode, setMode] = useState<TransferMode>("copy");

  // A pull picks from the host's own list, which has to be fetched.
  const [theirs, setTheirs] = useState<RemoteServer[] | null>(null);
  /**
   * Why the host could not be asked what it holds, when it could not.
   *
   * "That host has no servers on it" is the host's own answer; a host that
   * never answered has not said that, and for a pull it is the whole content
   * of this dialog.
   */
  const [theirsFailed, setTheirsFailed] = useState<string | null>(null);
  const [picked, setPicked] = useState("");
  /**
   * Bumped by Try again below. The read lives in an effect because it belongs
   * to whichever host is selected, and the effect's own stale guard is what
   * keeps a slow answer from a previous attempt out of the list — so asking
   * again is a change to what that effect depends on rather than a second
   * call beside it.
   */
  const [askAgain, setAskAgain] = useState(0);

  const [size, setSize] = useState<TransferMeasure | null>(null);
  const [sizing, setSizing] = useState(false);
  const [busy, setBusy] = useState(false);

  const loadHosts = useCallback(
    () =>
      api
        .remoteHosts()
        .then((listed) => {
          setHosts(listed);
          setHostsFailed(null);
          if (listed.length > 0) setHost(listed[0].id);
        })
        .catch((e) => {
          setHosts([]);
          setHostsFailed(errorMessage(e));
          reportError("Could not read your list of hosts", e);
        }),
    [reportError],
  );

  useEffect(() => {
    void loadHosts();
  }, [loadHosts]);

  // The host's servers, for a pull. Cleared when the host changes, so a list
  // from the last one cannot be picked from by mistake.
  useEffect(() => {
    if (direction !== "pull" || !host) return;
    let stale = false;
    setTheirs(null);
    setTheirsFailed(null);
    setPicked("");
    api
      .remoteHostServers(host)
      .then((listed) => {
        if (!stale) setTheirs(listed);
      })
      .catch((e) => {
        if (stale) return;
        setTheirs([]);
        setTheirsFailed(errorMessage(e));
        reportError("Could not reach that host", e);
      });
    return () => {
      stale = true;
    };
  }, [direction, host, reportError, askAgain]);

  const id = direction === "push" ? (server?.id ?? "") : picked;
  const chosen = hosts?.find((one) => one.id === host) ?? null;
  const theirServer = theirs?.find((one) => one.id === picked) ?? null;
  const running = direction === "push" ? !!server?.running : !!theirServer?.running;

  // What is about to cross the network, measured on whichever machine holds it
  // now. Asked again when the mode changes, because a move carries a little
  // more than a copy does.
  useEffect(() => {
    if (!id || (direction === "pull" && !host)) {
      setSize(null);
      return;
    }
    let stale = false;
    setSizing(true);
    setSize(null);
    api
      .transferSize(direction === "pull" ? host : null, id, mode)
      .then((measured) => {
        if (!stale) setSize(measured);
      })
      .catch(() => {
        // Not worth a toast: the figure is a courtesy, and the transfer itself
        // will say what is wrong in words.
        if (!stale) setSize(null);
      })
      .finally(() => {
        if (!stale) setSizing(false);
      });
    return () => {
      stale = true;
    };
  }, [direction, host, id, mode]);

  const start = useCallback(async () => {
    if (!chosen || !id) return;
    setBusy(true);
    try {
      const arrived =
        direction === "push"
          ? await api.sendServerToHost(chosen.id, id, mode)
          : await api.bringServerFromHost(chosen.id, id, mode);
      await refreshServers();
      toast({
        kind: "success",
        title:
          mode === "move"
            ? `Moved to ${direction === "push" ? chosen.name : "this machine"}`
            : `Copied to ${direction === "push" ? chosen.name : "this machine"}`,
        body: `${arrived.files} files, ${fileSize(arrived.bytes)}, every one checked on arrival.`,
      });
      // A server moved away is a page that no longer has anything behind it.
      if (direction === "push" && mode === "move") navigate({ view: "servers" });
      onClose();
    } catch (e) {
      reportError(
        mode === "move" ? "The server was not moved" : "The server was not copied",
        e,
      );
    } finally {
      setBusy(false);
    }
  }, [
    chosen,
    direction,
    id,
    mode,
    navigate,
    onClose,
    refreshServers,
    reportError,
    toast,
  ]);

  const name = direction === "push" ? server?.name : theirServer?.name;
  const verb = mode === "move" ? "Move" : "Copy";

  const sharing = useMemo(() => {
    if (mode === "copy") {
      return "The copy arrives unshared, with no password of its own. Links players already hold keep pointing here — a second machine serving this server's pack under the same password would be two servers nobody could tell apart.";
    }
    return direction === "push"
      ? "The server's password travels with it and arrives stopped. A host has no window and nothing on it serves a pack, so the server cannot be shared again from there — the links players already hold will point at this machine, where the server no longer is."
      : "The server's password comes with it and arrives stopped. Share it here when you are ready — the Share tab will say that the links already in players' hands point at the host it came from.";
  }, [direction, mode]);

  return (
    <Modal
      title={
        direction === "push"
          ? `${verb} ${server?.name ?? "this server"} to a host`
          : `${verb} a server from a host`
      }
      subtitle={
        direction === "push"
          ? "Everything in the server travels: its world, its settings, its content, its artwork and its backups."
          : "Bring a whole server onto this machine, backups and all."
      }
      onClose={onClose}
      width={560}
      footer={
        <>
          <button className="btn" onClick={onClose} disabled={busy}>
            {busy ? "Close" : "Cancel"}
          </button>
          <button
            className={clsx("btn", mode === "move" ? "btn-danger" : "btn-primary")}
            onClick={() => void start()}
            disabled={busy || !chosen || !id || running}
          >
            {busy && <Loader2 size={14} className="spin" />}
            {busy ? "Transferring…" : `${verb} it`}
          </button>
        </>
      }
    >
      {hosts === null ? (
        <div className={styles.loading}>
          <Loader2 size={16} className="spin" />
        </div>
      ) : hostsFailed ? (
        /* Before the empty state, and without its offer to go and add one:
           Settings → Server hosts reads the same file this just failed on, so
           sending somebody there would only confirm the false claim. */
        <div className={styles.empty}>
          <AlertTriangle size={22} className={styles.emptyIcon} />
          <p>
            Could not read your list of hosts, so there is nothing here to pick
            from. Which is not the same as having none — nothing has been
            changed, and what is in the file is unknown until it reads.
          </p>
          <span className="hint">{hostsFailed}</span>
          <button className="btn btn-sm" onClick={() => void loadHosts()}>
            Try again
          </button>
        </div>
      ) : hosts.length === 0 ? (
        <div className={styles.empty}>
          <Unplug size={22} className={styles.emptyIcon} />
          <p>
            Geode does not know of any hosts yet. Add one in{" "}
            <button
              className={styles.link}
              onClick={() => {
                navigate({ view: "settings", tab: "hosts" });
                onClose();
              }}
            >
              Settings → Server hosts
            </button>
            : its address, and the token its compose file was given.
          </p>
        </div>
      ) : (
        <div className={styles.form}>
          <div className={styles.field}>
            <label className="label" htmlFor="transfer-host">
              {direction === "push" ? "Send it to" : "Bring it from"}
            </label>
            <select
              id="transfer-host"
              className="select"
              value={host}
              onChange={(e) => setHost(e.target.value)}
              disabled={busy}
            >
              {hosts.map((one) => (
                <option key={one.id} value={one.id}>
                  {one.name} — {one.address}
                </option>
              ))}
            </select>
          </div>

          {direction === "pull" &&
            (theirs === null ? (
              <div className={styles.loading}>
                <Loader2 size={16} className="spin" />
              </div>
            ) : theirsFailed ? (
              /* Before "no servers on it", which is the host's own answer and
                 not something to put in its mouth. The toast said the host
                 could not be reached; this is the same thing said where the
                 list would have been, so the panel and the toast agree once
                 the toast has gone. */
              <div className={styles.field}>
                <p className={styles.blocked}>
                  <AlertTriangle size={13} />
                  That host could not be reached, so what it holds is unknown —
                  it has not said it has nothing. Nothing on either machine has
                  been changed.
                </p>
                <span className="hint">{theirsFailed}</span>
                <button
                  className="btn btn-sm"
                  style={{ alignSelf: "flex-start" }}
                  onClick={() => setAskAgain((n) => n + 1)}
                >
                  Try again
                </button>
              </div>
            ) : theirs.length === 0 ? (
              <p className={styles.note}>
                That host has no servers on it.
              </p>
            ) : (
              <div className={styles.list}>
                {theirs.map((one) => (
                  <button
                    key={one.id}
                    className={clsx(
                      styles.row,
                      one.running && styles.unusable,
                      picked === one.id && styles.selected,
                    )}
                    onClick={() => setPicked(one.id)}
                    disabled={busy}
                  >
                    <ServerIcon size={17} className={styles.icon} />
                    <span className={styles.meta}>
                      <span className={styles.name}>{one.name}</span>
                      <span className={styles.detail}>
                        {one.kindName} {one.mcVersion}
                      </span>
                    </span>
                    {one.running && (
                      <span className={styles.warn}>
                        <AlertTriangle size={12} />
                        running — stop it first
                      </span>
                    )}
                    {picked === one.id && !one.running && (
                      <Check size={15} className={styles.check} />
                    )}
                  </button>
                ))}
              </div>
            ))}

          {/* Copy or move, as two things to choose between rather than a
              checkbox: one of them deletes a world, and that should never be
              the state a dialog happens to be left in. */}
          <div className={styles.modes}>
            <button
              className={clsx(styles.mode, mode === "copy" && styles.modeOn)}
              onClick={() => setMode("copy")}
              disabled={busy}
            >
              <Copy size={15} />
              <span>
                <strong>Copy</strong>
                <span className={styles.modeWhy}>
                  {direction === "push"
                    ? "Leaves this one where it is. You end up with two."
                    : "Leaves the host's where it is. You end up with two."}
                </span>
              </span>
            </button>
            <button
              className={clsx(styles.mode, mode === "move" && styles.modeOn)}
              onClick={() => setMode("move")}
              disabled={busy}
            >
              <ArrowRight size={15} />
              <span>
                <strong>Move</strong>
                <span className={styles.modeWhy}>
                  {direction === "push"
                    ? "Deletes this one — but only after the host has checked every file it received."
                    : "Deletes the host's — but only after this machine has checked every file."}
                </span>
              </span>
            </button>
          </div>

          <div className={styles.size}>
            {sizing ? (
              <span className={styles.sizing}>
                <Loader2 size={13} className="spin" /> Measuring…
              </span>
            ) : size ? (
              <>
                <strong>
                  {fileSize(size.bytes)} in {size.files.toLocaleString()}{" "}
                  {size.files === 1 ? "file" : "files"}
                </strong>
                {size.backupBytes > 0 && (
                  <span className={styles.sizeWhy}>
                    {fileSize(size.backupBytes)} of that is{" "}
                    {size.backupFiles === 1 ? "one backup" : `${size.backupFiles} backups`}{" "}
                    — delete any you do not want first.
                  </span>
                )}
              </>
            ) : id ? (
              <span className={styles.sizeWhy}>
                How big this is could not be worked out, so the transfer will
                say what is wrong when it starts.
              </span>
            ) : (
              <span className={styles.sizeWhy}>Pick a server to see how big it is.</span>
            )}
          </div>

          <p className={styles.sharing}>{sharing}</p>

          {running && (
            <p className={styles.blocked}>
              <AlertTriangle size={13} />
              {name ?? "That server"} is running. A world being written cannot
              be copied — stop it first.
            </p>
          )}

          {busy && (
            <p className={styles.note}>
              This can take a while. The progress panel at the bottom of the
              window shows how far it has got, and can cancel it — a cancelled
              transfer leaves both sides exactly as they were.
            </p>
          )}
        </div>
      )}
    </Modal>
  );
}

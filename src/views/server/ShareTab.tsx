import { useEffect, useState } from "react";
import clsx from "clsx";
import {
  Check,
  Copy,
  Loader2,
  RefreshCw,
  Settings as SettingsIcon,
  Share2,
  Square,
} from "lucide-react";

import Modal from "../../components/Modal";
import StatusNote from "../../components/StatusNote";
import { api, errorMessage } from "../../lib/api";
import {
  linkPendingReason,
  listenLine,
  readOutHint,
  replacedPasswordBody,
  routerLine,
  stoppedLine,
} from "../../lib/share";
import { useStore } from "../../lib/store";
import type { Server, ServerShareView } from "../../lib/types";

// The server page's own sheet for the shell every tab shares — sections,
// headings, the toggle with a sentence under it — so this tab cannot drift from
// Settings and Automation beside it.
import viewStyles from "../ServerView.module.css";
import styles from "./ShareTab.module.css";

function CopyButton({ text, what }: { text: string; what: string }) {
  const reportError = useStore((s) => s.reportError);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1800);
    return () => clearTimeout(timer);
  }, [copied]);

  return (
    <button
      className="btn"
      aria-label={`Copy the ${what}`}
      onClick={() =>
        navigator.clipboard
          .writeText(text)
          .then(() => setCopied(true))
          .catch((e) => reportError(`Could not copy the ${what}`, e))
      }
    >
      {copied ? <Check size={14} /> : <Copy size={14} />}
      {copied ? "Copied" : "Copy"}
    </button>
  );
}

/**
 * Handing this server's pack to the people who play on it: the link, the
 * password, what goes in the pack, and whether players can reach it at all.
 */
export default function ShareTab({ server }: { server: Server }) {
  const config = useStore((s) => s.config);
  const status = useStore((s) => s.shareStatus);
  const loadShareStatus = useStore((s) => s.loadShareStatus);
  const navigate = useStore((s) => s.navigate);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  /**
   * `undefined` while loading; `null` when this server has never been shared.
   * A server whose sharing is stopped still has one, with `shared` false, so
   * its password is there to share again with.
   */
  const [share, setShare] = useState<ServerShareView | null | undefined>(undefined);
  const [shareError, setShareError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"share" | "config" | "rotate" | "stop" | null>(null);
  const [confirming, setConfirming] = useState<"rotate" | "stop" | null>(null);

  const hasPublicAddress = Boolean(config?.share.publicAddress);

  useEffect(() => {
    loadShareStatus()
      .then(() => setStatusError(null))
      .catch((e) => setStatusError(errorMessage(e)));
  }, [loadShareStatus]);

  // The link is built from the address, the port and the certificate, so when
  // any of those changes — the router reports its address, the admin sets a
  // public one — the link shown here is stale until it is read again.
  useEffect(() => {
    let cancelled = false;
    api
      .getServerShare(server.id)
      .then((view) => {
        if (cancelled) return;
        setShare(view);
        setShareError(null);
      })
      .catch((e) => !cancelled && setShareError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [server.id, status?.address, status?.port, status?.fingerprint]);

  async function run<T>(
    what: NonNullable<typeof busy>,
    failure: string,
    action: () => Promise<T>,
  ): Promise<T | undefined> {
    setBusy(what);
    try {
      return await action();
    } catch (e) {
      reportError(failure, e);
      return undefined;
    } finally {
      setBusy(null);
    }
  }

  async function start() {
    const view = await run("share", `Could not share ${server.name}`, () =>
      api.shareServer(server.id),
    );
    if (view) {
      setShare(view);
      loadShareStatus().catch(() => {});
    }
  }

  async function setIncludeConfig(include: boolean) {
    const view = await run("config", "Could not change what is shared", () =>
      api.setServerShareConfig(server.id, include),
    );
    if (view) setShare(view);
  }

  async function rotate() {
    setConfirming(null);
    const view = await run("rotate", "Could not replace the password", () =>
      api.rotateServerShareCode(server.id),
    );
    if (view) {
      setShare(view);
      toast({
        kind: "success",
        title: "Password replaced",
        body: replacedPasswordBody(view.shared),
      });
    }
  }

  async function stop() {
    setConfirming(null);
    const done = await run("stop", `Could not stop sharing ${server.name}`, async () => {
      await api.unshareServer(server.id);
      return true;
    });
    if (done) {
      // Read back rather than cleared: the server keeps its password, and the
      // tab offers to share again with it.
      api
        .getServerShare(server.id)
        .then(setShare)
        .catch((e) => {
          setShare(undefined);
          setShareError(errorMessage(e));
        });
      loadShareStatus().catch(() => {});
    }
  }

  const settingsButton = (
    <button
      className="btn btn-sm"
      onClick={() => navigate({ view: "settings", tab: "sharing" })}
    >
      <SettingsIcon size={13} />
      Sharing settings
    </button>
  );

  const newPasswordButton = (
    <button
      className="btn"
      onClick={() => setConfirming("rotate")}
      disabled={busy !== null}
    >
      {busy === "rotate" ? (
        <Loader2 size={14} className="spin" />
      ) : (
        <RefreshCw size={14} />
      )}
      New password
    </button>
  );

  const aboutSharing = (
    <p className={viewStyles.eula}>
      Players open a link, or type this computer&apos;s address and the
      server&apos;s password, and their Geode sets up this server&apos;s loader
      and mods, ready to join. The pack comes straight from this computer:
      nothing is uploaded anywhere. Mods on Modrinth or CurseForge travel as
      references; every other jar in the mods folder, including jars you added
      by hand, is sent as a copy. Client mods go to players too, but this server
      never loads them.
    </p>
  );

  return (
    <div className={viewStyles.settings}>
      <section className={viewStyles.section}>
        <h2 className={viewStyles.sectionTitle}>Share with players</h2>

        {shareError && share === undefined ? (
          // Said as a failure rather than shown as "not shared": offering to
          // share a server that may already be shared would be a guess
          // presented as a fact.
          <StatusNote
            line={{
              tone: "bad",
              title: "Could not read whether this server is shared",
              detail: shareError,
            }}
          />
        ) : share === undefined ? (
          <div className="skeleton" style={{ height: 120 }} />
        ) : share === null ? (
          <div className={styles.stack}>
            {aboutSharing}
            <p className={viewStyles.pending}>
              Only someone with the password can download anything. Geode makes
              one the first time you share this server, and it stays the same
              until you replace it.{" "}
              {config &&
                (config.share.mapping === "manual"
                  ? `Router setup is manual, so you forward TCP port ${config.share.port} to this computer yourself.`
                  : `Geode asks your router to open TCP port ${config.share.port} for as long as a server is shared.`)}
            </p>
            <div className={styles.actions}>
              <button
                className="btn btn-primary"
                onClick={() => void start()}
                disabled={busy !== null}
              >
                {busy === "share" ? (
                  <Loader2 size={14} className="spin" />
                ) : (
                  <Share2 size={14} />
                )}
                Share this server
              </button>
              {settingsButton}
            </div>
          </div>
        ) : !share.shared ? (
          <div className={styles.stack}>
            {aboutSharing}
            <StatusNote line={stoppedLine(share.includeConfig)} />
            <div className={styles.actions}>
              <button
                className="btn btn-primary"
                onClick={() => void start()}
                disabled={busy !== null}
              >
                {busy === "share" ? (
                  <Loader2 size={14} className="spin" />
                ) : (
                  <Share2 size={14} />
                )}
                Share again
              </button>
              {newPasswordButton}
              {settingsButton}
            </div>
          </div>
        ) : share.unsupported ? (
          <div className={styles.stack}>
            <StatusNote
              line={{
                tone: "bad",
                title: "This server cannot be shared",
                detail: share.unsupported,
              }}
            />
            <div className={styles.actions}>
              <button
                className="btn btn-danger"
                onClick={() => setConfirming("stop")}
                disabled={busy !== null}
              >
                {busy === "stop" ? (
                  <Loader2 size={14} className="spin" />
                ) : (
                  <Square size={12} fill="currentColor" />
                )}
                Stop sharing
              </button>
            </div>
          </div>
        ) : (
          <div className={styles.stack}>
            <div>
              <span className="label">Link</span>
              {share.link ? (
                <>
                  <div className={styles.copyRow}>
                    <input
                      className={clsx("input", styles.mono)}
                      value={share.link}
                      readOnly
                      spellCheck={false}
                      aria-label="Link for players"
                      onFocus={(e) => e.currentTarget.select()}
                    />
                    <CopyButton text={share.link} what="link" />
                  </div>
                  <p className="hint">
                    Send this to players. Opening it shows them the pack in
                    Geode before anything is downloaded.
                  </p>
                </>
              ) : (
                <StatusNote
                  line={{
                    tone: "info",
                    title: "No link yet",
                    detail: linkPendingReason(status, hasPublicAddress),
                  }}
                  busy={status?.mapping.state === "searching"}
                />
              )}
            </div>

            {/* The address as well as the password: typed by hand, a player
                needs both, and outside a link the address — its port above
                all — is shown nowhere else. */}
            <div>
              <span className="label">Address and password</span>
              <div className={styles.readOut}>
                {status?.address && (
                  <div className={styles.copyRow}>
                    <code className={styles.code}>{status.address}</code>
                    <CopyButton text={status.address} what="address" />
                  </div>
                )}
                <div className={styles.copyRow}>
                  <code className={styles.code}>{share.code}</code>
                  <CopyButton text={share.code} what="password" />
                </div>
              </div>
              <p className="hint">{readOutHint(status?.address ?? null)}</p>
            </div>

            <label className={viewStyles.settingToggle}>
              <input
                type="checkbox"
                checked={share.includeConfig}
                disabled={busy !== null}
                onChange={(e) => void setIncludeConfig(e.target.checked)}
              />
              <span>
                <strong>Include the config folder</strong>
                <span className={viewStyles.settingWhy}>
                  Sends this server&apos;s config folder with the pack, for
                  mods whose settings have to match on both sides.{" "}
                  <span className={styles.secret}>
                    Configs can hold secrets — bot tokens, database passwords,
                    proxy keys — and everyone with the password gets them.
                    Look through the folder before turning this on.
                  </span>
                </span>
              </span>
            </label>

            <div className={styles.actions}>
              {newPasswordButton}
              <button
                className="btn btn-danger"
                onClick={() => setConfirming("stop")}
                disabled={busy !== null}
              >
                {busy === "stop" ? (
                  <Loader2 size={14} className="spin" />
                ) : (
                  <Square size={12} fill="currentColor" />
                )}
                Stop sharing
              </button>
            </div>
          </div>
        )}
      </section>

      {share?.shared && !share.unsupported && (
        <section className={viewStyles.section}>
          <h2 className={viewStyles.sectionTitle}>Can players connect?</h2>
          <div className={styles.stack}>
            {statusError ? (
              <StatusNote
                line={{
                  tone: "bad",
                  title: "Could not read the sharing status",
                  detail: statusError,
                }}
              />
            ) : !status ? (
              <div className="skeleton" style={{ height: 110 }} />
            ) : (
              <>
                <StatusNote line={listenLine(status)} />
                <StatusNote
                  line={routerLine(status, hasPublicAddress)}
                  busy={status.mapping.state === "searching"}
                />
              </>
            )}
            <div className={styles.actions}>
              <p className={clsx(viewStyles.pending, styles.grow)}>
                Every server you share uses the same port and router setup.
              </p>
              {settingsButton}
            </div>
          </div>
        </section>
      )}

      {confirming === "rotate" && (
        <Modal
          title="Replace the password?"
          subtitle={server.name}
          onClose={() => setConfirming(null)}
          width={460}
          footer={
            <>
              <button className="btn" onClick={() => setConfirming(null)}>
                Cancel
              </button>
              <button className="btn btn-danger" onClick={() => void rotate()}>
                Replace password
              </button>
            </>
          }
        >
          <p className={viewStyles.eula}>
            Everyone holding the current link or password is cut off: they
            cannot download this pack again until you send them the new link or
            password. What they already imported stays on their computers.
          </p>
          <p className={viewStyles.eula}>
            Do this when the password has reached someone it should not have.
          </p>
        </Modal>
      )}

      {confirming === "stop" && (
        <Modal
          title={`Stop sharing ${server.name}?`}
          onClose={() => setConfirming(null)}
          width={460}
          footer={
            <>
              <button className="btn" onClick={() => setConfirming(null)}>
                Cancel
              </button>
              <button className="btn btn-danger" onClick={() => void stop()}>
                Stop sharing
              </button>
            </>
          }
        >
          <p className={viewStyles.eula}>
            Players can no longer download this pack from here, and the link you
            sent stops working until you share again. The password stays the
            same, so sharing again brings that link back; to cut someone off for
            good, use New password. What players already imported stays on
            their computers.
          </p>
        </Modal>
      )}
    </div>
  );
}

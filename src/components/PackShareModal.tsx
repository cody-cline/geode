import { useEffect, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Check, Copy, ExternalLink, Loader2, Settings, Upload } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";

import Modal from "./Modal";
import StatusNote from "./StatusNote";
import { api, errorMessage } from "../lib/api";
import { fileSize } from "../lib/format";
import {
  PUBLIC_LINK_WARNING,
  driveLine,
  packShareMessage,
  publicLinkLine,
  publishSummary,
} from "../lib/share";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type { DriveShare, DriveStatus, Instance, StagedPack } from "../lib/types";

import viewStyles from "../views/ServerView.module.css";
import styles from "./PackShareModal.module.css";

/**
 * Putting an instance's pack in the user's own Google Drive and handing back a
 * message to paste.
 *
 * Three things have to be true before the upload button is pressed, and all
 * three are on screen: what the pack is, **what travels as bytes** — because a
 * jar carried as a copy is about to be put on a public URL, which is a
 * redistribution decision and not just a transfer — and that anyone holding
 * the link can download it. The export is the real one, so the counts shown
 * are what the pack actually did rather than what usually happens.
 */
export default function PackShareModal({
  instance,
  onClose,
}: {
  instance: Instance;
  onClose: () => void;
}) {
  const reportError = useStore((s) => s.reportError);
  const navigate = useStore((s) => s.navigate);

  const [status, setStatus] = useState<DriveStatus | null>(null);
  const [staged, setStaged] = useState<StagedPack | null>(null);
  const [share, setShare] = useState<DriveShare | null>(null);
  const [includeSaves, setIncludeSaves] = useState(false);
  const [working, setWorking] = useState<"preparing" | "uploading" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    api
      .driveStatus()
      .then(setStatus)
      .catch((e) => setError(errorMessage(e)));
  }, []);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1800);
    return () => clearTimeout(timer);
  }, [copied]);

  async function prepare() {
    setWorking("preparing");
    setError(null);
    setStaged(null);
    try {
      setStaged(await api.stageDrivePack(instance.id, includeSaves));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setWorking(null);
    }
  }

  async function upload() {
    if (!staged) return;
    setWorking("uploading");
    setError(null);
    try {
      setShare(await api.uploadDrivePack(staged.stage, instance.name));
    } catch (e) {
      setError(errorMessage(e));
      // The staged copy is gone once an upload has run, whichever way it went,
      // so there is nothing left to press Upload on.
      setStaged(null);
    } finally {
      setWorking(null);
    }
  }

  const message = share
    ? packShareMessage({
        name: instance.name,
        link: share.link,
        fileUrl: share.fileUrl,
        size: fileSize(share.size),
      })
    : "";

  const loader = LOADERS.find((l) => l.id === instance.loader)?.label ?? instance.loader;
  const connected = Boolean(status?.connected);
  const step = share ? "done" : !connected ? "connect" : staged ? "upload" : "prepare";

  return (
    <Modal
      title="Share this pack"
      subtitle="Put it in your own Google Drive and hand out one link."
      onClose={onClose}
      width={560}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            {step === "done" ? "Done" : "Cancel"}
          </button>
          {step === "connect" && (
            <button
              className="btn btn-primary"
              onClick={() => {
                navigate({ view: "settings", tab: "drive" });
                onClose();
              }}
            >
              <Settings size={14} />
              Open Settings
            </button>
          )}
          {step === "prepare" && (
            <button
              className="btn btn-primary"
              onClick={() => void prepare()}
              disabled={working !== null}
            >
              {working === "preparing" && <Loader2 size={14} className="spin" />}
              Prepare the pack
            </button>
          )}
          {step === "upload" && (
            <button
              className="btn btn-primary"
              onClick={() => void upload()}
              disabled={working !== null}
            >
              {working === "uploading" ? (
                <Loader2 size={14} className="spin" />
              ) : (
                <Upload size={14} />
              )}
              Upload and share
            </button>
          )}
        </>
      }
    >
      {status && !connected && <StatusNote line={driveLine(status)} />}

      {error && (
        <div className={styles.error} role="alert">
          <AlertTriangle size={15} />
          <span>{error}</span>
        </div>
      )}

      {connected && !staged && !share && (
        <>
          <p className="hint" style={{ marginTop: 0 }}>
            Geode exports the pack first and shows you what is in it, including
            which jars travel as copies rather than as references, before
            anything leaves this computer.
          </p>
          <label className={viewStyles.settingToggle}>
            <input
              type="checkbox"
              checked={includeSaves}
              disabled={working !== null}
              onChange={(e) => setIncludeSaves(e.target.checked)}
            />
            <span>
              <strong>Include worlds</strong>
              <span className={viewStyles.settingWhy}>
                Your saves go in the pack too. They are usually the largest
                thing in it, and whoever downloads the link gets them.
              </span>
            </span>
          </label>
        </>
      )}

      {working === "preparing" && (
        <p className={styles.pending}>
          <Loader2 size={13} className="spin" />
          Preparing the pack…
        </p>
      )}

      {staged && !share && (
        <>
          <div className={styles.pack}>
            <div className={styles.packHead}>
              <span className={styles.packName}>{staged.name}</span>
              <span className={styles.packSize}>{fileSize(staged.size)}</span>
            </div>
            <dl className={styles.facts}>
              <dt>Minecraft</dt>
              <dd>{instance.mcVersion}</dd>
              <dt>Loader</dt>
              <dd>
                {loader}
                {instance.loaderVersion && ` ${instance.loaderVersion}`}
              </dd>
            </dl>
            <ul className={styles.summary}>
              {publishSummary(staged.report).map((line, i) => (
                <li key={i}>{line}</li>
              ))}
            </ul>
          </div>

          <StatusNote
            line={{
              tone: "warn",
              title: "This becomes a public download",
              detail: PUBLIC_LINK_WARNING,
            }}
          />
        </>
      )}

      {working === "uploading" && (
        <p className={styles.pending}>
          <Loader2 size={13} className="spin" />
          Uploading to Google Drive…
          <button
            className="btn btn-sm"
            onClick={() => void api.cancelTask(`drive-upload:${staged?.stage ?? ""}`)}
          >
            Cancel
          </button>
        </p>
      )}

      {share && (
        <>
          <div>
            <label className="label" htmlFor="pack-message">
              The message to send
            </label>
            <textarea
              id="pack-message"
              className={clsx("input", styles.message)}
              value={message}
              readOnly
              spellCheck={false}
              rows={10}
            />
            <div className={styles.messageActions}>
              <button
                className="btn"
                aria-label="Copy the message"
                onClick={() =>
                  navigator.clipboard
                    .writeText(message)
                    .then(() => setCopied(true))
                    .catch((e) => reportError("Could not copy the message", e))
                }
              >
                {copied ? <Check size={14} /> : <Copy size={14} />}
                {copied ? "Copied" : "Copy"}
              </button>
              <button className="btn" onClick={() => void openUrl(share.fileUrl)}>
                <ExternalLink size={14} />
                Open in Drive
              </button>
            </div>
          </div>

          <StatusNote line={publicLinkLine(share.publicCheck)} />
        </>
      )}
    </Modal>
  );
}

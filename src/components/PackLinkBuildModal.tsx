import { useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Check, Copy, FileUp, Link2, Loader2 } from "lucide-react";

import Modal from "./Modal";
import StatusNote from "./StatusNote";
import { api, errorMessage } from "../lib/api";
import { PUBLIC_LINK_WARNING, publicLinkLine } from "../lib/share";
import { useStore } from "../lib/store";
import type { PackLinkBuilt } from "../lib/types";

import styles from "./PackLinkBuildModal.module.css";

/**
 * Turning a pack file the sharer has already uploaded into a `geode://` link.
 *
 * Geode does not host anything and holds no account anywhere, so the upload is
 * the sharer's own — a chat attachment, a release asset, their own web space.
 * This takes the two things they cannot check themselves and checks them.
 *
 * The **fingerprint** comes from the file on disk, not from the URL, so the
 * link promises the bytes they exported rather than whatever the address
 * happens to serve later. The **fetch** happens the way the recipient's will,
 * with nothing signed in, because the usual mistake is pasting the page a file
 * sits on instead of a link that downloads it — and that is invisible to the
 * person pasting, whose own browser is signed in and sees the file fine.
 */
export default function PackLinkBuildModal({ onClose }: { onClose: () => void }) {
  const toast = useStore((s) => s.toast);

  const [file, setFile] = useState("");
  const [url, setUrl] = useState("");
  const [working, setWorking] = useState(false);
  const [built, setBuilt] = useState<PackLinkBuilt | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  async function pick() {
    const picked = await openDialog({
      filters: [{ name: "Geode pack", extensions: ["geodepack", "mrpack", "zip"] }],
    });
    if (typeof picked === "string") {
      setFile(picked);
      setBuilt(null);
    }
  }

  async function build() {
    setWorking(true);
    setError(null);
    setBuilt(null);
    try {
      setBuilt(await api.buildPackLink(file, url.trim()));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setWorking(false);
    }
  }

  async function copy() {
    if (!built) return;
    try {
      await navigator.clipboard.writeText(built.link);
      setCopied(true);
      setTimeout(() => setCopied(false), 1800);
    } catch {
      toast({
        kind: "error",
        title: "Could not reach the clipboard",
        body: "Select the link above and copy it by hand.",
      });
    }
  }

  const ready = file.trim() !== "" && url.trim() !== "";

  return (
    <Modal
      title="Make a link for a pack"
      subtitle="Upload the pack wherever you like, then paste the address here."
      onClose={onClose}
      footer={
        <div className={styles.actions}>
          <button className="btn" onClick={onClose}>
            {built ? "Done" : "Cancel"}
          </button>
          <button
            className="btn btn-primary"
            onClick={() => void build()}
            disabled={!ready || working}
          >
            {working ? <Loader2 size={14} className="spin" /> : <Link2 size={14} />}
            {built ? "Check again" : "Make the link"}
          </button>
        </div>
      }
    >
      <div className={styles.body}>
        <div className={styles.field}>
          <label className="label" htmlFor="pack-file">
            The pack you exported
          </label>
          <div className={styles.row}>
            <input
              id="pack-file"
              className="input"
              value={file}
              placeholder="No file chosen"
              onChange={(e) => {
                setFile(e.target.value);
                setBuilt(null);
              }}
            />
            <button className="btn" onClick={() => void pick()}>
              <FileUp size={14} />
              Choose
            </button>
          </div>
          <p className="hint">
            Its fingerprint goes in the link, so whoever opens it gets exactly
            these bytes or nothing.
          </p>
        </div>

        <div className={styles.field}>
          <label className="label" htmlFor="pack-url">
            Where you put it
          </label>
          <input
            id="pack-url"
            className="input"
            value={url}
            placeholder="https://..."
            onChange={(e) => {
              setUrl(e.target.value);
              setBuilt(null);
            }}
            onKeyDown={(e) => e.key === "Enter" && ready && !working && void build()}
          />
          <p className="hint">
            A direct download address, not the page the file sits on. Geode
            fetches it to tell the difference.
          </p>
        </div>

        {error && (
          <StatusNote line={{ tone: "warn", title: "Could not make a link", detail: error }} />
        )}

        {built && (
          <>
            <StatusNote line={publicLinkLine(built.check)} />
            <div className={styles.field}>
              <label className="label" htmlFor="pack-link">
                The link to hand over
              </label>
              <div className={styles.row}>
                <input id="pack-link" className="input" value={built.link} readOnly />
                <button className="btn" onClick={() => void copy()}>
                  {copied ? <Check size={14} /> : <Copy size={14} />}
                  {copied ? "Copied" : "Copy"}
                </button>
              </div>
              <p className="hint">{PUBLIC_LINK_WARNING}</p>
            </div>
          </>
        )}
      </div>

    </Modal>
  );
}

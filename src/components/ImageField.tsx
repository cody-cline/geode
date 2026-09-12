import { useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { convertFileSrc } from "@tauri-apps/api/core";
import clsx from "clsx";
import { ImageIcon, Loader2, Trash2 } from "lucide-react";

import { useStore } from "../lib/store";

import styles from "./ImageField.module.css";

/**
 * Pick a picture for something, or take the one it has away.
 *
 * The file is copied into the owner's own folder by the backend, so the
 * original can be moved or deleted afterwards without the picture vanishing.
 */
export default function ImageField({
  label,
  hint,
  path,
  wide = false,
  onChoose,
  onClear,
}: {
  label: string;
  hint: string;
  /** Absolute path of the current image, or null. */
  path: string | null;
  /** Banners are wide; icons are square. */
  wide?: boolean;
  onChoose: (source: string) => Promise<void>;
  onClear: () => Promise<void>;
}) {
  const reportError = useStore((s) => s.reportError);
  const [busy, setBusy] = useState(false);

  async function choose() {
    const picked = await openDialog({
      multiple: false,
      filters: [
        { name: "Images", extensions: ["png", "jpg", "jpeg", "webp", "gif"] },
      ],
    });
    const source = Array.isArray(picked) ? picked[0] : picked;
    if (!source) return;

    setBusy(true);
    try {
      await onChoose(source);
    } catch (e) {
      reportError(`Could not set the ${label.toLowerCase()}`, e);
    } finally {
      setBusy(false);
    }
  }

  async function clear() {
    setBusy(true);
    try {
      await onClear();
    } catch (e) {
      reportError(`Could not remove the ${label.toLowerCase()}`, e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className={styles.field}>
      <span className="label">{label}</span>
      <div className={styles.row}>
        <div className={clsx(styles.preview, wide && styles.previewWide)}>
          {path ? (
            <img src={convertFileSrc(path)} alt="" />
          ) : (
            <ImageIcon size={wide ? 20 : 18} className={styles.placeholder} />
          )}
        </div>
        <div className={styles.actions}>
          <button className="btn btn-sm" onClick={choose} disabled={busy}>
            {busy ? <Loader2 size={13} className="spin" /> : <ImageIcon size={13} />}
            {path ? "Replace" : "Choose image"}
          </button>
          {path && (
            <button
              className="btn btn-sm btn-ghost"
              onClick={clear}
              disabled={busy}
              title={`Remove the ${label.toLowerCase()}`}
            >
              <Trash2 size={13} />
              Remove
            </button>
          )}
        </div>
      </div>
      <p className="hint">{hint}</p>
    </div>
  );
}

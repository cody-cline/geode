import { useEffect, useRef, useState } from "react";
import { AlertTriangle, ArrowRight, Download, Loader2, Package } from "lucide-react";

import Modal from "./Modal";
import StatusNote from "./StatusNote";
import { api, errorMessage } from "../lib/api";
import { arrivalLine, packStep, sourceName } from "../lib/share";
import type { PackRequest } from "../lib/share";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type { Instance, PackArrival, PackLinkView } from "../lib/types";

import styles from "./PackLinkModal.module.css";

/**
 * Opening a `geode://pack` link somebody sent.
 *
 * A link can arrive from any web page or any program on this machine, and
 * importing fetches jars that run as code, so the order is fixed: the link is
 * read by the strict parser in Rust, the dialog names the host that parsing
 * proved, the download is checked against the fingerprint the link carries,
 * and only then is there anything to import. Nothing is imported before the
 * hash has matched, and nothing at all happens without a press.
 */
export default function PackLinkModal({
  request,
  onClose,
}: {
  request: PackRequest;
  onClose: () => void;
}) {
  const [view, setView] = useState<PackLinkView | null>(null);
  const [arrival, setArrival] = useState<PackArrival | null>(null);
  const [instance, setInstance] = useState<Instance | null>(null);
  const [working, setWorking] = useState<"downloading" | "importing" | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Once, for the link this dialog was opened with. StrictMode runs effects
  // twice on mount and the ref survives that, so the link is read once.
  const read = useRef(false);

  useEffect(() => {
    if (read.current) return;
    read.current = true;
    api
      .previewPackLink(request.link)
      .then(setView)
      .catch((e) => setError(errorMessage(e)));
  }, [request.link]);

  async function download() {
    setWorking("downloading");
    setError(null);
    try {
      // The link goes back as text, so the same strict parser reads it again:
      // what came out of this window is no more trustworthy than what went in.
      setArrival(await api.fetchPackLink(request.link));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setWorking(null);
    }
  }

  async function importPack() {
    if (!arrival) return;
    const { navigate, refreshInstances, toast, reportError } = useStore.getState();
    setWorking("importing");
    setError(null);
    try {
      const { instance: made, problems } = await api.importPackStage(arrival.stage);
      setInstance(made);
      await refreshInstances();
      navigate({ view: "instance", id: made.id });
      // An instance was made either way, so it is opened either way; what the
      // pack named and this Geode could not fetch is said beside it rather
      // than instead of it, because importing again would only make a second
      // instance.
      if (problems.length === 0) {
        toast({
          kind: "success",
          title: `Imported ${made.name}`,
          body: view ? `From ${sourceName(view)}.` : undefined,
        });
      } else {
        reportError(`Imported ${made.name}, with problems`, problems.join(" "));
      }
      onClose();
    } catch (e) {
      setError(errorMessage(e));
      setWorking(null);
    }
  }

  const step = packStep(view, arrival, instance);
  const preview = arrival?.preview;
  const loader = preview
    ? (LOADERS.find((l) => l.id === preview.loader)?.label ?? preview.loader)
    : "";

  return (
    <Modal
      title="A pack somebody shared"
      subtitle="Download it, see what is in it, and install it only if you want to."
      onClose={onClose}
      width={540}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          {step === "download" && (
            <button
              className="btn btn-primary"
              onClick={() => void download()}
              disabled={working !== null}
            >
              {working === "downloading" ? (
                <Loader2 size={14} className="spin" />
              ) : (
                <Download size={14} />
              )}
              Download
            </button>
          )}
          {step === "import" && (
            <button
              className="btn btn-primary"
              onClick={() => void importPack()}
              disabled={working !== null}
            >
              {working === "importing" ? (
                <Loader2 size={14} className="spin" />
              ) : (
                <ArrowRight size={14} />
              )}
              Import
            </button>
          )}
        </>
      }
    >
      {error && (
        <div className={styles.error} role="alert">
          <AlertTriangle size={15} />
          <span>{error}</span>
        </div>
      )}

      {view && !arrival && (
        <>
          <StatusNote line={arrivalLine(view)} />
          <dl className={styles.facts}>
            <dt>From</dt>
            <dd className={styles.mono}>{view.host}</dd>
            <dt>Fingerprint</dt>
            <dd className={styles.mono}>{view.hash}</dd>
          </dl>
          <p className="hint" style={{ marginTop: 0 }}>
            The pack is checked against that fingerprint before Geode reads a
            thing out of it. A pack that does not match is deleted, not opened.
          </p>
        </>
      )}

      {working === "downloading" && (
        <p className={styles.pending}>
          <Loader2 size={13} className="spin" />
          Downloading the pack…
          <button
            className="btn btn-sm"
            onClick={() => void api.cancelTask("pack-link")}
          >
            Cancel
          </button>
        </p>
      )}

      {preview && (
        <>
          <div className={styles.pack}>
            <div className={styles.packHead}>
              <span className={styles.packIcon}>
                <Package size={16} />
              </span>
              <span className={styles.packName}>{preview.name}</span>
            </div>
            <dl className={styles.facts}>
              <dt>Minecraft</dt>
              <dd>{preview.mcVersion}</dd>
              <dt>Loader</dt>
              <dd>
                {loader}
                {preview.loaderVersion && ` ${preview.loaderVersion}`}
              </dd>
              <dt>Files</dt>
              <dd>{preview.fileCount === 0 ? "None" : preview.fileCount}</dd>
            </dl>
          </div>

          {preview.needsCurseforgeKey && (
            <StatusNote
              line={{
                tone: "warn",
                title: "This pack names mods on CurseForge",
                detail:
                  "Finishing the import needs your own CurseForge API key, which you can add in Settings → Mod sources. Without one those mods are left out and listed.",
              }}
            />
          )}

          <p className="hint" style={{ marginTop: 0 }}>
            Importing makes a new instance and downloads the mods this pack
            names, which run as code when you play. Import packs from people
            you trust.
          </p>
        </>
      )}
    </Modal>
  );
}

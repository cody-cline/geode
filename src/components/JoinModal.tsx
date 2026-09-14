import { useEffect, useRef, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, ArrowRight, Loader2, Package, Search } from "lucide-react";

import Modal from "./Modal";
import StatusNote from "./StatusNote";
import { api, errorMessage } from "../lib/api";
import {
  DEFAULT_SHARE_PORT,
  joinInput,
  joinStep,
  outcomeLine,
  verificationLine,
} from "../lib/share";
import type { JoinForm, JoinRequest } from "../lib/share";
import { useStore } from "../lib/store";
import { LOADERS } from "../lib/types";
import type { JoinOutcome, JoinPreview } from "../lib/types";

import styles from "./JoinModal.module.css";

/** `neoforge` as NeoForge, `paper` as Paper — ids are for machines. */
function softwareName(id: string): string {
  const loader = LOADERS.find((l) => l.id === id);
  if (loader) return loader.label;
  return id ? id[0].toUpperCase() + id.slice(1) : id;
}

/**
 * Joining a server someone shared: look it up, show what it is, and import it
 * only when the player says so.
 *
 * A link can arrive from any web page or any program on the machine, and
 * importing fetches jars that run as code, so nothing is downloaded or written
 * until the player has seen what the server says it is and how sure Geode is
 * that it is talking to the right machine. Looking a server up is a single
 * read, so a link that opened Geode is looked up straight away.
 */
export default function JoinModal({
  request,
  onClose,
}: {
  request: JoinRequest;
  onClose: () => void;
}) {
  const [form, setForm] = useState<JoinForm>({
    mode: "link",
    link: request.link ?? "",
    address: "",
    code: "",
  });
  const [preview, setPreview] = useState<JoinPreview | null>(null);
  const [looking, setLooking] = useState(false);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** An import that made its instance but came back with problems. */
  const [outcome, setOutcome] = useState<JoinOutcome | null>(null);

  // Each lookup's number, so an answer for what the form said before an edit
  // is dropped instead of shown against what it says now.
  const lookups = useRef(0);
  const looked = useRef(false);

  const input = joinInput(form);
  // Any edit throws the preview away, so the input on screen is the one that
  // was looked up.
  const typed = input !== null && "address" in input;
  const step = joinStep(preview, outcome);

  async function lookUp() {
    if (!input || importing) return;
    const id = ++lookups.current;
    setLooking(true);
    setError(null);
    setPreview(null);
    try {
      const found = await api.previewJoin(input);
      if (id === lookups.current) setPreview(found);
    } catch (e) {
      if (id === lookups.current) setError(errorMessage(e));
    } finally {
      if (id === lookups.current) setLooking(false);
    }
  }

  // Once, for the link this dialog was opened with. StrictMode runs effects
  // twice on mount and the ref survives that, so the server is asked once.
  useEffect(() => {
    if (looked.current || !request.link) return;
    looked.current = true;
    void lookUp();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function edit(patch: Partial<JoinForm>) {
    lookups.current++;
    setForm((was) => ({ ...was, ...patch }));
    setPreview(null);
    setLooking(false);
    setError(null);
  }

  async function importPack() {
    if (!preview || importing) return;
    const { navigate, refreshInstances, reportError, toast } = useStore.getState();
    // Whether this is still the dialog on screen. It may have been closed, or
    // replaced by a newer link, while the download ran — the import finishes
    // either way, but only the dialog still showing should move the view.
    const current = () => useStore.getState().join === request;

    setImporting(true);
    setError(null);
    let joined: JoinOutcome;
    try {
      // What the player approved goes back with the link, so a pack that has
      // come to describe something else by the time it downloads is refused.
      joined = await api.joinServer(preview.link, preview.info);
    } catch (e) {
      // Nothing was made: an import that gets as far as an instance comes back
      // as an outcome, problems and all. So this is safe to try again, from a
      // fresh look at the server — the likeliest reason to be refused is a
      // pack that changed since this preview.
      if (current()) {
        setError(errorMessage(e));
        setPreview(null);
        setImporting(false);
      } else {
        reportError(`Could not import ${preview.info.name}`, e);
      }
      return;
    }

    const { instance, problems } = joined;
    await refreshInstances();
    if (current()) navigate({ view: "instance", id: instance.id });
    if (problems.length === 0) {
      toast({
        kind: "success",
        title: `Imported ${instance.name}`,
        body: `From ${preview.address}.`,
      });
      if (current()) onClose();
    } else if (current()) {
      // Kept open over the instance, listing what still needs the player, and
      // with nothing left to import: another import would be another instance.
      setOutcome(joined);
      setImporting(false);
    } else {
      toast({
        kind: "error",
        title: `Imported ${instance.name}, with problems`,
        body: problems.join(" "),
      });
    }
  }

  function openExisting() {
    if (!preview?.existing) return;
    useStore.getState().navigate({ view: "instance", id: preview.existing.id });
    onClose();
  }

  const info = preview?.info;
  const loader = info ? softwareName(info.loader) : "";

  if (step === "done") {
    return (
      <Modal
        title="Join a server"
        subtitle="Get a shared server's pack from its admin's Geode, ready to play."
        onClose={onClose}
        width={540}
        footer={
          <button className="btn btn-primary" onClick={onClose}>
            Done
          </button>
        }
      >
        {outcome && (
          <>
            <StatusNote line={outcomeLine(outcome)} />
            <ul className={styles.problems}>
              {outcome.problems.map((problem, i) => (
                <li key={i}>{problem}</li>
              ))}
            </ul>
          </>
        )}
      </Modal>
    );
  }

  return (
    <Modal
      title="Join a server"
      subtitle="Get a shared server's pack from its admin's Geode, ready to play."
      onClose={onClose}
      width={540}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            {importing ? "Close" : "Cancel"}
          </button>
          {step === "lookUp" && (
            <button
              className="btn btn-primary"
              onClick={() => void lookUp()}
              disabled={!input || looking}
            >
              {looking ? <Loader2 size={14} className="spin" /> : <Search size={14} />}
              Look up
            </button>
          )}
          {step === "import" && (
            <button
              className="btn btn-primary"
              onClick={() => void importPack()}
              disabled={importing}
            >
              {importing && <Loader2 size={14} className="spin" />}
              Import
            </button>
          )}
          {step === "openExisting" && (
            <>
              <button className="btn" onClick={() => void importPack()} disabled={importing}>
                {importing && <Loader2 size={14} className="spin" />}
                Import a second copy
              </button>
              <button className="btn btn-primary" onClick={openExisting} disabled={importing}>
                <ArrowRight size={14} />
                Open instance
              </button>
            </>
          )}
        </>
      }
    >
      <div className={styles.modes} role="tablist">
        {(
          [
            ["link", "Link"],
            ["address", "Address and code"],
          ] as const
        ).map(([mode, label]) => (
          <button
            key={mode}
            role="tab"
            aria-selected={form.mode === mode}
            className={clsx(styles.mode, form.mode === mode && styles.modeActive)}
            onClick={() => edit({ mode })}
            disabled={importing}
          >
            {label}
          </button>
        ))}
      </div>

      {form.mode === "link" ? (
        <div>
          <label className="label" htmlFor="join-link">
            Link
          </label>
          <input
            id="join-link"
            className={clsx("input", styles.mono)}
            value={form.link}
            placeholder="geode://join?…"
            spellCheck={false}
            autoComplete="off"
            disabled={importing}
            onChange={(e) => edit({ link: e.target.value })}
            onKeyDown={(e) => e.key === "Enter" && void lookUp()}
          />
          <p className="hint">The link a server&apos;s admin copied from Geode.</p>
        </div>
      ) : (
        <div>
          <div className={styles.pair}>
            <div>
              <label className="label" htmlFor="join-address">
                Address
              </label>
              <input
                id="join-address"
                className="input"
                value={form.address}
                placeholder="play.example.com:25590"
                spellCheck={false}
                autoComplete="off"
                disabled={importing}
                onChange={(e) => edit({ address: e.target.value })}
                onKeyDown={(e) => e.key === "Enter" && void lookUp()}
              />
            </div>
            <div>
              <label className="label" htmlFor="join-code">
                Code
              </label>
              <input
                id="join-code"
                className={clsx("input", styles.mono)}
                value={form.code}
                placeholder="GEODE-XXXXX-XXXXX"
                spellCheck={false}
                autoComplete="off"
                disabled={importing}
                onChange={(e) => edit({ code: e.target.value })}
                onKeyDown={(e) => e.key === "Enter" && void lookUp()}
              />
            </div>
          </div>
          {/* Once looked up, the verification note below says this itself. */}
          {!preview && (
            <p className="hint">
              Type the port too, unless it is {DEFAULT_SHARE_PORT}. Without a
              link there is nothing to check a first connection against, so
              Geode trusts it, and holds this address to that certificate once
              you import. Ask the admin for the link if you can.
            </p>
          )}
        </div>
      )}

      {looking && (
        <p className={styles.pending}>
          <Loader2 size={13} className="spin" />
          Asking the server what it shares…
        </p>
      )}

      {error && (
        <div className={styles.error} role="alert">
          <AlertTriangle size={15} />
          <span>{error}</span>
        </div>
      )}

      {preview && info && (
        <>
          <div className={styles.pack}>
            <div className={styles.packHead}>
              <span className={styles.packIcon}>
                <Package size={16} />
              </span>
              <span className={styles.packName}>{info.name}</span>
            </div>
            <dl className={styles.facts}>
              <dt>Minecraft</dt>
              <dd>{info.mcVersion}</dd>
              <dt>Loader</dt>
              <dd>
                {loader}
                {info.loaderVersion && ` ${info.loaderVersion}`}
              </dd>
              <dt>Mods</dt>
              <dd>{info.mods === 0 ? "None" : info.mods}</dd>
              <dt>Server</dt>
              <dd>{softwareName(info.serverKind)}</dd>
              <dt>Address</dt>
              <dd className={styles.mono}>{preview.address}</dd>
            </dl>
          </div>

          <StatusNote line={verificationLine(preview.verified, typed)} />

          {preview.existing && (
            <StatusNote
              line={{
                tone: "info",
                title: `Already joined as ${preview.existing.name}`,
                detail:
                  "This server's pack is already in that instance, so open it to play. Importing again makes a second, separate instance: Geode cannot update the first one from the server yet, so a copy is only worth making if the server's pack has changed.",
              }}
            />
          )}

          <p className="hint" style={{ marginTop: 0 }}>
            Importing makes a new instance and downloads this server&apos;s mods,
            which run as code when you play. Join servers you trust.
          </p>
        </>
      )}
    </Modal>
  );
}

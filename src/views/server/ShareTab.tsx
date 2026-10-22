import { useEffect, useState } from "react";
import clsx from "clsx";
import {
  Check,
  Copy,
  ListTree,
  Loader2,
  RefreshCw,
  Settings as SettingsIcon,
  Share2,
  SlidersHorizontal,
  Square,
} from "lucide-react";

import Modal from "../../components/Modal";
import StatusNote from "../../components/StatusNote";
import { api, errorMessage } from "../../lib/api";
import { fileSize } from "../../lib/format";
import {
  addressChangedLine,
  clientFilesHint,
  configChoices,
  forbiddenAsTyped,
  forbiddenLabel,
  forbiddenRuleHint,
  gameAddressHint,
  linkPendingReason,
  listenLine,
  localAddressHint,
  movedSetting,
  nothingToPushHint,
  pickingSettingsHint,
  pushingHint,
  readOutHint,
  refusedSettingLine,
  replacedPasswordBody,
  routerLine,
  settingsRowNote,
  stoppedLine,
  whenPlayersGetChanges,
  wholeConfigHint,
} from "../../lib/share";
import { useStore } from "../../lib/store";
import type {
  ConfigPolicy,
  Forbidden,
  PushedSetting,
  Server,
  ServerShareView,
  SettingsPicker,
  SettingsReview,
  WantedSetting,
} from "../../lib/types";

/**
 * One setting, named the same way wherever it is listed: by its file and the
 * parts of its key, with nothing joined by a character a real key could hold.
 */
function settingId(one: { path: string; key: string[] }): string {
  return [one.path, ...one.key].join("\u0000");
}

/**
 * The new-rule fields before the admin has typed anything. The site is where
 * the select starts rather than part of the rule — `forbiddenAsTyped` drops it
 * when there is no project id for it to belong to, which is what lets a rule
 * name a mod id on its own.
 */
const NO_RULE: Forbidden = { provider: "modrinth", projectId: "", modId: "", reason: "" };

// The server page's own sheet for the shell every tab shares — sections,
// headings, the toggle with a sentence under it — so this tab cannot drift from
// Settings and Automation beside it.
import viewStyles from "../ServerView.module.css";
import styles from "./ShareTab.module.css";

function CopyButton({
  text,
  what,
  label = "Copy",
  small = false,
  onCopied,
}: {
  text: string;
  what: string;
  /** The wording on the button, for one that is not beside what it copies. */
  label?: string;
  /** Smaller, for a copy that is secondary to the one above it. */
  small?: boolean;
  onCopied?: () => void;
}) {
  const reportError = useStore((s) => s.reportError);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1800);
    return () => clearTimeout(timer);
  }, [copied]);

  return (
    <button
      className={clsx("btn", small && "btn-sm")}
      aria-label={`Copy the ${what}`}
      onClick={() =>
        navigator.clipboard
          .writeText(text)
          .then(() => {
            setCopied(true);
            onCopied?.();
          })
          .catch((e) => reportError(`Could not copy the ${what}`, e))
      }
    >
      {copied ? <Check size={14} /> : <Copy size={14} />}
      {copied ? "Copied" : label}
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
  const setShareStatus = useStore((s) => s.setShareStatus);
  // The sidebar and the Servers cards badge a stale link from the server
  // list's own payload, so a change made here has to be asked for again or
  // the badge goes on saying what was true before.
  const refreshServers = useStore((s) => s.refreshServers);
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
  const [busy, setBusy] = useState<
    | "share"
    | "config"
    | "listing"
    | "forbidden"
    | "rotate"
    | "stop"
    | "review"
    | "push"
    | "picker"
    | "picks"
    | null
  >(null);
  /**
   * What Review found, while the admin looks at it. `null` when the screen is
   * closed — it is read fresh every time it opens, because the file on disk is
   * the admin's and may have moved on since.
   */
  const [review, setReview] = useState<SettingsReview | null>(null);
  /** Which of those settings are ticked, by [`settingId`]. */
  const [chosen, setChosen] = useState<Set<string>>(new Set());
  /**
   * Every settings file this server has, while the admin is picking from them.
   * `null` when the list is closed — it reads each file to say whether that
   * file can be kept in sync, so it is read when it is asked for and not with
   * the rest of the tab.
   */
  const [picker, setPicker] = useState<SettingsPicker | null>(null);
  /**
   * The picks the admin is making, by path. A path in here travels; one that
   * is not does not. Kept beside `picker` rather than in it so nothing is
   * saved until they press Save.
   */
  const [picks, setPicks] = useState<Map<string, ConfigPolicy>>(new Map());
  /** The rule the admin is typing, before it is added to the list. */
  const [rule, setRule] = useState<Forbidden>(NO_RULE);
  const [confirming, setConfirming] = useState<"rotate" | "stop" | null>(null);
  /**
   * The router is being asked again, right now. Its own state rather than
   * `busy`: the ask changes nothing, so none of the buttons that do need
   * disabling while it runs.
   */
  const [asking, setAsking] = useState(false);
  /**
   * The two listing fields while the admin is editing them. `null` means they
   * have not touched them, so what is shown is what the backend last said —
   * the stored pair, or its suggestion.
   */
  const [editing, setEditing] = useState<{ name: string; address: string } | null>(null);

  const hasPublicAddress = Boolean(config?.share.publicAddress);
  const changed = addressChangedLine(status, share?.lastLinkAddress ?? null);

  /**
   * What the two listing fields hold: the admin's edits, else the saved pair,
   * else the suggestion. A suggested address of null leaves the field empty,
   * which is the honest answer when Geode cannot work one out.
   */
  const fields =
    editing ??
    (share
      ? {
          name: share.listing?.name ?? share.suggested.name,
          address: share.listing?.address ?? share.suggested.address ?? "",
        }
      : null);
  const listingChanged =
    Boolean(share) &&
    Boolean(fields) &&
    (fields!.name !== share!.listing?.name || fields!.address !== share!.listing?.address);

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

  /**
   * Ask the router now instead of waiting for the next time Geode would.
   *
   * For the admin who has just bridged a modem or turned UPnP on: the answer
   * here is otherwise up to half an hour old, and nothing on the page can
   * tell them whether what they just did worked. Changes nothing on the
   * router, and comes back with the same status the page already reads.
   */
  async function askRouter() {
    setAsking(true);
    try {
      setShareStatus(await api.recheckRouter());
    } catch (e) {
      reportError("Could not ask your router again", e);
    } finally {
      setAsking(false);
    }
  }

  async function start() {
    const view = await run("share", `Could not share ${server.name}`, () =>
      api.shareServer(server.id),
    );
    if (view) {
      setShare(view);
      loadShareStatus().catch(() => {});
      // Sharing again keeps the address the last link was taken at, so a
      // server that was already stale is stale again the moment it is shared.
      void refreshServers();
    }
  }

  /**
   * Save the whole list, which is what the backend takes: the list here is the
   * list, so an entry taken out of it is taken out there with nothing to
   * reconcile.
   */
  async function saveForbidden(next: Forbidden[]): Promise<boolean> {
    const view = await run("forbidden", "Could not change which mods are allowed", () =>
      api.setServerForbidden(server.id, next),
    );
    if (view) setShare(view);
    return Boolean(view);
  }

  async function addRule() {
    if (!share) return;
    if (await saveForbidden([...share.forbidden, forbiddenAsTyped(rule)])) setRule(NO_RULE);
  }

  /**
   * Send the config folder or stop sending it, and pick a behaviour for a
   * player's own changes to it when that is what the admin clicked.
   *
   * The switch sends no behaviour: turning the folder off and on again is
   * whatever the admin already picked, and this screen has nothing to send
   * back anyway, since a folder nobody is sent reports no behaviour. A guess
   * here would replace their choice and write every player's settings files
   * whole.
   */
  async function sendConfig(include: boolean, policy: ConfigPolicy | null) {
    const view = await run("config", "Could not change what is shared", () =>
      api.setServerShareConfig(server.id, include, policy),
    );
    if (view) setShare(view);
  }

  /**
   * Open the list of settings files, reading each one so the rows can say
   * which of them can be kept in sync. Closed again by pressing the same
   * button, which throws the unsaved picks away.
   */
  async function togglePicker() {
    if (picker) {
      setPicker(null);
      return;
    }
    const found = await run("picker", "Could not read this server's settings files", () =>
      api.getServerSettingsPicker(server.id),
    );
    if (!found) return;
    setPicks(
      new Map(found.files.filter((row) => row.sent).map((row) => [row.path, row.policy])),
    );
    setPicker(found);
  }

  /** Tick a file to send, or untick it. */
  function pickFile(path: string, policy: ConfigPolicy, sent: boolean) {
    setPicks((was) => {
      const next = new Map(was);
      if (sent) next.set(path, policy);
      else next.delete(path);
      return next;
    });
  }

  /**
   * Save the whole list of picks, which is what the backend takes. An empty
   * list goes back to sending the config folder whole — including files a mod
   * writes later — so the button says which of the two it will do.
   */
  async function savePicks() {
    const view = await run("picks", "Could not change which settings files are sent", () =>
      api.setServerSettingsPicks(
        server.id,
        [...picks].map(([path, policy]) => ({ path, policy })),
      ),
    );
    if (!view) return;
    setShare(view);
    // Read back rather than kept: a pick can change what another row would do
    // — the behaviour chosen for one file, and whether a path is refused — and
    // the backend is the only thing that decides either.
    const found = await api.getServerSettingsPicker(server.id).catch(() => null);
    if (found) {
      setPicks(
        new Map(found.files.filter((row) => row.sent).map((row) => [row.path, row.policy])),
      );
      setPicker(found);
    }
  }

  /**
   * Open Review: read what has moved since this server last published, and
   * tick everything but the removals.
   *
   * Removals start unticked because taking a setting out of somebody's config
   * is the one thing here that changing a value again cannot undo.
   */
  async function openReview() {
    const found = await run("review", "Could not read this server's settings", () =>
      api.reviewServerSettings(server.id),
    );
    if (!found) return;
    setChosen(
      new Set(found.moved.filter((one) => one.now !== null).map(settingId)),
    );
    setReview(found);
  }

  /** Send the settings named. Players get them the next time they press Play. */
  async function send(wanted: WantedSetting[]) {
    const view = await run("push", "Could not send those settings", () =>
      api.pushServerSettings(server.id, wanted),
    );
    if (!view) return;
    setShare(view);
    setReview(null);
    toast({
      kind: "success",
      title: wanted.length === 1 ? "1 setting sent" : `${wanted.length} settings sent`,
      body: "Players get it the next time they press Play, spliced into their own copies. Nothing else in those files changes.",
    });
  }

  /** Send one the admin already owns again, which is what a setting changed
   * and changed back needs: the value has not moved, so Review cannot see it. */
  function sendAgain(one: PushedSetting) {
    void send([{ path: one.path, key: one.key, value: one.value, was: one.was }]);
  }

  /**
   * The admin has taken the link, so Geode can tell them later if the address
   * it points at has changed. Only this link: the one for this network points
   * somewhere players elsewhere were never sent.
   *
   * A failure here costs nothing but that note, and the copy itself has
   * already worked, so it is not worth an error in front of the admin.
   */
  async function remember() {
    try {
      setShare(await api.shareLinkCopied(server.id));
      // The address just written is the one the badge compares, so without
      // this the badge never appears for a first link and never clears for a
      // replacement.
      void refreshServers();
    } catch {
      /* the link is on the clipboard either way */
    }
  }

  async function saveListing() {
    if (!fields) return;
    const view = await run("listing", "Could not save how players see this server", () =>
      api.setServerListing(server.id, fields.name, fields.address),
    );
    if (view) {
      setShare(view);
      // Back to showing what the backend says, which is the saved pair
      // normalised — `play.example.com:25565` comes back without the port.
      setEditing(null);
      toast({
        kind: "success",
        title: "Saved",
        body: `Players get ${view.listing?.name ?? server.name} in their server list the next time they press Play.`,
      });
    }
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
      // A share nobody can download from has no links worth re-sending, so
      // the badge has to go with it.
      void refreshServers();
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
      never loads them. The one exception is a mod you keep on this server,
      which you say per mod in the Content tab: players get nothing of it, not
      the file and not its name.
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
                    <CopyButton
                      text={share.link}
                      what="link"
                      onCopied={() => void remember()}
                    />
                  </div>
                  <p className="hint">
                    Send this to players. Opening it shows them the pack in
                    Geode before anything is downloaded.
                  </p>
                  {/* Only once a link has been taken and the address has moved
                      on since. Nothing is said before that: an address that
                      has not changed is not news. */}
                  {changed && <StatusNote line={changed} />}
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

            <p className="hint" style={{ marginTop: 0 }}>
              {whenPlayersGetChanges(share.configPolicy)}
            </p>

            {/* The address as well as the password: typed by hand, a player
                needs both, and outside a link the address — its port above
                all — is shown nowhere else. */}
            <div>
              {/* "Pack", because the game address is a field of its own just
                  below and an unqualified "address" is the confusion itself. */}
              <span className="label">Pack address and password</span>
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
              <p className="hint">{readOutHint(status)}</p>
            </div>

            {/* The same machine, reached the short way. A player in the house
                cannot always be sent out to the router and back, and neither
                can the admin testing their own link. */}
            {status?.localAddress && (
              <div>
                <span className="label">On this network</span>
                <div className={styles.readOut}>
                  <div className={styles.copyRow}>
                    <code className={styles.code}>{status.localAddress}</code>
                    <CopyButton
                      text={status.localAddress}
                      what="address for this network"
                    />
                  </div>
                  {share.localLink && (
                    // Smaller than the link above on purpose: that one is what
                    // players are sent, and this one reaches no further than
                    // this network.
                    <div className={styles.actions}>
                      <CopyButton
                        small
                        label="Copy the link for this network"
                        text={share.localLink}
                        what="link for this network"
                      />
                    </div>
                  )}
                </div>
                <p className="hint">{localAddressHint(status)}</p>
              </div>
            )}

            {/* The game address, beside the pack address above rather than
                anywhere else, because the one mistake this screen can cause is
                an admin reading the pack port as the one players connect on. */}
            {fields && (
              <div>
                <span className="label">In players&apos; server lists</span>
                <div className={styles.pair}>
                  <div>
                    <label className="label" htmlFor="listing-name">
                      Server name
                    </label>
                    <input
                      id="listing-name"
                      className="input"
                      value={fields.name}
                      placeholder={share.suggested.name}
                      maxLength={64}
                      spellCheck={false}
                      disabled={busy !== null}
                      onChange={(e) =>
                        setEditing({ ...fields, name: e.target.value })
                      }
                      onKeyDown={(e) => e.key === "Enter" && void saveListing()}
                    />
                  </div>
                  <div>
                    <label className="label" htmlFor="listing-address">
                      Game address
                    </label>
                    <input
                      id="listing-address"
                      className={clsx("input", styles.mono)}
                      value={fields.address}
                      placeholder="play.example.com:25565"
                      spellCheck={false}
                      autoComplete="off"
                      disabled={busy !== null}
                      onChange={(e) =>
                        setEditing({ ...fields, address: e.target.value })
                      }
                      onKeyDown={(e) => e.key === "Enter" && void saveListing()}
                    />
                  </div>
                </div>
                <p className="hint">{gameAddressHint(share)}</p>
                {listingChanged && (
                  <div className={styles.actions}>
                    <button
                      className="btn btn-primary"
                      onClick={() => void saveListing()}
                      disabled={busy !== null || !fields.address.trim()}
                    >
                      {busy === "listing" && <Loader2 size={14} className="spin" />}
                      Save
                    </button>
                    <button
                      className="btn"
                      onClick={() => setEditing(null)}
                      disabled={busy !== null}
                    >
                      Cancel
                    </button>
                  </div>
                )}
              </div>
            )}

            <div>
              <label className={viewStyles.settingToggle}>
                <input
                  type="checkbox"
                  checked={share.configPolicy !== null}
                  disabled={busy !== null}
                  onChange={(e) => void sendConfig(e.target.checked, null)}
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
              {/* Only once the folder is going anywhere. What happens to a
                  player's own changes to a folder nobody is sent is not a
                  choice worth making. */}
              {share.configPolicy !== null && (
                <div className={styles.choices}>
                  {configChoices.map((choice) => (
                    <label
                      key={choice.policy}
                      className={viewStyles.settingToggle}
                    >
                      <input
                        type="radio"
                        name="config-policy"
                        checked={share.configPolicy === choice.policy}
                        disabled={busy !== null}
                        onChange={() => void sendConfig(true, choice.policy)}
                      />
                      <span>
                        <strong>{choice.label}</strong>
                        <span className={viewStyles.settingWhy}>
                          {choice.detail}
                        </span>
                      </span>
                    </label>
                  ))}
                  {/* Only the behaviour that has anything to send. The hint
                      stays here beside the button rather than appearing once
                      in a dialog: an edit nobody sent reaches nobody, and a
                      push cannot be recalled. */}
                  {share.configPolicy === "merge" && (
                    <div className={styles.push}>
                      <button
                        className="btn"
                        onClick={() => void openReview()}
                        disabled={busy !== null}
                      >
                        {busy === "review" ? (
                          <Loader2 size={14} className="spin" />
                        ) : (
                          <SlidersHorizontal size={14} />
                        )}
                        Review settings changes
                      </button>
                      <p className="hint">{pushingHint()}</p>
                    </div>
                  )}

                  {/* Picking files is a refinement of the choice above, so it
                      sits under it: the behaviour is what happens to a file,
                      and this is which files there are. */}
                  <div className={styles.push}>
                    <button
                      className="btn"
                      onClick={() => void togglePicker()}
                      disabled={busy !== null}
                    >
                      {busy === "picker" ? (
                        <Loader2 size={14} className="spin" />
                      ) : (
                        <ListTree size={14} />
                      )}
                      {picker ? "Hide the file list" : "Choose which files travel"}
                    </button>
                    {!picker && <p className="hint">{pickingSettingsHint()}</p>}
                  </div>

                  {picker && (
                    <div className={styles.picker}>
                      <p className="hint">{pickingSettingsHint()}</p>
                      {picker.wholeConfig && (
                        <StatusNote
                          line={{
                            tone: "info",
                            title: "Sending the whole folder",
                            detail: wholeConfigHint(),
                          }}
                        />
                      )}
                      {picker.files.length === 0 ? (
                        <p className="hint">
                          This server has no settings files yet. One appears here
                          once a mod writes its config, or once you put a copy of
                          a client file in its client-files folder.
                        </p>
                      ) : (
                        <ul className={styles.files}>
                          {picker.files.map((row) => {
                            const chosenPolicy = picks.get(row.path);
                            const sent = chosenPolicy !== undefined;
                            const policy = chosenPolicy ?? row.policy;
                            return (
                              <li className={styles.file} key={row.path}>
                                <label className={styles.fileWhat}>
                                  <input
                                    type="checkbox"
                                    checked={sent}
                                    disabled={busy !== null}
                                    onChange={(e) =>
                                      pickFile(row.path, policy, e.target.checked)
                                    }
                                  />
                                  <span className={styles.fileName}>
                                    <strong className={styles.mono}>{row.path}</strong>
                                    <span className={viewStyles.settingWhy}>
                                      {fileSize(row.size)} ·{" "}
                                      {settingsRowNote({ ...row, policy })}
                                    </span>
                                  </span>
                                </label>
                                <select
                                  className="input"
                                  aria-label={`What happens to a player's own changes to ${row.path}`}
                                  value={policy}
                                  disabled={busy !== null || !sent}
                                  onChange={(e) =>
                                    pickFile(row.path, e.target.value as ConfigPolicy, true)
                                  }
                                >
                                  {configChoices.map((choice) => (
                                    <option
                                      key={choice.policy}
                                      value={choice.policy}
                                      // Nothing can be pushed out of a file no
                                      // setting can be found inside, so it is
                                      // not offered — the row says why.
                                      disabled={choice.policy === "merge" && !row.canSync}
                                    >
                                      {choice.label}
                                    </option>
                                  ))}
                                </select>
                              </li>
                            );
                          })}
                        </ul>
                      )}

                      {picker.refused.length > 0 && (
                        <ul className={styles.refused}>
                          {picker.refused.map((row) => (
                            <li key={row.path}>{refusedSettingLine(row)}</li>
                          ))}
                        </ul>
                      )}

                      <p className="hint">{clientFilesHint()}</p>
                      <div className={styles.actions}>
                        <button
                          className="btn btn-primary"
                          onClick={() => void savePicks()}
                          disabled={busy !== null}
                        >
                          {busy === "picks" && <Loader2 size={14} className="spin" />}
                          {picks.size === 0
                            ? "Send the whole folder"
                            : picks.size === 1
                              ? "Send this 1 file"
                              : `Send these ${picks.size} files`}
                        </button>
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>

            {/* A rule, and the tab says so where it is written: Geode turns a
                mod off in its own launcher and nothing more. An admin who
                reads this as a wall is worse off than one who was told. */}
            <div>
              <span className="label">Mods players may not use</span>
              {share.forbidden.length > 0 && (
                <ul className={styles.rules}>
                  {share.forbidden.map((entry, at) => (
                    <li className={styles.rule} key={`${entry.provider}/${entry.projectId}/${entry.modId}`}>
                      <span className={styles.ruleWhat}>
                        <strong>{forbiddenLabel(entry)}</strong>
                        <span className={viewStyles.settingWhy}>{entry.reason}</span>
                      </span>
                      <button
                        className="btn btn-sm"
                        aria-label={`Allow ${forbiddenLabel(entry)} again`}
                        disabled={busy !== null}
                        onClick={() =>
                          void saveForbidden(share.forbidden.filter((_, i) => i !== at))
                        }
                      >
                        Allow again
                      </button>
                    </li>
                  ))}
                </ul>
              )}
              <div className={styles.pair}>
                <div>
                  <label className="label" htmlFor="forbid-mod-id">
                    Mod id
                  </label>
                  <input
                    id="forbid-mod-id"
                    className={clsx("input", styles.mono)}
                    value={rule.modId}
                    placeholder="xaerominimap"
                    spellCheck={false}
                    autoComplete="off"
                    disabled={busy !== null}
                    onChange={(e) => setRule({ ...rule, modId: e.target.value })}
                  />
                </div>
                <div>
                  <label className="label" htmlFor="forbid-project-id">
                    Or its project id
                  </label>
                  <div className={styles.copyRow}>
                    <select
                      className="input"
                      aria-label="Which site that project id is from"
                      value={rule.provider}
                      disabled={busy !== null}
                      onChange={(e) => setRule({ ...rule, provider: e.target.value })}
                    >
                      <option value="modrinth">Modrinth</option>
                      <option value="curseforge">CurseForge</option>
                    </select>
                    <input
                      id="forbid-project-id"
                      className={clsx("input", styles.mono)}
                      value={rule.projectId}
                      placeholder="AANobbMI"
                      spellCheck={false}
                      autoComplete="off"
                      disabled={busy !== null}
                      onChange={(e) => setRule({ ...rule, projectId: e.target.value })}
                    />
                  </div>
                </div>
              </div>
              <div className={styles.actions}>
                <input
                  className={clsx("input", styles.grow)}
                  aria-label="Why this mod is not allowed"
                  value={rule.reason}
                  placeholder="Why — players are shown this"
                  maxLength={200}
                  disabled={busy !== null}
                  onChange={(e) => setRule({ ...rule, reason: e.target.value })}
                  onKeyDown={(e) => e.key === "Enter" && void addRule()}
                />
                <button
                  className="btn"
                  onClick={() => void addRule()}
                  disabled={
                    busy !== null ||
                    !rule.reason.trim() ||
                    (!rule.modId.trim() && !rule.projectId.trim())
                  }
                >
                  {busy === "forbidden" && <Loader2 size={14} className="spin" />}
                  Do not allow
                </button>
              </div>
              <p className="hint">
                A mod id is what the jar calls itself, which matches whoever has
                it; a project id matches only a copy Geode installed.{" "}
                {forbiddenRuleHint()}
              </p>
            </div>

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
                <StatusNote
                  line={listenLine(status)}
                  busy={Boolean(status.retry) && !status.retry?.needsYou}
                />
                <StatusNote
                  line={routerLine(status, hasPublicAddress)}
                  busy={asking || status.mapping.state === "searching"}
                />
              </>
            )}
            <div className={styles.actions}>
              {/* The answer above is otherwise as much as half an hour old,
                  which is no use to an admin who has just changed something on
                  the router and wants to know whether it worked. */}
              {status && !statusError && (
                <button
                  className="btn"
                  onClick={() => void askRouter()}
                  disabled={asking}
                  title="Ask your router for its address again, without touching the mapping it already has"
                >
                  {asking ? (
                    <Loader2 size={14} className="spin" />
                  ) : (
                    <RefreshCw size={14} />
                  )}
                  {asking ? "Asking the router…" : "Ask the router again"}
                </button>
              )}
              {/* The port above is the one packs come down, and the only one
                  Geode ever opens — an admin who reads it as the game address
                  hands out a port nothing is listening on. */}
              <p className={clsx(viewStyles.pending, styles.grow)}>
                Every server you share uses the same port and router setup. It
                is the port packs are downloaded on, not the one Minecraft
                connects to: forward this server&apos;s server-port yourself,
                or point a tunnel at it.
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

      {/* What moved since this server last published, for the admin to look
          at before anybody gets it. A confirm step, so it is a dialog — but
          what it costs is written on the tab behind it, where it is read
          before this is ever opened. */}
      {review && (
        <Modal
          title="Settings to send"
          subtitle={server.name}
          onClose={() => setReview(null)}
          width={620}
          footer={
            <>
              <button className="btn" onClick={() => setReview(null)}>
                Cancel
              </button>
              <button
                className="btn btn-primary"
                disabled={busy !== null || chosen.size === 0}
                onClick={() =>
                  void send(
                    review.moved
                      .filter((one) => chosen.has(settingId(one)))
                      .map((one) => ({
                        path: one.path,
                        key: one.key,
                        value: one.now,
                        was: one.was,
                      })),
                  )
                }
              >
                {busy === "push" && <Loader2 size={14} className="spin" />}
                {chosen.size === 1 ? "Send 1 setting" : `Send ${chosen.size} settings`}
              </button>
            </>
          }
        >
          {review.moved.length === 0 ? (
            <p className="hint">{nothingToPushHint()}</p>
          ) : (
            <ul className={styles.rules}>
              {review.moved.map((one) => {
                const id = settingId(one);
                return (
                  <li className={styles.rule} key={id}>
                    <label className={viewStyles.settingToggle}>
                      <input
                        type="checkbox"
                        checked={chosen.has(id)}
                        disabled={busy !== null}
                        onChange={(e) =>
                          setChosen((was) => {
                            const next = new Set(was);
                            if (e.target.checked) next.add(id);
                            else next.delete(id);
                            return next;
                          })
                        }
                      />
                      <span>
                        <strong className={styles.mono}>{one.path}</strong>
                        <span className={viewStyles.settingWhy}>
                          {movedSetting(one)}
                        </span>
                      </span>
                    </label>
                  </li>
                );
              })}
            </ul>
          )}

          {/* Named rather than left out: a label reading "only the settings I
              change" over a file that silently does something else is the one
              thing this screen must not do. */}
          {review.unreadable.length > 0 && (
            <>
              <span className="label">Sent once instead</span>
              <ul className={styles.rules}>
                {review.unreadable.map((one) => (
                  <li className={styles.rule} key={one.path}>
                    <span className={styles.ruleWhat}>
                      <strong className={styles.mono}>{one.path}</strong>
                      <span className={viewStyles.settingWhy}>{one.why}</span>
                    </span>
                  </li>
                ))}
              </ul>
            </>
          )}

          {review.pushed.length > 0 && (
            <>
              <span className="label">Already sent</span>
              <ul className={styles.rules}>
                {review.pushed.map((one) => (
                  <li className={styles.rule} key={settingId(one)}>
                    <span className={styles.ruleWhat}>
                      <strong className={styles.mono}>{one.path}</strong>
                      <span className={viewStyles.settingWhy}>
                        {one.key.join(" → ")}
                        {one.value === null
                          ? " — removed"
                          : ` — ${one.value}`}
                      </span>
                    </span>
                    <button
                      className="btn btn-sm"
                      disabled={busy !== null}
                      onClick={() => sendAgain(one)}
                    >
                      Send again
                    </button>
                  </li>
                ))}
              </ul>
            </>
          )}
        </Modal>
      )}
    </div>
  );
}

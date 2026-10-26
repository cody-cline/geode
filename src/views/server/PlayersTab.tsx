import { useCallback, useEffect, useState } from "react";
import clsx from "clsx";
import {
  AlertTriangle,
  Ban,
  Globe,
  Loader2,
  LogOut,
  Plus,
  RefreshCw,
  Shield,
  Trash2,
  Users,
} from "lucide-react";

import Modal from "../../components/Modal";
import { api } from "../../lib/api";
import { useStore } from "../../lib/store";
import type {
  OnlinePlayers,
  PlayerChange,
  PlayerConsole,
  PlayerListEntry,
  PlayerListId,
  PlayerLists,
  Server,
} from "../../lib/types";

import viewStyles from "../ServerView.module.css";
import styles from "./PlayersTab.module.css";

/**
 * One of the four lists, and how it is described to the person using it.
 *
 * The file name is here because the screen says it out loud: when the server is
 * stopped Geode writes that file, and an admin is owed the name of the thing
 * being written.
 */
const LISTS = [
  {
    id: "whitelist" as PlayerListId,
    title: "Whitelist",
    file: "whitelist.json",
    icon: Users,
    blurb: "Who may join, when the whitelist is on.",
    empty: "Nobody is whitelisted.",
    remove: "Remove",
    placeholder: "Minecraft name",
    takes: "name" as const,
    reason: false,
    level: false,
  },
  {
    id: "ops" as PlayerListId,
    title: "Operators",
    file: "ops.json",
    icon: Shield,
    blurb: "Who may run the server's commands.",
    empty: "No operators.",
    remove: "Remove",
    placeholder: "Minecraft name",
    takes: "name" as const,
    reason: false,
    level: true,
  },
  {
    id: "bans" as PlayerListId,
    title: "Banned players",
    file: "banned-players.json",
    icon: Ban,
    blurb: "Kept out until the ban is lifted.",
    empty: "Nobody is banned.",
    remove: "Lift the ban",
    placeholder: "Minecraft name",
    takes: "name" as const,
    reason: true,
    level: false,
  },
  {
    id: "ip-bans" as PlayerListId,
    title: "Banned IP addresses",
    file: "banned-ips.json",
    icon: Globe,
    blurb: "Everyone connecting from that address is kept out.",
    empty: "No addresses are banned.",
    remove: "Lift the ban",
    placeholder: "203.0.113.9",
    takes: "ip" as const,
    reason: true,
    level: false,
  },
];

type ListConfig = (typeof LISTS)[number];

/**
 * How one change is reported, whichever way it went.
 *
 * Three words rather than one because the two directions know different
 * things. A file Geode wrote either was written or threw, so `done` can state
 * the outcome. A console command's outcome is whatever the server said, and
 * that is reported under `asked` — the thing that was asked for — so the
 * screen never puts a success headline above the server saying no.
 */
type Labels = {
  done: string;
  asked: string;
  failed: string;
};

/** A confirmation that names who it affects and says what it does. */
type Ask = Labels & {
  title: string;
  body: string;
  verb: string;
  danger: boolean;
  run: () => Promise<PlayerChange>;
};

/**
 * What a row is called on screen, and in the sentence asking about it.
 *
 * An address ban has no account behind it, so it is called by its address. A
 * row the file recorded with a UUID and no name — hand-edited, or a plugin's
 * doing — has nothing else to show, and calling it by the key it is keyed by
 * beats calling it by a blank.
 */
function labelOf(entry: PlayerListEntry): string {
  return entry.ip ?? (entry.name || entry.uuid);
}

/**
 * What a ban does, said the same way wherever it is asked for.
 *
 * Shared so the ban offered from the online list and the ban typed into a
 * pane's own field cannot come to say different things about the same act.
 */
function banBody(list: PlayerListId, who: string): string {
  return list === "ip-bans"
    ? `Everyone connecting from ${who} is kept out until the ban is lifted.`
    : `${who} is put off the server and kept out until the ban is lifted. That is not a kick — a kick lets them come straight back.`;
}

function entriesOf(lists: PlayerLists, list: PlayerListId): PlayerListEntry[] {
  switch (list) {
    case "whitelist":
      return lists.whitelist;
    case "ops":
      return lists.ops;
    case "bans":
      return lists.bans;
    case "ip-bans":
      return lists.ipBans;
  }
}

/**
 * Whether this server reported having the command an addition would need.
 *
 * Only consulted while the server is running — a stopped server's lists are
 * edited as files and need no command at all.
 */
function canAdd(console: PlayerConsole, list: PlayerListId): boolean {
  switch (list) {
    case "whitelist":
      return console.whitelistAdd;
    case "ops":
      return console.op;
    case "bans":
      return console.ban;
    case "ip-bans":
      return console.banIp;
  }
}

function canRemove(console: PlayerConsole, list: PlayerListId): boolean {
  switch (list) {
    case "whitelist":
      return console.whitelistRemove;
    case "ops":
      return console.deop;
    case "bans":
      return console.pardon;
    case "ip-bans":
      return console.pardonIp;
  }
}

/**
 * Who may play: who is on now, the whitelist, the operators and the bans.
 *
 * The one thing this screen must never do is imply a change happened when it
 * did not, so it says which way every change goes. While the server runs it
 * owns these files and changes go through its console, and what the server said
 * back is shown as it said it. While it is stopped the files are Geode's to
 * edit, and that is the direction that can also set an operator's level.
 */
export default function PlayersTab({ server }: { server: Server }) {
  const reportError = useStore((s) => s.reportError);
  const toast = useStore((s) => s.toast);

  const [lists, setLists] = useState<PlayerLists | null>(null);
  const [online, setOnline] = useState<OnlinePlayers | null>(null);
  const [onlineAt, setOnlineAt] = useState<string | null>(null);
  const [onlineProblem, setOnlineProblem] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [ask, setAsk] = useState<Ask | null>(null);

  const load = useCallback(
    () =>
      api
        .serverPlayerLists(server.id)
        .then(setLists)
        .catch((e) => reportError("Could not read this server's player lists", e)),
    [server.id, reportError],
  );

  useEffect(() => {
    void load();
  }, [load, server.running]);

  /**
   * Asked for rather than polled: every call is a real `/list` typed into the
   * console, which appears in the console pane. A refresh an admin pressed is
   * worth a line there; one every few seconds is not.
   */
  const refreshOnline = useCallback(async () => {
    setAsking(true);
    setOnlineProblem(null);
    try {
      const now = await api.serverOnlinePlayers(server.id);
      setOnline(now);
      setOnlineAt(new Date().toLocaleTimeString());
    } catch (e) {
      setOnline(null);
      setOnlineProblem(e instanceof Error ? e.message : String(e));
    } finally {
      setAsking(false);
    }
  }, [server.id]);

  const liveList = Boolean(lists?.running && lists.console.whoIsOnline);
  useEffect(() => {
    if (liveList) void refreshOnline();
    else {
      setOnline(null);
      setOnlineAt(null);
      setOnlineProblem(null);
    }
  }, [liveList, refreshOnline]);

  /**
   * Run a change and report what actually happened.
   *
   * Through the console, the server's own words are the result and are shown as
   * it said them — under what was asked for, not under a claim that it worked,
   * because "That player does not exist" is as likely an answer as "Added". A
   * file Geode wrote is a result Geode knows.
   */
  async function apply(labels: Labels, run: () => Promise<PlayerChange>) {
    setBusy(true);
    try {
      const change = await run();
      setLists(change.lists);
      toast(
        change.via === "console"
          ? {
              kind: "info",
              title: labels.asked,
              body: `The server said: ${change.said.join(" ")}`,
            }
          : {
              kind: "success",
              title: labels.done,
              body: "Geode changed the file — the server was not running.",
            },
      );
      if (change.via === "console" && liveList) void refreshOnline();
    } catch (e) {
      reportError(labels.failed, e);
    } finally {
      setBusy(false);
      setAsk(null);
    }
  }

  if (!lists) {
    return <div className="skeleton" style={{ height: 160 }} />;
  }

  /**
   * A console command of either kind is outstanding, so nothing may send
   * another one.
   *
   * Whoever waits on a reply takes every line the console produces while they
   * wait — the replies are not correlated, because a server's answer to a
   * command is in its own words and nothing marks which command it answers. So
   * two overlapping commands collect each other's replies, and a `list` typed
   * while a `whitelist add` waits puts the whitelist's answer in the online
   * panel and the online panel's in the whitelist's toast.
   */
  const inFlight = busy || asking;

  /** The sentence every confirmation ends with, so the direction is never a surprise. */
  const direction = (file: string): string =>
    lists.running
      ? "The server is running, so this goes through its console and the server writes its own file."
      : `The server is stopped, so Geode writes ${file} directly.`;

  function confirmRemove(config: ListConfig, entry: PlayerListEntry) {
    const who = labelOf(entry);
    const copy: Record<PlayerListId, { title: string; body: string }> = {
      whitelist: {
        title: `Take ${who} off the whitelist?`,
        body: `While the whitelist is on, ${who} will not be able to join until they are put back. It does not remove anyone already in the game.`,
      },
      ops: {
        title: `Take ${who}'s operator status away?`,
        body: `${who} keeps their place on the server and loses every operator command.`,
      },
      bans: {
        title: `Lift the ban on ${who}?`,
        body: `${who} will be able to join again straight away.`,
      },
      "ip-bans": {
        title: `Lift the ban on ${who}?`,
        body: `Anyone connecting from ${who} will be able to join again straight away.`,
      },
    };
    const lifting = config.id === "bans" || config.id === "ip-bans";
    setAsk({
      ...copy[config.id],
      body: `${copy[config.id].body} ${direction(config.file)}`,
      verb: lifting ? "Lift the ban" : "Remove",
      danger: config.id === "whitelist" || config.id === "ops",
      done: lifting
        ? `Lifted the ban on ${who}`
        : `Removed ${who} from ${config.title.toLowerCase()}`,
      asked: lifting
        ? `Asked the server to lift the ban on ${who}`
        : `Asked the server to take ${who} off ${config.title.toLowerCase()}`,
      failed: lifting
        ? `Could not lift the ban on ${who}`
        : `Could not take ${who} off ${config.title.toLowerCase()}`,
      // Named to the core by the key its file is keyed by, not by the name it
      // shows: two accounts can be recorded under one name, and only the UUID
      // picks out the row the admin clicked.
      run: () =>
        api.removeFromServerList(
          server.id,
          config.id,
          entry.ip ?? entry.name,
          entry.uuid || undefined,
        ),
    });
  }

  /**
   * Put somebody on a list, confirming it first when the list is a ban.
   *
   * The four panes share one layout and three of them take a plain Minecraft
   * name, so a name meant for the whitelist lands in "Banned players" easily
   * enough. A ban keeps somebody out until somebody lifts it, so it is
   * confirmed wherever it is asked for — the same as a ban from the online
   * list, and for the same reason.
   */
  function addTo(
    config: ListConfig,
    target: string,
    reason?: string,
    level?: number,
  ) {
    const labels = {
      done: `Added ${target} to ${config.title.toLowerCase()}`,
      asked: `Asked the server to add ${target} to ${config.title.toLowerCase()}`,
      failed: `Could not add ${target} to ${config.title.toLowerCase()}`,
    };
    const run = () =>
      api.addToServerList(server.id, config.id, target, reason, level);
    if (config.id !== "bans" && config.id !== "ip-bans") {
      void apply(labels, run);
      return;
    }
    setAsk({
      ...labels,
      title: `Ban ${target}?`,
      body: `${banBody(config.id, target)} ${direction(config.file)}`,
      verb: "Ban",
      danger: true,
      run,
    });
  }

  function confirmBan(name: string) {
    setAsk({
      title: `Ban ${name}?`,
      body: `${banBody("bans", name)} ${direction("banned-players.json")}`,
      verb: "Ban",
      danger: true,
      done: `Banned ${name}`,
      asked: `Asked the server to ban ${name}`,
      failed: `Could not ban ${name}`,
      run: () => api.addToServerList(server.id, "bans", name),
    });
  }

  function confirmKick(name: string) {
    setAsk({
      title: `Kick ${name}?`,
      body: `${name} is put off the server now and can join again straight away. It is not a ban, and nothing is written to any list.`,
      verb: "Kick",
      danger: true,
      done: `Kicked ${name}`,
      asked: `Asked the server to kick ${name}`,
      failed: `Could not kick ${name}`,
      run: () => api.kickServerPlayer(server.id, name),
    });
  }

  function confirmOp(name: string) {
    setAsk({
      title: `Make ${name} an operator?`,
      body: `Operators can run this server's commands, including banning other players. ${direction("ops.json")}`,
      verb: "Make operator",
      danger: false,
      done: `${name} is an operator`,
      asked: `Asked the server to make ${name} an operator`,
      failed: `Could not make ${name} an operator`,
      run: () => api.addToServerList(server.id, "ops", name),
    });
  }

  return (
    // The pane's own geometry comes from `settings`, the class every other tab
    // of this view wraps itself in: without it this one neither padded nor
    // scrolled, and the ban lists below the fold could not be reached at all.
    <div className={clsx(viewStyles.settings, styles.stack)}>
      {/* What direction changes take, said once, before any of them. */}
      <p className="hint">
        {lists.running
          ? "This server is running, so it owns these files: changes go through its console, and what it says back is shown as it said it."
          : "This server is stopped, so these files are Geode's to edit. This is also the only way to set an operator's level — /op has no level argument."}{" "}
        Names are shown as the server recorded them; a Minecraft name can change
        hands, so the UUID beneath each one is the identity.
      </p>

      {lists.running && !lists.console.known && (
        <p className={styles.warn}>
          <AlertTriangle size={13} />
          Geode has not read this server's list of commands, so it will not offer
          commands it cannot be sure the server has. Start it once with “Read
          commands” on in Settings, or stop it and edit the lists directly.
        </p>
      )}

      {/* A server whose commands Geode does read, but which has no `list`: say
          why there is no online list rather than leaving a gap. */}
      {lists.running && lists.console.known && !lists.console.whoIsOnline && (
        <p className="hint">
          This server reported no <code>list</code> command, so Geode cannot ask
          it who is online.
        </p>
      )}

      {/* Who is on now. Only a running server can be asked. */}
      {lists.running && lists.console.whoIsOnline && (
        <section className={styles.section}>
          <header className={styles.head}>
            <h3 className={styles.title}>
              <Users size={15} />
              Online now
              {online?.players && (
                <span className={styles.count}>{online.players.length}</span>
              )}
            </h3>
            <div className={styles.headActions}>
              {onlineAt && <span className="hint">as of {onlineAt}</span>}
              <button
                className="btn btn-sm"
                onClick={() => void refreshOnline()}
                disabled={inFlight}
              >
                {asking ? (
                  <Loader2 size={13} className="spin" />
                ) : (
                  <RefreshCw size={13} />
                )}
                Refresh
              </button>
            </div>
          </header>

          {onlineProblem ? (
            <p className={styles.warn}>
              <AlertTriangle size={13} />
              {onlineProblem}
            </p>
          ) : !online ? (
            <p className="hint">Asking the server…</p>
          ) : online.players === null ? (
            <>
              <p className={styles.warn}>
                <AlertTriangle size={13} />
                Geode could not read a player list out of the server's answer, so
                here it is as the server gave it.
              </p>
              <pre className={styles.said}>{online.said.join("\n")}</pre>
            </>
          ) : online.players.length === 0 ? (
            <p className="hint">Nobody is on the server.</p>
          ) : (
            <ul className={styles.rows}>
              {online.players.map((name) => (
                <li key={name} className={styles.row}>
                  <div className={styles.who}>
                    <span className={styles.name}>{name}</span>
                  </div>
                  <div className={styles.rowActions}>
                    {lists.console.op && (
                      <button
                        className="btn btn-sm"
                        disabled={inFlight}
                        onClick={() => confirmOp(name)}
                      >
                        <Shield size={13} />
                        Op
                      </button>
                    )}
                    {lists.console.kick && (
                      <button
                        className="btn btn-sm"
                        disabled={inFlight}
                        onClick={() => confirmKick(name)}
                      >
                        <LogOut size={13} />
                        Kick
                      </button>
                    )}
                    {lists.console.ban && (
                      <button
                        className="btn btn-sm btn-danger"
                        disabled={inFlight}
                        onClick={() => confirmBan(name)}
                      >
                        <Ban size={13} />
                        Ban
                      </button>
                    )}
                  </div>
                </li>
              ))}
            </ul>
          )}
        </section>
      )}

      {LISTS.map((config) => (
        <ListPane
          key={config.id}
          config={config}
          lists={lists}
          busy={inFlight}
          onRemove={(entry) => confirmRemove(config, entry)}
          onAdd={(target, reason, level) =>
            addTo(config, target, reason, level)
          }
          onToggleWhitelist={(on) =>
            apply(
              {
                done: on ? "Whitelist on" : "Whitelist off",
                asked: on
                  ? "Asked the server to turn the whitelist on"
                  : "Asked the server to turn the whitelist off",
                failed: on
                  ? "Could not turn the whitelist on"
                  : "Could not turn the whitelist off",
              },
              () => api.setServerWhitelist(server.id, on),
            )
          }
        />
      ))}

      {ask && (
        <Modal
          title={ask.title}
          onClose={() => setAsk(null)}
          width={460}
          footer={
            <>
              <button className="btn" onClick={() => setAsk(null)}>
                Cancel
              </button>
              <button
                className={clsx("btn", ask.danger ? "btn-danger" : "btn-primary")}
                disabled={inFlight}
                onClick={() =>
                  void apply(
                    { done: ask.done, asked: ask.asked, failed: ask.failed },
                    ask.run,
                  )
                }
              >
                {busy && <Loader2 size={14} className="spin" />}
                {ask.verb}
              </button>
            </>
          }
        >
          <p className={styles.askBody}>{ask.body}</p>
        </Modal>
      )}
    </div>
  );
}

/** One list, its toggle if it has one, its entries and a field to add to it. */
function ListPane({
  config,
  lists,
  busy,
  onAdd,
  onRemove,
  onToggleWhitelist,
}: {
  config: ListConfig;
  lists: PlayerLists;
  busy: boolean;
  onAdd: (target: string, reason?: string, level?: number) => void;
  onRemove: (entry: PlayerListEntry) => void;
  onToggleWhitelist: (on: boolean) => void;
}) {
  const [target, setTarget] = useState("");
  const [reason, setReason] = useState("");
  const [level, setLevel] = useState(4);

  const entries = entriesOf(lists, config.id);
  const Icon = config.icon;
  // While the server runs the console is the only way in, so a command it never
  // reported having is not offered. Stopped, the file is ours and no command is
  // involved at all.
  const addable = lists.running ? canAdd(lists.console, config.id) : true;
  const removable = lists.running ? canRemove(lists.console, config.id) : true;
  // Only while stopped: /op takes no level, so a running server sets it from
  // its own op-permission-level.
  const levelled = config.level && !lists.running;

  function submit() {
    if (!target.trim()) return;
    onAdd(
      target.trim(),
      config.reason && reason.trim() ? reason.trim() : undefined,
      levelled ? level : undefined,
    );
    setTarget("");
    setReason("");
  }

  return (
    <section className={styles.section}>
      <header className={styles.head}>
        <h3 className={styles.title}>
          <Icon size={15} />
          {config.title}
          <span className={styles.count}>{entries.length}</span>
        </h3>

        {/* The toggle that decides whether the whitelist is consulted at all,
            next to the list it governs. */}
        {config.id === "whitelist" && (
          <label className={styles.switch}>
            <input
              type="checkbox"
              checked={lists.whitelistOn}
              disabled={
                busy || (lists.running && !lists.console.whitelistToggle)
              }
              onChange={(e) => onToggleWhitelist(e.target.checked)}
            />
            <span>{lists.whitelistOn ? "On" : "Off"}</span>
          </label>
        )}
      </header>

      <p className="hint">
        {config.blurb}
        {config.id === "whitelist" &&
          (lists.running
            ? lists.console.whitelistToggle
              ? " Turning it on or off is sent to the console now, and written to server.properties so it survives a restart."
              : " Geode has not seen a whitelist command on this server, so the toggle waits until it is stopped."
            : " The toggle writes white-list in server.properties, which the server reads when it next starts.")}
      </p>

      {entries.length === 0 ? (
        <p className="hint">{config.empty}</p>
      ) : (
        <ul className={styles.rows}>
          {entries.map((entry) => (
            <li key={entry.uuid || entry.ip || entry.name} className={styles.row}>
              <div className={styles.who}>
                <span className={styles.name}>
                  {labelOf(entry)}
                  {entry.level !== null && (
                    <span className={styles.level}>level {entry.level}</span>
                  )}
                </span>
                {/* The identity, as against the name, which may be stale —
                    unless it is already standing in as the name above. */}
                {entry.uuid && entry.uuid !== labelOf(entry) && (
                  <span className={styles.uuid}>{entry.uuid}</span>
                )}
                {(entry.reason || entry.created) && (
                  <span className={styles.meta}>
                    {[entry.reason, entry.created].filter(Boolean).join(" · ")}
                  </span>
                )}
              </div>
              <div className={styles.rowActions}>
                <button
                  className="btn btn-sm btn-icon"
                  title={
                    removable
                      ? config.remove
                      : "This server reported no command for it"
                  }
                  aria-label={`${config.remove} — ${labelOf(entry)}`}
                  disabled={busy || !removable}
                  onClick={() => onRemove(entry)}
                >
                  <Trash2 size={13} />
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}

      <div className={styles.add}>
        <input
          className={clsx("input", styles.grow)}
          placeholder={config.placeholder}
          value={target}
          disabled={busy || !addable}
          onChange={(e) => setTarget(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && submit()}
        />
        {config.reason && (
          <input
            className={clsx("input", styles.grow)}
            placeholder="Reason (optional)"
            value={reason}
            disabled={busy || !addable}
            onChange={(e) => setReason(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
          />
        )}
        {levelled && (
          <select
            className="select"
            value={level}
            disabled={busy}
            onChange={(e) => setLevel(Number(e.target.value))}
            aria-label="Operator level"
          >
            {[1, 2, 3, 4].map((l) => (
              <option key={l} value={l}>
                Level {l}
                {l === 4 ? " — everything" : ""}
              </option>
            ))}
          </select>
        )}
        <button
          className="btn"
          disabled={busy || !addable || !target.trim()}
          onClick={submit}
        >
          <Plus size={13} />
          Add
        </button>
      </div>

      {!addable ? (
        <p className="hint">
          Geode has not seen the command this needs on this server, so it is not
          offered. Stop the server to edit {config.file} directly.
        </p>
      ) : (
        config.takes === "name" &&
        !lists.running && (
          <p className="hint">
            {lists.onlineMode
              ? `${config.file} is keyed by UUID, so the name is checked with Mojang before anything is written. A name Mojang does not know is reported rather than guessed at.`
              : "This server has authentication off, so the UUID is the one Minecraft itself derives from the name — no lookup needed."}
          </p>
        )
      )}

      {config.level && lists.running && (
        <p className="hint">
          An operator's level comes from this server's own op-permission-level
          while it is running. Stop it to set a level per operator.
        </p>
      )}
    </section>
  );
}

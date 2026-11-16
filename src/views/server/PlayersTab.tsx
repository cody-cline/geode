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
import { api, errorMessage } from "../../lib/api";
import { PROPERTY_INFO } from "../../lib/serverProperties";
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
 *
 * Every placeholder ends in "or several", which is where the field says it
 * takes a pasted list. Here rather than as a note under each card: four cards
 * would have carried four copies of one sentence, which is the thing the note
 * at the foot of `ListPane` was written to stop.
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
    placeholder: "Minecraft name, or several",
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
    placeholder: "Minecraft name, or several",
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
    placeholder: "Minecraft name, or several",
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
    placeholder: "203.0.113.9, or several",
    reason: true,
    level: false,
  },
];

type ListConfig = (typeof LISTS)[number];

/**
 * What each operator level grants, in the Properties tab's own words.
 *
 * Only level 4 was glossed here, so choosing 2 or 3 meant already knowing
 * Minecraft's permission ladder — the one value on this tab that had to come
 * from memory. The words are read off `op-permission-level` rather than
 * written again, because that setting is the very thing the hint at the foot
 * of this card sends the reader to, one tab away: a server describing one
 * ladder two ways is worse than a ladder left unglossed. Each label carries
 * its own number already — "1 — bypass spawn protection".
 */
const opLevelControl = PROPERTY_INFO["op-permission-level"].control;
const OP_LEVELS = opLevelControl.kind === "enum" ? opLevelControl.options : [];

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

/**
 * What one name out of a pasted list got back.
 *
 * The same split as `Labels`, for the same reason. Through the console the
 * server's own words are the whole of the result; a file Geode wrote is the
 * one direction whose outcome Geode can state itself; and anything else —
 * including a server that answered nothing at all, which the core refuses to
 * call a result — arrives here as the reason it did not happen. So a name is
 * never counted into a total that claims more than its own answer did.
 */
type Outcome = {
  name: string;
  /** What the server said, when the console was the way in. */
  said: string[] | null;
  /** Why it did not happen, when it did not. */
  failed: string | null;
};

/** A confirmation that names who it affects and says what it does. */
type Ask = {
  title: string;
  body: string;
  verb: string;
  danger: boolean;
  /**
   * What pressing the verb does, reporting what came of it as it goes.
   *
   * A function rather than the change itself, because one press is not always
   * one change: a pasted list is one confirmation over several, and the
   * several cannot be reported as one.
   */
  go: () => Promise<void>;
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
 * `many` moves the verb and nothing else: a pasted list of names is that same
 * act said of several people, and saying it a second way for them is the very
 * thing this function exists to stop.
 */
function banBody(list: PlayerListId, who: string, many = false): string {
  return list === "ip-bans"
    ? `Everyone connecting from ${who} is kept out until the ban is lifted.`
    : `${who} ${many ? "are" : "is"} put off the server and kept out until the ban is lifted. That is not a kick — a kick lets them come straight back.`;
}

/**
 * The names in what was typed or pasted into an add field.
 *
 * Whitespace and commas both. A list of players arrives as a column out of a
 * spreadsheet or a comma-separated line out of a message, and a single-line
 * field does not keep the newlines of the first — the browser hands them over
 * as spaces — so the spaces have to count as separators too. Nothing is lost
 * by that: a Minecraft name is three to sixteen letters, numbers or
 * underscores, and an IP address has no spaces either, so a space in this
 * field was never inside one target. The same name twice is one name, because
 * a pasted list repeats and two answers for one person is not a report.
 */
function namesIn(text: string): string[] {
  return [...new Set(text.split(/[\s,]+/).filter(Boolean))];
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

  /**
   * What each name of a pasted list got back, once they all have.
   *
   * A report rather than a toast because a toast is one headline over one
   * body and takes itself away after a few seconds, and ten names can have
   * ten different answers — none of which may be summed into an eleventh.
   */
  const [report, setReport] = useState<{
    config: ListConfig;
    done: Outcome[];
  } | null>(null);

  /**
   * Why the lists could not be read, when they could not.
   *
   * The same shape as `onlineProblem` below, for the same reason: the tab had
   * a real error state for who is online and none for the four lists the tab
   * is made of. A failed read left `lists` null, which is the skeleton — so
   * the surface went on claiming to be fetching, with the toast gone in nine
   * seconds and nothing on screen to say otherwise.
   */
  const [listsProblem, setListsProblem] = useState<string | null>(null);

  const load = useCallback(
    () =>
      api
        .serverPlayerLists(server.id)
        .then((found) => {
          setLists(found);
          setListsProblem(null);
        })
        .catch((e) => {
          reportError("Could not read this server's player lists", e);
          setListsProblem(errorMessage(e));
        }),
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

  /**
   * Run one add per name and keep what each one answered.
   *
   * One after another because the console is: whoever waits on a reply takes
   * every line it produces while they wait, so two of these overlapping would
   * collect each other's answers. A name that fails does not stop the ones
   * after it — a name Mojang cannot place is that name's problem and no
   * other's.
   *
   * Nothing is added up. Ten names can get ten different answers, and a
   * single "done" over them could only be arrived at by treating some answer
   * as a success nobody gave — which is the one thing this screen must never
   * do. So every name keeps its own answer and the report shows them.
   */
  async function applyMany(
    config: ListConfig,
    names: string[],
    reason?: string,
    level?: number,
  ) {
    setBusy(true);
    const done: Outcome[] = [];
    try {
      for (const name of names) {
        try {
          const change = await api.addToServerList(
            server.id,
            config.id,
            name,
            reason,
            level,
          );
          setLists(change.lists);
          done.push({
            name,
            said: change.via === "console" ? change.said : null,
            failed: null,
          });
        } catch (e) {
          done.push({ name, said: null, failed: errorMessage(e) });
        }
      }
    } finally {
      setBusy(false);
      setAsk(null);
      setReport({ config, done });
      if (liveList && done.some((d) => d.said)) void refreshOnline();
    }
  }

  if (!lists) {
    return listsProblem ? (
      <div className={styles.loadFailed}>
        <p className={styles.warn}>
          <AlertTriangle size={13} />
          Could not read this server&apos;s player lists, so none of them are
          shown. {listsProblem}
        </p>
        <button className="btn btn-sm" onClick={() => void load()}>
          Try again
        </button>
      </div>
    ) : (
      <div className="skeleton" style={{ height: 160 }} />
    );
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
    const labels = {
      done: lifting
        ? `Lifted the ban on ${who}`
        : `Removed ${who} from ${config.title.toLowerCase()}`,
      asked: lifting
        ? `Asked the server to lift the ban on ${who}`
        : `Asked the server to take ${who} off ${config.title.toLowerCase()}`,
      failed: lifting
        ? `Could not lift the ban on ${who}`
        : `Could not take ${who} off ${config.title.toLowerCase()}`,
    };
    setAsk({
      ...copy[config.id],
      body: `${copy[config.id].body} ${direction(config.file)}`,
      verb: lifting ? "Lift the ban" : "Remove",
      danger: config.id === "whitelist" || config.id === "ops",
      // Named to the core by the key its file is keyed by, not by the name it
      // shows: two accounts can be recorded under one name, and only the UUID
      // picks out the row the admin clicked.
      go: () =>
        apply(labels, () =>
          api.removeFromServerList(
            server.id,
            config.id,
            entry.ip ?? entry.name,
            entry.uuid || undefined,
          ),
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
    const go = () =>
      apply(labels, () =>
        api.addToServerList(server.id, config.id, target, reason, level),
      );
    if (config.id !== "bans" && config.id !== "ip-bans") {
      void go();
      return;
    }
    setAsk({
      title: `Ban ${target}?`,
      body: `${banBody(config.id, target)} ${direction(config.file)}`,
      verb: "Ban",
      danger: true,
      go,
    });
  }

  /**
   * Put a pasted list of names on a list, one at a time.
   *
   * Confirmed whichever list it is, where a single name on the whitelist is
   * not. The dialog is where the split is shown back, and that is worth a
   * press: a paste that came apart somewhere unexpected is visible there for
   * the price of reading it, rather than as three refusals afterwards.
   */
  function addMany(
    config: ListConfig,
    names: string[],
    reason?: string,
    level?: number,
  ) {
    const banning = config.id === "bans" || config.id === "ip-bans";
    setAsk({
      title: banning
        ? `Ban ${names.length} ${config.id === "ip-bans" ? "addresses" : "players"}?`
        : `Add ${names.length} names to ${config.title.toLowerCase()}?`,
      body: `${
        banning
          ? banBody(config.id, names.join(", "), names.length > 1)
          : `${names.join(", ")}.`
      } ${direction(config.file)} Each name goes on its own, so each one comes back with its own answer.`,
      verb: banning ? "Ban" : "Add",
      danger: banning,
      go: () => applyMany(config, names, reason, level),
    });
  }

  function confirmBan(name: string) {
    setAsk({
      title: `Ban ${name}?`,
      body: `${banBody("bans", name)} ${direction("banned-players.json")}`,
      verb: "Ban",
      danger: true,
      go: () =>
        apply(
          {
            done: `Banned ${name}`,
            asked: `Asked the server to ban ${name}`,
            failed: `Could not ban ${name}`,
          },
          () => api.addToServerList(server.id, "bans", name),
        ),
    });
  }

  function confirmKick(name: string) {
    setAsk({
      title: `Kick ${name}?`,
      body: `${name} is put off the server now and can join again straight away. It is not a ban, and nothing is written to any list.`,
      verb: "Kick",
      danger: true,
      go: () =>
        apply(
          {
            done: `Kicked ${name}`,
            asked: `Asked the server to kick ${name}`,
            failed: `Could not kick ${name}`,
          },
          () => api.kickServerPlayer(server.id, name),
        ),
    });
  }

  function confirmOp(name: string) {
    setAsk({
      title: `Make ${name} an operator?`,
      body: `Operators can run this server's commands, including banning other players. ${direction("ops.json")}`,
      verb: "Make operator",
      danger: false,
      go: () =>
        apply(
          {
            done: `${name} is an operator`,
            asked: `Asked the server to make ${name} an operator`,
            failed: `Could not make ${name} an operator`,
          },
          () => api.addToServerList(server.id, "ops", name),
        ),
    });
  }

  // A pasted list counted in the registers its directions allow, and never
  // summed across them: a file Geode wrote is a result Geode knows, a console
  // reply is only what the server said, and a name that got neither did not
  // happen at all.
  const wrote = report?.done.filter((d) => !d.failed && !d.said).length ?? 0;
  const answered = report?.done.filter((d) => d.said).length ?? 0;
  const missed = report?.done.filter((d) => d.failed).length ?? 0;

  return (
    // The pane's own geometry comes from `settings`, the class every other tab
    // of this view wraps itself in: without it this one neither padded nor
    // scrolled, and the ban lists below the fold could not be reached at all.
    <div className={clsx(viewStyles.settings, styles.stack)}>
      {/* What direction changes take and how a name becomes an identity,
          said once, before any of them. The second of those used to be a note
          under each card, which put the same sentence on the screen three
          times with only the file name differing — and the rule is not the
          file's, it is this screen's. The three lists it holds for are named
          rather than described as "where a list is keyed by UUID": that was
          written to stay true of banned-ips.json, which is keyed by address
          and is one of the four cards below, but naming the other three is
          exactly as true and is a sentence an admin already has the words
          for. */}
      <p className="hint">
        {lists.running
          ? "This server is running, so it owns these files: changes go through its console, and what it says back is shown as it said it."
          : "This server is stopped, so these files are Geode's to edit. This is also the only way to set an operator's level — /op has no level argument."}{" "}
        Names are shown as the server recorded them; a Minecraft name can change
        hands, so the UUID beneath each one is the identity.{" "}
        {!lists.running &&
          (lists.onlineMode
            ? "The whitelist, operators and banned players go by UUID, so a name is checked with Mojang before anything is written, and one Mojang does not know is reported rather than guessed at."
            : "This server has authentication off, so a UUID is the one Minecraft itself derives from the name — no lookup needed.")}
      </p>

      {lists.running && !lists.console.known && (
        <p className={styles.warn}>
          <AlertTriangle size={13} />
          Geode has not read this server's list of commands, so it will not offer
          commands it cannot be sure the server has. Start it once with “Read
          this server’s commands” on in Settings, or stop it and edit the lists
          directly.
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
          onAddMany={(names, reason, level) =>
            addMany(config, names, reason, level)
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
                onClick={() => void ask.go()}
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

      {/* A row per name, each carrying the answer that name got, under a lead
          that counts them without ever adding one register to the other. */}
      {report && (
        <Modal
          title={`${report.done.length} names, one at a time`}
          onClose={() => setReport(null)}
          width={460}
          footer={
            <button className="btn btn-primary" onClick={() => setReport(null)}>
              Done
            </button>
          }
        >
          <p className={styles.askBody}>
            {answered > 0 &&
              `${answered} went to the console, and what the server said to each is beside it. `}
            {wrote > 0 && `Geode wrote ${wrote} to ${report.config.file}. `}
            {missed > 0 &&
              `${missed} of the ${report.done.length} did not happen, and why is beside the name.`}
          </p>
          <ul className={styles.rows}>
            {report.done.map((d) => (
              <li key={d.name} className={styles.row}>
                <div className={styles.who}>
                  <span className={styles.name}>{d.name}</span>
                  <span className={d.failed ? styles.failed : styles.meta}>
                    {d.failed ?? (d.said ? d.said.join(" ") : "Written")}
                  </span>
                </div>
              </li>
            ))}
          </ul>
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
  onAddMany,
  onRemove,
  onToggleWhitelist,
}: {
  config: ListConfig;
  lists: PlayerLists;
  busy: boolean;
  onAdd: (target: string, reason?: string, level?: number) => void;
  onAddMany: (names: string[], reason?: string, level?: number) => void;
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
  // What is in the field, read as the list it may be. One name is the path it
  // always was — the same call, the same toast — so nothing about adding one
  // person changes for somebody who has never pasted anything here.
  const typed = namesIn(target);

  function submit() {
    if (typed.length === 0) return;
    const why = config.reason && reason.trim() ? reason.trim() : undefined;
    const lvl = levelled ? level : undefined;
    if (typed.length === 1) onAdd(typed[0], why, lvl);
    else onAddMany(typed, why, lvl);
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
                  {/* The same gloss as the picker's, because while the server
                      is running there is no picker and this badge is the only
                      place a level is named. */}
                  {entry.level !== null && (
                    <span
                      className={styles.level}
                      title={
                        OP_LEVELS.find(
                          (o) => Number(o.value) === entry.level,
                        )?.label
                      }
                    >
                      level {entry.level}
                    </span>
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
                {/* Ghost, like every other row-removal button in the app —
                    this one was the lone solid one, which read as the row's
                    main action rather than the quiet way out of it. */}
                <button
                  className="btn btn-ghost btn-icon btn-sm"
                  title={
                    removable
                      ? config.remove
                      : "This server reported no command for it"
                  }
                  aria-label={`${config.remove} — ${labelOf(entry)}`}
                  disabled={busy || !removable}
                  onClick={() => onRemove(entry)}
                >
                  <Trash2 size={14} />
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
            {OP_LEVELS.map((o) => (
              <option key={o.value} value={o.value}>
                Level {o.label}
              </option>
            ))}
          </select>
        )}
        {/* The count is how the field says it read a paste as several names
            before any of them is sent, which is the only moment a split that
            went wrong can still be taken back for nothing. */}
        <button
          className="btn"
          disabled={busy || !addable || typed.length === 0}
          onClick={submit}
        >
          <Plus size={13} />
          {typed.length > 1 ? `Add ${typed.length}` : "Add"}
        </button>
      </div>

      {/* The only note left here is the one that is actually about this
          card: what this server cannot be asked to do, and which file to edit
          instead. How a name is resolved is the same for every list and is
          said once at the top.

          And only when the tree was actually read. With no tree at all every
          command fails this test at once, so all four cards printed this
          sentence together, under a warning at the top of the screen that had
          already said the same thing — five statements of one fact, differing
          only in which file each named. Where the tree *was* read and this one
          command is missing from it, the file name is the new information and
          the sentence is worth its space. */}
      {!addable && lists.console.known && (
        <p className="hint">
          This server did not report the command this needs, so it is not
          offered. Stop it to edit {config.file} directly.
        </p>
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

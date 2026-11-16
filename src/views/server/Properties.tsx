import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Eye, EyeOff, Loader2, Search, Zap } from "lucide-react";

import { api, errorMessage } from "../../lib/api";
import { actionHint } from "../../lib/shortcuts";
import { useActionShortcut } from "../../lib/useActionShortcut";
import { useStore } from "../../lib/store";
import {
  CREATION_ONLY,
  GROUP_ORDER,
  NOW_A_GAMERULE,
  PROPERTY_INFO,
  RETIRED_KEYS,
  type Control,
} from "../../lib/serverProperties";
import type { Server, ServerProperty } from "../../lib/types";

import styles from "./Properties.module.css";

/** Where keys the table does not recognise go. */
const OTHER = "Other";

/** The one key whose value another server can be holding. */
const PORT_KEY = "server-port";

/**
 * What two servers on one port means, naming the others.
 *
 * A warning rather than a refusal: servers that are never run at the same
 * time share a port perfectly well, and Geode cannot know which those are.
 * What it can do is say so now instead of letting the second one find out by
 * failing to start.
 *
 * `port` is how to refer to the port, because there are two places to say
 * this: beside the setting, where the number is on screen, and on a server
 * that has never started, where there is no file and so no number yet.
 */
function portClashLine(names: string[], port: string): string {
  return `${listOf(names)} ${names.length === 1 ? "is" : "are"} also set to ${port}. Only one of them can run at a time — whichever starts second will fail to bind it.`;
}

/** "a", "a and b", "a, b and c" — how this screen names a handful of things. */
function listOf(names: string[]): string {
  if (names.length === 1) return names[0];
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/** How to name the port of a server that has not written its file yet. */
const UNWRITTEN_PORT = "the port this server will take the first time it starts";

/** What a number in this file can be: digits, and nothing else. */
const WHOLE_NUMBER = /^-?\d+$/;

/**
 * Where a port stops, and the one upper bound in the catalogue that is a fact
 * rather than a guess.
 *
 * It is also how the four port keys are told apart from everything else here:
 * `server-port`, `query.port`, `rcon.port` and `management-server-port` are
 * the only settings whose maximum is this, because they are the only ones
 * measuring a TCP port. A fork may well raise `max-players` past 200; none
 * will ever bind 99999. Geode already says so where it asks for a port of its
 * own — the share port in Settings wants "a whole number from 1024 to 65535"
 * and will not save anything else.
 */
const MAX_PORT = 65535;

/**
 * What is wrong with a number somebody has typed, and whether it is wrong
 * enough to hold the save back.
 *
 * Two different things, which is why both come back together. A box holding
 * no whole number — emptied, half-typed, or `1e5`, all of which a
 * `type="number"` input hands over quite happily — would go into the file
 * exactly as it stands, and a line the server reads as a number is not the
 * place to find that out, so the save waits for one.
 *
 * A whole number outside the catalogue's range is a different claim. Those
 * bounds are where a setting usually sits, not what a server refuses:
 * `max-players` is listed as 0 to 200 and `view-distance` as 2 to 32, and a
 * fork, a plugin or a version nobody here has heard of may take more. So that
 * one is said and then saved, for the same reason the port two servers share
 * is warned about rather than refused — Geode cannot know, and a refusal
 * would be it pretending otherwise.
 *
 * Over `MAX_PORT` is the exception, and the only one: Geode does know, so it
 * refuses. The bound below a port stays a warning even so, because
 * `server-port=0` is a real setting meaning "any free port" that the
 * catalogue's `min: 1` would otherwise turn away.
 */
function numberProblem(
  value: string,
  { min, max }: { min?: number; max?: number },
): { line: string; blocks: boolean } | null {
  const range =
    min !== undefined && max !== undefined
      ? `${min} to ${max}`
      : min !== undefined
        ? `${min} or more`
        : max !== undefined
          ? `${max} or less`
          : null;

  const typed = value.trim();
  if (!WHOLE_NUMBER.test(typed)) {
    return {
      line:
        "Type a whole number — the box would go into the file exactly as it stands." +
        (range ? ` ${range} is the usual range.` : ""),
      blocks: true,
    };
  }

  const n = Number(typed);
  if (max === MAX_PORT && n > max) {
    return {
      line: `Type a port of ${MAX_PORT} or less. Nothing can listen on a higher number, so the server would not start.`,
      blocks: true,
    };
  }
  if (
    range !== null &&
    ((min !== undefined && n < min) || (max !== undefined && n > max))
  ) {
    return {
      line: `This is outside the usual ${range}. Geode saves what you typed rather than refusing it — a fork, a plugin or a newer version may take a value this screen has not heard of.`,
      blocks: false,
    };
  }
  return null;
}

/**
 * Which kind of box a key gets.
 *
 * A key nobody has described still gets the right kind, guessed from the
 * value the server wrote. A mod's own setting is usable without anyone here
 * having heard of it.
 *
 * `written` is the file's own value and not the one in the box, so the guess
 * cannot change as somebody types. It used to: clearing a guessed number box
 * made it a text box mid-edit, and an emptied box that is no longer a number
 * box is one nothing can mark as empty.
 */
function controlFor(key: string, written: string): Control {
  const known = PROPERTY_INFO[key]?.control;
  if (known) return known;
  if (written === "true" || written === "false") return { kind: "bool" };
  return WHOLE_NUMBER.test(written.trim()) ? { kind: "int" } : { kind: "text" };
}

/**
 * A readable title for a key nobody has written a description for.
 *
 * `max-chained-neighbor-updates` becomes "Max chained neighbor updates". Not
 * clever, but better than a raw key, and it means a setting added by a future
 * Minecraft version or a mod still reads like part of the page.
 */
function humanize(key: string): string {
  const words = key.replace(/[.\-_]+/g, " ").trim();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/**
 * What a group's tagged rows have in common, said once above them.
 *
 * Takes both sentences rather than patching one, because the groups split the
 * tagged keys unevenly — the World group holds six "world creation" rows and
 * five "now a game rule" ones, while Security holds exactly one — and
 * "Settings … are" standing over a single field is the same kind of wrong as
 * "1 items". Nothing is printed for a group with none.
 */
function groupNote(n: number, one: string, many: string) {
  if (n === 0) return null;
  return <p className={clsx("hint", styles.groupNote)}>{n === 1 ? one : many}</p>;
}

/**
 * The server's own `server.properties`, laid out like the settings above it.
 *
 * The file stays the source of truth for which keys exist — every key in it
 * gets a row, whether or not this app has heard of it. The catalog only
 * decorates: a better label, a sentence, and the right control. That is what
 * lets a new Minecraft version, a fork or a mod's own key appear and save
 * without anyone updating a list.
 */
export default function Properties({ server }: { server: Server }) {
  const reportError = useStore((s) => s.reportError);
  const toast = useStore((s) => s.toast);

  const [props, setProps] = useState<ServerProperty[] | null>(null);
  const [edited, setEdited] = useState<Record<string, string>>({});
  const [filter, setFilter] = useState("");
  const filterBox = useRef<HTMLInputElement>(null);

  /**
   * Ctrl+F reaches this filter too. `server.properties` is the longest list
   * in the app — seventy-odd rows — so it is the one most worth not scrolling,
   * and it was the one the key did nothing on.
   */
  useActionShortcut("focusFilter", (e) => {
    const box = filterBox.current;
    if (!box) return;
    e.preventDefault();
    box.focus();
    box.select();
  });
  const [saving, setSaving] = useState(false);
  const [portClash, setPortClash] = useState<string[]>([]);
  /**
   * Why the port could not be checked against the other servers, when it
   * could not.
   *
   * An empty list is the answer "nobody else is on this port", which is the
   * one thing a check that did not run cannot say — and it says it by showing
   * a clean screen, the same screen a server with no clash gets. The read
   * below this one has a comment about exactly this trap; this effect used to
   * fall into it on the next line.
   */
  const [clashFailed, setClashFailed] = useState<string | null>(null);
  /**
   * Bumped by the Try again the warning carries. The check lives in an effect
   * because it belongs to the port currently on screen, and that effect owns
   * the guard keeping a late answer about an older port out of the warning.
   */
  const [askClash, setAskClash] = useState(0);
  /**
   * Why the file could not be read, when it could not.
   *
   * Not the same as the file not being there. `properties.rs` answers a
   * missing file with an empty list, which is the case the copy below is
   * written for, so this catch only ever fires on a real failure — a
   * permission, a bad mount, the call itself. Every time it did, the screen
   * said the server had never started.
   */
  const [failed, setFailed] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .readServerProperties(server.id)
      .then((found) => {
        setProps(found);
        setFailed(null);
      })
      .catch((e) => {
        // It also used to be swallowed whole: no toast, no mark on the screen,
        // nothing anywhere to say a read had failed at all.
        reportError("Could not read this server's settings", e);
        setProps([]);
        setFailed(errorMessage(e));
      });
  }, [server.id, reportError]);

  useEffect(() => {
    load();
  }, [load]);

  const changedCount = Object.keys(edited).length;
  const valueOf = (p: ServerProperty) => edited[p.key] ?? p.value;

  // The port as it stands, edited or not, and `undefined` when this file has
  // no such line — a server that has never started has no file at all.
  const port = edited[PORT_KEY] ?? props?.find((p) => p.key === PORT_KEY)?.value;

  // Asked whenever the port is on screen rather than only after a change: a
  // second server made today took the default port exactly as the first one
  // did, and nobody set it. With no port to pass, the backend answers about
  // whatever port this server has — including the one it has not written yet.
  useEffect(() => {
    if (props === null) return;
    const typed = port === undefined ? undefined : Number(port);
    if (
      typed !== undefined &&
      (!Number.isInteger(typed) || typed < 0 || typed > 65535)
    ) {
      // Not a failure: what is in the box is not a port, so there is nothing
      // to ask about. The warning stands down rather than reporting a check
      // that was never going to run.
      setPortClash([]);
      setClashFailed(null);
      return;
    }
    let cancelled = false;
    api
      .gamePortClashes(server.id, typed)
      .then((names) => {
        if (cancelled) return;
        setPortClash(names);
        setClashFailed(null);
      })
      .catch((e) => {
        if (cancelled) return;
        // No toast: this runs again on every keystroke in the port box, so a
        // backend that is refusing would stack one per character. The note
        // beside the setting is where the answer belongs anyway, and it is
        // the place somebody is looking when they read a port.
        setPortClash([]);
        setClashFailed(errorMessage(e));
      });
    return () => {
      cancelled = true;
    };
  }, [server.id, props, port, askClash]);
  /**
   * Record an edit, and forget it again when it comes back to where it started.
   *
   * Every keystroke and every toggle used to go into `edited` and stay there,
   * and nothing compared the new value with the old one — so typing a
   * character and deleting it, or ticking a box and unticking it, left the
   * save bar up saying "1 setting changed. Restart the server to apply." A
   * count of keys touched is not a count of keys changed, and this one is
   * attached to an instruction to restart a server.
   */
  const edit = (key: string, value: string) =>
    setEdited((was) => {
      const next = { ...was };
      if (value === props?.find((p) => p.key === key)?.value) delete next[key];
      else next[key] = value;
      return next;
    });

  const shown = useMemo(() => {
    const query = filter.trim().toLowerCase();
    if (!query) return props ?? [];
    // The raw key is matched even though it is no longer on screen, so
    // searching for what the wiki calls it still works.
    return (props ?? []).filter((p) => {
      const info = PROPERTY_INFO[p.key];
      return (
        p.key.toLowerCase().includes(query) ||
        humanize(p.key).toLowerCase().includes(query) ||
        (info?.label.toLowerCase().includes(query) ?? false) ||
        (info?.description.toLowerCase().includes(query) ?? false)
      );
    });
  }, [props, filter]);

  /**
   * What is wrong with the numbers in the edited boxes, keyed by setting.
   *
   * Only the edited keys, because only those are written. A number already in
   * the file is the server's own — it wrote the file — and this app's idea of
   * the usual range is no reason to put a mark on it.
   *
   * Through `controlFor`, so a mod's own numeric key — one that got a number
   * box by guess rather than out of the catalogue — is held to the same rule
   * as the keys this screen has heard of. Two boxes that look identical and
   * behave differently is the worse half of the bug this is here to stop.
   */
  const problems = useMemo(() => {
    const found = new Map<string, { line: string; blocks: boolean }>();
    for (const p of props ?? []) {
      const value = edited[p.key];
      if (value === undefined) continue;
      const control = controlFor(p.key, p.value);
      if (control.kind !== "int") continue;
      const problem = numberProblem(value, control);
      if (problem) found.set(p.key, problem);
    }
    return found;
  }, [edited, props]);

  /** The settings that cannot be written as they stand, named for the save bar. */
  const unwritable = [...problems]
    .filter(([, p]) => p.blocks)
    .map(([key]) => PROPERTY_INFO[key]?.label ?? humanize(key));

  const groups = useMemo(() => {
    const buckets = new Map<string, ServerProperty[]>();
    for (const p of shown) {
      const group = PROPERTY_INFO[p.key]?.group ?? OTHER;
      const list = buckets.get(group);
      if (list) list.push(p);
      else buckets.set(group, [p]);
    }
    // Known groups in a fixed order, then whatever is left over. A group with
    // nothing in it never appears, so a server missing half these keys does
    // not show half-empty headings.
    return [...GROUP_ORDER, OTHER]
      .filter((g) => buckets.has(g))
      .map((g) => [g, buckets.get(g)!] as const);
  }, [shown]);

  /**
   * Only the edited keys are sent. The backend leaves the rest of the file
   * exactly as it found it — comments, order and all — which is what makes a
   * hand-tuned file survivable.
   */
  async function save() {
    setSaving(true);
    try {
      await api.writeServerProperties(
        server.id,
        Object.entries(edited).map(([key, value]) => ({ key, value })),
      );
      setProps(await api.readServerProperties(server.id));
      setEdited({});
    } catch (e) {
      reportError("Could not save server.properties", e);
    } finally {
      setSaving(false);
    }
  }

  async function applyNow(key: string, command: string) {
    try {
      await api.sendServerCommand(server.id, command);
      toast({
        kind: "success",
        title: `Applied ${key} now`,
        body: `Sent “${command}” to the console. Save to keep it after a restart.`,
      });
    } catch (e) {
      reportError("Could not send that command", e);
    }
  }

  if (props === null) {
    return <div className="skeleton" style={{ height: 120 }} />;
  }

  if (failed) {
    // Before the empty state, and not sharing its port-clash warning: that
    // branch asks about UNWRITTEN_PORT, so a server whose port was not read
    // could be warned about a clash on a port it does not have.
    return (
      <div className={styles.readFailed}>
        <p className="hint">
          Could not read this server&apos;s settings, so what is in the file is
          unknown — this is not a server that has never started. {failed}
        </p>
        <button className="btn btn-sm" onClick={load}>
          Try again
        </button>
      </div>
    );
  }

  /**
   * The check did not run, said in both places it could matter: the table of
   * settings, and the server that has not written its file yet. One element
   * rather than two copies of the sentence, because it is the same fact
   * either way — and silence here is a clean screen, which is what a server
   * with no clash gets.
   */
  const clashUnknown = clashFailed ? (
    <>
      <p className={styles.danger}>
        <AlertTriangle size={12} />
        Whether another server is already set to this port could not be
        checked, so nothing here says there is no clash. No setting has
        changed.
      </p>
      <div className={styles.readFailed}>
        <p className="hint">{clashFailed}</p>
        <button className="btn btn-sm" onClick={() => setAskClash((n) => n + 1)}>
          Try again
        </button>
      </div>
    </>
  ) : null;

  if (props.length === 0) {
    return (
      <>
        <p className="hint">
          This file appears the first time the server starts — it is written by
          the server, not by Geode. Start it once and its settings show up here.
        </p>
        {/* Nobody set this port: a new server takes the default, which is
            exactly what the server beside it took. Worth saying before either
            has ever run, since there is no row here to hang it off. */}
        {portClash.length > 0 && (
          <p className={styles.danger}>
            <AlertTriangle size={12} />
            {portClashLine(portClash, UNWRITTEN_PORT)}
          </p>
        )}
        {clashUnknown}
      </>
    );
  }

  return (
    <>
      <div className={styles.head}>
        {/* The magnifier and the printed key the other filter boxes carry.
            This one had neither, which made the same control look like a
            different one depending on which tab you were on — and left the
            only list in the app you cannot reach from the keyboard. */}
        <div className={styles.searchWrap}>
          <Search size={14} className={styles.searchIcon} />
          <input
            ref={filterBox}
            className={clsx("input", styles.search)}
            placeholder="Filter settings"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
          <kbd className={styles.searchKey}>{actionHint("focusFilter")}</kbd>
        </div>
      </div>

      {/* Above the groups rather than on the port's own row: the row is where
          a clash is said, and this is the absence of an answer about the whole
          screen — it is also still readable when the filter has hidden the
          port. */}
      {clashUnknown}

      {groups.length === 0 ? (
        <p className="hint">Nothing matches “{filter}”.</p>
      ) : (
        groups.map(([group, rows]) => (
          <div key={group} className={styles.group}>
            <h3 className={styles.groupTitle}>
              {group}
              <span className={styles.groupCount}>{rows.length}</span>
            </h3>
            {/* Said once for the group, not under every field in it. Six of
                the World group's keys are creation-only and five of them
                became game rules, so these two sentences used to print eleven
                times between them, one under each row, none of them saying
                anything about the row it sat under. The rows carry the tag;
                this is what the tag means.

                Written to agree with how many rows carry it, because the
                groups split them unevenly — the World group has six tagged
                "world creation" and five tagged "now a game rule", while
                Security has exactly one, and "Settings … are" above a single
                field is the same kind of wrong as "1 items". */}
            {groupNote(
              rows.filter((p) => CREATION_ONLY.has(p.key)).length,
              "The setting tagged “world creation” is only read when the world is created — changing it does nothing to chunks that already exist.",
              "Settings tagged “world creation” are only read when the world is created — changing one does nothing to chunks that already exist.",
            )}
            {groupNote(
              rows.filter((p) => NOW_A_GAMERULE.has(p.key)).length,
              "The setting tagged “now a game rule” is set with a game rule in newer versions instead, which is why an up-to-date server has no such line in this file.",
              "Settings tagged “now a game rule” are set with a game rule in newer versions instead, which is why an up-to-date server has no such line in this file.",
            )}
            <div className={styles.fields}>
              {rows.map((p) => (
                <Field
                  key={p.key}
                  property={p}
                  value={valueOf(p)}
                  changed={p.key in edited}
                  running={server.running}
                  warning={
                    p.key === PORT_KEY && portClash.length > 0
                      ? portClashLine(portClash, "this port")
                      : undefined
                  }
                  problem={problems.get(p.key)?.line}
                  onEdit={edit}
                  onApplyNow={applyNow}
                />
              ))}
            </div>
          </div>
        ))
      )}

      {changedCount > 0 && (
        <div className={"save-bar"}>
          {/* The count gives way to the reason the button is off, the way the
              sharing settings' bar does: a bar that still says how much is
              pending beside a Save that will not go is the one sentence
              somebody does not need. The count comes back as soon as every box
              holds a number the server can use.

              Named rather than counted, because the filter box above can be
              hiding the row that carries the mark, which would leave this the
              only thing on screen saying anything is wrong and nothing in it
              to search for. */}
          <span className="save-hint">
            {unwritable.length === 1 ? (
              `${unwritable[0]} has to be a number the server can use before this can be saved.`
            ) : unwritable.length > 1 ? (
              `${listOf(unwritable)} have to be numbers the server can use before this can be saved.`
            ) : (
              <>
                {changedCount} setting{changedCount === 1 ? "" : "s"} changed.
                {server.running ? " Restart the server to apply." : ""}
              </>
            )}
          </span>
          <button className="btn" onClick={() => setEdited({})}>
            Discard
          </button>
          <button
            className="btn btn-primary"
            onClick={save}
            disabled={saving || unwritable.length > 0}
          >
            {saving && <Loader2 size={14} className="spin" />}
            Save properties
          </button>
        </div>
      )}
    </>
  );
}

function Field({
  property,
  value,
  changed,
  running,
  warning,
  problem,
  onEdit,
  onApplyNow,
}: {
  property: ServerProperty;
  value: string;
  changed: boolean;
  running: boolean;
  /** Something true about the value as it stands, said where it is set. */
  warning?: string;
  /** What is wrong with the number in the box, when something is. */
  problem?: string;
  onEdit: (key: string, value: string) => void;
  onApplyNow: (key: string, command: string) => void;
}) {
  const [revealed, setRevealed] = useState(false);
  const info = PROPERTY_INFO[property.key];
  const retired = RETIRED_KEYS[property.key];
  const id = `prop-${property.key}`;

  const control: Control = controlFor(property.key, property.value);

  // Free text needs the room; a checkbox or a dropdown does not.
  const wide = control.kind === "text";

  return (
    <div
      className={clsx(
        styles.field,
        wide && styles.fieldWide,
        changed && styles.fieldChanged,
      )}
    >
      {/* The raw key lives in the tooltip rather than on screen. It is what
          the wiki calls this setting, so it has to stay reachable — but it is
          not what most people are looking for. */}
      <label className="label" htmlFor={id} title={property.key}>
        {info?.label ?? humanize(property.key)}
        {/* Two words rather than the sentence: the sentence is above the
            group once, and the tag is what points at it. */}
        {CREATION_ONLY.has(property.key) && (
          <span className={styles.tag}>world creation</span>
        )}
        {NOW_A_GAMERULE.has(property.key) && (
          <span className={styles.tag}>now a game rule</span>
        )}
      </label>

      <div className={styles.control}>
        {control.kind === "bool" ? (
          <label className={styles.switch}>
            <input
              id={id}
              type="checkbox"
              checked={value === "true"}
              onChange={(e) =>
                onEdit(property.key, e.target.checked ? "true" : "false")
              }
            />
            <span>{value === "true" ? "On" : "Off"}</span>
          </label>
        ) : control.kind === "enum" ? (
          <select
            id={id}
            className="select"
            value={value}
            onChange={(e) => onEdit(property.key, e.target.value)}
          >
            {/* A value the catalog does not list is kept and selected rather
                than silently rewritten to whatever happens to be first. */}
            {!control.options.some(
              (o) => o.value.toLowerCase() === value.toLowerCase(),
            ) && <option value={value}>{value} — already set</option>}
            {control.options.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </select>
        ) : control.kind === "int" ? (
          <>
            <input
              id={id}
              className="input"
              type="number"
              min={control.min}
              max={control.max}
              value={value}
              onChange={(e) => onEdit(property.key, e.target.value)}
            />
            {control.unit && <span className={styles.unit}>{control.unit}</span>}
          </>
        ) : (
          <>
            <input
              id={id}
              className="input"
              type={control.secret && !revealed ? "password" : "text"}
              placeholder={control.placeholder}
              value={value}
              onChange={(e) => onEdit(property.key, e.target.value)}
            />
            {control.secret && (
              <button
                className="btn btn-sm btn-icon"
                onClick={() => setRevealed((r) => !r)}
                title={revealed ? "Hide" : "Show"}
                aria-label={revealed ? "Hide value" : "Show value"}
              >
                {revealed ? <EyeOff size={13} /> : <Eye size={13} />}
              </button>
            )}
          </>
        )}

        {/* The file makes it stick; the command makes it true now. */}
        {running && info?.liveCommand && (
          <button
            className="btn btn-sm"
            onClick={() => onApplyNow(property.key, info.liveCommand!(value))}
            title={`Run "${info.liveCommand(value)}" in the console`}
          >
            <Zap size={12} />
            Apply now
          </button>
        )}
      </div>

      {info?.description && <p className="hint">{info.description}</p>}
      {retired && <p className="hint">{retired}</p>}
      {/* First of the three, because it is about the characters that were
          typed a moment ago rather than about the setting. */}
      {problem && (
        <p className={styles.danger}>
          <AlertTriangle size={12} />
          {problem}
        </p>
      )}
      {/* Not only while the server runs, unlike the note below: this is about
          the value itself, and the point is to say it as the port is set. */}
      {warning && (
        <p className={styles.danger}>
          <AlertTriangle size={12} />
          {warning}
        </p>
      )}
      {running && info?.danger && (
        <p className={styles.danger}>
          <AlertTriangle size={12} />
          {info.danger}
        </p>
      )}
    </div>
  );
}

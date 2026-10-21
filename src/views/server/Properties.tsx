import { useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Eye, EyeOff, Loader2, Zap } from "lucide-react";

import { api } from "../../lib/api";
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
  const others =
    names.length === 1
      ? names[0]
      : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
  return `${others} ${names.length === 1 ? "is" : "are"} also set to ${port}. Only one of them can run at a time — whichever starts second will fail to bind it.`;
}

/** How to name the port of a server that has not written its file yet. */
const UNWRITTEN_PORT = "the port this server will take the first time it starts";

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
  const [saving, setSaving] = useState(false);
  const [portClash, setPortClash] = useState<string[]>([]);

  useEffect(() => {
    api
      .readServerProperties(server.id)
      .then(setProps)
      .catch(() => setProps([]));
  }, [server.id]);

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
      setPortClash([]);
      return;
    }
    let cancelled = false;
    api
      .gamePortClashes(server.id, typed)
      .then((names) => !cancelled && setPortClash(names))
      .catch(() => !cancelled && setPortClash([]));
    return () => {
      cancelled = true;
    };
  }, [server.id, props, port]);
  const edit = (key: string, value: string) =>
    setEdited((was) => ({ ...was, [key]: value }));

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
      </>
    );
  }

  return (
    <>
      <div className={styles.head}>
        <input
          className={clsx("input", styles.search)}
          placeholder="Filter settings"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>

      {groups.length === 0 ? (
        <p className="hint">Nothing matches “{filter}”.</p>
      ) : (
        groups.map(([group, rows]) => (
          <div key={group} className={styles.group}>
            <h3 className={styles.groupTitle}>
              {group}
              <span className={styles.groupCount}>{rows.length}</span>
            </h3>
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
                  onEdit={edit}
                  onApplyNow={applyNow}
                />
              ))}
            </div>
          </div>
        ))
      )}

      {changedCount > 0 && (
        <div className={styles.saveBar}>
          <span className={styles.saveHint}>
            {changedCount} setting{changedCount === 1 ? "" : "s"} changed.
            {server.running ? " Restart the server to apply." : ""}
          </span>
          <button className="btn" onClick={() => setEdited({})}>
            Discard
          </button>
          <button className="btn btn-primary" onClick={save} disabled={saving}>
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
  onEdit,
  onApplyNow,
}: {
  property: ServerProperty;
  value: string;
  changed: boolean;
  running: boolean;
  /** Something true about the value as it stands, said where it is set. */
  warning?: string;
  onEdit: (key: string, value: string) => void;
  onApplyNow: (key: string, command: string) => void;
}) {
  const [revealed, setRevealed] = useState(false);
  const info = PROPERTY_INFO[property.key];
  const retired = RETIRED_KEYS[property.key];
  const id = `prop-${property.key}`;

  // A key nobody has described still gets the right kind of box, guessed from
  // the value the server wrote. A mod's own setting is usable without anyone
  // here having heard of it.
  const control: Control =
    info?.control ??
    (value === "true" || value === "false"
      ? { kind: "bool" }
      : /^-?\d+$/.test(value.trim())
        ? { kind: "int" }
        : { kind: "text" });

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
      {CREATION_ONLY.has(property.key) && (
        <p className="hint">
          Only read when the world is created — changing it does nothing to
          chunks that already exist.
        </p>
      )}
      {NOW_A_GAMERULE.has(property.key) && (
        <p className="hint">
          Newer versions set this with a game rule instead, which is why an
          up-to-date server has no such line in this file.
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

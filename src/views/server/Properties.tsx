import { useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import { AlertTriangle, Eye, EyeOff, Loader2, Zap } from "lucide-react";

import { api } from "../../lib/api";
import { useStore } from "../../lib/store";
import {
  CREATION_ONLY,
  GROUP_ORDER,
  PROPERTY_INFO,
  RETIRED_KEYS,
  type Control,
} from "../../lib/serverProperties";
import type { Server, ServerProperty } from "../../lib/types";

import styles from "./Properties.module.css";

/** Where keys the table does not recognise go. */
const OTHER = "Other";

/**
 * The server's own `server.properties`, presented rather than dumped.
 *
 * The file stays the source of truth for which keys exist — every key in it is
 * shown, whether or not this app has ever heard of it. The catalog only
 * decorates: a label, a sentence, and the right control. Anything unrecognised
 * falls back to guessing the control from the value, which is how it behaved
 * for everything before.
 */
export default function Properties({ server }: { server: Server }) {
  const reportError = useStore((s) => s.reportError);
  const toast = useStore((s) => s.toast);

  const [props, setProps] = useState<ServerProperty[] | null>(null);
  const [edited, setEdited] = useState<Record<string, string>>({});
  const [filter, setFilter] = useState("");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    api
      .readServerProperties(server.id)
      .then(setProps)
      .catch(() => setProps([]));
  }, [server.id]);

  const dirty = Object.keys(edited).length > 0;
  const valueOf = (p: ServerProperty) => edited[p.key] ?? p.value;
  const edit = (key: string, value: string) =>
    setEdited((was) => ({ ...was, [key]: value }));

  const shown = useMemo(() => {
    const query = filter.trim().toLowerCase();
    if (!query) return props ?? [];
    // Matching the label and the description too, or searching "whitelist"
    // would never find `white-list`.
    return (props ?? []).filter((p) => {
      const info = PROPERTY_INFO[p.key];
      return (
        p.key.toLowerCase().includes(query) ||
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
    return [...GROUP_ORDER, OTHER]
      .filter((g) => buckets.has(g))
      .map((g) => [g, buckets.get(g)!] as const);
  }, [shown]);

  /**
   * Only the edited keys are sent. The backend leaves the rest of the file
   * exactly as it found it — comments, order and all — and that is what keeps a
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
    return <div className="skeleton" style={{ height: 90 }} />;
  }

  if (props.length === 0) {
    return (
      <p className={styles.note}>
        This file appears the first time the server starts — it is written by
        the server, not by Geode. Start it once and its settings show up here.
      </p>
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
        <p className={styles.note}>Nothing matches “{filter}”.</p>
      ) : (
        groups.map(([group, rows]) => (
          <section key={group} className={styles.group}>
            <h3 className={styles.groupTitle}>{group}</h3>
            <div className={styles.rows}>
              {rows.map((p) => (
                <Row
                  key={p.key}
                  property={p}
                  value={valueOf(p)}
                  changed={p.key in edited}
                  running={server.running}
                  onEdit={edit}
                  onApplyNow={applyNow}
                />
              ))}
            </div>
          </section>
        ))
      )}

      <div className={styles.foot}>
        <span className={styles.note}>
          {dirty
            ? `${Object.keys(edited).length} changed. Restart the server to apply.`
            : "Changes apply the next time the server starts."}
        </span>
        <button
          className="btn btn-sm btn-primary"
          onClick={save}
          disabled={!dirty || saving}
        >
          {saving && <Loader2 size={12} className="spin" />}
          Save properties
        </button>
      </div>
    </>
  );
}

function Row({
  property,
  value,
  changed,
  running,
  onEdit,
  onApplyNow,
}: {
  property: ServerProperty;
  value: string;
  changed: boolean;
  running: boolean;
  onEdit: (key: string, value: string) => void;
  onApplyNow: (key: string, command: string) => void;
}) {
  const [revealed, setRevealed] = useState(false);
  const info = PROPERTY_INFO[property.key];
  const retired = RETIRED_KEYS[property.key];
  const id = `prop-${property.key}`;

  // Unknown keys keep the old behaviour: guess the control from the value. A
  // plugin's own setting gets a sensible field without anyone knowing about it.
  const control: Control =
    info?.control ??
    (value === "true" || value === "false"
      ? { kind: "bool" }
      : /^-?\d+$/.test(value.trim())
        ? { kind: "int" }
        : { kind: "text" });

  return (
    <div className={clsx(styles.row, changed && styles.rowChanged)}>
      <div className={styles.rowHead}>
        <label className={styles.label} htmlFor={id}>
          {info?.label ?? property.key}
        </label>
        {/* The raw key stays visible: it is what the wiki, every forum answer
            and the file itself call this setting. */}
        <code className={styles.key}>{property.key}</code>
      </div>

      <div className={styles.control}>
        {control.kind === "bool" ? (
          <label className={styles.switch}>
            <input
              id={id}
              type="checkbox"
              checked={value === "true"}
              onChange={(e) => onEdit(property.key, e.target.checked ? "true" : "false")}
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
            {/* A value the table does not list is kept and selected rather
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
          <span className={styles.withUnit}>
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
          </span>
        ) : (
          <span className={styles.withUnit}>
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
          </span>
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

      {info?.description && <p className={styles.hint}>{info.description}</p>}
      {retired && <p className={styles.hint}>{retired}</p>}
      {CREATION_ONLY.has(property.key) && (
        <p className={styles.hint}>
          Only read when the world is first created — changing it does nothing
          to chunks that already exist.
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

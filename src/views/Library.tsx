import { useMemo, useState } from "react";
import clsx from "clsx";
import {
  Boxes,
  Layers,
  Loader2,
  Play,
  Search,
  Square,
  Clock,
  Package,
} from "lucide-react";

import InstanceIcon from "../components/InstanceIcon";
import NewInstanceModal from "../components/NewInstanceModal";
import { launchInstance, stopInstance } from "../lib/actions";
import { formatDuration, useStore } from "../lib/store";
import { hashHue, relativeTime } from "../lib/format";
import { LOADERS } from "../lib/types";
import type { Instance } from "../lib/types";

import styles from "./Library.module.css";

type Sort = "recent" | "name" | "played";

export default function Library() {
  const instances = useStore((s) => s.instances);
  const loaded = useStore((s) => s.instancesLoaded);
  const navigate = useStore((s) => s.navigate);

  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<Sort>("recent");
  const [creating, setCreating] = useState(false);

  const shown = useMemo(() => {
    const q = search.trim().toLowerCase();
    const filtered = q
      ? instances.filter(
          (i) =>
            i.name.toLowerCase().includes(q) ||
            i.mcVersion.includes(q) ||
            i.loader.includes(q),
        )
      : instances;

    const sorted = [...filtered];
    if (sort === "name") {
      sorted.sort((a, b) => a.name.localeCompare(b.name));
    } else if (sort === "played") {
      sorted.sort((a, b) => b.playSeconds - a.playSeconds);
    }
    // "recent" is the backend's own ordering, so leave it alone.
    return sorted;
  }, [instances, search, sort]);

  const grouped = useMemo(() => {
    const groups = new Map<string, Instance[]>();
    for (const instance of shown) {
      const key = instance.group?.trim() || "";
      const list = groups.get(key);
      if (list) list.push(instance);
      else groups.set(key, [instance]);
    }
    // Ungrouped first, then named groups alphabetically.
    return [...groups.entries()].sort(([a], [b]) =>
      a === "" ? -1 : b === "" ? 1 : a.localeCompare(b),
    );
  }, [shown]);

  return (
    <>
      <header className={styles.header}>
        <h1 className={styles.heading}>Library</h1>
        <div className={styles.headerTools}>
          <div className={styles.searchWrap}>
            <Search size={14} className={styles.searchIcon} />
            <input
              className={clsx("input", styles.search)}
              placeholder="Search instances"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
            />
          </div>
          <select
            className="select"
            style={{ width: 140 }}
            value={sort}
            onChange={(e) => setSort(e.target.value as Sort)}
            aria-label="Sort instances"
          >
            <option value="recent">Recently played</option>
            <option value="name">Name</option>
            <option value="played">Playtime</option>
          </select>
          <button className="btn btn-primary" onClick={() => setCreating(true)}>
            New instance
          </button>
        </div>
      </header>

      <div className={styles.scroll}>
        {!loaded ? (
          <div className={styles.grid}>
            {Array.from({ length: 6 }, (_, i) => (
              <div key={i} className="skeleton" style={{ height: 148 }} />
            ))}
          </div>
        ) : instances.length === 0 ? (
          <div className={styles.empty}>
            <Boxes size={40} className={styles.emptyIcon} />
            <h2>No instances yet</h2>
            <p>
              An instance is one Minecraft install: its own version, mods, and
              worlds. Make as many as you like — they share downloads, so extras
              are nearly free on disk.
            </p>
            <button className="btn btn-primary" onClick={() => setCreating(true)}>
              Create your first instance
            </button>
          </div>
        ) : shown.length === 0 ? (
          <p className={styles.noResults}>Nothing matches “{search}”.</p>
        ) : (
          grouped.map(([group, list]) => (
            <section key={group || "__ungrouped"} className={styles.group}>
              {group && (
                <h2 className={styles.groupTitle}>
                  <Layers size={13} />
                  {group}
                  <span className={styles.groupCount}>{list.length}</span>
                </h2>
              )}
              <div className={styles.grid}>
                {list.map((instance) => (
                  <InstanceCard
                    key={instance.id}
                    instance={instance}
                    onOpen={() => navigate({ view: "instance", id: instance.id })}
                  />
                ))}
              </div>
            </section>
          ))
        )}
      </div>

      {creating && <NewInstanceModal onClose={() => setCreating(false)} />}
    </>
  );
}

function InstanceCard({
  instance,
  onOpen,
}: {
  instance: Instance;
  onOpen: () => void;
}) {
  const progress = useStore((s) => s.progress[`launch:${instance.id}`]);
  const loaderLabel = LOADERS.find((l) => l.id === instance.loader)?.label;
  const preparing = Boolean(progress) && !instance.running;

  const [artFailed, setArtFailed] = useState(false);
  const banner = instance.pack?.bannerUrl ?? instance.pack?.iconUrl ?? null;
  const hue = hashHue(instance.id);

  return (
    <article className={clsx(styles.card, instance.running && styles.cardRunning)}>
      {/* The art leads. Instances without pack artwork get a generated wash
          from their own hue, so the grid keeps one silhouette instead of
          alternating between tall and short cards. */}
      <button className={styles.art} onClick={onOpen} tabIndex={-1} aria-hidden="true">
        {banner && !artFailed ? (
          <img
            src={banner}
            alt=""
            loading="lazy"
            onError={() => setArtFailed(true)}
          />
        ) : (
          <span
            className={styles.artFallback}
            style={{
              background: `linear-gradient(135deg, hsl(${hue} 46% 34%), hsl(${(hue + 55) % 360} 44% 20%))`,
            }}
          />
        )}
        <span className={styles.artScrim} />
      </button>

      <button className={styles.cardBody} onClick={onOpen}>
        <span className={styles.iconRing}>
          <InstanceIcon instance={instance} size={46} />
        </span>
        <div className={styles.cardText}>
          <h3 className={styles.cardName}>{instance.name}</h3>
          <p className={styles.cardMeta}>
            {instance.mcVersion}
            {instance.loader !== "vanilla" && ` · ${loaderLabel}`}
          </p>
          {instance.pack && (
            <p className={styles.cardPack}>
              <Package size={10} />
              {instance.pack.name} {instance.pack.version}
            </p>
          )}
          <div className={styles.cardStats}>
            {instance.modCount > 0 && (
              <span className={styles.stat}>
                {instance.modCount} mod{instance.modCount === 1 ? "" : "s"}
              </span>
            )}
            {instance.playSeconds > 0 && (
              <span className={styles.stat}>
                <Clock size={11} />
                {formatDuration(instance.playSeconds)}
              </span>
            )}
          </div>
        </div>
      </button>

      <div className={styles.cardFoot}>
        <span className={styles.lastPlayed}>
          {instance.running
            ? "Running"
            : preparing
              ? (progress?.stage ?? "Preparing…")
              : instance.lastPlayed
                ? `Played ${relativeTime(instance.lastPlayed)}`
                : "Never played"}
        </span>

        {instance.running ? (
          <button
            className="btn btn-sm btn-danger"
            onClick={() => void stopInstance(instance)}
          >
            <Square size={12} fill="currentColor" />
            Stop
          </button>
        ) : (
          <button
            className="btn btn-sm btn-primary"
            onClick={() => void launchInstance(instance)}
            disabled={preparing}
          >
            {preparing ? (
              <Loader2 size={12} className="spin" />
            ) : (
              <Play size={12} fill="currentColor" />
            )}
            Play
          </button>
        )}
      </div>
    </article>
  );
}

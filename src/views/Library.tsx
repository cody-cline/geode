import { useMemo, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import clsx from "clsx";
import {
  Boxes,
  Layers,
  TriangleAlert,
  Loader2,
  Play,
  Search,
  Square,
  Clock,
  Package,
  UserRound,
} from "lucide-react";

import AccountsModal from "../components/AccountsModal";
import InstanceIcon from "../components/InstanceIcon";
import NewInstanceModal from "../components/NewInstanceModal";
import StopGameModal from "../components/StopGameModal";
import { launchInstance } from "../lib/actions";
import { actionHint } from "../lib/shortcuts";
import { useActionShortcut } from "../lib/useActionShortcut";
import { formatDuration, useStore } from "../lib/store";
import { hashHue, relativeTime } from "../lib/format";
import { LOADERS } from "../lib/types";
import type { Instance } from "../lib/types";

import styles from "./Library.module.css";

type Sort = "recent" | "name" | "played";

export default function Library() {
  const instances = useStore((s) => s.instances);
  const loaded = useStore((s) => s.instancesLoaded);
  const loadError = useStore((s) => s.instancesError);
  const refreshInstances = useStore((s) => s.refreshInstances);
  const navigate = useStore((s) => s.navigate);

  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<Sort>("recent");
  const [creating, setCreating] = useState(false);
  const [signingIn, setSigningIn] = useState(false);
  // Which instance a card has asked to stop. Held here rather than in the card
  // for the same reason `signingIn` is: a card is `overflow: hidden` and grows
  // a `transform` on hover, which makes it the containing block for anything
  // fixed inside it — a dialog opened from a card would be clipped into the
  // card it was opened from.
  const [stopping, setStopping] = useState<Instance | null>(null);

  useActionShortcut("newThing", (e) => {
    e.preventDefault();
    setCreating(true);
  });

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
          {/* Searching and sorting nothing are controls that cannot do
              anything, and on first run they sat live over an empty page. */}
          {instances.length > 0 && (
            <>
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
            </>
          )}
          {/* Quiet while the page below is offering the same thing. Two
              accent-filled buttons for the one action available, three hundred
              pixels apart, read as two different actions — and the one in the
              middle of the page is the one with the explanation attached. */}
          <button
            className={clsx("btn", instances.length > 0 && "btn-primary")}
            onClick={() => setCreating(true)}
          >
            New instance
            <kbd className={styles.newKey}>{actionHint("newThing")}</kbd>
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
        ) : loadError && instances.length === 0 ? (
          /* Before the empty state, because they are different claims and only
             one of them is ours to make. An unread list is not an empty one,
             and this screen is the first thing Geode shows: telling somebody
             their instances are not there, on the front door, over a read that
             failed, is the worst version of guessing. */
          <div className={styles.empty}>
            <TriangleAlert size={40} className={styles.emptyIcon} />
            <h2>Could not read your instances</h2>
            <p>
              Which is not the same as having none. Nothing has been changed or
              removed — the list could not be read, so what is in it is unknown.
            </p>
            <p className={styles.why}>{loadError}</p>
            <button className="btn" onClick={() => void refreshInstances()}>
              Try again
            </button>
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
                    onSignIn={() => setSigningIn(true)}
                    onStop={() => setStopping(instance)}
                  />
                ))}
              </div>
            </section>
          ))
        )}
      </div>

      {creating && <NewInstanceModal onClose={() => setCreating(false)} />}
      {signingIn && <AccountsModal onClose={() => setSigningIn(false)} />}

      {stopping && (
        <StopGameModal instance={stopping} onClose={() => setStopping(null)} />
      )}
    </>
  );
}

function InstanceCard({
  instance,
  onOpen,
  onSignIn,
  onStop,
}: {
  instance: Instance;
  onOpen: () => void;
  onSignIn: () => void;
  onStop: () => void;
}) {
  const progress = useStore((s) => s.progress[`launch:${instance.id}`]);
  const accounts = useStore((s) => s.accounts);
  const loaderLabel = LOADERS.find((l) => l.id === instance.loader)?.label;
  const preparing = Boolean(progress) && !instance.running;

  const [artFailed, setArtFailed] = useState(false);
  const banner = instance.bannerPath
    ? convertFileSrc(instance.bannerPath)
    : (instance.pack?.bannerUrl ?? instance.pack?.iconUrl ?? null);
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
        {/* Spans, not headings and paragraphs: this all sits inside a button,
            and block elements are not allowed there. The classes carry the
            display, so nothing moves. */}
        <span className={styles.cardText}>
          <span className={styles.cardName}>{instance.name}</span>
          <span className={styles.cardMeta}>
            {instance.mcVersion}
            {instance.loader !== "vanilla" && ` · ${loaderLabel}`}
          </span>
          {instance.pack && (
            <span className={styles.cardPack}>
              <Package size={10} />
              {instance.pack.name} {instance.pack.version}
            </span>
          )}
          <span className={styles.cardStats}>
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
          </span>
        </span>
      </button>

      <div className={styles.cardFoot}>
        <span className={styles.lastPlayed}>
          {instance.running
            ? "Running"
            : preparing
              ? (progress?.stage ?? "Preparing…")
              : instance.lastPlayed
                ? /* "Last played", the same words the instance's own header
                     uses for the same fact. The card said "Played 3 days ago"
                     and the header "Last played 3 days ago", which is one
                     timestamp in two voices on two screens a click apart. */
                  `Last played ${relativeTime(instance.lastPlayed)}`
                : "Never played"}
        </span>

        {instance.running ? (
          <button className="btn btn-sm btn-danger" onClick={onStop}>
            <Square size={12} fill="currentColor" />
            Stop
          </button>
        ) : accounts.length === 0 ? (
          /* The same reasoning as the instance header's: the precondition is
             known before the click, so the button says what is missing rather
             than being a Play that fails into a corner. This card is the one
             most people press, so fixing only the header left the common path
             dead-ending. */
          <button className="btn btn-sm btn-primary" onClick={onSignIn}>
            <UserRound size={12} />
            Sign in to play
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

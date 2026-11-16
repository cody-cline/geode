import { useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
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

import { term } from "../lib/glossary";
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
// The mod list's keyboard, imported rather than written again — see `cardKeys`.
import { rowKeys } from "./instance/ContentTab";

import styles from "./Library.module.css";

type Sort = "recent" | "name" | "played";

/**
 * The two front-door grids' keyboard, which is the mod list's with one key
 * added.
 *
 * `rowKeys` is the whole of the walk — one tab stop for the grid, Up and Down
 * between the cards, Left and Right across the card's own controls, Home and
 * End to the ends — and it is imported rather than copied so the four arrows
 * mean the same two things in every list in this app. Twenty instances were
 * forty tab stops here, two per card, with no arrow key doing anything at
 * all; they are one stop and a walk now, and End is the far end of it.
 *
 * Up and Down walk the cards in reading order however many columns the window
 * happens to fit, for the reason Browse's category rail gives about its own:
 * the column count here is `auto-fill`'s answer to the width, so it is the
 * window's fact and not the list's, and "the next card" is the only thing a
 * press can mean that is the same at every size.
 *
 * The one key `rowKeys` leaves to the caller is Enter. On the mod list it
 * takes a row into the selection; these grids have none, so here it does what
 * clicking the card does — but only bare. Ctrl+Enter is `launch`, bound at
 * the window and therefore read first, and a card that answered both would
 * start the game and walk away from it in the same press.
 *
 * Exported for `Servers`, the way `rowKeys` itself is exported for the
 * server's Content tab. The two grids already share a stylesheet so they
 * cannot drift apart on screen — this is the same promise about the keyboard.
 */
export function cardKeys(
  e: ReactKeyboardEvent<HTMLElement>,
  open: (id: string) => void,
) {
  const bareEnter =
    e.key === "Enter" && !e.ctrlKey && !e.metaKey && !e.altKey;
  const card = (e.target as HTMLElement).closest<HTMLElement>("[data-row]");
  if (bareEnter && card && e.target === card && card.dataset.row) {
    e.preventDefault();
    open(card.dataset.row);
    return;
  }
  rowKeys(e);
}

/**
 * Press the Play or Start button of the card the keyboard is on.
 *
 * The button rather than the act behind it, which is what makes the key
 * honest: every reason a card has for not offering it — the game is already
 * running, nobody is signed in, the server has no software yet, a download is
 * still going — is written into that button already, so a key that presses it
 * can never do something the card is not offering. It is also the button the
 * hint is printed inside, so the two cannot come to disagree.
 *
 * Read off what has focus rather than off a remembered card: the key presses
 * *that* card, and one that acted on a card the reader had since left would
 * launch something they are not looking at.
 */
export function pressLaunch(e: KeyboardEvent) {
  const card = document.activeElement?.closest<HTMLElement>("[data-row]");
  const button = card?.querySelector<HTMLButtonElement>("[data-launch]");
  if (!button || button.disabled) return;
  e.preventDefault();
  button.click();
}

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
  // Which card the keyboard is on, so Tab comes back to the one it left
  // rather than to the top of the grid. The same bookkeeping the mod list
  // keeps for its rows, and checked against the list below rather than
  // trusted, because it outlives the search it was set under.
  const [keyed, setKeyed] = useState<string | null>(null);
  const searchBox = useRef<HTMLInputElement>(null);

  useActionShortcut("newThing", (e) => {
    e.preventDefault();
    setCreating(true);
  });

  /**
   * Ctrl+F reaches this screen's search box, as it does every filter box
   * inside an instance or a server.
   *
   * Only once there is something to search: the box is not rendered until
   * there is an instance, and a key that focused nothing would cost the
   * webview's own find bar and give nothing back. The hint is inside the box,
   * so what is printed and what works appear together.
   */
  useActionShortcut("focusFilter", (e) => {
    const box = searchBox.current;
    if (!box) return;
    e.preventDefault();
    box.focus();
    box.select();
  });

  useActionShortcut("launch", pressLaunch);

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
        <div>
          <h1 className={styles.heading}>Library</h1>
          {/* The sentence saying what an instance is lived only in the empty
              state, so it disappeared the moment somebody made one and was
              never readable again — the same thing Plans fixed by putting it
              here. Out of the glossary, which is the one copy of it: the
              empty state below renders these same words. */}
          <p className={styles.lede}>{term("Instance").short}</p>
        </div>
        <div className={styles.headerTools}>
          {/* Searching and sorting nothing are controls that cannot do
              anything, and on first run they sat live over an empty page. */}
          {instances.length > 0 && (
            <>
              <div className={styles.searchWrap}>
                <Search size={14} className={styles.searchIcon} />
                <input
                  ref={searchBox}
                  className={clsx("input", styles.search)}
                  placeholder="Search instances"
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                />
                {/* The binding, printed in the box it reaches — an action has
                    no sidebar row to be found from, so the thing it presses
                    is the only place it can be come across. The same hint in
                    the same corner as the four filter boxes deeper in. */}
                <kbd className={styles.searchKey}>{actionHint("focusFilter")}</kbd>
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
              {term("Instance").short} {term("Instance").more}
            </p>
            <button className="btn btn-primary" onClick={() => setCreating(true)}>
              Create your first instance
            </button>
          </div>
        ) : shown.length === 0 ? (
          <p className={styles.noResults}>Nothing matches “{search}”.</p>
        ) : (
          grouped.map(([group, list]) => {
            // The one card in this group that answers Tab. A group is its own
            // list and keeps its own stop, so every group stays reachable
            // whichever one the keyboard was last in.
            const stop = list.some((i) => i.id === keyed) ? keyed : list[0].id;
            return (
              <section key={group || "__ungrouped"} className={styles.group}>
                {group && (
                  <h2 className={styles.groupTitle}>
                    <Layers size={13} />
                    {group}
                    <span className={styles.groupCount}>{list.length}</span>
                  </h2>
                )}
                {/* A grid, the same shape the mod lists and Browse's results
                    declare and for the same two reasons: a list whose items
                    carry controls of their own is a grid, and a grid is the
                    role whose documented keyboard is the one `cardKeys`
                    implements, so the arrows are promised rather than left to
                    be found. `listitem` was the other candidate and takes no
                    name from what is inside it, which would have made a
                    focused card announce the word "card" and nothing else.
                    Two cells on every card, whatever it holds. */}
                <div
                  className={styles.grid}
                  role="grid"
                  aria-label={group ? `${group} instances` : "Instances"}
                  onKeyDown={(e) =>
                    cardKeys(e, (id) => navigate({ view: "instance", id }))
                  }
                  // Where the keyboard is, recorded as it arrives. Bound here
                  // because focus bubbles in React and the card is not always
                  // the element focused — it may be a button inside it.
                  onFocus={(e) => {
                    const card = (e.target as HTMLElement).closest<HTMLElement>(
                      "[data-row]",
                    );
                    if (card?.dataset.row) setKeyed(card.dataset.row);
                  }}
                >
                  {list.map((instance) => (
                    <InstanceCard
                      key={instance.id}
                      instance={instance}
                      stop={instance.id === stop}
                      onOpen={() => navigate({ view: "instance", id: instance.id })}
                      onSignIn={() => setSigningIn(true)}
                      onStop={() => setStopping(instance)}
                    />
                  ))}
                </div>
              </section>
            );
          })
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
  stop,
  onOpen,
  onSignIn,
  onStop,
}: {
  instance: Instance;
  stop: boolean;
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
    <article
      className={clsx(styles.card, instance.running && styles.cardRunning)}
      role="row"
      data-row={instance.id}
      tabIndex={stop ? 0 : -1}
    >
      {/* The art leads. Instances without pack artwork get a generated wash
          from their own hue, so the grid keeps one silhouette instead of
          alternating between tall and short cards.

          A span rather than a button, and that is the arrow walk's doing: the
          walk steps across everything in the card that Tab has been taken off
          and this is hidden from a reader, so as a button it was a stop that
          announced nothing. Opening the card by clicking its artwork is a
          pointer's shortcut to what the card itself already does — Enter on
          the card, or the title beside it. */}
      <span className={styles.art} onClick={onOpen} aria-hidden="true">
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
      </span>

      {/* The card's two cells — what the thing is, and what you can do about
          it — which is the same two on every card in the grid. `role` goes on
          a wrapper here rather than on the button inside it, because putting
          it on the button would take the button's own role away; the same
          place `ProjectCard` puts its two. */}
      <div className={styles.bodyCell} role="gridcell">
        <button className={styles.cardBody} onClick={onOpen} tabIndex={-1}>
          <span className={styles.iconRing}>
            <InstanceIcon instance={instance} size={46} />
          </span>
          {/* Spans, not headings and paragraphs: this all sits inside a
              button, and block elements are not allowed there. The classes
              carry the display, so nothing moves. */}
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
      </div>

      <div className={styles.cardFoot} role="gridcell">
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
          <button className="btn btn-sm btn-danger" onClick={onStop} tabIndex={-1}>
            <Square size={12} fill="currentColor" />
            Stop
          </button>
        ) : accounts.length === 0 ? (
          /* The same reasoning as the instance header's: the precondition is
             known before the click, so the button says what is missing rather
             than being a Play that fails into a corner. This card is the one
             most people press, so fixing only the header left the common path
             dead-ending. */
          <button className="btn btn-sm btn-primary" onClick={onSignIn} tabIndex={-1}>
            <UserRound size={12} />
            Sign in to play
          </button>
        ) : (
          <button
            className="btn btn-sm btn-primary"
            onClick={() => void launchInstance(instance)}
            disabled={preparing}
            tabIndex={-1}
            data-launch
          >
            {preparing ? (
              <Loader2 size={12} className="spin" />
            ) : (
              <Play size={12} fill="currentColor" />
            )}
            Play
            {/* The key, printed in the button it presses — and shown only
                while the keyboard is in this card, because that is exactly
                when it works. One copy on screen, on the card it would act
                on, says something true; twenty standing copies of it would
                each be a claim about a card the key is not pointed at, and
                would crowd the timestamp beside them off every footer in the
                grid. Hidden in CSS, on `.card:focus-within`. */}
            <kbd className={styles.launchKey}>{actionHint("launch")}</kbd>
          </button>
        )}
      </div>
    </article>
  );
}

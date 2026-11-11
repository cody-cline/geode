import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import {
  ArrowUpCircle,
  FilePlus2,
  Link2,
  ListChecks,
  Loader2,
  Lock,
  MoreHorizontal,
  Package,
  Plus,
  Power,
  PowerOff,
  RefreshCw,
  Search,
  Trash2,
  Unlock,
} from "lucide-react";

import Modal from "../../components/Modal";
import Tooltip from "../../components/Tooltip";
import { api } from "../../lib/api";
import { fileSize, providerLabel, shownName } from "../../lib/format";
import { rovingKey } from "../../lib/rovingFocus";
import { actionHint } from "../../lib/shortcuts";
import { useActionShortcut } from "../../lib/useActionShortcut";
import { useStore } from "../../lib/store";
import type { ContentItem, ContentKind, Instance, UpdateCandidate } from "../../lib/types";

import styles from "./ContentTab.module.css";

const KINDS: { id: ContentKind; label: string; extensions: string[] }[] = [
  { id: "mod", label: "Mods", extensions: ["jar"] },
  { id: "resourcepack", label: "Resource Packs", extensions: ["zip"] },
  { id: "shaderpack", label: "Shaders", extensions: ["zip"] },
  { id: "datapack", label: "Data Packs", extensions: ["zip"] },
];

const KIND_IDS = KINDS.map((k) => k.id);

/**
 * A selection, with every file that has just been renamed following its row.
 *
 * Turning a file on or off *is* a rename on disk — `foo.jar` becomes
 * `foo.jar.disabled` — and a selection is keyed by the file name because that
 * is the only thing unique to a row. Without this, disabling twelve mods would
 * empty the selection the moment it worked, and the obvious next press would
 * have nothing left to act on.
 */
function followRenames(selected: Set<string>, moves: Map<string, string>) {
  const next = new Set(selected);
  for (const [from, to] of moves) {
    if (!next.has(from)) continue;
    next.delete(from);
    next.add(to);
  }
  return next;
}

/** A row of the list, carrying the file name that identifies it. */
const ROW = "[data-row]";

/**
 * Everything in a row that has been taken out of the tab order, which is the
 * row's own arrow walk.
 *
 * Written as the absence of a tab stop rather than as a list of the controls,
 * because that is the invariant worth keeping: a control added to a row has to
 * be given `tabIndex={-1}` or the list goes back to costing a tab stop per
 * button — and giving it one puts it in this walk with no further thought. A
 * disabled control is skipped, since focusing one tells nobody anything.
 */
const IN_ROW = '[tabindex="-1"]:not(:disabled)';

/**
 * The list's keyboard, bound to the grid rather than to a row.
 *
 * The longest list in this app was reachable by Tab alone: every row's three
 * or four controls were a stop of their own, so six mods cost about twenty-one
 * presses and an eighty-mod pack about three hundred, with no arrow key doing
 * anything at all. So the **row** is the tab stop — one for the whole list —
 * and the keys below are what reaches everything inside it:
 *
 * - **Up and Down** move between rows, always landing on the row itself rather
 *   than on the control that happens to sit above or below. A row holds one,
 *   two or four buttons depending on the folder and the file, so a column
 *   position is not a thing to keep; and landing on the row is what gets the
 *   whole row read out.
 * - **Left and Right** walk the controls of the row the keyboard is on,
 *   wrapping. Wrapping is what keeps both ends of a row cheap: Right from the
 *   row is its first control and Left from the row is its last, so Delete is
 *   one press away however many badges sit in between.
 * - **Home and End** are the first and last row.
 * - **Space and Enter** take the row into the selection, where there is one to
 *   take it into — see `select` below.
 *
 * Why not `rovingKey`, which every pick-one strip in this app uses: that
 * helper's contract is that the option the arrows land on is *chosen*, exactly
 * as clicking it would choose it, and it treats its group as one axis. Neither
 * holds here. Landing on a row must not select it, landing on a button must
 * not press it, and this is two nested axes rather than one. The same
 * distinction `ShareMenu` makes about its own arrows.
 *
 * `select` is optional because the two panes differ in exactly one way: the
 * instance's list can be selected from, the server's cannot, and a key that
 * does nothing is better left unclaimed than bound to nothing.
 */
export function rowKeys(
  e: KeyboardEvent<HTMLElement>,
  select?: (fileName: string, extend: boolean) => void,
) {
  const grid = e.currentTarget;
  const row = (e.target as HTMLElement).closest<HTMLElement>(ROW);
  const file = row?.dataset.row;
  if (!row || !file) return;

  // A `<select>` owns every key while the keyboard is in it — the same rule
  // `shortcuts.ts` keeps for a bare binding, and for the same reason: an arrow
  // on a closed select is how its value is changed, so walking the row from
  // one would be this handler answering a press meant to pick an option. The
  // server's Side control is the one of these in a row, and Shift+Tab is the
  // way back out to the row, which is the grid's one tab stop.
  if (e.target instanceof HTMLSelectElement) return;

  const rows = Array.from(grid.querySelectorAll<HTMLElement>(ROW));
  const at = rows.indexOf(row);

  /** A row, clamped at the ends: a list is a place, and the top of it is not
   *  somewhere Down should arrive at. The press is swallowed either way, so it
   *  never scrolls the panel out from under the row it was pointed at. */
  const toRow = (to: number) => {
    e.preventDefault();
    rows[Math.max(0, Math.min(to, rows.length - 1))]?.focus();
  };

  switch (e.key) {
    case "ArrowDown":
      return toRow(at + 1);
    case "ArrowUp":
      return toRow(at - 1);
    case "Home":
      return toRow(0);
    case "End":
      return toRow(rows.length - 1);
    case "ArrowRight":
    case "ArrowLeft": {
      const controls = Array.from(row.querySelectorAll<HTMLElement>(IN_ROW));
      if (controls.length === 0) return;
      e.preventDefault();
      const step = e.key === "ArrowRight" ? 1 : -1;
      const from = controls.indexOf(e.target as HTMLElement);
      // From the row — or from anything else that is not one of the controls —
      // a step in either direction means the end it points at.
      const to =
        from < 0
          ? step > 0
            ? 0
            : controls.length - 1
          : (from + step + controls.length) % controls.length;
      controls[to].focus();
      return;
    }
    case " ":
    case "Enter":
      // Only on the row. On a control these are the control's own: Space
      // toggles the checkbox the keyboard is on and Enter presses the button,
      // and taking either would be this handler answering for a press that was
      // aimed at something specific.
      if (e.target !== row || !select) return;
      e.preventDefault();
      select(file, e.shiftKey);
      return;
  }
}


/** One of the folder's occasional acts, as the menu below draws it. */
type MoreItem = {
  label: string;
  /** A fragment to the right of the label — a count, or why it is off. */
  note?: string;
  icon: ReactNode;
  disabled?: boolean;
  onSelect: () => void;
};

/**
 * The acts that are not why anybody opened this screen.
 *
 * Three controls stood in the toolbar for things done once a month: adding a
 * file by hand, checking for updates, and taking every row into a selection.
 * They were the same size and weight as the filter box, which is the tool for
 * the one thing this screen is *for*, and they pushed the primary action off
 * the edge at the width the window can open to. So they are behind one press,
 * which is what a disclosure is: not hiding, but saying which things are the
 * subject and which are the drawer.
 *
 * Not portalled, where `ShareMenu` has to be: that one drops out of a hero
 * carrying `isolation: isolate` and could not escape it. This toolbar is a
 * plain block below the hero, so an absolutely-positioned menu paints over
 * the list underneath it and is clicked on by it.
 *
 * `open` belongs to the screen rather than to this component because the
 * screen's Escape binding has to stand aside while the menu is up — the same
 * rule `useActionShortcut` keeps for a dialog, one level down.
 */
function MoreMenu({
  open,
  setOpen,
  busy,
  items,
}: {
  open: boolean;
  setOpen: (open: boolean) => void;
  /** Something the menu launched is still running, said on the button. */
  busy?: string;
  items: MoreItem[];
}) {
  const wrap = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);

  // Closing puts the keyboard back on the button, for the reason `ShareMenu`
  // spells out: the item pressed is about to stop existing, and focus left on
  // it strands the keyboard at the top of the document.
  const close = useCallback(() => {
    trigger.current?.focus();
    setOpen(false);
  }, [setOpen]);

  useEffect(() => {
    if (!open) return;
    const stops = () =>
      Array.from(
        wrap.current?.querySelectorAll<HTMLButtonElement>(
          "[data-menuitem]:not(:disabled)",
        ) ?? [],
      );
    // The keyboard goes in as the menu opens, which is what makes the arrows
    // below worth binding at all.
    stops()[0]?.focus();

    const away = (e: MouseEvent) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    const keys = (e: globalThis.KeyboardEvent) => {
      // The button answers for itself; the press that opened the menu is still
      // bubbling when this listener goes on.
      if (e.target === trigger.current) return;
      // Tab is not prevented: the keyboard carries on out of the toolbar from
      // the button, which is where `close` has just put it.
      if (e.key === "Escape" || e.key === "Tab") return close();
      const step = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
      if (step === 0) return;
      const list = stops();
      if (list.length === 0) return;
      e.preventDefault();
      const from = list.indexOf(document.activeElement as HTMLButtonElement);
      list[(from + step + list.length) % list.length].focus();
    };
    // A scroll closes it rather than moving it: the toolbar does not scroll,
    // so what moved is the list the menu is hanging over. Captured, because
    // the thing that scrolls is a pane inside the view.
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", keys);
    document.addEventListener("scroll", close, true);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", keys);
      document.removeEventListener("scroll", close, true);
    };
  }, [open, close, setOpen]);

  return (
    <div className={styles.more} ref={wrap}>
      <button
        ref={trigger}
        className="btn btn-icon"
        onClick={() => setOpen(!open)}
        onKeyDown={(e) => {
          if (e.key !== "ArrowDown") return;
          e.preventDefault();
          setOpen(true);
        }}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={busy ?? "More"}
      >
        {busy ? (
          <Loader2 size={16} className="spin" />
        ) : (
          <MoreHorizontal size={16} />
        )}
      </button>

      {open && (
        <div className={styles.menu} role="menu" aria-label="More">
          {items.map((item) => (
            <button
              key={item.label}
              data-menuitem
              role="menuitem"
              className={styles.menuItem}
              disabled={item.disabled}
              onClick={() => {
                close();
                item.onSelect();
              }}
            >
              {item.icon}
              <span>{item.label}</span>
              {item.note && <span className={styles.menuNote}>{item.note}</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export default function ContentTab({
  instance,
  onBrowse,
}: {
  instance: Instance;
  onBrowse: () => void;
}) {
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);
  const refreshInstances = useStore((s) => s.refreshInstances);

  const [kind, setKind] = useState<ContentKind>("mod");
  const [items, setItems] = useState<ContentItem[] | null>(null);
  const [filter, setFilter] = useState("");
  const [updates, setUpdates] = useState<UpdateCandidate[]>([]);
  const [checking, setChecking] = useState(false);
  const [updating, setUpdating] = useState<string | null>(null);
  const [linking, setLinking] = useState(false);

  // The rows a press is about to act on, by file name, and the row a plain
  // click last landed on so Shift can measure a span from it.
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  // The row the keyboard was last on, which is the row that answers Tab.
  const [keyed, setKeyed] = useState<string | null>(null);
  const [bulkBusy, setBulkBusy] = useState(false);
  const [askingDelete, setAskingDelete] = useState(false);
  // Whether the toolbar's drawer is open, held here because Escape means
  // something else on this screen and the drawer has first call on it.
  const [moreOpen, setMoreOpen] = useState(false);
  const filterBox = useRef<HTMLInputElement>(null);

  const load = useCallback(async () => {
    try {
      setItems(await api.listContent(instance.id, kind));
    } catch (e) {
      reportError("Could not read the content folder", e);
      setItems([]);
    }
  }, [instance.id, kind, reportError]);

  useEffect(() => {
    setItems(null);
    // A selection is of files in one folder, so changing folders drops it
    // rather than carrying names that are not in the list any more.
    setSelected(new Set());
    setAnchor(null);
    void load();
  }, [load]);

  const shown = useMemo(() => {
    if (!items) return null;
    const q = filter.trim().toLowerCase();
    return q
      ? items.filter(
          (i) =>
            i.name.toLowerCase().includes(q) ||
            i.fileName.toLowerCase().includes(q),
        )
      : items;
  }, [items, filter]);

  // Everything about the selection is read back off `items`, so a name left in
  // it by a file that has since gone counts for nothing anywhere on screen.
  const picked = useMemo(
    () => items?.filter((i) => selected.has(i.fileName)) ?? [],
    [items, selected],
  );
  const pickedShown = useMemo(
    () => shown?.filter((i) => selected.has(i.fileName)).length ?? 0,
    [shown, selected],
  );
  // The server's own mods are not the player's to delete — the row's own
  // button is disabled for them and Rust refuses it as well — so a bulk delete
  // counts what it can really take.
  const deletable = picked.filter((i) => i.origin !== "server");
  const offCount = picked.filter((i) => !i.enabled).length;
  const onCount = picked.filter((i) => i.enabled).length;

  /**
   * Take a row into the selection, or out of it.
   *
   * Shift adds the span between the row a plain click last landed on and this
   * one, rather than replacing what is selected: the gesture reads as "and
   * these too". The span is measured over what is *shown*, so it runs along
   * the rows in front of you and never through the ones the filter is hiding.
   */
  function pick(item: ContentItem, extend: boolean) {
    const rows = shown ?? [];
    setSelected((prev) => {
      const next = new Set(prev);
      const from = rows.findIndex((r) => r.fileName === anchor);
      const to = rows.findIndex((r) => r.fileName === item.fileName);
      if (extend && from >= 0 && to >= 0) {
        for (const row of rows.slice(Math.min(from, to), Math.max(from, to) + 1)) {
          next.add(row.fileName);
        }
        return next;
      }
      if (next.has(item.fileName)) next.delete(item.fileName);
      else next.add(item.fileName);
      return next;
    });
    // The anchor stays where a plain click put it, so a second Shift-click
    // adjusts the same span instead of walking away from it.
    if (!extend) setAnchor(item.fileName);
  }

  function clearSelection() {
    setSelected(new Set());
    setAnchor(null);
  }

  /**
   * The Content tab's own keys, bound while the tab is on screen rather than
   * at the window: an action shortcut means nothing on a screen that cannot
   * carry it out, and `InstanceView` mounts this tab only while it is the one
   * showing, so leaving the listener with the component is the whole of
   * keeping these keys in context.
   *
   * Captured, so it runs before `Modal`'s own Escape handler and can still see
   * that a dialog is open. A dialog is a mode — the same reason `App` holds the
   * sidebar's keys back for one — and a press that belongs to it must not also
   * reach the screen underneath.
   */
  useActionShortcut("focusFilter", (e) => {
    // The webview's find bar is not what this is for, and the text that is
    // there is selected so the next keystroke replaces the old filter.
    e.preventDefault();
    const box = filterBox.current;
    if (box) {
      box.focus();
      box.select();
    }
  });

  // Not prevented: Escape has no default of its own here, and anything else
  // listening for it should still get it. The toolbar's drawer is one of those
  // — it is a mode on top of this screen, the way a dialog is a mode on top of
  // the app, so a press that dismisses it must not also empty the selection
  // underneath.
  useActionShortcut("clearSelection", () => {
    if (moreOpen) return;
    setSelected((prev) => (prev.size === 0 ? prev : new Set<string>()));
    setAnchor(null);
  });

  // The folder as well as the project: the check spans every folder while this
  // list shows one, so a row takes only a candidate from its own folder — the
  // same rule the server tab keeps.
  const updateFor = (item: ContentItem) =>
    updates.find(
      (u) =>
        u.projectId === item.projectId &&
        u.provider === item.provider &&
        u.kind === kind,
    );

  async function toggle(item: ContentItem) {
    try {
      const renamed = await api.setContentEnabled(
        instance.id,
        kind,
        item.fileName,
        !item.enabled,
      );
      setSelected((prev) =>
        followRenames(prev, new Map([[item.fileName, renamed]])),
      );
      // And the tab stop follows the rename too. Turning a file off renames it
      // on disk, so the row it was on is replaced by one with a different key:
      // without this, disabling the fifth mod puts the list's one tab stop
      // back on the first.
      setKeyed((k) => (k === item.fileName ? renamed : k));
      await load();
    } catch (e) {
      reportError("Could not toggle that file", e);
    }
  }

  async function remove(item: ContentItem) {
    try {
      await api.removeContent(instance.id, kind, item.fileName);
      await Promise.all([load(), refreshInstances()]);
    } catch (e) {
      reportError(`Could not delete ${item.name}`, e);
    }
  }

  /**
   * Turn every selected file on, or every one off.
   *
   * A loop over the same command a row presses, not a new one: each rename is
   * independent of the others and the list is reread once at the end rather
   * than after every one. Files already the way they are being asked to be are
   * left out, so "Disable" on a mixed selection is not a round trip per row
   * that changes nothing.
   */
  async function setPickedEnabled(enable: boolean) {
    const targets = picked.filter((i) => i.enabled !== enable);
    if (targets.length === 0) return;
    setBulkBusy(true);
    const moves = new Map<string, string>();
    try {
      for (const item of targets) {
        moves.set(
          item.fileName,
          await api.setContentEnabled(instance.id, kind, item.fileName, enable),
        );
      }
    } catch (e) {
      // Whatever landed before the failure has landed, so this says how far it
      // got instead of implying none of it happened.
      reportError(
        moves.size > 0
          ? `${enable ? "Enabled" : "Disabled"} ${moves.size} of ${targets.length} files, then stopped`
          : `Nothing was ${enable ? "enabled" : "disabled"}`,
        e,
      );
    } finally {
      setSelected((prev) => followRenames(prev, moves));
      setBulkBusy(false);
      await load();
    }
  }

  /** Delete every selected file that is this instance's to delete. */
  async function deletePicked() {
    const targets = deletable;
    setBulkBusy(true);
    const gone: string[] = [];
    try {
      for (const item of targets) {
        await api.removeContent(instance.id, kind, item.fileName);
        gone.push(item.fileName);
      }
    } catch (e) {
      reportError(
        gone.length > 0
          ? `Deleted ${gone.length} of ${targets.length} files, then stopped`
          : "Nothing was deleted",
        e,
      );
    } finally {
      setSelected((prev) => {
        const next = new Set(prev);
        for (const file of gone) next.delete(file);
        return next;
      });
      setAskingDelete(false);
      setBulkBusy(false);
      await Promise.all([load(), refreshInstances()]);
    }
  }

  /**
   * Stop asking which Minecraft version this mod's builds are for, or start
   * again.
   *
   * The toast says what the choice does and does not do rather than only that
   * it was saved: nothing has looked inside the jar, so the one honest claim
   * is about the question Geode will put next time.
   */
  async function ignoreVersion(item: ContentItem, ignore: boolean) {
    try {
      await api.setContentIgnoreGameVersion(
        instance.id,
        kind,
        item.fileName,
        ignore,
      );
      await load();
      toast({
        kind: "success",
        title: ignore
          ? `${item.name} ignores the Minecraft version`
          : `${item.name} needs a build for Minecraft ${instance.mcVersion}`,
        body: ignore
          ? "Updates and version changes will take its newest build for this loader, whatever version it was built for. It may stop the game starting."
          : undefined,
      });
    } catch (e) {
      reportError(`Could not change what ${item.name} asks for`, e);
    }
  }

  async function addFiles() {
    const spec = KINDS.find((k) => k.id === kind)!;
    const picked = await openDialog({
      multiple: true,
      filters: [{ name: spec.label, extensions: spec.extensions }],
    });
    const files = Array.isArray(picked) ? picked : picked ? [picked] : [];
    if (files.length === 0) return;

    try {
      const added = await api.addLocalContent(instance.id, kind, files);
      await Promise.all([load(), refreshInstances()]);
      toast({
        kind: "success",
        title: `Added ${added} file${added === 1 ? "" : "s"}`,
      });
    } catch (e) {
      // A pick holding one file that would land on the server's own mod is
      // refused whole, before anything is copied, so the title says what the
      // reason under it cannot: the rest did not go in either.
      reportError("Nothing was added", e);
    }
  }

  async function linkUntracked() {
    setLinking(true);
    try {
      const report = await api.linkUntrackedContent(instance.id);
      await Promise.all([load(), refreshInstances()]);
      toast({
        kind: report.identified > 0 ? "success" : "info",
        title:
          report.identified > 0
            ? `Linked ${report.identified} file${report.identified === 1 ? "" : "s"}`
            : "Nothing new could be identified",
        body:
          report.unknown > 0
            ? `${report.unknown} file${report.unknown === 1 ? " could" : "s could"} not be named, so ${report.unknown === 1 ? "it stays" : "they stay"} untracked.`
            : undefined,
      });
    } catch (e) {
      reportError("Could not identify files", e);
    } finally {
      setLinking(false);
    }
  }

  async function checkUpdates() {
    setChecking(true);
    try {
      const found = await api.checkContentUpdates(instance.id);
      setUpdates(found);
      toast({
        kind: found.length > 0 ? "info" : "success",
        title:
          found.length > 0
            ? `${found.length} update${found.length === 1 ? "" : "s"} available`
            : "Everything is up to date",
      });
    } catch (e) {
      reportError("Update check failed", e);
    } finally {
      setChecking(false);
    }
  }

  async function applyUpdate(candidate: UpdateCandidate) {
    setUpdating(candidate.projectId);
    try {
      await api.applyContentUpdate(
        instance.id,
        candidate.provider,
        candidate.projectId,
        candidate.newVersionId,
      );
      setUpdates((prev) =>
        prev.filter(
          (u) =>
            u.provider !== candidate.provider ||
            u.projectId !== candidate.projectId ||
            u.kind !== candidate.kind,
        ),
      );
      await Promise.all([load(), refreshInstances()]);
      toast({
        kind: "success",
        title: `Updated ${candidate.name}`,
        body: `${candidate.currentVersion} → ${candidate.newVersionNumber}`,
      });
    } catch (e) {
      reportError(`Could not update ${candidate.name}`, e);
    } finally {
      setUpdating(null);
    }
  }

  async function updateAll() {
    for (const candidate of [...updates]) {
      await applyUpdate(candidate);
    }
  }

  const trackedCount = items?.filter((i) => i.projectId).length ?? 0;
  const untrackedCount = items?.filter((i) => i.fromDisk).length ?? 0;
  /** What the primary button offers to add, and the folder's name in prose. */
  const noun = KINDS.find((k) => k.id === kind)!.label.toLowerCase();
  // A filter over nothing can only ever say "nothing matches", which turns an
  // empty folder into a result — so it is off until there is something to
  // filter, including while the folder is still being read. The same rule the
  // Worlds, Logs and dependency-graph tabs keep.
  const canFilter = (items?.length ?? 0) > 0;

  // The one row that answers Tab. Checked against the list rather than
  // trusted, because `keyed` outlives the folder and the filter it was set in.
  const rowStop = shown?.some((i) => i.fileName === keyed)
    ? keyed
    : shown?.[0]?.fileName;

  return (
    <div className={styles.wrap}>
      <div className={styles.toolbar}>
        {/* A tab set nested inside the screen's own: this strip swaps the list
            below it, which is the act `InstanceView.module.css` describes, and
            it was drawn as that act's shape while saying nothing at all. The
            role goes straight on the strip because it holds the four kinds and
            nothing else — the tools beside it are a sibling of this element,
            not a child, so there is no stray button to wrap away from. One tab
            stop for the strip, arrows between the kinds from there. */}
        <div
          className={styles.kinds}
          role="tablist"
          aria-label="Content folders"
          onKeyDown={(e) => {
            const next = rovingKey(e, KIND_IDS);
            if (next) setKind(next);
          }}
        >
          {KINDS.map((k) => (
            <button
              key={k.id}
              id={`instance-content-tab-${k.id}`}
              className={clsx(styles.kindTab, kind === k.id && styles.kindActive)}
              onClick={() => setKind(k.id)}
              role="tab"
              aria-selected={kind === k.id}
              aria-controls="instance-content-panel"
              tabIndex={kind === k.id ? 0 : -1}
              data-roving={k.id}
            >
              {k.label}
            </button>
          ))}
        </div>

        {/* The tools, in the order they are reached for. The filter is the one
            that gives way: it takes whatever room the rest leave, which is why
            this row is one row at every width rather than two at most of them.
            Three things used to sit between it and the primary action, each
            the same size and weight as the thing people actually came here to
            press. */}
        <div className={styles.tools}>
          <div className={clsx(styles.searchWrap, styles.searchGrow)}>
            <Search size={14} className={styles.searchIcon} />
            <input
              ref={filterBox}
              className={clsx("input", styles.search, canFilter && styles.searchHinted)}
              placeholder="Filter"
              value={filter}
              disabled={!canFilter}
              onChange={(e) => setFilter(e.target.value)}
            />
            {/* The binding, printed in the box it reaches, and only while the
                box can be reached — a key printed on something switched off is
                a promise nothing keeps. The sidebar is where a screen's
                shortcut is discoverable; an action's only home is the thing it
                acts on. */}
            {canFilter && (
              <kbd className={styles.searchKey}>{actionHint("focusFilter")}</kbd>
            )}
          </div>
          {/* The one occasional control still in the open, because it is not
              really a command but a count: files are sitting here that nothing
              can update, and this number is the only at-a-glance signal that
              anything is waiting. It appears only when there are some. Below
              the width the pane narrows at it keeps the icon and the number
              and drops the word. */}
          {untrackedCount > 0 && (
            <Tooltip text="Match untracked files against Modrinth and CurseForge by file hash">
              <button
                className={clsx("btn", styles.toolCollapse, styles.toolCounted)}
                onClick={linkUntracked}
                disabled={linking}
                aria-label={`Identify ${untrackedCount} untracked file${untrackedCount === 1 ? "" : "s"}`}
              >
                {linking ? (
                  <Loader2 size={14} className="spin" />
                ) : (
                  <Link2 size={14} />
                )}
                <span className={styles.toolLabel}>Identify</span>
                {/* Outside the label, so the collapse takes the word and
                    leaves the number. */}
                <span className={styles.toolCount}>{untrackedCount}</span>
              </button>
            </Tooltip>
          )}
          <MoreMenu
            open={moreOpen}
            setOpen={setMoreOpen}
            busy={checking ? "Checking for updates" : undefined}
            items={[
              ...(shown && shown.length > 1
                ? [
                    {
                      label: "Select all",
                      note: `${shown.length}`,
                      icon: <ListChecks size={14} />,
                      onSelect: () =>
                        setSelected(new Set(shown.map((i) => i.fileName))),
                    },
                  ]
                : []),
              {
                label: "Add a file from this computer",
                icon: <FilePlus2 size={14} />,
                onSelect: () => void addFiles(),
              },
              {
                label: "Check for updates",
                // The reason, where a disabled button could only have carried
                // it in a `title` — which is to say to a pointer and to
                // nothing else.
                note: trackedCount === 0 ? "nothing tracked here" : undefined,
                icon: <RefreshCw size={14} />,
                disabled: checking || trackedCount === 0,
                onSelect: () => void checkUpdates(),
              },
            ]}
          />
          {/* The one filled button on the screen, and what it does is the
              second reason anybody opened it. The folder's own name rather
              than "Browse": what this press is for is putting something in
              *here*, and the screen it opens is the means. The noun goes at
              the width the pane narrows at — the strip on the left of this
              row has been saying which folder all along. */}
          <button className="btn btn-primary" onClick={onBrowse}>
            <Plus size={15} />
            {/* One flex item, so the gap between the icon and the word is the
                button's own and the space before the noun is a space. */}
            <span>
              Add<span className={styles.addNoun}>{` ${noun}`}</span>
            </span>
          </button>
        </div>
      </div>

      {updates.length > 0 && (
        <div className={styles.updateBanner}>
          <ArrowUpCircle size={15} />
          <span>
            {updates.length} update{updates.length === 1 ? "" : "s"} available
          </span>
          <button
            className="btn btn-sm btn-primary"
            onClick={updateAll}
            disabled={updating !== null}
          >
            {updating !== null && <Loader2 size={12} className="spin" />}
            Update all
          </button>
        </div>
      )}

      {/* Only while something is selected, and every press on it names what it
          will do to how many — a bar that says "Disable" over a selection you
          have scrolled away from is a bar that acts on rows you are not
          looking at. */}
      {picked.length > 0 && (
        <div className={styles.selectBar}>
          <div className={styles.selectText}>
            {bulkBusy && <Loader2 size={13} className="spin" />}
            <span className={styles.selectCount}>{picked.length} selected</span>
            {/* Why "Enable 1" sits beside "Disable 5" under a count of six:
                the two presses act on the part of the selection that is not
                already the way it is being asked to be. Said only when the
                selection is mixed, because that is the only time the numbers
                differ from the count and need accounting for. */}
            {onCount > 0 && offCount > 0 && (
              <span className={styles.selectNote}>
                {onCount} on, {offCount} off
              </span>
            )}
            {/* A selection outlives the filter that made it, so the count says
                so rather than letting a press act on rows nobody can see. */}
            {picked.length > pickedShown && (
              <span className={styles.selectNote}>
                {picked.length - pickedShown} not shown by this filter
              </span>
            )}
          </div>
          <button className="btn btn-sm btn-ghost" onClick={clearSelection}>
            Clear
            <kbd className={styles.selectKey}>
              {actionHint("clearSelection")}
            </kbd>
          </button>
          <button
            className="btn btn-sm"
            onClick={() => void setPickedEnabled(true)}
            disabled={bulkBusy || offCount === 0}
          >
            Enable {offCount}
          </button>
          <button
            className="btn btn-sm"
            onClick={() => void setPickedEnabled(false)}
            disabled={bulkBusy || onCount === 0}
          >
            Disable {onCount}
          </button>
          {/* Set apart from the two presses that can be taken back, and it
              asks before it does anything. */}
          <button
            className={clsx("btn btn-sm btn-danger", styles.selectDelete)}
            onClick={() => setAskingDelete(true)}
            disabled={bulkBusy || deletable.length === 0}
            title={
              deletable.length === 0
                ? "Every selected file is from the server's pack — turn those off instead"
                : undefined
            }
          >
            <Trash2 size={13} />
            Delete {deletable.length}
          </button>
        </div>
      )}

      {/* The one panel the four kinds swap, so each of them points at it and it
          names the kind it is showing.

          Focusable only while it is empty, which is the whole reason — not
          that it scrolls. A panel with rows in it, or with the button the
          true-empty state offers, already answers Tab through what is inside
          it, and a stop on the panel as well is a press that lands nowhere a
          person can see. The two states with nothing to focus are the folder
          still being read, and a filter that has matched none of it. `-1`
          rather than no attribute at all, so a panel holding focus when the
          rows arrive keeps it instead of dropping the keyboard on `<body>`. */}
      <div
        className={styles.list}
        id="instance-content-panel"
        role="tabpanel"
        aria-labelledby={`instance-content-tab-${kind}`}
        tabIndex={shown === null || (shown.length === 0 && canFilter) ? 0 : -1}
      >
        {shown === null ? (
          <div className={styles.skeletons}>
            {Array.from({ length: 5 }, (_, i) => (
              <div key={i} className="skeleton" style={{ height: 54 }} />
            ))}
          </div>
        ) : /* Which empty this is, read off the folder rather than off the
               filter box. A filter left behind from another folder used to
               turn "no shaders yet" into "nothing matches 'sodium'", which is
               an empty folder reported as a search result — and the box is
               switched off in that state now, so the two agree. */
        items && items.length === 0 ? (
          <div className={styles.empty}>
            <Package size={32} className={styles.emptyIcon} />
            <p>
              {instance.loader === "vanilla" && kind === "mod"
                ? "This is a vanilla instance — switch it to a mod loader in Settings to use mods."
                : `No ${noun} yet.`}
            </p>
            <button className="btn btn-primary btn-sm" onClick={onBrowse}>
              <Plus size={14} />
              Add {noun}
            </button>
          </div>
        ) : shown.length === 0 ? (
          <div className={styles.empty}>
            <Package size={32} className={styles.emptyIcon} />
            <p>Nothing matches “{filter}”.</p>
          </div>
        ) : (
          /* A grid, which is the one ARIA shape that fits what a row of this
             list is. A listbox was the tempting answer — the rows really are
             selectable, and `aria-selected` would say so — but an option may
             not hold working controls, and every row here holds three or four
             that act independently of each other and of the selection. A grid
             is two axes with arbitrary widgets in its cells, and its documented
             keyboard is the one implemented in `rowKeys`: the row is the tab
             stop, Up and Down move between rows, Left and Right along one.

             No `aria-colcount`: a row holds one, two or four buttons depending
             on the folder and the file, so the cell that carries them carries
             a different number each time, and a column count that is not the
             same twice is worse than none. The name comes from the kind tab
             above rather than from a second string to keep in step with it. */
          <div
            className={styles.rows}
            role="grid"
            aria-labelledby={`instance-content-tab-${kind}`}
            aria-multiselectable="true"
            onKeyDown={(e) =>
              rowKeys(e, (file, extend) => {
                const row = shown.find((i) => i.fileName === file);
                if (row) pick(row, extend);
              })
            }
            // Where the keyboard is, recorded as it arrives, so the row it is
            // on is the row Tab comes back to. Bound to the grid because focus
            // bubbles in React and the row is not always the element focused —
            // it may be one of the controls inside it.
            onFocus={(e) => {
              const row = (e.target as HTMLElement).closest<HTMLElement>("[data-row]");
              if (row?.dataset.row) setKeyed(row.dataset.row);
            }}
          >
          {shown.map((item) => {
            const update = updateFor(item);
            const isPicked = selected.has(item.fileName);
            return (
              <div
                key={item.fileName}
                className={clsx(styles.row, isPicked && styles.rowPicked)}
                role="row"
                aria-selected={isPicked}
                data-row={item.fileName}
                tabIndex={item.fileName === rowStop ? 0 : -1}
              >
                {/* Selecting, and the only checkbox on the row.

                    There used to be two marks here 20px apart: a square box
                    that decided whether the game loads the file, and — on
                    hover only — a round one that decided whether the next
                    press acts on it, drawn *over* the artwork, so pointing at
                    a row replaced the one thing the row is about. A selection
                    affordance that exists only under a pointer cannot be come
                    across by a keyboard at all, which is why the gesture ended
                    up written out in a tooltip four sentences long.

                    So the leading column is selection, which is what a leading
                    column of checkboxes means everywhere else, and it is
                    always there; the artwork is artwork again; and whether the
                    game loads the file is a button among the row's other
                    buttons, where the rest of the verbs are. Two things fall
                    out of that and both are better than what they replaced:
                    the accent now means one thing on this row — checked here,
                    and the bar down the edge in `.rowPicked` — and Space means
                    the same thing on the row and on the first control in it,
                    where before it selected on the one and toggled the mod on
                    the other. */}
                <label className={styles.selectCell} role="gridcell">
                  <input
                    type="checkbox"
                    checked={isPicked}
                    // Shift off the event the browser raised, so the gesture
                    // is the same from the pointer and from Space.
                    onChange={(e) => {
                      const raised = e.nativeEvent;
                      pick(item, raised instanceof MouseEvent && raised.shiftKey);
                    }}
                    aria-label={`Select ${item.name}`}
                    tabIndex={-1}
                  />
                </label>

                <div className={styles.itemIcon} role="gridcell">
                  {item.iconUrl ? (
                    <img src={item.iconUrl} alt="" loading="lazy" />
                  ) : (
                    <Package size={16} />
                  )}
                </div>

                <div className={styles.itemText} role="gridcell">
                  <div className={styles.itemTitle}>
                    <span className={styles.itemName}>{item.name}</span>
                    {/* First in the line, because it is the most important
                        thing true of this row — and a word, where the row used
                        to say it only by being drawn at half strength. That
                        read as "loading" or "unavailable" as readily as "off",
                        and it was unmeasurable besides: an opacity composites a
                        colour that then appears in no stylesheet for
                        `contrast.test.mjs` to pair off, and the two lines here
                        came out at 3.49:1 and 2.14:1 in the light theme against
                        an enabled row's 16.9:1 and 5.4:1. WCAG 1.4.3 exempts an
                        inactive component, which this is not: the toggle beside
                        it and the delete button at the end both work. The mod
                        is off, not the row. */}
                    {/* Each of these is one word whose meaning is the sentence
                        attached to it, and until now that sentence was a
                        `title` — which is to say it was there for a mouse and
                        for nothing else. "untracked" is the plainest case: the
                        word is jargon, and what it means existed only in a
                        native tooltip that never opens on focus and never on
                        touch.

                        So each carries a real tooltip and a `tabIndex` of -1,
                        which puts it in its row's arrow walk and in no tab
                        order at all. That is the trade and it is only this
                        cheap because of the grid above: as four more *tab*
                        stops a row these would have undone the whole of what
                        the grid is for, but as four more steps in a walk that
                        wraps they cost nothing at either end of the row — the
                        buttons are still one press left of the row, the
                        checkbox one press right of it, and the badges sit in
                        the middle where the least-pressing information in the
                        row belongs. The provider badge below takes neither,
                        because "Modrinth" is not a word that needs a sentence
                        and a focus stop that explains nothing is dead weight. */}
                    {!item.enabled && (
                      <Tooltip text="Still here, and the loader skips it.">
                        <span className="badge" tabIndex={-1}>
                          disabled
                        </span>
                      </Tooltip>
                    )}
                    {item.dependency && (
                      <Tooltip text="Installed as a dependency">
                        <span className="badge" tabIndex={-1}>
                          dependency
                        </span>
                      </Tooltip>
                    )}
                    {item.fromDisk && (
                      <Tooltip text="Added manually — not tracked for updates">
                        <span className="badge" tabIndex={-1}>
                          untracked
                        </span>
                      </Tooltip>
                    )}
                    {item.origin === "server" && (
                      <Tooltip text="From the server's pack. You can turn it off, but the server decides what is installed.">
                        <span className="badge" tabIndex={-1}>
                          server
                        </span>
                      </Tooltip>
                    )}
                    {item.origin === "server" && item.sided?.side === "server" && (
                      <Tooltip text="Only the server needs this. Turn all of these on or off in Settings, under Server.">
                        <span className="badge" tabIndex={-1}>
                          server-side
                        </span>
                      </Tooltip>
                    )}
                    {item.ignoreGameVersion && (
                      <Tooltip text="Its Minecraft version is ignored. Geode takes its newest build for this instance’s loader, whatever Minecraft version that build was made for, instead of refusing to find one.">
                        <span className="badge" tabIndex={-1}>
                          any version
                        </span>
                      </Tooltip>
                    )}
                    {/* Spelled out, the way Browse spells it. "MR" and
                        "Modrinth" are the same provider two screens apart,
                        and the short form saved a few pixels on a row that
                        has them. */}
                    {item.provider && (
                      <span className={clsx("badge", `badge-${item.provider}`)}>
                        {providerLabel(item.provider)}
                      </span>
                    )}
                  </div>
                  <div className={styles.itemMeta}>
                    {item.version && <span>{item.version}</span>}
                    <span className={styles.fileName}>
                      {shownName(item.fileName)}
                    </span>
                    <span>{fileSize(item.size)}</span>
                  </div>
                </div>

                {/* The row's buttons, as one cell. How many there are depends
                    on the folder and on the file, so one cell each would give
                    the grid a column count that is never the same twice; the
                    arrows walk the buttons rather than the cells, so nothing
                    is lost by holding them together. */}
                <div className={styles.actions} role="gridcell">
                  {update && (
                    <button
                      className="btn btn-sm btn-primary"
                      onClick={() => applyUpdate(update)}
                      disabled={updating !== null}
                      title={`Update to ${update.newVersionNumber}`}
                      tabIndex={-1}
                    >
                      {updating === update.projectId ? (
                        <Loader2 size={12} className="spin" />
                      ) : (
                        <ArrowUpCircle size={13} />
                      )}
                      Update
                    </button>
                  )}

                  {/* Whether the game loads this file, which used to be the
                      checkbox at the head of the row — see the selection
                      column above for why it moved. The glyph carries the
                      state rather than a colour: a row that is off shows the
                      press that puts it back. */}
                  <Tooltip text={item.enabled ? "Turn off" : "Turn on"}>
                    <button
                      className="btn btn-ghost btn-icon btn-sm"
                      onClick={() => toggle(item)}
                      aria-label={`${item.enabled ? "Disable" : "Enable"} ${item.name}`}
                      tabIndex={-1}
                    >
                      {item.enabled ? (
                        <Power size={14} />
                      ) : (
                        <PowerOff size={14} />
                      )}
                    </button>
                  </Tooltip>

                  {/* Offered only where it could mean anything: a mod, from a
                      platform that can be asked for another build, and not one
                      the server owns.

                      The padlock used to be open in both states, which said
                      the opposite of the truth half the time: not ignoring the
                      version is the *locked* state, since a build for this
                      Minecraft version is what Geode insists on. Shut when it
                      insists, open when it has been told not to, and the
                      colour is the second thing it says rather than the only
                      one. */}
                  {kind === "mod" &&
                    item.provider &&
                    item.origin !== "server" && (
                      <Tooltip
                        text={
                          item.ignoreGameVersion
                            ? `Require a build for Minecraft ${instance.mcVersion} again.`
                            : "Ignore the Minecraft version. Its newest build for this loader is used instead, which may stop the game starting."
                        }
                      >
                        <button
                          className={clsx(
                            "btn btn-ghost btn-icon btn-sm",
                            item.ignoreGameVersion && styles.anyVersion,
                          )}
                          onClick={() =>
                            void ignoreVersion(item, !item.ignoreGameVersion)
                          }
                          aria-label={
                            item.ignoreGameVersion
                              ? `Require a build for Minecraft ${instance.mcVersion} for ${item.name}`
                              : `Ignore the Minecraft version for ${item.name}`
                          }
                          aria-pressed={item.ignoreGameVersion ?? false}
                          tabIndex={-1}
                        >
                          {item.ignoreGameVersion ? (
                            <Unlock size={14} />
                          ) : (
                            <Lock size={14} />
                          )}
                        </button>
                      </Tooltip>
                    )}

                  {/* The server's own mods stay. Offering a button that always
                      fails is worse than not offering one, so the row explains
                      itself instead — turning it off is right there.

                      Both tooltips, and a `title` as well for the one state
                      the bubble cannot reach: a disabled control receives
                      neither a pointer nor focus, so the platform's own is all
                      that is left to say why this press is not on offer. */}
                  <Tooltip
                    text={
                      item.origin === "server"
                        ? "From the server's pack — turn it off instead"
                        : "Delete"
                    }
                  >
                    <button
                      className="btn btn-ghost btn-icon btn-sm"
                      onClick={() => remove(item)}
                      disabled={item.origin === "server"}
                      title={
                        item.origin === "server"
                          ? "From the server's pack — turn it off instead"
                          : undefined
                      }
                      aria-label={`Delete ${item.name}`}
                      tabIndex={-1}
                    >
                      <Trash2 size={14} />
                    </button>
                  </Tooltip>
                </div>
              </div>
            );
          })}
          </div>
        )}
      </div>

      {/* A row's own Delete button asks nothing — one press, one file, and the
          name is right under the cursor. A press that takes a dozen at once is
          not that, so this names every one of them: a count is what was
          clicked, the names are what is about to go, and a selection made
          three filters ago is the easy way to delete something you had
          forgotten was in it. */}
      {askingDelete && (
        <Modal
          title={`Delete ${deletable.length} file${deletable.length === 1 ? "" : "s"}?`}
          subtitle={`Removed from this instance's ${noun} folder. It cannot be undone.`}
          onClose={() => setAskingDelete(false)}
          width={480}
          footer={
            <>
              <button className="btn" onClick={() => setAskingDelete(false)}>
                Cancel
              </button>
              <button
                className="btn btn-danger"
                onClick={() => void deletePicked()}
                disabled={bulkBusy}
              >
                {bulkBusy && <Loader2 size={14} className="spin" />}
                Delete permanently
              </button>
            </>
          }
        >
          <ul className={styles.confirmList}>
            {deletable.map((item) => (
              <li key={item.fileName}>
                <span className={styles.confirmName}>{item.name}</span>
                <span className={styles.fileName}>
                  {shownName(item.fileName)}
                </span>
              </li>
            ))}
          </ul>
          {picked.length > deletable.length && (
            <p className={styles.confirmNote}>
              {picked.length - deletable.length === 1
                ? "One more file in the selection came from the server's pack and stays — turn it off instead if you do not want it."
                : `${picked.length - deletable.length} more files in the selection came from the server's pack and stay — turn those off instead if you do not want them.`}
            </p>
          )}
        </Modal>
      )}
    </div>
  );
}

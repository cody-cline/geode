import { useCallback, useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import { Loader2, Package, Search, Share2 } from "lucide-react";

import { api } from "../../lib/api";
import { useStore } from "../../lib/store";
import type { ContentItem, Instance } from "../../lib/types";

import styles from "./GraphTab.module.css";

/* Geometry ---------------------------------------------------------------
 *
 * Every number here is fixed, so a card's drawing is a pure function of the
 * mods in it: no measuring, no resize observer, and the same folder produces
 * the same picture on any machine at any window width.
 */
const NODE_H = 44;
const W1 = 242; // interior width of a one-column card
const W2 = 534; // ... and of a two-column one
const ROW_GAP = 50; // vertical space between stacked nodes
const STANDOFF = 5; // gap between a node's edge and the line touching it

/**
 * Mod ids left out of the drawing.
 *
 * Fabric API is required by very nearly every Fabric mod, so drawing it makes
 * one node every arrow points at and tells you nothing you did not already
 * know. `fabric` is its legacy alias and `fabric_api` is what the Forgified
 * build calls itself on NeoForge — the same library, three names.
 */
const UBIQUITOUS = new Set(["fabric_api", "fabric"]);

/** Loaders differ on punctuation for the same id; the id itself does not. */
const idKey = (id: string) => id.toLowerCase().replace(/-/g, "_");

/** Grid columns a card of this interior width needs. */
const columns = (width: number) => Math.max(1, Math.ceil((width + 48) / 292));

type Point = { x: number; y: number };
type Rect = { x: number; y: number; w: number; h: number };

type Placed = { item: ContentItem; rect: Rect };

/** A straight run between two nodes, or the one orthogonal exception. */
type Edge =
  | { kind: "line"; from: string; to: string; a: Point; b: Point }
  | { kind: "skip"; from: string; to: string; segs: [string, number][] };

type Card = {
  id: string;
  shape: "pair" | "chain" | "fan" | "hub" | "tangle" | "loose";
  /** Grid columns to occupy; "full" is the run of loose mods. */
  span: number | "full";
  nodes: Placed[];
  edges: Edge[];
  /** Shown as chips rather than as drawn nodes. */
  chips: ContentItem[];
  heading?: string;
  note?: string;
  width: number;
  height: number;
};

/**
 * Drop the ubiquitous libraries and every edge touching them.
 *
 * A view decision, not a claim the dependency is absent: the edges stay in the
 * data, still counted, still what says a library has nothing needing it.
 */
function omitUbiquitous(items: ContentItem[]): {
  items: ContentItem[];
  omitted: ContentItem[];
} {
  const omitted = items.filter((i) =>
    i.modIds.some((id) => UBIQUITOUS.has(idKey(id))),
  );
  if (omitted.length === 0) return { items, omitted };

  const gone = new Set(omitted.map((i) => i.fileName));
  return {
    items: items
      .filter((i) => !gone.has(i.fileName))
      .map((i) => ({
        ...i,
        requires: i.requires.filter((f) => !gone.has(f)),
        requiredBy: i.requiredBy.filter((f) => !gone.has(f)),
      })),
    omitted,
  };
}

/** Split the linked mods into the separate little graphs they form. */
function components(
  linked: ContentItem[],
  byFile: Map<string, ContentItem>,
): ContentItem[][] {
  const seen = new Set<string>();
  const groups: ContentItem[][] = [];

  for (const start of linked) {
    if (seen.has(start.fileName)) continue;
    const files: string[] = [];
    // Walked as a queue, in sorted order at every step. Popping a stack over
    // the arrays as they arrive would make the grouping depend on the order
    // the folder happened to be read in — the kind of thing that quietly
    // breaks "the same folder draws the same picture".
    const queue = [start.fileName];
    seen.add(start.fileName);
    while (queue.length > 0) {
      const file = queue.shift()!;
      files.push(file);
      const item = byFile.get(file);
      if (!item) continue;
      for (const step of [...item.requires, ...item.requiredBy].sort()) {
        if (byFile.has(step) && !seen.has(step)) {
          seen.add(step);
          queue.push(step);
        }
      }
    }
    groups.push(
      files
        .map((f) => byFile.get(f)!)
        .sort((a, b) => a.fileName.localeCompare(b.fileName)),
    );
  }
  return groups;
}

/** Longest path from a source, over one component only. */
function ranks(group: ContentItem[]): Map<string, number> {
  const inside = new Set(group.map((i) => i.fileName));
  const depth = new Map<string, number>();

  const walk = (file: string, seen: Set<string>): number => {
    const cached = depth.get(file);
    if (cached !== undefined) return cached;
    if (seen.has(file)) return 0;
    const item = group.find((i) => i.fileName === file);
    const parents = (item?.requiredBy ?? []).filter((p) => inside.has(p));
    if (parents.length === 0) {
      depth.set(file, 0);
      return 0;
    }
    seen.add(file);
    let deepest = 0;
    for (const p of parents) deepest = Math.max(deepest, walk(p, seen) + 1);
    seen.delete(file);
    depth.set(file, deepest);
    return deepest;
  };

  for (const item of group) walk(item.fileName, new Set());
  return depth;
}

/**
 * A straight run down the middle between two stacked nodes.
 *
 * The gap is deliberately longer than the node is tall. Closer than that and
 * the connector reads as a seam between two boxes rather than as a span
 * carrying one to the other.
 */
function stacked(x: number, from: Placed, to: Placed): Edge {
  return {
    kind: "line",
    from: from.item.fileName,
    to: to.item.fileName,
    a: { x, y: from.rect.y + from.rect.h + STANDOFF },
    b: { x, y: to.rect.y - STANDOFF },
  };
}

/**
 * Draw one component, choosing the shape from what the component actually is.
 *
 * This is the whole idea. A mods folder contains about four shapes — a pair, a
 * chain, a library with several dependents, and the occasional tangle — and
 * each has one obviously right drawing. A general layout engine has to
 * rediscover that from scratch every time and settles for a compromise that
 * suits all four and fits none, which is what made the earlier attempts read
 * as a grid with the data pushed into it.
 */
function drawCard(group: ContentItem[], index: number): Card {
  const inside = new Set(group.map((i) => i.fileName));
  const id = `c${index}:${group[0].fileName}`;
  const needs = (i: ContentItem) => i.requires.filter((f) => inside.has(f));
  const neededBy = (i: ContentItem) => i.requiredBy.filter((f) => inside.has(f));
  const byName = (a: ContentItem, b: ContentItem) =>
    a.name.localeCompare(b.name) || a.fileName.localeCompare(b.fileName);

  if (group.length === 2) {
    const [dependent, library] =
      needs(group[0]).length > 0 ? [group[0], group[1]] : [group[1], group[0]];
    const nodes: Placed[] = [
      { item: dependent, rect: { x: 0, y: 0, w: W1, h: NODE_H } },
      { item: library, rect: { x: 0, y: NODE_H + ROW_GAP, w: W1, h: NODE_H } },
    ];
    return {
      id,
      shape: "pair",
      span: 1,
      nodes,
      edges: [stacked(W1 / 2, nodes[0], nodes[1])],
      chips: [],
      width: W1,
      height: NODE_H * 2 + ROW_GAP,
    };
  }

  // A library that everything else in the component points straight at.
  const hub = group.find(
    (i) => needs(i).length === 0 && neededBy(i).length === group.length - 1,
  );
  const isFan =
    !!hub &&
    group.every(
      (i) => i === hub || (needs(i).length === 1 && neededBy(i).length === 0),
    );

  if (hub && isFan) {
    const leaves = group.filter((i) => i !== hub).sort(byName);
    const k = leaves.length;

    // Past six spokes a star repeats one bit of information per ray and eats
    // the card. Enclosure says the same thing in a fraction of the space.
    if (k >= 7) {
      return {
        id,
        shape: "hub",
        span: k >= 13 ? "full" : columns(W2),
        nodes: [{ item: hub, rect: { x: 0, y: 0, w: W2, h: NODE_H } }],
        edges: [],
        chips: leaves,
        note: `required by ${k} mods`,
        width: W2,
        height: NODE_H,
      };
    }

    const leafGap = 14;
    const blockH = k * NODE_H + (k - 1) * leafGap;
    const hubH = Math.min(120, Math.max(NODE_H, 22 * k));
    const hubY = (blockH - hubH) / 2;
    const nodes: Placed[] = leaves.map((item, i) => ({
      item,
      rect: { x: 0, y: i * (NODE_H + leafGap), w: W1, h: NODE_H },
    }));
    nodes.push({ item: hub, rect: { x: 292, y: hubY, w: W1, h: hubH } });

    // Spread the landing points down the hub's face. Aimed at its centre they
    // would pile into one spot — the same stacking that made arrowheads
    // unreadable before, and what stops a fan looking like a fan.
    const edges: Edge[] = leaves.map((leaf, i) => ({
      kind: "line" as const,
      from: leaf.fileName,
      to: hub.fileName,
      a: { x: W1 + STANDOFF, y: i * (NODE_H + leafGap) + NODE_H / 2 },
      b: {
        x: 292 - STANDOFF,
        y: hubY + hubH * 0.15 + ((i + 0.5) / k) * hubH * 0.7,
      },
    }));

    return {
      id,
      shape: "fan",
      span: columns(W2),
      nodes,
      edges,
      chips: [],
      width: W2,
      height: blockH,
    };
  }

  // Anything else: rank by depth, and let a rank hold several mods side by
  // side. One node per row turns a library with six dependents into a strip a
  // thousand pixels tall, and loses the fact that those six are siblings.
  const depth = ranks(group);
  const byDepth = new Map<number, ContentItem[]>();
  for (const item of group) {
    const d = depth.get(item.fileName) ?? 0;
    byDepth.set(d, [...(byDepth.get(d) ?? []), item]);
  }

  // Order each rank so its edges to the next one do not cross.
  //
  // A rank here holds a handful of mods, so the orderings can simply be
  // enumerated and the best one kept. The usual barycentre heuristic exists
  // because ranks are normally too big for that; at this size it is guesswork
  // where an answer is available. Alphabetical, meanwhile, is close to worst
  // case in a drawing — three mods each needing one library below will cross
  // all three lines whenever the two ranks happen to sort differently.
  const levels = [...byDepth.keys()].sort((a, b) => a - b);
  for (const d of levels) byDepth.get(d)!.sort(byName);

  const crossings = (upper: ContentItem[], lower: ContentItem[]) => {
    const at = new Map(lower.map((it, i) => [it.fileName, i]));
    const links: [number, number][] = [];
    upper.forEach((u, ui) => {
      for (const f of [...needs(u), ...neededBy(u)]) {
        const li = at.get(f);
        if (li !== undefined) links.push([ui, li]);
      }
    });
    let n = 0;
    for (let i = 0; i < links.length; i++) {
      for (let j = i + 1; j < links.length; j++) {
        if ((links[i][0] - links[j][0]) * (links[i][1] - links[j][1]) < 0) n++;
      }
    }
    return n;
  };

  const arrangements = (list: ContentItem[]): ContentItem[][] => {
    if (list.length <= 1) return [list];
    const out: ContentItem[][] = [];
    list.forEach((item, i) => {
      const rest = [...list.slice(0, i), ...list.slice(i + 1)];
      for (const tail of arrangements(rest)) out.push([item, ...tail]);
    });
    return out;
  };

  const best = (rank: ContentItem[], against: ContentItem[], above: boolean) => {
    // 720 orderings at six wide; past that the enumeration stops being free
    // and the alphabetical order it already has is left alone.
    if (rank.length < 2 || rank.length > 6 || against.length === 0) return rank;
    let winner = rank;
    let fewest = Infinity;
    for (const candidate of arrangements(rank)) {
      const n = above
        ? crossings(against, candidate)
        : crossings(candidate, against);
      if (n < fewest) {
        fewest = n;
        winner = candidate;
      }
    }
    return winner;
  };

  for (let sweep = 0; sweep < 2; sweep++) {
    for (let i = 1; i < levels.length; i++) {
      const d = levels[i];
      byDepth.set(d, best(byDepth.get(d)!, byDepth.get(levels[i - 1])!, true));
    }
    for (let i = levels.length - 2; i >= 0; i--) {
      const d = levels[i];
      byDepth.set(d, best(byDepth.get(d)!, byDepth.get(levels[i + 1])!, false));
    }
  }

  const PER_ROW = 3;
  const rows: { items: ContentItem[]; depth: number }[] = [];
  for (const d of levels) {
    const rank = byDepth.get(d)!;
    for (let i = 0; i < rank.length; i += PER_ROW) {
      rows.push({ items: rank.slice(i, i + PER_ROW), depth: d });
    }
  }

  const isChain = rows.every((r) => r.items.length === 1);
  const skips = group.some((item) =>
    needs(item).some(
      (f) => (depth.get(f) ?? 0) - (depth.get(item.fileName) ?? 0) > 1,
    ),
  );
  const gutter = skips ? 30 : 0;
  const widest = Math.max(...rows.map((r) => r.items.length));
  const inner = widest * W1 + (widest - 1) * 20;
  const width = gutter + inner;

  const nodes: Placed[] = [];
  rows.forEach((row, r) => {
    const rowW = row.items.length * W1 + (row.items.length - 1) * 20;
    const left = gutter + (inner - rowW) / 2;
    row.items.forEach((item, i) => {
      nodes.push({
        item,
        rect: { x: left + i * (W1 + 20), y: r * (NODE_H + ROW_GAP), w: W1, h: NODE_H },
      });
    });
  });
  const at = new Map(nodes.map((n) => [n.item.fileName, n]));

  const edges: Edge[] = [];
  for (const item of group) {
    for (const need of needs(item)) {
      const from = at.get(item.fileName);
      const to = at.get(need);
      if (!from || !to) continue;
      const span = (depth.get(need) ?? 0) - (depth.get(item.fileName) ?? 0);
      if (span === 1) {
        // Straight, and diagonal when the two sit in different columns. A
        // sloped line between two boxes is still the shortest path between
        // them, which is the whole point.
        edges.push({
          kind: "line",
          from: item.fileName,
          to: need,
          a: { x: from.rect.x + W1 / 2, y: from.rect.y + NODE_H + STANDOFF },
          b: { x: to.rect.x + W1 / 2, y: to.rect.y - STANDOFF },
        });
        continue;
      }
      // The one place a straight line will not do: a dependency that skips a
      // rank would otherwise cut through whatever sits between. A quarter
      // circle inscribed at a right angle stays inside that corner's own box,
      // so unlike a spline it cannot bulge away from the path it rounds.
      const lane = 12;
      const r = 8;
      const y1 = from.rect.y + NODE_H / 2;
      const y2 = to.rect.y + NODE_H / 2;
      edges.push({
        kind: "skip",
        from: item.fileName,
        to: need,
        segs: [
          [`M${from.rect.x},${y1} H${lane + r} A${r},${r} 0 0 0 ${lane},${y1 + r}`, 2.6],
          [`M${lane},${y1 + r} V${y2 - r}`, 1.9],
          [`M${lane},${y2 - r} A${r},${r} 0 0 0 ${lane + r},${y2} H${to.rect.x}`, 1.3],
        ],
      });
    }
  }

  return {
    id,
    shape: isChain ? "chain" : "tangle",
    span: columns(width),
    nodes,
    edges,
    chips: [],
    width,
    height: rows.length * NODE_H + (rows.length - 1) * ROW_GAP,
  };
}

/**
 * A tapered wedge from dependent to library, rather than a stroke plus an
 * arrowhead.
 *
 * Holten and van Wijk found tapered links beat arrowheads for reading
 * direction, and it dodges a problem this data has: several edges arriving at
 * one library stack their heads on the same few pixels.
 */
function taper(a: Point, b: Point): string {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const len = Math.hypot(dx, dy) || 1;
  const nx = -dy / len;
  const ny = dx / len;
  // Narrow. A wedge sized for a 26px hop reads as a slab when the same shape
  // is stretched over a 200px diagonal.
  return (
    `M${a.x + nx * 1.6},${a.y + ny * 1.6}` +
    ` L${b.x + nx * 0.5},${b.y + ny * 0.5}` +
    ` L${b.x - nx * 0.5},${b.y - ny * 0.5}` +
    ` L${a.x - nx * 1.6},${a.y - ny * 1.6} Z`
  );
}

function build(all: ContentItem[]) {
  // Sorted before anything else looks at it, so nothing downstream can inherit
  // the order the folder happened to be read in.
  const sorted = [...all].sort((a, b) => a.fileName.localeCompare(b.fileName));
  const { items, omitted } = omitUbiquitous(sorted);
  const byFile = new Map(items.map((i) => [i.fileName, i]));

  const linked = items.filter(
    (i) => i.requires.length > 0 || i.requiredBy.length > 0,
  );
  const loose = items.filter(
    (i) => i.requires.length === 0 && i.requiredBy.length === 0,
  );

  const rank = { tangle: 0, hub: 1, fan: 2, chain: 3, pair: 4, loose: 5 };
  const cards = components(linked, byFile)
    .map(drawCard)
    .sort(
      (a, b) =>
        rank[a.shape] - rank[b.shape] ||
        b.nodes.length + b.chips.length - (a.nodes.length + a.chips.length) ||
        a.id.localeCompare(b.id),
    );

  if (loose.length > 0) {
    cards.push({
      id: "loose",
      shape: "loose",
      span: "full",
      nodes: [],
      edges: [],
      chips: [...loose].sort((a, b) => a.name.localeCompare(b.name)),
      heading: `Stands alone · ${loose.length}`,
      note: loose.some((i) => !i.declaresDeps)
        ? "dashed ones did not state their dependencies"
        : undefined,
      width: 0,
      height: 0,
    });
  }

  return { cards, omitted, linked: linked.length, total: items.length };
}

export default function GraphTab({ instance }: { instance: Instance }) {
  const reportError = useStore((s) => s.reportError);

  const [items, setItems] = useState<ContentItem[] | null>(null);
  const [focus, setFocus] = useState<string | null>(null);
  const [query, setQuery] = useState("");

  useEffect(() => {
    let cancelled = false;
    api
      .listContent(instance.id, "mod")
      .then((found) => !cancelled && setItems(found))
      .catch((e) => {
        if (!cancelled) {
          reportError("Could not read the mods folder", e);
          setItems([]);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [instance.id, reportError]);

  const { cards, omitted, linked, total } = useMemo(
    () => build(items ?? []),
    [items],
  );

  const matches = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return null;
    return new Set(
      (items ?? [])
        .filter((i) => i.name.toLowerCase().includes(q))
        .map((i) => i.fileName),
    );
  }, [items, query]);

  const toggle = (item: ContentItem) =>
    setFocus((f) => (f === item.fileName ? null : item.fileName));

  const label = (item: ContentItem) =>
    `${item.name}${item.version ? ` · ${item.version}` : ""}`;

  const nodeClass = useCallback(
    (item: ContentItem) =>
      clsx(
        styles.node,
        item.dependency ? styles.nodeDep : styles.nodeRoot,
        item.unused && styles.nodeUnused,
        !item.enabled && styles.nodeOff,
        focus === item.fileName && styles.nodeFocus,
        matches?.has(item.fileName) && styles.nodeMatch,
      ),
    [focus, matches],
  );

  const chipClass = useCallback(
    (item: ContentItem) =>
      clsx(
        styles.chip,
        !item.declaresDeps && styles.chipUnknown,
        !item.enabled && styles.nodeOff,
        focus === item.fileName && styles.chipFocus,
        matches?.has(item.fileName) && styles.chipMatch,
      ),
    [focus, matches],
  );

  return (
    <div className={styles.wrap}>
      <div className={styles.toolbar}>
        <div className={styles.searchWrap}>
          <Search size={14} className={styles.searchIcon} />
          <input
            className={clsx("input", styles.search)}
            placeholder="Find a mod"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        {items !== null && total > 0 && (
          <p className={styles.summary}>
            {linked} of {total} mods are connected to something.
            {omitted.length > 0 && (
              <span className={styles.omitted}>
                {omitted.map((i) => i.name).join(", ")} hidden — nearly
                everything needs it
              </span>
            )}
          </p>
        )}
      </div>

      {items === null ? (
        <div className={styles.centre}>
          <Loader2 size={20} className="spin" />
        </div>
      ) : cards.length === 0 ? (
        <div className={styles.centre}>
          <Share2 size={28} className={styles.emptyIcon} />
          <p>No mods installed yet.</p>
        </div>
      ) : (
        <div className={styles.deck}>
          {cards.map((card) => (
            <section
              key={card.id}
              className={clsx(styles.card, card.span === "full" && styles.full)}
              style={
                typeof card.span === "number" && card.span > 1
                  ? { gridColumn: `span ${card.span}`, width: "auto" }
                  : undefined
              }
            >
              {card.heading && (
                <header className={styles.cardHead}>
                  <span className={styles.cardTitle}>{card.heading}</span>
                  {card.note && <em className={styles.cardNote}>{card.note}</em>}
                </header>
              )}

              {card.nodes.length > 0 && (
                <div
                  className={styles.plot}
                  style={{ width: card.width, height: card.height }}
                >
                  <svg
                    className={styles.wires}
                    width={card.width}
                    height={card.height}
                  >
                    {card.edges.map((edge) =>
                      edge.kind === "line" ? (
                        <path
                          key={`${edge.from}->${edge.to}`}
                          className={styles.wire}
                          d={taper(edge.a, edge.b)}
                        />
                      ) : (
                        edge.segs.map(([d, w], i) => (
                          <path
                            key={`${edge.from}->${edge.to}:${i}`}
                            className={styles.skip}
                            strokeWidth={w}
                            d={d}
                          />
                        ))
                      ),
                    )}
                  </svg>

                  {card.nodes.map(({ item, rect }) => (
                    <button
                      key={item.fileName}
                      className={nodeClass(item)}
                      style={{
                        left: rect.x,
                        top: rect.y,
                        width: rect.w,
                        height: rect.h,
                      }}
                      onClick={() => toggle(item)}
                      title={label(item)}
                    >
                      <span className={styles.nodeIcon}>
                        {item.iconUrl ? (
                          <img src={item.iconUrl} alt="" loading="lazy" />
                        ) : (
                          <Package size={13} />
                        )}
                      </span>
                      <span className={styles.nodeName}>{item.name}</span>
                    </button>
                  ))}
                </div>
              )}

              {card.shape === "hub" && card.note && (
                <p className={styles.rule}>
                  <span>{card.note}</span>
                </p>
              )}

              {card.chips.length > 0 && (
                <div className={styles.chips}>
                  {card.chips.map((item) => (
                    <button
                      key={item.fileName}
                      className={chipClass(item)}
                      onClick={() => toggle(item)}
                      title={
                        item.declaresDeps
                          ? label(item)
                          : `${label(item)} — this jar did not state its dependencies, so it may have some`
                      }
                    >
                      <span className={styles.chipIcon}>
                        {item.iconUrl ? (
                          <img src={item.iconUrl} alt="" loading="lazy" />
                        ) : (
                          <Package size={11} />
                        )}
                      </span>
                      <span className={styles.chipName}>{item.name}</span>
                    </button>
                  ))}
                </div>
              )}
            </section>
          ))}
        </div>
      )}
    </div>
  );
}

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { Crosshair, Loader2, Minus, Plus, Search, Share2 } from "lucide-react";

import { api } from "../../lib/api";
import { useStore } from "../../lib/store";
import type { ContentItem, Instance } from "../../lib/types";

import styles from "./GraphTab.module.css";

const NODE_W = 168;
const NODE_H = 38;
const GAP_X = 22;
const GAP_Y = 74;
const PAD = 48;
const ROW_MAX = 9;
/** Space between one cluster and the next. */
const CLUSTER_GAP = 64;
/** Width to wrap the row of clusters at. */
const PACK_W = 1240;
/** Breathing room between a cluster's panel and the nodes inside it. */
const CLUSTER_PAD = 20;

/**
 * Mod ids left out of the diagram.
 *
 * Fabric API is required by very nearly every Fabric mod, so drawing it makes
 * one node every arrow points at and tells you nothing you did not already
 * know. Removing it lets the structure that varies between instances actually
 * show. `fabric` is its legacy alias and `fabric_api` is what the Forgified
 * build calls itself on NeoForge — the same library, three names.
 */
const UBIQUITOUS = new Set(["fabric_api", "fabric"]);

/** Loaders differ on punctuation for the same id; the id itself does not. */
const idKey = (id: string) => id.toLowerCase().replace(/-/g, "_");

/**
 * Drop the ubiquitous libraries and every edge touching them.
 *
 * Done here rather than in the backend graph on purpose: the edges are real,
 * and the rest of the app — the fan-in counts, working out that nothing needs a
 * library any more — still depends on them. This is a view choosing what to
 * draw, not a claim that the dependency is not there.
 */
function omitUbiquitous(items: ContentItem[]): {
  items: ContentItem[];
  omitted: ContentItem[];
} {
  const omitted = items.filter((i) => i.modIds.some((id) => UBIQUITOUS.has(idKey(id))));
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

type Node = {
  item: ContentItem;
  x: number;
  y: number;
  /** How deep it sits: 0 is a mod nothing else needs. */
  depth: number;
};

type Layout = {
  nodes: Node[];
  edges: { from: number; to: number }[];
  /** Index into `nodes`, by file name. */
  index: Map<string, number>;
  loose: ContentItem[];
  /** Left out for being in nearly everything's dependency list. */
  omitted: ContentItem[];
  /** Bounding box of each separate little graph, for drawing it as a group. */
  clusters: Cluster[];
  width: number;
  height: number;
};

type Cluster = { x: number; y: number; width: number; height: number };

/**
 * Rank each mod by how far it sits below something that wants it.
 *
 * Depth 0 is a mod nothing else requires — one you picked. Everything else
 * settles one level below its deepest dependent, which sinks the shared
 * libraries to the bottom and makes every edge point the same way down the
 * canvas. Cycles are rare but real, so the walk carries its own stack.
 */
function depths(items: ContentItem[], byFile: Map<string, ContentItem>): Map<string, number> {
  const depth = new Map<string, number>();

  const walk = (file: string, stack: Set<string>): number => {
    const cached = depth.get(file);
    if (cached !== undefined) return cached;
    if (stack.has(file)) return 0;

    const item = byFile.get(file);
    if (!item || item.requiredBy.length === 0) {
      depth.set(file, 0);
      return 0;
    }

    stack.add(file);
    let deepest = 0;
    for (const parent of item.requiredBy) {
      deepest = Math.max(deepest, walk(parent, stack) + 1);
    }
    stack.delete(file);

    depth.set(file, deepest);
    return deepest;
  };

  for (const item of items) walk(item.fileName, new Set());
  return depth;
}

/**
 * Order each row by the average position of the mods above it.
 *
 * This is the barycentre heuristic — the cheap half of proper layered graph
 * drawing. It will not remove every crossing, but a couple of sweeps take out
 * most of them, which is the difference between a diagram and a hairball.
 */
function untangle(rows: string[][], byFile: Map<string, ContentItem>): void {
  const position = new Map<string, number>();
  const record = () =>
    rows.forEach((row) => row.forEach((file, i) => position.set(file, i)));
  record();

  for (let pass = 0; pass < 4; pass++) {
    for (let r = 1; r < rows.length; r++) {
      const anchor = new Map<string, number>();
      for (const file of rows[r]) {
        const parents = byFile.get(file)?.requiredBy ?? [];
        const known = parents
          .map((p) => position.get(p))
          .filter((n): n is number => n !== undefined);
        anchor.set(
          file,
          known.length > 0
            ? known.reduce((a, b) => a + b, 0) / known.length
            : Number.MAX_SAFE_INTEGER,
        );
      }
      rows[r].sort((a, b) => (anchor.get(a) ?? 0) - (anchor.get(b) ?? 0));
    }
    record();
  }
}

/**
 * Split the linked mods into the separate little graphs they actually form.
 *
 * Once Fabric API is out of the picture most instances are not one graph at
 * all — they are a handful of unrelated pairs and triples. Laying those out on
 * shared rows scatters each one across the full width and drags long crossing
 * edges between them. Grouping first is what turns the diagram back into
 * something you can read at a glance.
 */
function clusterFiles(
  linked: ContentItem[],
  byFile: Map<string, ContentItem>,
): string[][] {
  const seen = new Set<string>();
  const groups: string[][] = [];

  for (const start of linked) {
    if (seen.has(start.fileName)) continue;
    const group: string[] = [];
    const queue = [start.fileName];
    seen.add(start.fileName);

    while (queue.length > 0) {
      const file = queue.pop()!;
      group.push(file);
      const item = byFile.get(file);
      if (!item) continue;
      // Undirected on purpose: a cluster is everything that reaches everything
      // else, whichever way the arrows point.
      for (const next of [...item.requires, ...item.requiredBy]) {
        if (byFile.has(next) && !seen.has(next)) {
          seen.add(next);
          queue.push(next);
        }
      }
    }
    groups.push(group);
  }
  return groups;
}

type Placed = {
  nodes: { file: string; x: number; y: number; depth: number }[];
  width: number;
  height: number;
};

/** Lay one cluster out on its own, top-down, from its own origin. */
function layoutCluster(files: string[], byFile: Map<string, ContentItem>): Placed {
  const members = files
    .map((f) => byFile.get(f))
    .filter((i): i is ContentItem => Boolean(i));

  const depth = depths(members, byFile);
  const rows: string[][] = [];
  for (const item of members) {
    const d = depth.get(item.fileName) ?? 0;
    (rows[d] ??= []).push(item.fileName);
  }
  for (const row of rows) row.sort();
  untangle(rows, byFile);

  // A cluster wide enough to need wrapping is rare, but a modpack's core
  // library can have twenty dependents and one line of those is unreadable.
  const lines: { files: string[]; depth: number }[] = [];
  rows.forEach((row, d) => {
    for (let i = 0; i < row.length; i += ROW_MAX) {
      lines.push({ files: row.slice(i, i + ROW_MAX), depth: d });
    }
  });

  const widest = Math.max(1, ...lines.map((l) => l.files.length));
  const width = widest * (NODE_W + GAP_X) - GAP_X;

  const nodes: Placed["nodes"] = [];
  let y = 0;
  lines.forEach((line, i) => {
    // A full gap between layers, a tighter one between wrapped halves of the
    // same layer, so the banding stays legible.
    if (i > 0) y += NODE_H + (line.depth === lines[i - 1].depth ? 12 : GAP_Y);
    const lineWidth = line.files.length * (NODE_W + GAP_X) - GAP_X;
    const left = (width - lineWidth) / 2;
    line.files.forEach((file, col) => {
      nodes.push({ file, x: left + col * (NODE_W + GAP_X), y, depth: line.depth });
    });
  });

  return { nodes, width, height: y + NODE_H };
}

function layout(all: ContentItem[]): Layout {
  const { items, omitted } = omitUbiquitous(all);
  const byFile = new Map(items.map((i) => [i.fileName, i]));

  // A mod with no links either way says nothing about structure, and in a big
  // pack most of them are like that. Left on the canvas they are a field of
  // dots you have to pan past; below it they are a list you can read.
  const linked = items.filter(
    (i) => i.requires.length > 0 || i.requiredBy.length > 0,
  );
  const loose = items.filter(
    (i) => i.requires.length === 0 && i.requiredBy.length === 0,
  );

  const placed = clusterFiles(linked, byFile)
    .map((files) => layoutCluster(files, byFile))
    // Biggest first, so the part with something to say is top-left where you
    // start reading, and the pairs fill in around it.
    .sort((a, b) => b.nodes.length - a.nodes.length || b.width - a.width);

  // Pack the clusters across, wrapping at a width that keeps the whole thing
  // roughly screen-shaped rather than a single long line of them.
  const clusters: Cluster[] = [];
  let cursor = 0;
  let rowTop = 0;
  let rowHeight = 0;
  for (const group of placed) {
    if (cursor > 0 && cursor + group.width > PACK_W) {
      rowTop += rowHeight + CLUSTER_GAP;
      cursor = 0;
      rowHeight = 0;
    }
    clusters.push({ x: cursor, y: rowTop, width: group.width, height: group.height });
    for (const n of group.nodes) {
      n.x += cursor;
      n.y += rowTop;
    }
    cursor += group.width + CLUSTER_GAP;
    rowHeight = Math.max(rowHeight, group.height);
  }

  const nodes: Node[] = [];
  const index = new Map<string, number>();
  for (const group of placed) {
    for (const n of group.nodes) {
      const item = byFile.get(n.file);
      if (!item) continue;
      index.set(n.file, nodes.length);
      nodes.push({ item, x: n.x, y: n.y, depth: n.depth });
    }
  }

  const edges: { from: number; to: number }[] = [];
  for (const node of nodes) {
    const from = index.get(node.item.fileName);
    if (from === undefined) continue;
    for (const need of node.item.requires) {
      const to = index.get(need);
      if (to !== undefined) edges.push({ from, to });
    }
  }

  return {
    nodes,
    edges,
    index,
    loose,
    omitted,
    clusters,
    width: Math.max(1, ...clusters.map((c) => c.x + c.width)),
    height: rowTop + rowHeight,
  };
}

/**
 * What this mod pulls in, and what pulls it in — following each direction on
 * its own.
 *
 * Chasing both directions from every node reached would spread sideways
 * through shared libraries and light up the whole diagram, which is the same
 * as highlighting nothing. Down through `requires` and up through `requiredBy`
 * gives the chain the mod is actually part of.
 */
function related(layout: Layout, file: string): Set<string> {
  const out = new Set<string>([file]);

  const walk = (start: string, next: (item: ContentItem) => string[]) => {
    const queue = [start];
    while (queue.length > 0) {
      const at = layout.index.get(queue.pop()!);
      if (at === undefined) continue;
      for (const step of next(layout.nodes[at].item)) {
        if (layout.index.has(step) && !out.has(step)) {
          out.add(step);
          queue.push(step);
        }
      }
    }
  };

  walk(file, (i) => i.requires);
  walk(file, (i) => i.requiredBy);
  return out;
}

export default function GraphTab({ instance }: { instance: Instance }) {
  const reportError = useStore((s) => s.reportError);

  const [items, setItems] = useState<ContentItem[] | null>(null);
  const [focus, setFocus] = useState<string | null>(null);
  const [hover, setHover] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [view, setView] = useState({ x: 0, y: 0, scale: 1 });

  const frame = useRef<HTMLDivElement>(null);
  const drag = useRef<{ x: number; y: number; ox: number; oy: number } | null>(
    null,
  );

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

  const graph = useMemo(() => layout(items ?? []), [items]);

  // Hovering traces a chain the same way clicking does, so you can follow one
  // without committing to it; a click pins it and opens the detail panel.
  const traced = focus ?? hover;
  const lit = useMemo(
    () => (traced ? related(graph, traced) : null),
    [graph, traced],
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

  const fit = useCallback(() => {
    const box = frame.current?.getBoundingClientRect();
    if (!box || graph.nodes.length === 0) {
      setView({ x: 0, y: 0, scale: 1 });
      return;
    }
    // Never shrink past the point where the labels stop being words. Below
    // that, showing everything at once is worth less than being able to read
    // any of it, and there is a whole canvas to pan.
    const scale = Math.max(
      0.45,
      Math.min(
        1,
        (box.width - PAD * 2) / graph.width,
        (box.height - PAD * 2) / graph.height,
      ),
    );
    // Centre it when it fits; pin to the top when it does not, so the mods you
    // chose are the first thing on screen and you scroll down into the
    // libraries rather than hunting for the start.
    const h = graph.height * scale;
    setView({
      x: (box.width - graph.width * scale) / 2,
      y: h + PAD * 2 < box.height ? (box.height - h) / 2 : PAD,
      scale,
    });
  }, [graph]);

  useEffect(() => {
    if (items) fit();
  }, [items, fit]);

  function onPointerDown(e: React.PointerEvent) {
    if ((e.target as HTMLElement).closest("[data-node]")) return;
    drag.current = { x: e.clientX, y: e.clientY, ox: view.x, oy: view.y };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onPointerMove(e: React.PointerEvent) {
    if (!drag.current) return;
    setView((v) => ({
      ...v,
      x: drag.current!.ox + (e.clientX - drag.current!.x),
      y: drag.current!.oy + (e.clientY - drag.current!.y),
    }));
  }

  function onPointerUp() {
    drag.current = null;
  }

  // Zoom toward the cursor, so the thing you are pointing at stays put.
  function onWheel(e: React.WheelEvent) {
    const box = frame.current?.getBoundingClientRect();
    if (!box) return;
    const px = e.clientX - box.left;
    const py = e.clientY - box.top;
    setView((v) => {
      const scale = Math.min(2.5, Math.max(0.2, v.scale * (e.deltaY < 0 ? 1.12 : 0.89)));
      const k = scale / v.scale;
      return { scale, x: px - (px - v.x) * k, y: py - (py - v.y) * k };
    });
  }

  const step = (factor: number) =>
    setView((v) => ({ ...v, scale: Math.min(2.5, Math.max(0.2, v.scale * factor)) }));

  const shownAt = traced ? graph.index.get(traced) : undefined;
  const shownItem = shownAt !== undefined ? graph.nodes[shownAt].item : undefined;

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
        <div className={styles.legend}>
          <span className={styles.legendItem}>
            <i className={clsx(styles.dot, styles.dotRoot)} /> chosen
          </span>
          <span className={styles.legendItem}>
            <i className={clsx(styles.dot, styles.dotDep)} /> dependency
          </span>
          <span className={styles.legendItem}>
            <i className={clsx(styles.dot, styles.dotLoose)} /> nothing needs it
          </span>
          {graph.omitted.length > 0 && (
            <span
              className={styles.omitted}
              title={`Hidden so the rest of the shape can show: ${graph.omitted
                .map((i) => i.name)
                .join(", ")}`}
            >
              {graph.omitted.map((i) => i.name).join(", ")} hidden
            </span>
          )}
        </div>
        <div className={styles.zoom}>
          <button className="btn btn-icon btn-sm" onClick={() => step(0.8)} title="Zoom out">
            <Minus size={14} />
          </button>
          <span className={styles.scale}>{Math.round(view.scale * 100)}%</span>
          <button className="btn btn-icon btn-sm" onClick={() => step(1.25)} title="Zoom in">
            <Plus size={14} />
          </button>
          <button className="btn btn-sm" onClick={fit} title="Fit to window">
            <Crosshair size={13} />
            Fit
          </button>
        </div>
      </div>

      <div
        ref={frame}
        className={styles.frame}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onWheel={onWheel}
      >
        {items === null ? (
          <div className={styles.centre}>
            <Loader2 size={20} className="spin" />
          </div>
        ) : graph.nodes.length === 0 ? (
          <div className={styles.centre}>
            <Share2 size={28} className={styles.emptyIcon} />
            <p>
              {items.length === 0
                ? "No mods installed yet."
                : graph.omitted.length > 0
                  ? "Nothing here depends on anything but Fabric API."
                  : "None of these mods depend on each other."}
            </p>
          </div>
        ) : (
          <div
            className={styles.canvas}
            style={{
              transform: `translate(${view.x}px, ${view.y}px) scale(${view.scale})`,
            }}
          >
            {/* One panel per separate little graph. Most instances are a set
                of unrelated pairs, and boxing them says so before you have
                traced a single line. */}
            {graph.clusters.map((c, i) => (
              <div
                key={i}
                className={styles.cluster}
                style={{
                  left: c.x - CLUSTER_PAD,
                  top: c.y - CLUSTER_PAD,
                  width: c.width + CLUSTER_PAD * 2,
                  height: c.height + CLUSTER_PAD * 2,
                }}
              />
            ))}

            <svg
              className={styles.wires}
              width={graph.width}
              height={graph.height}
              // Curves start and end outside the box bounds, so give the
              // drawing surface room rather than clipping them.
              style={{ overflow: "visible" }}
            >
              <defs>
                <marker
                  id="graph-arrow"
                  viewBox="0 0 8 8"
                  refX="7"
                  refY="4"
                  markerWidth="6"
                  markerHeight="6"
                  orient="auto-start-reverse"
                >
                  <path d="M0,1 L7,4 L0,7 z" className={styles.arrow} />
                </marker>
              </defs>
              {graph.edges.map(({ from, to }) => {
                const a = graph.nodes[from];
                const b = graph.nodes[to];
                const x1 = a.x + NODE_W / 2;
                const y1 = a.y + NODE_H;
                const x2 = b.x + NODE_W / 2;
                const y2 = b.y;
                const bend = Math.max(24, (y2 - y1) / 2);
                const on =
                  !lit ||
                  (lit.has(a.item.fileName) && lit.has(b.item.fileName));
                return (
                  <path
                    key={`${a.item.fileName}->${b.item.fileName}`}
                    className={clsx(
                      styles.wire,
                      on && lit && styles.wireLit,
                      !on && styles.wireDim,
                    )}
                    markerEnd="url(#graph-arrow)"
                    d={`M${x1},${y1} C${x1},${y1 + bend} ${x2},${y2 - bend} ${x2},${y2}`}
                  />
                );
              })}
            </svg>

            {graph.nodes.map((node) => {
              const { item } = node;
              const dim = lit ? !lit.has(item.fileName) : false;
              return (
                <button
                  key={item.fileName}
                  data-node
                  className={clsx(
                    styles.node,
                    item.dependency ? styles.nodeDep : styles.nodeRoot,
                    item.unused && styles.nodeUnused,
                    !item.enabled && styles.nodeOff,
                    dim && styles.nodeDim,
                    focus === item.fileName && styles.nodeFocus,
                    matches?.has(item.fileName) && styles.nodeMatch,
                  )}
                  style={{ left: node.x, top: node.y, width: NODE_W, height: NODE_H }}
                  onClick={() =>
                    setFocus((f) => (f === item.fileName ? null : item.fileName))
                  }
                  onPointerEnter={() => setHover(item.fileName)}
                  onPointerLeave={() =>
                    setHover((h) => (h === item.fileName ? null : h))
                  }
                >
                  <span className={styles.nodeName}>{item.name}</span>
                  {item.requiredBy.length > 1 && (
                    <span className={styles.fanIn}>{item.requiredBy.length}</span>
                  )}
                </button>
              );
            })}
          </div>
        )}

        {shownItem && (
          <aside className={styles.detail}>
            <h3>{shownItem.name}</h3>
            <p className={styles.detailMeta}>
              {shownItem.version || shownItem.fileName}
            </p>
            <dl>
              <dt>Needs</dt>
              <dd>
                {shownItem.requires.length === 0
                  ? "nothing"
                  : shownItem.requires
                      .map((f) => graph.nodes[graph.index.get(f) ?? -1]?.item.name ?? f)
                      .join(", ")}
              </dd>
              <dt>Needed by</dt>
              <dd>
                {shownItem.requiredBy.length === 0
                  ? "nothing"
                  : shownItem.requiredBy
                      .map((f) => graph.nodes[graph.index.get(f) ?? -1]?.item.name ?? f)
                      .join(", ")}
              </dd>
            </dl>
            {focus ? (
              <button className="btn btn-sm" onClick={() => setFocus(null)}>
                Clear
              </button>
            ) : (
              <p className={styles.hint}>Click to keep this one up</p>
            )}
          </aside>
        )}
      </div>

      {graph.loose.length > 0 && (
        <div className={styles.shelf}>
          <span className={styles.shelfTitle}>
            No links · {graph.loose.length}
            {graph.loose.some((i) => !i.declaresDeps) && (
              <em className={styles.shelfNote}>
                dashed ones did not state their dependencies
              </em>
            )}
          </span>
          <div className={styles.shelfItems}>
            {graph.loose.map((item) => (
              <span
                key={item.fileName}
                className={clsx(
                  styles.chip,
                  // Unreadable jars are not the same as unconnected ones, and
                  // parking them here silently would claim more than we know.
                  !item.declaresDeps && styles.chipUnknown,
                  matches?.has(item.fileName) && styles.chipMatch,
                )}
                title={
                  item.declaresDeps
                    ? item.fileName
                    : `${item.fileName} — this jar did not state its dependencies, so it may have some`
                }
              >
                {item.name}
              </span>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

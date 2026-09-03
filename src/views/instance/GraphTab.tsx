import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { Crosshair, Loader2, Minus, Package, Plus, Search, Share2 } from "lucide-react";

import dagre from "@dagrejs/dagre";

import { api } from "../../lib/api";
import { useStore } from "../../lib/store";
import type { ContentItem, Instance } from "../../lib/types";

import styles from "./GraphTab.module.css";

const NODE_W = 210;
const NODE_H = 48;
const PAD = 48;
/**
 * Space between one cluster and the next.
 *
 * Comfortably more than any gap inside one, because that difference is the
 * only thing saying where a group ends now that nothing is drawn around it.
 */
const CLUSTER_GAP = 118;
/** Width to wrap the row of clusters at. */
const PACK_W = 1240;

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

type Node = { item: ContentItem; x: number; y: number };

/**
 * A rounded path through dagre's routed points.
 *
 * The points are a polyline, and drawn as one every bend is a corner. This is
 * the standard Catmull-Rom conversion: each span becomes a cubic whose control
 * points lean on the neighbouring points, so the line curves through all of
 * them without overshooting any.
 */
function spline(points: Point[]): string {
  if (points.length < 2) return "";
  if (points.length === 2) {
    return `M${points[0].x},${points[0].y} L${points[1].x},${points[1].y}`;
  }
  let d = `M${points[0].x},${points[0].y}`;
  for (let i = 0; i < points.length - 1; i++) {
    const p0 = points[i - 1] ?? points[i];
    const p1 = points[i];
    const p2 = points[i + 1];
    const p3 = points[i + 2] ?? p2;
    d +=
      ` C${p1.x + (p2.x - p0.x) / 6},${p1.y + (p2.y - p0.y) / 6}` +
      ` ${p2.x - (p3.x - p1.x) / 6},${p2.y - (p3.y - p1.y) / 6}` +
      ` ${p2.x},${p2.y}`;
  }
  return d;
}

/** A routed edge: the polyline dagre threaded between the two boxes. */
type Edge = { from: number; to: number; points: Point[] };

type Point = { x: number; y: number };

type Layout = {
  nodes: Node[];
  edges: Edge[];
  /** Index into `nodes`, by file name. */
  index: Map<string, number>;
  loose: ContentItem[];
  /** Left out for being in nearly everything's dependency list. */
  omitted: ContentItem[];
  width: number;
  height: number;
};

/** Where one cluster's box landed while packing. */
type Cluster = { x: number; y: number; width: number; height: number };

/**
 * Split the linked mods into the separate little graphs they actually form.
 *
 * Once Fabric API is out of the picture most instances are not one graph at
 * all — they are a handful of unrelated pairs and triples. Laying each out on
 * its own and packing the results keeps every relationship short, and lets the
 * packing be tight in a way a single layout pass will not manage.
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
  nodes: { file: string; x: number; y: number }[];
  edges: { from: string; to: string; points: Point[] }[];
  width: number;
  height: number;
};

/**
 * Lay one cluster out with dagre.
 *
 * This is the Sugiyama method — rank the nodes into layers, order each layer to
 * cut crossings, then assign coordinates — and it is what every serious tool
 * for drawing a dependency graph uses. Getting it right by hand means dummy
 * nodes for edges that span layers and a proper coordinate pass to straighten
 * what is left, which is a great deal of subtle code to reimplement badly.
 *
 * The dummy nodes are the part worth naming, because their absence is what
 * made the earlier attempts look wrong: they are why a long edge bends politely
 * around the boxes between its ends instead of cutting straight through them,
 * and they are why the routed points come back at all.
 */
function layoutCluster(files: string[], byFile: Map<string, ContentItem>): Placed {
  const g = new dagre.graphlib.Graph();
  g.setGraph({
    rankdir: "TB",
    nodesep: 26,
    edgesep: 14,
    ranksep: 62,
    // Slower than the alternatives and the one that packs ranks best; these
    // graphs are tens of nodes, so the cost is not worth thinking about.
    ranker: "network-simplex",
  });
  g.setDefaultEdgeLabel(() => ({}));

  for (const file of files) {
    g.setNode(file, { width: NODE_W, height: NODE_H });
  }
  for (const file of files) {
    const item = byFile.get(file);
    if (!item) continue;
    for (const need of item.requires) {
      if (byFile.has(need)) g.setEdge(file, need);
    }
  }

  dagre.layout(g);

  // dagre positions by centre; everything here is drawn from the corner.
  const nodes = files.map((file) => {
    const at = g.node(file);
    return { file, x: at.x - NODE_W / 2, y: at.y - NODE_H / 2 };
  });
  const edges = g.edges().map((e) => ({
    from: e.v,
    to: e.w,
    points: (g.edge(e).points ?? []) as Point[],
  }));

  const xs = nodes.map((n) => n.x);
  const ys = nodes.map((n) => n.y);
  const minX = Math.min(...xs);
  const minY = Math.min(...ys);
  for (const n of nodes) {
    n.x -= minX;
    n.y -= minY;
  }
  for (const e of edges) {
    e.points = e.points.map((pt) => ({ x: pt.x - minX, y: pt.y - minY }));
  }

  return {
    nodes,
    edges,
    width: Math.max(...nodes.map((n) => n.x)) + NODE_W,
    height: Math.max(...nodes.map((n) => n.y)) + NODE_H,
  };
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
    // Tallest first is what a skyline packs tightest, and the tall cluster is
    // the deep one — the part with something to say, sitting where you start
    // reading, with the pairs tucked in around it.
    .sort((a, b) => b.height - a.height || b.nodes.length - a.nodes.length);

  // Pack the clusters by dropping each into the lowest gap wide enough to take
  // it. Clusters differ in height by a factor of three — a pair against a
  // three-deep chain — and laying them in rows gives every cluster in a row the
  // height of the tallest, which leaves bands of nothing.
  const SLOT = 24;
  const slots = Math.ceil(PACK_W / SLOT);
  const skyline = new Array(slots).fill(0);

  const clusters: Cluster[] = [];
  for (const group of placed) {
    const span = Math.min(slots, Math.ceil((group.width + CLUSTER_GAP) / SLOT));
    let bestAt = 0;
    let bestY = Infinity;
    for (let start = 0; start + span <= slots; start++) {
      let top = 0;
      for (let k = start; k < start + span; k++) top = Math.max(top, skyline[k]);
      if (top < bestY) {
        bestY = top;
        bestAt = start;
      }
    }
    const x = bestAt * SLOT;
    const y = bestY;
    clusters.push({ x, y, width: group.width, height: group.height });
    for (const n of group.nodes) {
      n.x += x;
      n.y += y;
    }
    for (const e of group.edges) {
      e.points = e.points.map((pt) => ({ x: pt.x + x, y: pt.y + y }));
    }
    for (let k = bestAt; k < bestAt + span; k++) {
      skyline[k] = y + group.height + CLUSTER_GAP;
    }
  }

  const nodes: Node[] = [];
  const index = new Map<string, number>();
  for (const group of placed) {
    for (const n of group.nodes) {
      const item = byFile.get(n.file);
      if (!item) continue;
      index.set(n.file, nodes.length);
      nodes.push({ item, x: n.x, y: n.y });
    }
  }

  const edges: Edge[] = [];
  for (const group of placed) {
    for (const e of group.edges) {
      const from = index.get(e.from);
      const to = index.get(e.to);
      if (from !== undefined && to !== undefined) {
        edges.push({ from, to, points: e.points });
      }
    }
  }

  return {
    nodes,
    edges,
    index,
    loose,
    omitted,
    width: Math.max(1, ...clusters.map((c) => c.x + c.width)),
    height: Math.max(1, ...clusters.map((c) => c.y + c.height)),
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
              {graph.edges.map(({ from, to, points }) => {
                const a = graph.nodes[from];
                const b = graph.nodes[to];
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
                    d={spline(points)}
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
                  <span className={styles.nodeIcon}>
                    {item.iconUrl ? (
                      <img src={item.iconUrl} alt="" loading="lazy" />
                    ) : (
                      <Package size={13} />
                    )}
                  </span>
                  <span className={styles.nodeName}>{item.name}</span>
                  {item.requiredBy.length > 1 && (
                    <span
                      className={styles.fanIn}
                      title={`${item.requiredBy.length} mods need this`}
                    >
                      {item.requiredBy.length}
                    </span>
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

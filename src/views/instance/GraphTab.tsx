import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import clsx from "clsx";
import { Crosshair, Loader2, Minus, Plus, RotateCcw, Search, Share2 } from "lucide-react";
import {
  forceCollide,
  forceLink,
  forceManyBody,
  forceSimulation,
  forceX,
  forceY,
  type Simulation,
} from "d3-force";

import { api } from "../../lib/api";
import { useStore } from "../../lib/store";
import type { ContentItem, Instance } from "../../lib/types";

import styles from "./GraphTab.module.css";

/* The field ---------------------------------------------------------------
 *
 * One force-directed field holding every mod, rather than any arrangement of
 * boxes. These numbers are not taste: each was measured against this graph's
 * actual shape — around 70 nodes, 24 edges, 60% of them connected to nothing.
 */

/** Collision radius of a bare disc. */
const R = 22;
/** Exactly 2R. Below it collide silently wins; above it crossings climb. */
const LINK_DISTANCE = 2 * R;
/** Rough width of a label glyph at 11px/500 — enough to reserve space for. */
const GLYPH = 5.9;
/** Charge per node, scaled by how many neighbours it has. */
const CHARGE = -60;
/**
 * Gravity toward the origin, split to bias the field landscape.
 *
 * `forceX`/`forceY` and never `forceCenter`. Centering is a translation, not a
 * force — it moves nodes without touching velocity, so nothing balances the
 * repulsion and a graph this sparse expands for as long as you let it tick.
 * Positional forces are real springs, so the field settles at a size.
 */
const GX = 0.09 * Math.sqrt(560 / 900);
const GY = 0.09 * Math.sqrt(900 / 560);
/** Pull toward each island's own centre of mass. */
const COHESION = 0.05;
/** Ticks to solve before the first paint. */
const TICKS = 600;

const UBIQUITOUS = new Set(["fabric_api", "fabric"]);
const idKey = (id: string) => id.toLowerCase().replace(/-/g, "_");

type Node = {
  item: ContentItem;
  id: string;
  degree: number;
  /** Which island, or -1 for a mod connected to nothing. */
  island: number;
  r: number;
  label: string;
  x: number;
  y: number;
  vx: number;
  vy: number;
  fx?: number | null;
  fy?: number | null;
};

type Link = { source: Node | string; target: Node | string };

/**
 * Seed a node's starting position from a hash of its own name.
 *
 * d3 seeds by array index, which sounds harmless and is not: adding one mod to
 * an alphabetical list shifts every later index, and the whole picture is
 * redrawn somewhere else. Hashing the name means a mod starts in the same spot
 * whatever else is installed, so adding one moves its neighbours a little and
 * leaves the rest of the field where it was.
 */
function seed(id: string): { x: number; y: number } {
  let h = 2166136261;
  for (let i = 0; i < id.length; i++) {
    h ^= id.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  const angle = ((h >>> 8) / 16777216) * Math.PI * 2;
  const radius = Math.sqrt((h & 255) / 255) * 150;
  return { x: Math.cos(angle) * radius, y: Math.sin(angle) * radius };
}

/** Hold each island together, so two of them never read as one tangle. */
function forceIslands(strength: number) {
  let nodes: Node[] = [];
  const force = (alpha: number) => {
    const centre = new Map<number, { x: number; y: number; n: number }>();
    for (const n of nodes) {
      if (n.island < 0) continue;
      const at = centre.get(n.island) ?? { x: 0, y: 0, n: 0 };
      at.x += n.x;
      at.y += n.y;
      at.n += 1;
      centre.set(n.island, at);
    }
    for (const n of nodes) {
      if (n.island < 0) continue;
      const at = centre.get(n.island)!;
      n.vx += (at.x / at.n - n.x) * strength * alpha;
      n.vy += (at.y / at.n - n.y) * strength * alpha;
    }
  };
  force.initialize = (list: Node[]) => {
    nodes = list;
  };
  return force;
}

/**
 * Drop the ubiquitous libraries and every edge touching them.
 *
 * A view decision, not a claim the dependency is absent: the edges stay in the
 * data, still counted, still what says a library has nothing needing it.
 */
function omitUbiquitous(items: ContentItem[]) {
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

/** A mod's name, without the noise every third author adds to it. */
function shortName(raw: string): string {
  const name = raw
    .replace(/\s*[([]\s*(fabric|forge|neoforge|quilt)\s*[)\]]\s*/gi, " ")
    .replace(/^(fabric|forge|neoforge|quilt)\s+/i, "")
    .replace(/\s+v?\d+(\.\d+)+.*$/, "")
    .trim();
  const out = name || raw;
  return out.length <= 16 ? out : `${out.slice(0, 9)}…${out.slice(-6)}`;
}

function buildField(all: ContentItem[]) {
  // Sorted first so nothing downstream inherits the order the folder was read
  // in. Positions come from name hashes, so this only fixes iteration order.
  const sorted = [...all].sort((a, b) => a.fileName.localeCompare(b.fileName));
  const { items, omitted } = omitUbiquitous(sorted);
  const present = new Set(items.map((i) => i.fileName));

  const nodes: Node[] = items.map((item) => {
    const degree =
      item.requires.filter((f) => present.has(f)).length +
      item.requiredBy.filter((f) => present.has(f)).length;
    return {
      item,
      id: item.fileName,
      degree,
      island: -1,
      r: Math.min(22, Math.max(10, 10 + 4 * Math.sqrt(degree))),
      label: shortName(item.name),
      ...seed(item.fileName),
      vx: 0,
      vy: 0,
    };
  });
  const byId = new Map(nodes.map((n) => [n.id, n]));

  const links: Link[] = [];
  for (const node of nodes) {
    for (const need of node.item.requires) {
      if (byId.has(need)) links.push({ source: node.id, target: need });
    }
  }

  // Islands, for the cohesion force. Singletons stay at -1: they have nothing
  // to cohere with, and the outward drift that gives them is the point.
  const near = new Map<string, string[]>();
  for (const node of nodes) {
    near.set(
      node.id,
      [...node.item.requires, ...node.item.requiredBy]
        .filter((f) => present.has(f))
        .sort(),
    );
  }
  let island = 0;
  for (const node of nodes) {
    if (node.island >= 0 || node.degree === 0) continue;
    const queue = [node.id];
    node.island = island;
    while (queue.length > 0) {
      const at = byId.get(queue.shift()!)!;
      for (const step of near.get(at.id) ?? []) {
        const next = byId.get(step);
        if (next && next.island < 0) {
          next.island = island;
          queue.push(step);
        }
      }
    }
    island += 1;
  }

  const degreeOf = new Map(nodes.map((n) => [n.id, n.degree]));
  const sim = forceSimulation<Node>(nodes)
    .force(
      "link",
      forceLink<Node, Link>(links)
        .id((d) => d.id)
        .distance(LINK_DISTANCE),
    )
    .force(
      "charge",
      forceManyBody<Node>().strength((d) => CHARGE * (1 + (degreeOf.get(d.id) ?? 0))),
    )
    // Weaker gravity on the mods that connect to nothing, so they settle in a
    // loose halo and leave the middle to the structure. They are still here and
    // still findable — hiding two thirds of a folder would misreport it — but
    // the part worth looking at gets the centre of the picture.
    .force("x", forceX<Node>(0).strength((d) => (d.degree > 0 ? GX : GX * 0.5)))
    .force("y", forceY<Node>(0).strength((d) => (d.degree > 0 ? GY : GY * 0.5)))
    .force("islands", forceIslands(COHESION))
    // Collide against the LABEL box, not the disc. Spacing discs apart still
    // lets their names overlap, and it is the names that read as clutter.
    // Only labelled nodes reserve the room; the unnamed singletons pack close.
    .force(
      "collide",
      forceCollide<Node>((d) =>
        d.degree > 0 ? Math.max(d.r + 6, (d.label.length * GLYPH) / 2 + 10) : d.r + 6,
      )
        .strength(1)
        .iterations(2),
    )
    .velocityDecay(0.2)
    .alphaDecay(1 - Math.pow(0.001, 1 / TICKS))
    .stop();

  // Solve before the first paint rather than letting the user watch springs
  // thrash. d3 jiggles coincident nodes with Math.random, so it is seeded for
  // the duration — otherwise the same folder would draw differently each time.
  const random = Math.random;
  let state = 0x2f6e2b1;
  Math.random = () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 4294967296;
  };
  try {
    for (let i = 0; i < TICKS; i++) sim.tick();
  } finally {
    Math.random = random;
  }

  // Framed once, here, from the settled positions. Recomputing it as things
  // move would rescale the whole picture under the cursor every time a node is
  // dragged, which is a horrible way to find out your drag is working.
  const pad = 70;
  const xs = nodes.map((n) => n.x);
  const ys = nodes.map((n) => n.y);
  const x0 = nodes.length ? Math.min(...xs) - pad : 0;
  const y0 = nodes.length ? Math.min(...ys) - pad : 0;
  const box = nodes.length
    ? `${x0} ${y0} ${Math.max(...xs) + pad - x0} ${Math.max(...ys) + pad - y0}`
    : "0 0 100 100";

  return {
    nodes,
    links,
    sim,
    omitted,
    box,
    linked: nodes.filter((n) => n.degree > 0).length,
  };
}

export default function GraphTab({ instance }: { instance: Instance }) {
  const reportError = useStore((s) => s.reportError);

  const [items, setItems] = useState<ContentItem[] | null>(null);
  const [query, setQuery] = useState("");
  const [focus, setFocus] = useState<string | null>(null);
  const [hover, setHover] = useState<string | null>(null);
  const [, repaint] = useReducer((n: number) => n + 1, 0);

  const svg = useRef<SVGSVGElement>(null);
  const world = useRef<SVGGElement>(null);
  const frame = useRef<number>(0);
  const dragging = useRef<Node | null>(null);
  const panning = useRef<{ x: number; y: number } | null>(null);

  // Pan and zoom ride on a transform over the solved field rather than on the
  // viewBox, so the layout itself never moves and a reset is just identity.
  const [view, setView] = useState({ x: 0, y: 0, k: 1 });

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

  const field = useMemo(() => buildField(items ?? []), [items]);

  useEffect(() => {
    const sim = field.sim;
    return () => {
      sim.stop();
      cancelAnimationFrame(frame.current);
    };
  }, [field]);

  /** Run the simulation live while something is being dragged. */
  const animate = useCallback(() => {
    field.sim.tick();
    repaint();
    if (field.sim.alpha() > field.sim.alphaMin() || dragging.current) {
      frame.current = requestAnimationFrame(animate);
    }
  }, [field]);

  /** Cursor in the field's own coordinates, panning and zoom accounted for. */
  const toModel = (e: React.PointerEvent | React.WheelEvent) => {
    const ctm = world.current?.getScreenCTM();
    if (!ctm) return null;
    const at = new DOMPoint(e.clientX, e.clientY).matrixTransform(ctm.inverse());
    return { x: at.x, y: at.y };
  };

  /** Cursor in the SVG's own units, which is where panning is measured. */
  const toCanvas = (e: React.PointerEvent) => {
    const ctm = svg.current?.getScreenCTM();
    if (!ctm) return null;
    const at = new DOMPoint(e.clientX, e.clientY).matrixTransform(ctm.inverse());
    return { x: at.x, y: at.y };
  };

  function onBackgroundDown(e: React.PointerEvent) {
    const at = toCanvas(e);
    if (!at) return;
    panning.current = at;
    e.currentTarget.setPointerCapture(e.pointerId);
  }

  // Zoom about the cursor, so whatever you are pointing at stays put.
  function onWheel(e: React.WheelEvent) {
    const at = toCanvas(e as unknown as React.PointerEvent);
    if (!at) return;
    setView((v) => {
      const k = Math.min(4, Math.max(0.25, v.k * (e.deltaY < 0 ? 1.12 : 0.893)));
      return clamp({
        k,
        x: at.x - ((at.x - v.x) / v.k) * k,
        y: at.y - ((at.y - v.y) / v.k) * k,
      });
    });
  }

  function onNodeDown(e: React.PointerEvent, node: Node) {
    e.stopPropagation();
    (e.target as Element).setPointerCapture(e.pointerId);
    dragging.current = node;
    node.fx = node.x;
    node.fy = node.y;
    (field.sim as Simulation<Node, undefined>).alphaTarget(0.3).restart();
    cancelAnimationFrame(frame.current);
    frame.current = requestAnimationFrame(animate);
  }

  function onNodeMove(e: React.PointerEvent) {
    if (dragging.current) {
      const at = toModel(e);
      if (!at) return;
      dragging.current.fx = at.x;
      dragging.current.fy = at.y;
      return;
    }
    if (panning.current) {
      const at = toCanvas(e);
      if (!at) return;
      // Move by the step since the LAST event, not since the press: the anchor
      // has to advance with the cursor or every frame re-applies the whole
      // distance travelled so far and the field takes off.
      const from = panning.current;
      panning.current = at;
      setView((v) => clamp({ ...v, x: v.x + at.x - from.x, y: v.y + at.y - from.y }));
    }
  }

  function onNodeUp() {
    panning.current = null;
    if (!dragging.current) return;
    // fx/fy are deliberately left set: a mod you moved stays where you put it.
    // Double-click hands it back to the field.
    dragging.current = null;
    (field.sim as Simulation<Node, undefined>).alphaTarget(0);
  }

  function release(node: Node) {
    node.fx = null;
    node.fy = null;
    (field.sim as Simulation<Node, undefined>).alpha(0.3).restart();
    cancelAnimationFrame(frame.current);
    frame.current = requestAnimationFrame(animate);
  }

  /**
   * Keep the field within reach of the viewport.
   *
   * Without this you can pan the whole thing off the edge and be left staring
   * at an empty canvas with no way back except the Fit button.
   */
  const clamp = useCallback(
    (v: { x: number; y: number; k: number }) => {
      const vb = svg.current?.viewBox.baseVal;
      if (!vb || vb.width === 0) return v;
      const edge = 120; // how much of the field must stay on screen
      return {
        k: v.k,
        x: Math.min(
          vb.x + vb.width - edge - vb.x * v.k,
          Math.max(vb.x + edge - (vb.x + vb.width) * v.k, v.x),
        ),
        y: Math.min(
          vb.y + vb.height - edge - vb.y * v.k,
          Math.max(vb.y + edge - (vb.y + vb.height) * v.k, v.y),
        ),
      };
    },
    [],
  );

  /** Zoom by a step, holding the centre of the view still. */
  const step = (by: number) => {
    const rect = svg.current?.viewBox.baseVal;
    const cx = rect ? rect.x + rect.width / 2 : 0;
    const cy = rect ? rect.y + rect.height / 2 : 0;
    setView((v) => {
      const k = Math.min(4, Math.max(0.25, v.k * by));
      return clamp({
        k,
        x: cx - ((cx - v.x) / v.k) * k,
        y: cy - ((cy - v.y) / v.k) * k,
      });
    });
  };

  const matches = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return null;
    return new Set(
      field.nodes
        .filter((n) => n.item.name.toLowerCase().includes(q))
        .map((n) => n.id),
    );
  }, [field, query]);

  // Hovering traces the chain this mod is part of; everything else recedes.
  const traced = hover ?? focus;
  const lit = useMemo(() => {
    if (!traced) return null;
    const out = new Set<string>([traced]);
    const queue = [traced];
    const byId = new Map(field.nodes.map((n) => [n.id, n]));
    while (queue.length > 0) {
      const at = byId.get(queue.shift()!);
      if (!at) continue;
      for (const step of [...at.item.requires, ...at.item.requiredBy]) {
        if (byId.has(step) && !out.has(step)) {
          out.add(step);
          queue.push(step);
        }
      }
    }
    return out;
  }, [field, traced]);

  const dim = (id: string) => (lit ? !lit.has(id) : false);

  // A mod connected to nothing gets its name on demand rather than always.
  // Two thirds of a folder is unconnected, and naming all of them at once
  // buries the handful of mods that actually relate to each other.
  const named = useMemo(() => {
    const out = new Set<string>();
    if (traced) out.add(traced);
    if (focus) out.add(focus);
    for (const id of matches ?? []) out.add(id);
    return out;
  }, [traced, focus, matches]);

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
        {items !== null && field.nodes.length > 0 && (
          <p className={styles.summary}>
            {field.linked} of {field.nodes.length} mods are connected to
            something.
            {field.omitted.length > 0 && (
              <span className={styles.omitted}>
                {field.omitted.map((i) => i.name).join(", ")} hidden — nearly
                everything needs it
              </span>
            )}
          </p>
        )}
        <span className={styles.hint}>drag a mod to move it</span>
        <div className={styles.zoom}>
          <button
            className="btn btn-icon btn-sm"
            onClick={() => step(1 / 1.3)}
            title="Zoom out"
          >
            <Minus size={14} />
          </button>
          <span className={styles.scale}>{Math.round(view.k * 100)}%</span>
          <button
            className="btn btn-icon btn-sm"
            onClick={() => step(1.3)}
            title="Zoom in"
          >
            <Plus size={14} />
          </button>
          <button
            className="btn btn-sm"
            onClick={() => setView({ x: 0, y: 0, k: 1 })}
            title="Back to the whole field"
          >
            <Crosshair size={13} />
            Fit
          </button>
        </div>
      </div>

      {items === null ? (
        <div className={styles.centre}>
          <Loader2 size={20} className="spin" />
        </div>
      ) : field.nodes.length === 0 ? (
        <div className={styles.centre}>
          <Share2 size={28} className={styles.emptyIcon} />
          <p>No mods installed yet.</p>
        </div>
      ) : (
        <svg
          ref={svg}
          className={styles.field}
          viewBox={field.box}
          preserveAspectRatio="xMidYMid meet"
          onPointerDown={onBackgroundDown}
          onPointerMove={onNodeMove}
          onPointerUp={onNodeUp}
          onPointerCancel={onNodeUp}
          onWheel={onWheel}
        >
          <defs>
            {field.nodes.map((n) =>
              n.item.iconUrl ? (
                <clipPath key={n.id} id={`disc-${n.id.replace(/[^\w-]/g, "")}`}>
                  <circle cx={0} cy={0} r={n.r - 1} />
                </clipPath>
              ) : null,
            )}
          </defs>

          <g
            ref={world}
            transform={`translate(${view.x},${view.y}) scale(${view.k})`}
          >
          <g className={styles.wires}>
            {field.links.map((link) => {
              const a = link.source as Node;
              const b = link.target as Node;
              const dx = b.x - a.x;
              const dy = b.y - a.y;
              const len = Math.hypot(dx, dy) || 1;
              const ux = dx / len;
              const uy = dy / len;
              const on = !lit || (lit.has(a.id) && lit.has(b.id));
              return (
                <line
                  key={`${a.id}->${b.id}`}
                  className={clsx(styles.wire, lit && on && styles.wireLit)}
                  x1={a.x + ux * (a.r + 2)}
                  y1={a.y + uy * (a.r + 2)}
                  x2={b.x - ux * (b.r + 4)}
                  y2={b.y - uy * (b.r + 4)}
                  markerEnd={lit && on ? "url(#tip-lit)" : "url(#tip)"}
                  opacity={lit && !on ? 0 : undefined}
                />
              );
            })}
          </g>

          <defs>
            {["tip", "tip-lit"].map((id) => (
              <marker
                key={id}
                id={id}
                viewBox="0 0 6 6"
                refX="5.4"
                refY="3"
                markerWidth="5"
                markerHeight="5"
                orient="auto-start-reverse"
              >
                <path
                  d="M0,0.8 L5.4,3 L0,5.2 z"
                  className={id === "tip" ? styles.tip : styles.tipLit}
                />
              </marker>
            ))}
          </defs>

          {field.nodes.map((node) => {
            const faded = dim(node.id);
            return (
              <g
                key={node.id}
                className={clsx(
                  styles.node,
                  node.degree === 0 && styles.alone,
                  faded && styles.faded,
                  focus === node.id && styles.picked,
                  matches?.has(node.id) && styles.found,
                  !node.item.enabled && styles.off,
                )}
                transform={`translate(${node.x},${node.y})`}
                onPointerDown={(e) => onNodeDown(e, node)}
                onPointerEnter={() => setHover(node.id)}
                onPointerLeave={() => setHover((h) => (h === node.id ? null : h))}
                onClick={() => setFocus((f) => (f === node.id ? null : node.id))}
                onDoubleClick={() => release(node)}
              >
                <circle className={styles.disc} r={node.r} />
                {node.item.iconUrl && (
                  <image
                    href={node.item.iconUrl}
                    x={-(node.r - 1)}
                    y={-(node.r - 1)}
                    width={(node.r - 1) * 2}
                    height={(node.r - 1) * 2}
                    clipPath={`url(#disc-${node.id.replace(/[^\w-]/g, "")})`}
                    preserveAspectRatio="xMidYMid slice"
                  />
                )}
                <circle className={styles.rim} r={node.r} />
                {!faded && (node.degree > 0 || named.has(node.id)) && (
                  <text
                    className={styles.label}
                    textAnchor="middle"
                    y={node.r + 14}
                  >
                    {node.label}
                  </text>
                )}
              </g>
            );
          })}
          </g>
        </svg>
      )}

      {focus && (
        <button
          className={clsx("btn", "btn-sm", styles.clear)}
          onClick={() => setFocus(null)}
        >
          <RotateCcw size={13} />
          Clear
        </button>
      )}
    </div>
  );
}

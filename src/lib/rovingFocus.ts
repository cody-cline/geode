import type { KeyboardEvent } from "react";

/** The arrow keys, by the step each one moves along a line of options. */
const STEP: Record<string, number> = {
  ArrowRight: 1,
  ArrowDown: 1,
  ArrowLeft: -1,
  ArrowUp: -1,
};

/**
 * An option's element. `SVGElement` as well as `HTMLElement` because both carry
 * `dataset` and `focus`, and a `<g>` is neither an `HTMLElement` nor anything
 * that could be made into one — which is the whole reason the dependency graph
 * could not reach this helper and wrote the same contract out again locally.
 */
type Option = HTMLElement | SVGElement;

/** The element carrying one option's id, inside the group that owns the press. */
function option(group: Element, id: string): Option | null {
  return group.querySelector<Option>(`[data-roving="${id}"]`);
}

/**
 * Which row each option sits on, read off the page, or `null` when any option
 * has no element to read.
 *
 * The geometry is discovered rather than declared because it is not a fact any
 * caller knows: the loader tiles are `repeat(auto-fit, minmax(210px, 1fr))` and
 * the accent swatches are `flex-wrap`, so how many fit across is a question
 * about the width the dialog ended up at. A number passed in from the caller
 * would be a guess that goes stale the first time somebody resizes the window.
 *
 * Options are laid out in the order they are given, top row first, so a row
 * ends wherever the next option starts lower down than the one before it. Half
 * a pixel of slack because a fractional track leaves fractional offsets.
 */
function rowOf(group: Element, options: readonly string[]): number[] | null {
  const tops: number[] = [];
  for (const id of options) {
    const el = option(group, id);
    if (!el) return null;
    tops.push(el.getBoundingClientRect().top);
  }

  const rows = [0];
  for (let i = 1; i < tops.length; i++) {
    rows.push(rows[i - 1] + (tops[i] > tops[i - 1] + 0.5 ? 1 : 0));
  }
  return rows;
}

/**
 * The option an arrow lands on, or `null` when it lands nowhere.
 *
 * A line of options — a tab strip, a segmented control, a row of chips, or a
 * stack of tiles one to a row — is one axis, and every arrow walks it, wrapping
 * at both ends. That is the WAI-ARIA radio-group contract and it is what this
 * helper has always done.
 *
 * A **grid** is two axes and the same walk is wrong in it: Right on the loader
 * grid's top-right tile used to select the one below and to the left, because
 * that is what comes next in the markup, and no arrow could move down a column
 * at all. So the two axes are separated, and each takes the convention that
 * belongs to it:
 *
 * - **Left and Right wrap inside the row they are on.** Wrapping is the radio
 *   group's own rule, and keeping it inside the row is what stops Right from
 *   jumping a row the way it did. Every option is still reachable: the rows
 *   are reachable by column and the options within one by row.
 * - **Up and Down stop at the first and last row**, which is what a grid does
 *   rather than what a radio group does. A row is a position on screen, and
 *   teleporting from the bottom of a picker back to the top of it is the kind
 *   of move somebody has to go looking for their focus after. The press is
 *   still swallowed, so it does not scroll the dialog out from under the thing
 *   it was pointed at.
 * - **A short last row clamps.** The loader grid is five options in two
 *   columns, so the last row holds one: Down from the right-hand tile above it
 *   lands on that one rather than refusing, because there is only one place
 *   down can mean there.
 */
function destination<T extends string>(
  group: Element,
  options: readonly T[],
  at: number,
  key: string,
): number | null {
  const step = STEP[key];
  const line = (at + step + options.length) % options.length;

  const rows = rowOf(group, options);
  if (!rows) return line;

  // Each row as the option index it starts at and the number of options on it.
  const start: number[] = [];
  const width: number[] = [];
  rows.forEach((row, i) => {
    if (start[row] === undefined) start[row] = i;
    width[row] = (width[row] ?? 0) + 1;
  });

  // One row, or one option to a row: a line either way, so every arrow walks
  // it and the two conventions below never come up.
  if (width.length === 1 || width.every((n) => n === 1)) return line;

  const here = rows[at];
  const column = at - start[here];

  if (key === "ArrowLeft" || key === "ArrowRight") {
    return start[here] + ((column + step + width[here]) % width[here]);
  }

  const row = here + step;
  if (row < 0 || row >= width.length) return null;
  return start[row] + Math.min(column, width[row] - 1);
}

/**
 * Where an arrow key moves a roving-focus group, or `null` when the key is not
 * one — or is one with nowhere to go, which only a grid's top and bottom rows
 * have.
 *
 * Two patterns in this app have the same keyboard contract and differ only in
 * what they call themselves to a screen reader. A row of chips picking one of
 * several is a **radio group**; a strip that swaps the panel below it is a
 * **tab set**. Both are one tab stop for the whole group, with arrows moving
 * between the options from there and the option they land on chosen, exactly
 * as clicking it would choose it. Claiming either role without that is worse
 * than claiming nothing, because it tells a screen reader to promise keys that
 * do nothing.
 *
 * Bind this to the element carrying `role="radiogroup"` or `role="tablist"`
 * and give every option a `data-roving` holding its id. Which option the press
 * came from is read off that attribute rather than off the current selection,
 * so a group with nothing chosen still steps from wherever the keyboard
 * actually is — which is the accent swatches whenever the colour in use is one
 * no swatch offers.
 *
 * An option may be SVG as well as HTML. Both carry `dataset` and `focus`, and
 * the narrower guard this used to have is why the dependency graph's nodes —
 * `<g>` elements, which no amount of markup makes into `HTMLElement`s — could
 * not use this and wrote the contract out a second time.
 *
 * Whether the group is a line or a grid is worked out here, from where its
 * options actually are; see `destination` for what each arrow then means. A
 * caller passes the same thing either way.
 *
 * The roles, the names and which `aria-` attribute marks the chosen one stay
 * with each caller, because that is the part the two patterns do not share:
 * a radio is `aria-checked`, a tab is `aria-selected` and also points at the
 * panel it controls.
 *
 * Returns the option to select; focus has already been moved to it.
 */
export function rovingKey<T extends string>(
  e: KeyboardEvent<Element>,
  options: readonly T[],
): T | null {
  if (!(e.key in STEP)) return null;

  const pressed = e.target;
  const from =
    pressed instanceof HTMLElement || pressed instanceof SVGElement
      ? pressed.dataset.roving
      : undefined;
  const at = options.findIndex((o) => o === from);
  if (at < 0) return null;

  e.preventDefault();
  const to = destination(e.currentTarget, options, at, e.key);
  if (to === null) return null;

  const next = options[to];
  option(e.currentTarget, next)?.focus();
  return next;
}

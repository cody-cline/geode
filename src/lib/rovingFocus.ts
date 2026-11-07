import type { KeyboardEvent } from "react";

/** The arrow keys, by the step each one moves. */
const STEP: Record<string, number> = {
  ArrowRight: 1,
  ArrowDown: 1,
  ArrowLeft: -1,
  ArrowUp: -1,
};

/**
 * Where an arrow key moves a roving-focus group, or `null` when the key is not
 * one.
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
 * The roles, the names and which `aria-` attribute marks the chosen one stay
 * with each caller, because that is the part the two patterns do not share:
 * a radio is `aria-checked`, a tab is `aria-selected` and also points at the
 * panel it controls.
 *
 * Returns the option to select; focus has already been moved to it.
 */
export function rovingKey<T extends string>(
  e: KeyboardEvent<HTMLElement>,
  options: readonly T[],
): T | null {
  const step = STEP[e.key];
  if (!step) return null;

  const from = e.target instanceof HTMLElement ? e.target.dataset.roving : undefined;
  const at = options.findIndex((o) => o === from);
  if (at < 0) return null;

  e.preventDefault();
  const next = options[(at + step + options.length) % options.length];
  e.currentTarget.querySelector<HTMLElement>(`[data-roving="${next}"]`)?.focus();
  return next;
}

import type { KeyboardEvent } from "react";

/** The arrow keys, by the step each one moves. */
const STEP: Record<string, number> = {
  ArrowRight: 1,
  ArrowDown: 1,
  ArrowLeft: -1,
  ArrowUp: -1,
};

/**
 * Where an arrow key moves a radio group, or `null` when the key is not one.
 *
 * A row of chips that picks one of several is a radio group, not a row of
 * independent toggles — and a group of `role="radio"` buttons has to behave
 * like the native control it is claiming to be: one tab stop for the whole
 * group, arrows moving between the options from there, and the option they
 * land on chosen, exactly as clicking it would choose it. Claiming the role
 * without that is worse than claiming nothing, because it tells a screen
 * reader to promise keys that do nothing.
 *
 * Bind this to the element carrying `role="radiogroup"` and give every option
 * a `data-radio` holding its id. Which option the press came from is read off
 * that attribute rather than off the current selection, so a group with
 * nothing chosen still steps from wherever the keyboard actually is — which is
 * the accent swatches whenever the colour in use is one no swatch offers.
 *
 * Returns the option to select; focus has already been moved to it.
 */
export function radioGroupKey<T extends string>(
  e: KeyboardEvent<HTMLElement>,
  options: readonly T[],
): T | null {
  const step = STEP[e.key];
  if (!step) return null;

  const from = e.target instanceof HTMLElement ? e.target.dataset.radio : undefined;
  const at = options.findIndex((o) => o === from);
  if (at < 0) return null;

  e.preventDefault();
  const next = options[(at + step + options.length) % options.length];
  e.currentTarget.querySelector<HTMLElement>(`[data-radio="${next}"]`)?.focus();
  return next;
}

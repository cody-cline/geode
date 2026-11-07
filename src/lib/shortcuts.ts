import type { Route } from "./store";

/**
 * The keyboard's shortcuts to the screens in the sidebar.
 *
 * This table is read by the window handler that acts on a press and by the
 * sidebar that prints the hint, because a shortcut nobody can see is one
 * nobody uses and a hint that names a key nothing is bound to is worse than
 * no hint at all. Every table in this file is read from both ends that way.
 *
 * Deliberately short. Five bindings beside the five things they reach are
 * five somebody finds; a page of them is a page nobody reads.
 */
const ROUTE_SHORTCUTS: { key: string; route: Route }[] = [
  // Numbered down the sidebar, so the hint and the order on screen agree.
  { key: "1", route: { view: "library" } },
  { key: "2", route: { view: "browse" } },
  { key: "3", route: { view: "plans" } },
  { key: "4", route: { view: "servers" } },
  // What every other desktop app binds settings to.
  { key: ",", route: { view: "settings" } },
];

/**
 * The keyboard's shortcuts to an *action*, which is a different kind of thing
 * from a screen and so has its own table rather than a nullable column in the
 * one above.
 *
 * A route is a place and is reachable from everywhere, so `App` binds those
 * once at the window. An action is a press on one screen and means nothing
 * anywhere else, so the screen that can carry it out is the screen that binds
 * it — and is also the only place its hint can go, since there is no sidebar
 * row to print it beside.
 *
 * `ctrl` is the whole of the modifier question. `true` wants Ctrl or Cmd and
 * therefore types nothing into a field; `false` wants the bare key, which is
 * a key somebody may be in the middle of using — see `shortcutAction`.
 *
 * `label` is what the hint prints, which is not always the key's own name:
 * `Escape` is written `Esc` on every keyboard that has one.
 */
const ACTION_SHORTCUTS = [
  /** Put the keyboard in the Content tab's Filter box. */
  { action: "focusFilter", key: "f", ctrl: true, label: "F" },
  /** Drop a selection of rows on the Content tab. */
  { action: "clearSelection", key: "Escape", ctrl: false, label: "Esc" },
] as const;

/** One of the actions a screen can bind, named by the table above. */
export type Action = (typeof ACTION_SHORTCUTS)[number]["action"];

/**
 * Whether this is a Mac, for the hint's sake only.
 *
 * Nothing in the frontend knows the platform — `AppInfo.platform` comes from
 * Rust and is fetched by the About tab alone — and a binding needs no
 * detection at all, since both modifiers are accepted below. Only the two
 * characters printed in the sidebar depend on the answer, so this asks the
 * webview rather than adding a command round-trip to the store for a label.
 */
const MAC = /Mac/i.test(navigator.userAgent);

/** One key written as the hint for it, held with whichever modifier this is. */
function held(label: string): string {
  return MAC ? `⌘${label}` : `Ctrl+${label}`;
}

/** What the sidebar prints beside a screen, or `null` if it has no shortcut. */
export function shortcutHint(view: Route["view"]): string | null {
  const bound = ROUTE_SHORTCUTS.find((s) => s.route.view === view);
  if (!bound) return null;
  return held(bound.key);
}

/** What a screen prints on the thing an action of its own presses. */
export function actionHint(action: Action): string {
  // Cannot be missing: `Action` is the table's own `action` field.
  const bound = ACTION_SHORTCUTS.find((a) => a.action === action)!;
  return bound.ctrl ? held(bound.label) : bound.label;
}

/**
 * The screen a press asks for, or `null` when it asks for nothing here.
 *
 * Ctrl or Cmd is accepted either way round rather than branching on the
 * platform: Linux and Windows send Ctrl, macOS sends Cmd, and neither sends
 * the other for these keys. Alt is excluded because AltGr arrives as Ctrl and
 * Alt together, and a layout that puts a digit or a comma behind it would
 * find the key navigating instead of typing.
 *
 * Matched on the character the key produced, which is the same character the
 * hint prints — so the binding is the one the sidebar promised, whatever the
 * layout calls that key. Every one of them carries a modifier and so types
 * nothing into a field, which is why the console and every other input can be
 * left alone to receive what is meant for them.
 */
export function shortcutRoute(e: KeyboardEvent): Route | null {
  if (!(e.ctrlKey || e.metaKey) || e.altKey) return null;
  return ROUTE_SHORTCUTS.find((s) => s.key === e.key)?.route ?? null;
}

/** Whether the keyboard is in something being typed into. */
function typing(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return (
    target.isContentEditable ||
    target.tagName === "INPUT" ||
    target.tagName === "TEXTAREA" ||
    target.tagName === "SELECT"
  );
}

/**
 * The action a press asks for, or `null` when it asks for nothing here.
 *
 * Ctrl and Cmd are interchangeable and Alt is excluded, both for the same
 * reasons as the routes above. Two rules are this table's own:
 *
 * A letter is only safe here because an action lives on one screen, and even
 * then `Ctrl+C`, `Ctrl+V`, `Ctrl+X`, `Ctrl+A` and `Ctrl+Z` are the webview's
 * on every screen there is, so none of those five is ever taken.
 *
 * A binding with no modifier is a bare key, which is a key somebody may be in
 * the middle of using: `Escape` closes an open `<select>` and dismisses what a
 * field's own handler put up. So a bare binding is not read as a shortcut at
 * all while the keyboard is in a field — the press belongs to whatever is
 * being typed into.
 */
export function shortcutAction(e: KeyboardEvent): Action | null {
  if (e.altKey) return null;
  const mod = e.ctrlKey || e.metaKey;
  const bound = ACTION_SHORTCUTS.find(
    (a) => a.ctrl === mod && a.key.toLowerCase() === e.key.toLowerCase(),
  );
  if (!bound) return null;
  if (!bound.ctrl && typing(e.target)) return null;
  return bound.action;
}

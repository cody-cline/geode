import type { Route } from "./store";

/**
 * The keyboard's shortcuts to the screens in the sidebar.
 *
 * One table, read by the window handler that acts on a press and by the
 * sidebar that prints the hint, because a shortcut nobody can see is one
 * nobody uses and a hint that names a key nothing is bound to is worse than
 * no hint at all.
 *
 * Deliberately short. Five bindings beside the five things they reach are
 * five somebody finds; a page of them is a page nobody reads.
 */
const SHORTCUTS: { key: string; route: Route }[] = [
  // Numbered down the sidebar, so the hint and the order on screen agree.
  { key: "1", route: { view: "library" } },
  { key: "2", route: { view: "browse" } },
  { key: "3", route: { view: "plans" } },
  { key: "4", route: { view: "servers" } },
  // What every other desktop app binds settings to.
  { key: ",", route: { view: "settings" } },
];

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

/** What the sidebar prints beside a screen, or `null` if it has no shortcut. */
export function shortcutHint(view: Route["view"]): string | null {
  const bound = SHORTCUTS.find((s) => s.route.view === view);
  if (!bound) return null;
  return MAC ? `⌘${bound.key}` : `Ctrl+${bound.key}`;
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
  return SHORTCUTS.find((s) => s.key === e.key)?.route ?? null;
}

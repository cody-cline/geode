// The keyboard's shortcuts, and the one rule that makes them worth having:
// every binding is printed where it applies. A shortcut nobody can come across
// is dead weight, so a binding added to a table in `shortcuts.ts` and left out
// of the screen that can carry it out is a feature that ships invisible.
//
// Where that is depends on which kind it is. A **route** is reachable from
// anywhere and the sidebar is the only place all five are named, so that is
// where those are printed. An **action** belongs to one screen, has no sidebar
// row, and is printed on the thing it presses — which is why `PRINTED_IN`
// below says, per action, which file has to be the one printing it.
//
// Checked by reading the files, the way `retarget.test.mjs` checks what spans
// two files: `npm test` has no DOM and no React renderer, so the hint cannot be
// rendered and looked at here. Plain JavaScript for the same reason those are:
// the project has no test runner and no Node typings.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

async function source(path) {
  return readFile(new URL(path, import.meta.url), "utf8");
}

/** One table, read out of `shortcuts.ts` by name. */
async function table(name) {
  const text = await source("./shortcuts.ts");
  const start = text.indexOf(`const ${name}`);
  assert.ok(start > 0, `shortcuts.ts no longer declares ${name}`);
  return text.slice(start, text.indexOf("];", start));
}

/** The shortcuts that open a screen. */
async function routeBindings() {
  const found = [
    ...(await table("ROUTE_SHORTCUTS")).matchAll(
      /key:\s*"([^"]+)"[^}]*view:\s*"([^"]+)"/g,
    ),
  ].map(([, key, view]) => ({ key, view }));
  assert.ok(found.length > 0, "no route bindings could be read out of the table");
  return found;
}

/** The shortcuts that press something on the screen that owns them. */
async function actionBindings() {
  const found = [
    ...(await table("ACTION_SHORTCUTS")).matchAll(
      /action:\s*"([^"]+)",\s*key:\s*"([^"]+)",\s*ctrl:\s*(true|false)/g,
    ),
  ].map(([, action, key, ctrl]) => ({ action, key, ctrl: ctrl === "true" }));
  assert.ok(found.length > 0, "no action bindings could be read out of the table");
  return found;
}

/**
 * The screen that has to print each action's hint.
 *
 * The test's own map rather than a column in the table, because a file path is
 * nothing the app itself needs to know. An action added to the table with no
 * entry here fails below rather than slipping through unprinted.
 */
const PRINTED_IN = {
  focusFilter: ["../views/instance/ContentTab.tsx"],
  clearSelection: ["../views/instance/ContentTab.tsx"],
  // Three, and all three have to print it. An action can belong to more than
  // one screen — "new" means something on each of these and nothing anywhere
  // else — and the screen that binds it without printing it is the one where
  // the key is a secret.
  newThing: ["../views/Library.tsx", "../views/Plans.tsx", "../views/Servers.tsx"],
};

test("every binding is printed beside the screen it opens", async () => {
  const sidebar = await source("../components/Sidebar.tsx");

  for (const { key, view } of await routeBindings()) {
    assert.ok(
      sidebar.includes(`shortcutHint("${view}")`),
      `${key} opens ${view}, and the sidebar prints no hint for it — nobody would find it`,
    );
  }
});

test("every action is printed on every screen that can carry it out", async () => {
  for (const { key, action } of await actionBindings()) {
    const screens = PRINTED_IN[action];
    assert.ok(
      screens,
      `${key} is bound to ${action} and no screen is named as one that ` +
        "prints it — a hint with no home is a binding nobody comes across",
    );
    for (const where of screens) {
      assert.ok(
        (await source(where)).includes(`actionHint("${action}")`),
        `${key} is bound to ${action}, and ${where} prints no hint for it — ` +
          "nobody would find it there",
      );
    }
  }
});

// Ctrl+C, Ctrl+V, Ctrl+A and Ctrl+Z are the webview's, and a person typing in
// the server console expects all four to do what they always do. A route is
// live on every screen there is, the console included, so binding any letter at
// all is how one of them gets taken — and so none is.
test("no route binding is a letter, so none can take one of the webview's", async () => {
  for (const { key, view } of await routeBindings()) {
    assert.ok(
      !/^[a-z]$/i.test(key),
      `${view} is bound to a letter, which is how Ctrl+C or Ctrl+V stops working`,
    );
  }
});

// An action is dead on every screen but its own, so a letter is safe there —
// except these five, which the webview answers everywhere.
test("no action binding takes one of the webview's own letters", async () => {
  for (const { key, action } of await actionBindings()) {
    assert.ok(
      !/^[cvxaz]$/i.test(key),
      `${action} is bound to Ctrl+${key.toUpperCase()}, which is the ` +
        "webview's own — cut, copy, paste, select all and undo are not ours to take",
    );
  }
});

// A modifier types nothing into a field, which is what lets every route
// binding ignore where the keyboard is. A bare key has no such cover: Escape
// closes an open `<select>` and dismisses what a field's own handler put up, so
// a bare binding has to stand aside while somebody is typing.
test("a binding with no modifier does nothing in a text field", async () => {
  const text = await source("./shortcuts.ts");
  const matcher = text.indexOf("export function shortcutAction");
  assert.ok(matcher > 0, "shortcuts.ts no longer matches a press to an action");

  assert.match(
    text.slice(matcher).replace(/\s+/g, " "),
    /!\w*\.?ctrl && typing\(/,
    "a press with no modifier is read as a shortcut wherever the keyboard is, " +
      "so a bare binding now fires in the middle of somebody's typing",
  );

  const guard = text.slice(text.indexOf("function typing"), matcher);
  for (const tag of ["INPUT", "TEXTAREA", "SELECT"]) {
    assert.ok(
      guard.includes(`"${tag}"`),
      `a bare binding fires inside a <${tag.toLowerCase()}>, which belongs to whatever is in it`,
    );
  }
  assert.ok(
    guard.includes("isContentEditable"),
    "a bare binding fires inside an editable element, which belongs to whatever is in it",
  );
});

// Three screens claim back without being on top of anything: browsing inside
// an instance, adding to a plan, and an opened pack. The sidebar is still
// beside all three, still printing these keys. The first version of the gate
// asked whether *any* back handler was registered, which made every binding
// dead there while the same sidebar row clicked with the mouse still worked —
// a printed key that silently does nothing, which is the one thing the test
// above exists to prevent.
test("a screen that claims back does not switch the shortcuts off", async () => {
  const app = await source("../App.tsx");
  // From the press onwards, with the line breaks taken out so the shape is
  // readable whatever the formatter did to it.
  const gate = app
    .slice(app.indexOf("shortcutRoute(e)"))
    .replace(/\s+/g, " ");

  assert.ok(
    /backHandlers\.some\( *\(?\w+\)? *=> *\w+\.overlay *\)/.test(gate),
    "the shortcut gate no longer asks which back handlers are overlays, so an " +
      "ordinary screen that claims back now kills every printed binding",
  );
  assert.ok(
    !/backHandlers\.length/.test(gate),
    "the shortcut gate counts back handlers again, which is what made the " +
      "bindings dead inside an instance",
  );
});

// Every dialog in the app is drawn by `Modal`, so one call decides for all of
// them. It has to be the overlay kind: a dialog is drawn outside the view and
// would be left hanging over whatever a shortcut put behind it.
test("a dialog registers as an overlay, so it does hold the shortcuts", async () => {
  const modal = await source("../components/Modal.tsx");
  assert.ok(
    /useBackHandler\(\s*true,\s*onClose,\s*"overlay"\s*\)/.test(modal),
    "Modal no longer claims back as an overlay, so a shortcut pressed with a " +
      "dialog open would change the screen underneath it",
  );
});

// The same rule, kept by the screen rather than by `App` — and it needs one
// thing `App` does not. `Modal` listens on the document and closes on Escape,
// which pops the very handler this gate reads; captured at the window, the
// screen is asked first and still sees the dialog that owns the press.
//
// Read off the hook rather than off a screen. Four screens bind an action now
// and they all go through it, so this guards every one of them — where before
// it watched the single copy that happened to be written first, and a second
// screen could have registered in the bubble phase with nothing to say so.
test("a screen's own shortcut holds off while a dialog is open", async () => {
  const hook = await source("./useActionShortcut.ts");
  const start = hook.indexOf("const onShortcut");
  assert.ok(start > 0, "useActionShortcut no longer binds keys through `onShortcut`");
  const handler = hook.slice(start, hook.indexOf("return () =>", start)).replace(/\s+/g, " ");

  assert.ok(
    /backHandlers\.some\( *\(?\w+\)? *=> *\w+\.overlay *\)/.test(handler),
    "a screen's keys no longer stand aside for a dialog, so Escape now closes " +
      "one and clears the selection behind it in the same press",
  );
  assert.match(
    handler,
    /addEventListener\("keydown", onShortcut, true\)/,
    "the listener is no longer captured, so Modal closes and unregisters " +
      "itself before the gate above is read, and the press does both things",
  );

  // And that the screens actually go through it, since the gate is only worth
  // anything on the path the keys really take.
  for (const where of new Set(Object.values(PRINTED_IN).flat())) {
    assert.ok(
      (await source(where)).includes("useActionShortcut("),
      `${where} prints a shortcut hint but does not bind through the hook, so ` +
        "its own listener answers to none of the rules above",
    );
  }
});

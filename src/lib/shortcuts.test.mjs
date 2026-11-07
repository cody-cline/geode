// The window's keyboard shortcuts, and the one rule that makes them worth
// having: every binding is printed beside the thing it opens. A shortcut
// nobody can come across is dead weight, and the only place these five are
// discoverable is the sidebar — so a sixth added to the table and left out of
// `Sidebar.tsx` is a feature that ships invisible.
//
// Checked by reading the two files, the way `retarget.test.mjs` checks what
// spans two files: `npm test` has no DOM and no React renderer, so the hint
// cannot be rendered and looked at here. Plain JavaScript for the same reason
// those are: the project has no test runner and no Node typings.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

async function source(path) {
  return readFile(new URL(path, import.meta.url), "utf8");
}

/** The table itself, read out of `shortcuts.ts`. */
async function bindings() {
  const text = await source("./shortcuts.ts");
  const start = text.indexOf("const SHORTCUTS");
  assert.ok(start > 0, "shortcuts.ts no longer declares a table of bindings");
  const table = text.slice(start, text.indexOf("];", start));

  const found = [
    ...table.matchAll(/key:\s*"([^"]+)"[^}]*view:\s*"([^"]+)"/g),
  ].map(([, key, view]) => ({ key, view }));
  assert.ok(found.length > 0, "no bindings could be read out of the table");
  return found;
}

test("every binding is printed beside the screen it opens", async () => {
  const sidebar = await source("../components/Sidebar.tsx");

  for (const { key, view } of await bindings()) {
    assert.ok(
      sidebar.includes(`shortcutHint("${view}")`),
      `${key} opens ${view}, and the sidebar prints no hint for it — nobody would find it`,
    );
  }
});

// Ctrl+C, Ctrl+V, Ctrl+A and Ctrl+Z are the webview's, and a person typing in
// the server console expects all four to do what they always do. Binding any
// letter at all is how one of them gets taken, so none is.
test("no binding is a letter, so none can take one of the webview's", async () => {
  for (const { key, view } of await bindings()) {
    assert.ok(
      !/^[a-z]$/i.test(key),
      `${view} is bound to a letter, which is how Ctrl+C or Ctrl+V stops working`,
    );
  }
});

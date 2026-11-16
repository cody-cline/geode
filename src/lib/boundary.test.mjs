// The two error boundaries, and why a test has to hold them.
//
// React unmounts the entire tree when a render throws and nothing catches it.
// On the web that leaves a blank tab and an address bar to reload from; Geode
// is a Tauri window, so it leaves a rectangle of `--bg` with no address bar,
// no reload, no menu and no way out but killing the process. For months there
// was no boundary anywhere in this app and nothing noticed, because the only
// thing that reveals it is a crash — which is exactly the shape of defect a
// test exists for.
//
// Both placements are load-bearing and they do different jobs. The one inside
// `<main>` is keyed by route, which is the whole recovery: a screen that
// throws leaves the sidebar alive, and going anywhere else gives the boundary
// a new key, so React remounts it and the next screen draws. Remove the key
// and the error panel becomes permanent until the process is restarted, which
// is most of the way back to where this started. The outer one catches a
// throw in the frame itself — the sidebar, the dock, a toast — where there is
// nothing left to navigate with.
//
// Checked by reading the files, like `shortcuts.test.mjs` and
// `retarget.test.mjs` beside it: `npm test` has no DOM and no React renderer,
// so the boundary cannot be made to catch anything here.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

async function source(path) {
  return readFile(new URL(path, import.meta.url), "utf8");
}

test("a screen that throws cannot take the window with it", async () => {
  const app = await source("../App.tsx");
  assert.match(app, /import ErrorBoundary from/);

  // Inside `<main>`, so the sidebar survives it. Outside `<main>` the panel
  // would replace the navigation it tells people to use.
  const main = app.indexOf("<main");
  const closed = app.indexOf("</main>");
  assert.ok(main > 0 && closed > main, "App.tsx no longer has a <main>");
  const inside = app.slice(main, closed);
  assert.match(inside, /<ErrorBoundary/, "the routed screens are not inside a boundary");

  // Keyed by route. Without this the panel outlives the screen that threw.
  assert.match(inside, /<ErrorBoundary key=\{routeKey\(route\)\}>/);
  // And the key tells two instances apart, or a throw drawing one leaves the
  // other showing the error when it is picked.
  assert.match(app, /function routeKey[\s\S]{0,400}?"id" in route/);
});

test("a throw in the frame cannot take the window with it either", async () => {
  const main = await source("../main.tsx");
  assert.match(main, /import ErrorBoundary from/);
  // Around everything, including the palette page, which is also a render.
  assert.match(main, /<ErrorBoundary whole>/);
});

test("the panel says what is true and names a way forward", async () => {
  const boundary = await source("../components/ErrorBoundary.tsx");

  // The claim a person needs first, and the one this app makes everywhere
  // else it reports a failure: the data is not what broke.
  assert.match(boundary, /Nothing has been changed or removed/);

  // Two different ways out, because the two placements leave different things
  // on screen. Offering the sidebar when there is no sidebar would be the
  // same lie as the empty states this app has spent a month removing.
  assert.match(boundary, /pick another screen in the sidebar/);
  assert.match(boundary, /Reopen Geode/);

  // The error's own words. Without them a bug report says "it went blank".
  assert.match(boundary, /error\.message/);

  // No "Try again". Every other failure panel in this app has one because a
  // read can be re-run; a render that threw would throw the same way, and a
  // button that does nothing is worse than no button. Measured on what is
  // drawn, not on the file: the comment above `render` says this in words and
  // would otherwise be the thing failing the test that holds it.
  const drawn = boundary.slice(boundary.indexOf("render() {"));
  assert.ok(
    !/Try again/.test(drawn),
    "a retry on a render that threw would repeat the throw",
  );
});

// Run with `npm test` (Node 22.18 or newer, which strips the types from
// update.ts itself). Plain JavaScript on purpose, the same as share.test.mjs:
// the project has no test runner and no Node typings, and TypeScript leaves
// .mjs files out of its check.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

import {
  availableToast,
  checkFailedLine,
  panelHint,
  panelLine,
  putOffLine,
  updateLine,
} from "./update.ts";

const status = (extra = {}) => ({
  current: "0.1.0",
  configured: true,
  available: null,
  notes: null,
  skipped: null,
  blocked: null,
  ...extra,
});

test("a build with no key still says which version it is running, and that it will not install one", () => {
  const line = updateLine(status({ configured: false }));
  assert.equal(line.tone, "ok");
  // A check did run — off the public releases API, which needs no key — so
  // this says what it found. It used to say only "this build cannot update
  // itself", which answered a question nobody had asked instead of the one
  // they had.
  assert.match(line.title, /0\.1\.0 is up to date/);
  assert.match(line.detail, /will not download or replace anything/);
  assert.match(line.detail, /releases page/);
});

test("a build with no key is told what is out, and that fetching it is its own job", () => {
  const line = updateLine(status({ configured: false, available: "0.2.0" }));
  assert.match(line.title, /0\.2\.0/);
  assert.match(line.detail, /0\.1\.0/);
  // The whole point of this half: say what is out. And the whole point of the
  // key: this is the one mode that must not claim it will install anything.
  assert.match(line.detail, /will not download or replace anything/);
  assert.match(line.detail, /run it yourself/);
  assert.doesNotMatch(line.title, /ready to install/);
  assert.doesNotMatch(line.detail, /Install downloads it/);
  assert.doesNotMatch(line.detail, /restarts/);
});

test("a waiting update names both versions and says what Install would do", () => {
  const line = updateLine(status({ available: "0.2.0" }));
  assert.match(line.title, /0\.2\.0/);
  assert.match(line.detail, /0\.1\.0/);
  assert.match(line.detail, /signed/);
  assert.match(line.detail, /restarts/);
});

test("nothing newer reads as up to date, on the version actually running", () => {
  const line = updateLine(status());
  assert.equal(line.tone, "ok");
  assert.match(line.title, /0\.1\.0 is up to date/);
});

test("a skipped version is said out loud, so a skip is not a one-way door", () => {
  const line = updateLine(status({ skipped: "0.2.0" }));
  assert.match(line.title, /0\.2\.0 is being skipped/);
  assert.match(line.detail, /stop skipping/);
});

test("an update waiting outranks a note about an older one that was skipped", () => {
  const line = updateLine(status({ available: "0.3.0", skipped: "0.2.0" }));
  assert.match(line.title, /0\.3\.0 is ready/);
});

test("the up-to-date line owns up to the check Geode makes at every start", () => {
  const line = updateLine(status());
  // It used to say only that Geode looks when this screen opens and when you
  // press Check again, which read as "it does not contact GitHub unless I am
  // on this screen" — and a cold start had already asked.
  assert.match(line.detail, /starts/);
  // Still true and still worth saying: nothing is on a timer.
  assert.match(line.detail, /schedule/);
});

test('"Not now" stops the panel describing an Install button it has hidden', () => {
  const line = putOffLine("0.2.0", true);
  assert.match(line.title, /0\.2\.0/);
  // The bug: the ready-to-install line stayed on screen with the buttons gone,
  // so the panel explained an Install nobody could find.
  assert.doesNotMatch(line.title, /ready to install/);
  assert.doesNotMatch(line.detail, /Install downloads it/);
  // And there has to be a way back that is not a fresh network check.
  assert.match(line.detail, /Show it again/);
  assert.match(line.detail, /next time/);
});

test('"Not now" in a build with no key names the link that comes back, not Install', () => {
  // Reachable only since a keyless build started comparing versions: before
  // that `available` was always null without a key, so this line could not be
  // drawn there at all. What "Show it again" brings back in that panel is the
  // releases-page link, and naming Install would name the one control it
  // deliberately does not have.
  const line = putOffLine("0.2.0", false);
  assert.match(line.detail, /Show it again/);
  assert.match(line.detail, /releases page/);
  assert.doesNotMatch(line.detail, /Install/);
});

test("a check that failed says so, rather than that it is still looking", () => {
  const line = checkFailedLine("error sending request for url (https://github.com)");
  assert.equal(line.tone, "warn");
  assert.match(line.title, /Could not check/);
  assert.doesNotMatch(line.title, /Looking for/);
  // What went wrong, ended as a sentence, plus both ways out — the panel used
  // to offer neither, because every button was gated on a status it never got.
  assert.match(line.detail, /error sending request for url \(https:\/\/github\.com\)\./);
  assert.match(line.detail, /Check again/);
  assert.match(line.detail, /releases page/);
});

test("a check that failed outranks the answer the last one gave", () => {
  // The bug this orders around: a failure does not clear the status, so a
  // second check that cannot reach GitHub left "up to date" on screen — a
  // green tick over a question nobody answered. Rate limited, offline, or a
  // 404 before the first release exists all land here.
  const line = panelLine({
    status: status(),
    failure: "returned HTTP 403 Forbidden",
    putOff: false,
  });
  assert.equal(line.tone, "warn");
  assert.match(line.title, /Could not check/);
  assert.doesNotMatch(line.title, /up to date/);
  assert.doesNotMatch(line.detail, /Nothing newer has been published/);
});

test("the panel reads as still looking only before any answer has arrived", () => {
  const looking = panelLine({ status: null, failure: null, putOff: false });
  assert.match(looking.title, /Looking for a newer Geode/);

  // And each of the other three states in its own order.
  assert.match(
    panelLine({ status: status({ available: "0.2.0" }), failure: null, putOff: true }).title,
    /waiting until you want it/,
  );
  assert.match(
    panelLine({ status: status({ available: "0.2.0" }), failure: null, putOff: false }).title,
    /ready to install/,
  );
  assert.match(
    panelLine({ status: status(), failure: null, putOff: false }).title,
    /up to date/,
  );
});

test("the hint says nothing at all until a check has said which build this is", () => {
  // `configured` is only ever known from a status, and a failed check leaves
  // none — offline, rate limited, or the 404 the releases API answers until
  // the first release exists. The bug was choosing the signed paragraph for
  // that case, so a build with no key (which is every build shipped so far)
  // read that Geode "replaces nothing until you press Install" and "replaces
  // the AppImage you are running" underneath a panel with no Install on it.
  assert.equal(panelHint(null), null);
});

test("each half of the feature gets the paragraph that describes it", () => {
  const signed = panelHint(status());
  assert.match(signed, /press Install/);
  assert.match(signed, /AppImage you are running/);

  const notify = panelHint(status({ configured: false }));
  assert.match(notify, /no updater/);
  assert.match(notify, /nothing here downloads, replaces or restarts/);
  assert.doesNotMatch(notify, /press Install/);
  assert.doesNotMatch(notify, /AppImage you are running/);
});

test("the toast at start points at a button that installs only when there is one", () => {
  const signed = availableToast(status({ available: "0.2.0" }));
  assert.match(signed.title, /0\.2\.0 is available/);
  assert.match(signed.body, /installing it would do/);

  const notify = availableToast(status({ available: "0.2.0", configured: false }));
  assert.match(notify.title, /0\.2\.0 is available/);
  assert.match(notify.body, /cannot install it/);
  assert.match(notify.body, /releases page/);
  assert.doesNotMatch(notify.body, /installing it would do/);
});

// The invariant below lives in a file `npm test` cannot run — there is no DOM
// and no React renderer under it — so it is checked where it is written, the
// way blocked.test.mjs checks the version list's Install button.
test("a build that cannot verify an update is never given a button that installs one", async () => {
  const settings = await readFile(new URL("../views/Settings.tsx", import.meta.url), "utf8");

  const before = settings.split("void install()")[0];
  assert.ok(before.length < settings.length, "Settings no longer installs an update");
  // Whatever guards the Install button has to be the key: with no key there is
  // no signature to check a download against and no plugin to check one with,
  // so a build without one must not be able to start an install at all. The
  // backend refuses it too, but a button that only ever errors is its own bug.
  // Matched with its opening brace, because the negation is the mistake this
  // is here to catch and `status.configured` on its own is a substring of
  // `!status.configured`: searching for the bare name found the same
  // characters inside an inverted guard and read the arms as unchanged, so
  // flipping one character handed the keyless build the Install button with
  // this test still green.
  const guard = before.lastIndexOf("{status.configured ? (");
  assert.ok(
    guard > 0,
    "the Install button is not inside a `{status.configured ? (` branch, so a build with " +
      "no updater key can press it",
  );

  // And the other half of the same ternary opens the releases page instead.
  const after = settings.slice(settings.indexOf("void install()"));
  assert.match(
    after.slice(0, after.indexOf("Not now")),
    /openUrl\(RELEASES_URL\)/,
    "a build with no key is offered nothing in place of Install",
  );
});

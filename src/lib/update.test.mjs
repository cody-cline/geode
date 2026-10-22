// Run with `npm test` (Node 22.18 or newer, which strips the types from
// update.ts itself). Plain JavaScript on purpose, the same as share.test.mjs:
// the project has no test runner and no Node typings, and TypeScript leaves
// .mjs files out of its check.

import { test } from "node:test";
import assert from "node:assert/strict";

import { checkFailedLine, putOffLine, updateLine } from "./update.ts";

const status = (extra = {}) => ({
  current: "0.1.0",
  configured: true,
  available: null,
  notes: null,
  skipped: null,
  blocked: null,
  ...extra,
});

test("a build with no key says so instead of offering a button that cannot work", () => {
  const line = updateLine(status({ configured: false }));
  assert.equal(line.tone, "info");
  assert.match(line.title, /cannot update itself/);
  assert.match(line.detail, /releases page/);
  // It must not claim to know anything about versions: nothing was checked.
  assert.doesNotMatch(line.title, /up to date/);
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
  const line = putOffLine("0.2.0");
  assert.match(line.title, /0\.2\.0/);
  // The bug: the ready-to-install line stayed on screen with the buttons gone,
  // so the panel explained an Install nobody could find.
  assert.doesNotMatch(line.title, /ready to install/);
  assert.doesNotMatch(line.detail, /Install downloads it/);
  // And there has to be a way back that is not a fresh network check.
  assert.match(line.detail, /Show it again/);
  assert.match(line.detail, /next time/);
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

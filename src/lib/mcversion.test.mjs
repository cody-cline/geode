// The arithmetic behind "this world was saved by a newer Minecraft than the
// instance holding it", which is the one case where opening a world is how a
// world gets lost.
//
// Worth its own file because it is a rare thing on this frontend: logic that
// can be run rather than read. The rest of `src/lib/*.test.mjs` checks
// invariants by grepping `.tsx` source, because `npm test` has no DOM and
// cannot import JSX — so a warning living in a component could only ever have
// its wording pinned, never its sum. `mcversion.ts` has no JSX, so Node strips
// its types and imports it, the way `share.test.mjs` imports `share.ts`.
//
// The cases that matter are the ones that look like they sort and do not.

import { test } from "node:test";
import assert from "node:assert/strict";

import { savedByNewer } from "./mcversion.ts";

test("a year-based version outranks the old scheme", () => {
  // Minecraft 26.3 really is newer than 1.21.11. The first segment decides.
  assert.equal(savedByNewer("26.3", "1.21.11"), true);
  assert.equal(savedByNewer("1.21.11", "26.3"), false);
});

test("segments compare as numbers, not as text", () => {
  // The whole reason this is not `a > b`: the string "1.21.9" sorts above
  // "1.21.10", which would have called the newer world the older one.
  assert.equal(savedByNewer("1.21.10", "1.21.9"), true);
  assert.equal(savedByNewer("1.21.9", "1.21.10"), false);
});

test("a missing segment is a zero, so 1.21 and 1.21.0 are one version", () => {
  assert.equal(savedByNewer("1.21", "1.21.0"), false);
  assert.equal(savedByNewer("1.21.0", "1.21"), false);
  assert.equal(savedByNewer("1.21.1", "1.21"), true);
});

test("a version is not newer than itself, and older is not newer", () => {
  assert.equal(savedByNewer("1.21.1", "1.21.1"), false);
  assert.equal(savedByNewer("1.20.4", "1.21.1"), false);
});

// A warning nobody can act on is worse than silence, so anything that is not a
// plain release answers "no" rather than guessing an order for it.
test("a snapshot or anything unparseable is never called newer", () => {
  for (const odd of ["26w14a", "1.21-pre2", "1.21.1-rc1", "", "fabric", "1.21.x"]) {
    assert.equal(savedByNewer(odd, "1.21.1"), false, `${odd} vs a release`);
    assert.equal(savedByNewer("1.21.1", odd), false, `a release vs ${odd}`);
  }
});

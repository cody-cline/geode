// The frontend half of `instance::pack_update`: the preview a player says yes
// to before an update writes anything.
//
// The invariant here lives in a file `npm test` cannot run — there is no DOM
// and no React renderer under it — so it is checked where it is written, the
// way `blocked.test.mjs` checks the version list. Plain JavaScript for the same
// reason that file is: the project has no test runner and no Node typings.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

async function source(path) {
  return readFile(new URL(path, import.meta.url), "utf8");
}

// `takenOver` is the player's own content this version of the pack claims: a
// file of theirs at a name the pack now writes is written over, and their own
// copy of a mod it now ships is turned off. So a preview that promises
// "nothing of yours is deleted" beside a list of files about to be overwritten
// is telling somebody their file is safe on the screen where they agree to
// lose it. The promise only holds when that list is empty.
test("the preview only promises nothing of yours is deleted when nothing of theirs is taken over", async () => {
  const modal = await source("../components/PackUpdateModal.tsx");

  const start = modal.indexOf("styles.keeps");
  assert.ok(start > 0, "the preview no longer says what an update keeps");
  const keeps = modal.slice(start, modal.indexOf("styles.facts"));

  const promise = keeps.indexOf("nothing of yours is deleted");
  assert.ok(promise > 0, "the preview no longer says what is safe at all");
  assert.match(
    keeps.slice(0, promise),
    /takenOver\.length === 0/,
    "the promise is made unconditionally, including for an update that writes the pack's copy over a file of theirs",
  );
});

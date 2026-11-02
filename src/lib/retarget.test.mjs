// The frontend half of `instance::retarget`: the panel that moves an instance
// to another Minecraft version or loader.
//
// The invariant here lives in a file `npm test` cannot run — there is no DOM
// and no React renderer under it — so it is checked where it is written, the
// way `pack-update.test.mjs` checks the preview. Plain JavaScript for the same
// reason that file is: the project has no test runner and no Node typings.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

async function source(path) {
  return readFile(new URL(path, import.meta.url), "utf8");
}

// The Mod loader section above the panel holds its own copy of the loader and
// its build, taken when the tab opened, and the save bar appears whenever that
// copy disagrees with the instance. A re-target that landed here moves both on
// the instance and leaves that copy behind — so the save bar comes up offering
// to write the *old* loader back over the one every jar was just rebuilt for,
// which is a worse instance than the one the person started with and one click
// away. The panel has to put that copy back in step itself.
test("a re-target that lands here puts the loader section back in step", async () => {
  const tab = await source("../views/instance/SettingsTab.tsx");

  const start = tab.indexOf("api.retargetInstance(");
  assert.ok(start > 0, "the settings tab no longer re-targets an instance");
  // To the end of the handler, which is where its own catch begins.
  const handler = tab.slice(start, tab.indexOf("Could not change the target", start));

  assert.match(
    handler,
    /setLoader\(/,
    "a re-target applied to this instance leaves the loader section showing the old loader, and the save bar will write it back",
  );
  assert.match(
    handler,
    /!report\.appliedToCopy/,
    "the loader section is put back in step unconditionally, including for a move that landed on a copy and left this instance alone",
  );
});

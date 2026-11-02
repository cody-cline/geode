// The frontend half of `instance::retarget`: the dialog that moves an instance
// to another Minecraft version or loader, at the end of the preview of what
// that would cost.
//
// The invariant here lives in files `npm test` cannot run — there is no DOM
// and no React renderer under it — so it is checked where it is written, the
// way `pack-update.test.mjs` checks the preview. Plain JavaScript for the same
// reason that file is: the project has no test runner and no Node typings.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

async function source(path) {
  return readFile(new URL(path, import.meta.url), "utf8");
}

// The Mod loader section of the settings tab holds its own copy of the loader
// and its build, taken when the tab opened, and the save bar appears whenever
// that copy disagrees with the instance. A re-target that landed here moves
// both on the instance and leaves that copy behind — so the save bar comes up
// offering to write the *old* loader back over the one every jar was just
// rebuilt for, which is a worse instance than the one the person started with
// and one click away.
//
// Applying happens in the dialog, which does not own that copy, so keeping it
// in step spans two files: the dialog hands the report back, and the tab puts
// its copy in step for a move that landed here. Both halves are checked,
// because either one missing is the same bug.
test("a re-target that lands here puts the loader section back in step", async () => {
  const modal = await source("../components/ReplanModal.tsx");

  assert.ok(
    modal.includes("api.retargetInstance("),
    "the replan dialog no longer re-targets an instance",
  );
  assert.match(
    modal,
    /onApplied\(report\)/,
    "the dialog applies a target without handing the report back, so the screen behind it still shows the loader the instance had before",
  );

  const tab = await source("../views/instance/SettingsTab.tsx");

  assert.match(
    tab,
    /onApplied=\{retargetLanded\}/,
    "the settings tab is never told a re-target landed, so its loader section keeps the loader the instance had before",
  );

  const start = tab.indexOf("function retargetLanded(");
  assert.ok(start > 0, "the settings tab no longer handles a landed re-target");
  // To the end of the handler, which is the first closing brace back at its
  // own indent inside the component.
  const handler = tab.slice(start, tab.indexOf("\n  }", start));

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

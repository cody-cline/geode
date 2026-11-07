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

  // Both halves of the section, because the save bar's `dirty` check reads
  // both: `loader !== instance.loader || loaderVersion !== instance.loaderVersion`.
  // A move that put the loader back in step and left its build behind is the
  // same bug with a narrower blast radius — Save would write the old Fabric
  // build over the one every jar was just rebuilt for.
  assert.match(
    handler,
    /setLoader\(/,
    "a re-target applied to this instance leaves the loader section showing the old loader, and the save bar will write it back",
  );
  assert.match(
    handler,
    /setLoaderVersion\(/,
    "a re-target applied to this instance leaves the loader section showing the old loader build, and the save bar will write it back",
  );
  assert.match(
    handler,
    /!report\.appliedToCopy/,
    "the loader section is put back in step unconditionally, including for a move that landed on a copy and left this instance alone",
  );
});

// The warning under "This instance" is the only place the dialog says what a
// move costs, and it is shown without knowing whether there is anything to
// lose: `ReplanModal` reads no world data at all, so it cannot state as a fact
// that this instance has worlds. It did — "This cannot be undone for the
// worlds in Survival" over an instance with an empty `saves`.
//
// Nor is the target necessarily newer. A re-target goes either way, and a
// warning that only describes an upgrade is wrong on half of them.
test("the world warning claims only what the dialog knows", async () => {
  const modal = await source("../components/ReplanModal.tsx");

  const start = modal.indexOf('landOn === "here" && (');
  assert.ok(start > 0, "the dialog no longer warns about the choice that cannot be undone");
  const warning = modal.slice(start, modal.indexOf("\n                )}", start));

  assert.ok(
    !/worlds in \$\{instance\.name\}/.test(warning),
    "the warning states that this instance has worlds, which nothing here read",
  );
  assert.match(
    warning,
    /Any world in this instance/,
    "the warning asserts there are worlds rather than saying what happens to any there are",
  );
  assert.match(
    warning,
    /older/,
    "the warning only describes moving to a newer Minecraft, and a re-target goes either way",
  );
});

// The primary button names the operation the "Apply to" select is set to. It
// was the literal "Move to Minecraft …" whatever the select said, over a
// default that copies — so the one action this dialog calls impossible to undo
// was the label on the choice that leaves the instance alone, and whoever
// wanted either of the two read the other's word and pressed it.
test("the primary button names what the chosen mode will do", async () => {
  const modal = await source("../components/ReplanModal.tsx");

  const start = modal.indexOf("function applyVerb(");
  assert.ok(start > 0, "the button's label is no longer worked out from the mode");
  const verb = modal.slice(start, modal.indexOf("\n}", start));

  assert.match(
    verb,
    /landOn === "here"/,
    "a move applied to this instance is not named a move, so the one action that cannot be undone is labelled as something else",
  );
  assert.match(
    verb,
    /landOn === "copy"/,
    "applying to a copy is not named a copy, so somebody who asked for one is told an instance is about to be replaced",
  );

  assert.match(
    modal,
    /\{applyVerb\(landOn\)\} Minecraft \{target\.gameVersion\}/,
    "the primary button names an operation of its own rather than the one the Apply to select is set to",
  );
});

// Minecraft's version numbers stopped sorting — 26.3 is newer than 1.21.11 —
// so a rail of candidates with no dates is a ranking nobody can check. The
// date is read from the version manifest, never from the version string and
// never from a table in here: what Mojang shipped when is Mojang's to say.
test("a candidate row dates itself from the manifest", async () => {
  for (const path of ["../components/ReplanModal.tsx", "../views/PlanView.tsx"]) {
    const screen = await source(path);
    assert.match(
      screen,
      /api\s*\n?\s*\.listGameVersions\(/,
      `${path} no longer asks what the published Minecraft versions are, so the dates beside its candidates come from somewhere this repo invented`,
    );
    assert.match(
      screen,
      /dates\.get\(option\.gameVersion\)/,
      `${path} draws a candidate without the release date that is the only thing ranking it for a reader`,
    );
  }
});

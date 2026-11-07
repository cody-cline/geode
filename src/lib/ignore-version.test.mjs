// The frontend half of the per-mod "ignore the Minecraft version" override:
// the box in Browse that makes the mod findable in the first place, and the
// line a re-target writes about a mod it carried over on a mismatched build.
//
// Both invariants live in files `npm test` cannot run — there is no DOM and no
// React renderer under it — so they are checked where they are written, the way
// `retarget.test.mjs` and `pack-update.test.mjs` check theirs. Plain JavaScript
// for the same reason those are: the project has no test runner and no Node
// typings.
//
// `tsc` already covers the hand-written mirror in `types.ts` here, which is why
// nothing below checks it: every new field is *read* somewhere in a `.tsx`, so
// a field left out of the mirror is a type error rather than a silent
// `undefined`. What `tsc` cannot see is whether a screen's words still match
// what the code asked for, and that is the whole of what this file pins.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

async function source(path) {
  return readFile(new URL(path, import.meta.url), "utf8");
}

// Searching is the real barrier, not the Install button. Browse filters its
// query to the instance's Minecraft version, so the one mod a person wants the
// override for is the one mod they cannot find — ticking a box that only
// changed how an install behaved would leave the feature unreachable from the
// screen where mods are added.
//
// And the choice has to travel from the search to the install, or the mod lands
// with nothing recorded against it and the next re-target turns it off.
test("ticking the box widens the search and travels into the install", async () => {
  const browse = await source("../views/Browse.tsx");

  // The query's own version, which is what `runSearch` passes as `gameVersion`.
  const start = browse.indexOf("const gameVersion = lockedInstance");
  assert.ok(start > 0, "Browse no longer derives the version it searches by");
  const derived = browse.slice(start, browse.indexOf("const loader = ", start));

  assert.match(
    derived,
    /useAnyVersion/,
    "the search is still filtered to the instance's Minecraft version when the box is ticked, so the mods it was ticked to find are not in the list at all",
  );
  assert.match(
    derived,
    /\?\s*null/,
    "the box does not drop the version from the query, so it widens nothing",
  );

  // The install call, which is the only place the choice can be recorded.
  const call = browse.indexOf("await api.installContent(");
  assert.ok(call > 0, "Browse no longer installs content");
  const args = browse.slice(call, browse.indexOf(");", call));
  assert.match(
    args,
    /useAnyVersion/,
    "a mod installed from the widened list is recorded with no override, so the next update check or re-target turns it off again",
  );

  // The claim under "Compatibility" is the screen's own statement about what it
  // filtered. Left unconditional it says "Filtering to what works with
  // Minecraft 1.21.4" over a list that was never filtered by version — a check
  // nothing ran.
  const section = browse.indexOf(">Compatibility<");
  assert.ok(section > 0, "Browse no longer says what it filtered to");
  const compat = browse.slice(section, browse.indexOf("</section>", section));

  assert.match(
    compat,
    /useAnyVersion \?/,
    "the compatibility note claims the list is filtered to this instance's Minecraft version whether or not it was",
  );
  assert.match(
    compat,
    /Filtering to what works with/,
    "the note no longer says what it filtered to in the ordinary case",
  );
});

// A mod carried across on a build for another version is none of the three
// things a re-target report already distinguishes. It is genuinely re-pointed —
// a file was fetched and the record names it — but `viaVersion` alone reads as
// the neighbouring patch the resolver deliberately rates as usable, and this is
// not that. Folding the two together dresses one person's override up as the
// resolver's judgement, which is the same lie `RepointedMod.viaVersion` exists
// to avoid telling about rounding a build off to the target.
test("a carried mod is counted apart from a near-version fit", async () => {
  const modal = await source("../components/ReplanModal.tsx");

  const start = modal.indexOf("const near = report.repointed.filter(");
  assert.ok(start > 0, "the re-target summary no longer counts near-version builds");
  const line = modal.slice(start, modal.indexOf("\n}", start));

  assert.match(
    line,
    /!one\.ignoredGameVersion/,
    "a mod carried over on an overridden build is counted among the ones the preview rated as fitting, so the summary credits the resolver with a choice the person made",
  );
  assert.match(
    line,
    /ignoredGameVersion\)/,
    "the summary never mentions the mods it carried over, so a re-target that overrode the version reads as a clean move",
  );

  // Tone, because a tick saying it all went fine is the screen deciding for
  // somebody that a mod which may not load is nothing to look at.
  const tone = line.slice(line.indexOf("tone:"));
  assert.match(
    tone,
    /anyway\.length > 0/,
    "a re-target that carried a mod onto a mismatched build reports success without reservation",
  );

  // And the mods themselves are named, not just counted: the report says which
  // build each one landed on and that whether it runs is not knowable here.
  assert.match(
    modal,
    /Carried over anyway/,
    "the report counts the carried mods without naming them, so nobody can tell which jar to turn off when the game stops starting",
  );
  // Pinned positively. Asserting the absence of phrases nobody would write
  // ("will run", "is compatible") cannot fail against any plausible
  // implementation, which is cover rather than a check. What has to hold is
  // that the section still says the two honest things it can: that whether the
  // mod runs is not knowable here, and which mod to turn off first when the
  // game stops starting.
  assert.match(
    modal,
    /whether it runs is not something\s+Geode can tell you/,
    "the carried-over section stopped saying that whether the mod runs is unknowable here, so the screen implies a check nothing ran",
  );
  assert.match(
    modal,
    /first thing to turn off/,
    "the carried-over section stopped telling somebody which mod to turn off first when the game will not start",
  );
});

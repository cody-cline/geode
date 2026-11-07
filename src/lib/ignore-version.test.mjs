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

// The other half of the same dialog, read *before* anything moves. A carried
// mod is `missing` at the target like every other mod with nothing published
// there — the resolver counts it that way on purpose, so that a ticked box
// cannot move which targets are offered or in what order — so a preview that
// read the fit alone listed it as about to be left behind, and then applying
// that very target carried it over. A screen and an install telling a person
// two different things is the bug this feature was fixed for; these are the
// three places the screen can tell it again.
test("the preview says a carried mod is carried, not left behind", async () => {
  const modal = await source("../components/ReplanModal.tsx");

  // The list the dialog heads "Would be left behind".
  const start = modal.indexOf("const blockers = (target?.fits ?? [])");
  assert.ok(start > 0, "the preview no longer works out its blockers");
  const blockers = modal.slice(start, modal.indexOf(";", start));
  assert.match(
    blockers,
    /!f\.carried/,
    "a mod a ticked box will carry over is listed as one that would be left behind, which is the opposite of what applying does with it",
  );

  // The count on every target card, which is what somebody reads before
  // choosing one. `missing` includes the carried mods; `blocked` is the number
  // being asked for.
  const line = modal.indexOf("function coverageLine");
  assert.ok(line > 0, "the target cards no longer say how much they cover");
  const coverage = modal.slice(line, modal.indexOf("\n}", line));
  assert.match(
    coverage,
    /\$\{option\.blocked\} left out/,
    "the card counts a carried mod among the ones left out, so the number somebody is asked to accept is too high",
  );
  assert.match(
    coverage,
    /option\.carried > 0/,
    "the card never mentions the mods that would be carried over, so a target that carries one reads as a clean move",
  );

  // And the mods themselves, named with the two things that are true of one
  // and nothing that is not. Which build a carry lands on is deliberately not
  // claimed: `Support` holds only releases for the loaders Geode installs, and
  // a blocked CurseForge file is dropped later still, so a version named here
  // could be one the install never fetches.
  const section = modal.indexOf("Would be carried over");
  assert.ok(section > 0, "the preview never names the mods it would carry over");
  const carried = modal.slice(section, modal.indexOf("</section>", section));

  assert.match(
    carried,
    /ignores the\s+Minecraft version/,
    "the preview does not say why this mod is being carried rather than turned off",
  );
  assert.match(
    carried,
    /newest build its author has\s+published for/,
    "the preview does not say what would be installed for a carried mod",
  );
  assert.match(
    carried,
    /one\.elsewhere/,
    "the preview drops where a carried mod is actually published, which is the only other thing it can truthfully say about one",
  );
  assert.match(
    carried,
    /not something Geode can tell you/,
    "the preview implies a carried mod will run, which is a check nothing ran",
  );
});

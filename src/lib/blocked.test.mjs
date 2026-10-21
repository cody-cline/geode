// The frontend half of `install::blocked`: a CurseForge file whose author
// switched third-party downloads off, and that author's own Modrinth release of
// the identical bytes.
//
// Both invariants here live in files `npm test` cannot run — there is no DOM and
// no React renderer under it — so they are checked where they are written, the
// way `share.test.mjs` checks the Share tab's handlers. Plain JavaScript for the
// same reason that file is: the project has no test runner and no Node typings.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

async function source(path) {
  return readFile(new URL(path, import.meta.url), "utf8");
}

// The substitution's one user-facing sentence is written in the Rust core,
// because a pack import, a shared pack's reference and an ordinary install all
// owe the same one and their own results come back in three different shapes.
// It travels on its own topic, and a topic nothing subscribes to is a payload
// the webview drops — which is a user who is never told their CurseForge mod
// was replaced.
test("the notice topic has a subscriber that puts it on screen", async () => {
  const store = await source("./store.ts");

  const after = store.split('listen<Notice>("geode://notice"')[1];
  assert.ok(
    after,
    "nothing in the store subscribes to geode://notice, so the sentence the core writes reaches nobody",
  );

  const handler = after.slice(0, after.indexOf("\n    });"));
  assert.match(
    handler,
    /toast\(/,
    "the notice is heard and then dropped: it has to end up somewhere the user looks",
  );
  for (const field of ["payload.title", "payload.body"]) {
    assert.ok(
      handler.includes(field),
      `the toast leaves out ${field}, and the core wrote both halves of the sentence`,
    );
  }
});

// A blocked file is still worth offering: the core looks for the author's own
// Modrinth release of the identical bytes and installs that when the hashes
// match. Greying the button out is how that never got asked for — and the
// version list is the one place a particular build is chosen, so it is the only
// route an ordinary install has to it. A pack is the exception: it is fetched as
// a file rather than found by hash.
test("the version list offers to install a file whose author blocked it", async () => {
  const panel = await source("../components/ProjectPanel.tsx");

  const after = panel.split("onRequestInstall(project, version.id)")[1];
  assert.ok(after, "the version list no longer installs a chosen build");
  const button = after.slice(0, after.indexOf("\n                      >"));

  const disabled = /disabled=\{([^}]*(?:\}[^}]*)*)\}\s*$/m.exec(button);
  if (disabled) {
    assert.match(
      disabled[1],
      /isModpack/,
      `a blocked file is refused before the core is ever asked: disabled={${disabled[1]}}`,
    );
  }
});

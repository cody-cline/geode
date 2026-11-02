import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

// types.ts is a hand-written mirror of the Rust serde structs and nothing
// cross-checks it: leave a field out and the frontend reads `undefined`, which
// is falsy, so a toggle renders unchecked and tells the user a feature is off
// while Rust has it on. `tsc` cannot catch that, because the mistake is the
// interface itself rather than any use of it — the same reason update.test.mjs
// pins a Settings.tsx invariant by reading the file as text.
test("the close-to-tray setting reaches the frontend", async () => {
  const types = await readFile(new URL("./types.ts", import.meta.url), "utf8");
  assert.match(
    types,
    /closeToTray: boolean;/,
    "Config in types.ts does not mirror Rust's close_to_tray, so config.closeToTray is undefined",
  );

  const settings = await readFile(new URL("../views/Settings.tsx", import.meta.url), "utf8");
  assert.ok(
    settings.includes("config.closeToTray"),
    "nothing in Settings reads the setting, so the one escape hatch from a tray icon " +
      "nobody can see is unreachable",
  );
  assert.ok(
    settings.includes("closeToTray: e.target.checked"),
    "the toggle does not write the setting back, so it cannot be turned off",
  );
});

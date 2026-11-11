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

// The JVM flags a fresh Geode starts with are written twice: once in
// `default_java_args()` in `src-tauri/src/config.rs`, and once in
// `Settings.tsx`, which needs them for its "Reset to default" button.
//
// The copy is not avoidable. Nothing hands the default over the IPC boundary —
// `get_config` returns the value in force, and once somebody has typed over it
// that payload no longer says what was there before — and importing `config.rs`
// at build time is worse than the duplication, because that file also holds
// `DEFAULT_CURSEFORGE_KEY` and the import would put it in the webview bundle.
//
// So the two are kept honest here instead. Letting them drift does not break
// anything loudly: Reset would quietly restore an older tuning, which is a
// valid G1 config and not a wrong one, so nobody would notice for a long time.
test("the JVM default the Reset button restores is the one Rust ships", async () => {
  const rust = await readFile(
    new URL("../../src-tauri/src/config.rs", import.meta.url),
    "utf8",
  );
  const fn = rust.slice(rust.indexOf("fn default_java_args()"));
  const literal = fn.slice(fn.indexOf('"') + 1, fn.indexOf('"', fn.indexOf('"') + 1));
  assert.ok(literal.includes("-XX:"), "default_java_args no longer holds a JVM flag string");
  // Rust's backslash-newline continuation eats the break *and* the indent that
  // follows it, leaving nothing in their place — so the separator is whatever
  // space was already there before the backslash. Putting one back here is how
  // this check first failed on a string that was perfectly correct.
  const fromRust = literal.replace(/\\\s*\n\s*/g, "").trim();

  const settings = await readFile(new URL("../views/Settings.tsx", import.meta.url), "utf8");
  const decl = settings.slice(settings.indexOf("const DEFAULT_JAVA_ARGS"));
  const fromTs = [...decl.slice(0, decl.indexOf(";")).matchAll(/"([^"]*)"/g)]
    .map(([, part]) => part)
    .join("");
  assert.ok(fromTs.includes("-XX:"), "Settings.tsx no longer declares DEFAULT_JAVA_ARGS");

  assert.equal(
    fromTs,
    fromRust,
    "Settings.tsx's DEFAULT_JAVA_ARGS has drifted from default_java_args() in " +
      "config.rs, so Reset to default now restores a tuning Geode does not ship",
  );
});

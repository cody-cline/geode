// What a console line means, as opposed to how it is painted.
//
// Another of the few files here whose logic can be run rather than read:
// `serverConsole.ts` holds no JSX, so Node strips its types and imports it the
// way `mcversion.test.mjs` imports its one function. The rest of
// `src/lib/*.test.mjs` greps `.tsx` source instead, because `npm test` has no
// DOM.
//
// Only one thing in here is worth pinning, and it is a claim about somebody
// else's software: a server writes its own level into its own prefix, and this
// screen must not print a different one inside those same brackets. Getting
// that wrong does not look like a bug — it looks like the server said it.

import { test } from "node:test";
import assert from "node:assert/strict";

import { parseLine } from "./serverConsole.ts";

const line = (text, stream) => ({ line: text, stream, at: 0 });

test("a server's own level survives the stream it arrived on", () => {
  // Plenty of setups send every line to stderr — a wrapper, a log4j appender
  // pointed there, an installer. The server still labelled this one INFO, and
  // it is printed back inside `[Server thread/…]`, so overriding it puts a
  // word in the server's mouth that the server did not say.
  const onErr = parseLine(line("[03:52:13] [Server thread/INFO]: Done (5.1s)!", "err"));
  assert.equal(onErr.level, "info");
  assert.equal(onErr.thread, "Server thread");
  assert.equal(onErr.message, "Done (5.1s)!");

  // And the same line on stdout reads identically, which is the point: the
  // stream is not what decides this.
  const onOut = parseLine(line("[03:52:13] [Server thread/INFO]: Done (5.1s)!", "out"));
  assert.deepEqual(onOut, onErr);
});

test("a labelled warning or error is still read off the label", () => {
  assert.equal(parseLine(line("[1] [main/WARN]: slow", "out")).level, "warn");
  assert.equal(parseLine(line("[1] [main/SEVERE]: bad", "out")).level, "error");
  assert.equal(parseLine(line("[1] [main/FATAL]: worse", "err")).level, "error");
});

test("a line with no prefix of its own takes its level from the stream", () => {
  // This is the case the stream really is the only evidence for: a JVM crash
  // arrives with no prefix at all, and painting it like ordinary output buries
  // the one thing somebody needs to see.
  const crash = parseLine(line("\tat net.minecraft.server.Main.main(Main.java:1)", "err"));
  assert.equal(crash.level, "error");
  assert.equal(crash.thread, undefined);

  const plain = parseLine(line("Picked up JAVA_TOOL_OPTIONS", "out"));
  assert.equal(plain.level, "info");
  assert.equal(plain.thread, undefined);
});

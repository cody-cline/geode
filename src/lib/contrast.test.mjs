// Every colour this app writes text in, against every surface it writes it on.
//
// The light theme shipped for months without anyone measuring it: it inherited
// colours chosen to read on #0b0f14 and left them to fend for themselves on
// white, where the accent came out at 1.7:1 and a provider badge at 1.6:1 —
// words you cannot make out. Nothing caught it because nothing looked. The
// default theme is dark, so that is the one everybody saw.
//
// Read out of `global.css` rather than off a rendered page, for the reason the
// other tests here read source: `npm test` has no DOM. That is enough, because
// these are plain token-against-token pairings and the arithmetic is the whole
// of the question.
//
// Not covered here, deliberately: the colours `applyTheme` computes at runtime
// from a user-chosen accent (`--accent-text`, `--text-on-tint`). Those are not
// in this file. What is in this file is the answer for the *default* accent,
// which is what the stylesheet serves before any script runs.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const AA = 4.5;

async function themes() {
  const css = await readFile(new URL("../styles/global.css", import.meta.url), "utf8");
  const read = (selector) => {
    const at = css.indexOf(selector);
    assert.ok(at > 0, `${selector} is no longer in global.css`);
    const body = css.slice(at, css.indexOf("}", at));
    return Object.fromEntries(
      [...body.matchAll(/(--[\w-]+):\s*(#[0-9a-fA-F]{6})/g)].map(([, k, v]) => [k, v]),
    );
  };
  const dark = read(":root {");
  return { dark, light: { ...dark, ...read(':root[data-theme="light"] {') } };
}

function luminance(hex) {
  const n = parseInt(hex.slice(1), 16);
  return [16, 8, 0]
    .map((shift) => ((n >> shift) & 255) / 255)
    .map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4))
    .reduce((sum, c, i) => sum + c * [0.2126, 0.7152, 0.0722][i], 0);
}

function contrast(a, b) {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** What `color-mix(in srgb, <fg> <pct>, transparent)` leaves over `bg`. */
function over(fg, bg, pct) {
  const [f, b] = [fg, bg].map((h) => parseInt(h.slice(1), 16));
  const channel = (shift) =>
    Math.round((((f >> shift) & 255) * pct + ((b >> shift) & 255) * (1 - pct)));
  return `#${[16, 8, 0].map((s) => channel(s).toString(16).padStart(2, "0")).join("")}`;
}

const SURFACES = ["--bg", "--surface", "--surface-2", "--surface-3"];
const TEXT = [
  "--text",
  "--text-dim",
  "--text-faint",
  "--accent-text",
  "--danger",
  "--warn",
  "--info",
  "--connector-text",
  "--modrinth-text",
  "--curseforge-text",
  "--spiget-text",
];

test("every text colour is readable on every surface, in both themes", async () => {
  for (const [name, tokens] of Object.entries(await themes())) {
    for (const text of TEXT) {
      assert.ok(tokens[text], `${name}: ${text} is gone`);
      for (const surface of SURFACES) {
        const ratio = contrast(tokens[text], tokens[surface]);
        assert.ok(
          ratio >= AA,
          `${name}: ${text} (${tokens[text]}) on ${surface} (${tokens[surface]}) is ${ratio.toFixed(2)}:1, under ${AA}`,
        );
      }
    }
  }
});

// A badge writes its provider's colour on a 14% wash of that same colour, so
// the pair stands or falls together and neither value may be changed alone.
test("a badge's own text is readable on its own tint", async () => {
  const pairs = [
    ["--modrinth", "--modrinth-text"],
    ["--curseforge", "--curseforge-text"],
    ["--spiget", "--spiget-text"],
    ["--connector", "--connector-text"],
  ];
  for (const [name, tokens] of Object.entries(await themes())) {
    for (const [brand, text] of pairs) {
      const tint = over(tokens[brand], tokens["--surface"], 0.14);
      const ratio = contrast(tokens[text], tint);
      assert.ok(
        ratio >= AA,
        `${name}: ${text} (${tokens[text]}) on its own tint ${tint} is ${ratio.toFixed(2)}:1, under ${AA}`,
      );
    }
  }
});

// The sidebar's current row washes the accent over the surface and writes the
// shortcut hint on top. With the default accent that is what ships; a chosen
// one is recomputed by `applyTheme`, which this cannot see.
test("the hint on the current sidebar row is readable with the default accent", async () => {
  for (const [name, tokens] of Object.entries(await themes())) {
    for (const pct of [0.13, 0.18]) {
      const tint = over(tokens["--accent"], tokens["--surface"], pct);
      const ratio = contrast(tokens["--text-on-tint"], tint);
      assert.ok(
        ratio >= AA,
        `${name}: --text-on-tint on the ${pct * 100}% accent row (${tint}) is ${ratio.toFixed(2)}:1, under ${AA}`,
      );
    }
  }
});

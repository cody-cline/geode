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
import { readFile, readdir } from "node:fs/promises";

const AA = 4.5;

async function themes() {
  const css = await readFile(new URL("../styles/global.css", import.meta.url), "utf8");
  const read = (selector) => {
    const at = css.indexOf(selector);
    assert.ok(at > 0, `${selector} is no longer in global.css`);
    const body = css.slice(at, css.indexOf("}", at));
    return Object.fromEntries([
      ...[...body.matchAll(/(--[\w-]+):\s*(#[0-9a-fA-F]{6})/g)].map(([, k, v]) => [k, v]),
      // A `<saturation> <lightness>` pair, which is how the hero wash below is
      // held: two of an `hsl()`'s three components, the hue arriving separately.
      ...[...body.matchAll(/(--[\w-]+):\s*([\d.]+)%\s+([\d.]+)%/g)].map(([, k, s, l]) => [
        k,
        [Number(s) / 100, Number(l) / 100],
      ]),
    ]);
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

/** What `color-mix(in srgb, <fg> <pct>, transparent)` leaves over `bg`. Also
 *  the blend a gradient makes between two stops, which is the same sum. */
function over(fg, bg, pct) {
  const [f, b] = [fg, bg].map((h) => parseInt(h.slice(1), 16));
  const channel = (shift) =>
    Math.round((((f >> shift) & 255) * pct + ((b >> shift) & 255) * (1 - pct)));
  return `#${[16, 8, 0].map((s) => channel(s).toString(16).padStart(2, "0")).join("")}`;
}

/** `hsl(<h> <s> <l>)` as a hex, so the helpers above can read it. */
function hslHex(h, s, l) {
  const hue = (((h % 360) + 360) % 360) / 30;
  const k = (n) => (n + hue) % 12;
  const a = s * Math.min(l, 1 - l);
  const channel = (n) =>
    Math.round(255 * (l - a * Math.max(-1, Math.min(k(n) - 3, 9 - k(n), 1))));
  return `#${[0, 8, 4].map((n) => channel(n).toString(16).padStart(2, "0")).join("")}`;
}

const SURFACES = ["--bg", "--surface", "--surface-2", "--surface-3"];
const TEXT = [
  "--text",
  "--text-dim",
  "--text-faint",
  "--accent-text",
  "--danger",
  "--warn",
  "--warn-text",
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

// A badge writes a colour on a wash of that same colour, so the pair stands or
// falls together and neither value may be changed alone.
//
// Both halves of this used to be written down here, and both were wrong. The
// list of variants was typed out, so `.badge-accent` and `.badge-warn` were
// simply missing. And the wash was composited over `--surface` — but it is
// mixed with `transparent`, so what it really lands on is whatever the badge
// sits on, and `--surface` happens to be the one surface where every variant
// passes. A Browse row turns `--surface-2` under the pointer and `--warn`
// came out at 3.90:1 on `--surface-3`. So the variants are read out of the
// stylesheet and measured against every surface.
test("anything written on a wash is readable, wherever that wash sits", async () => {
  // Every rule in the app that writes a colour on a `color-mix(…, transparent)`
  // wash — not only the `.badge-*` ones, and not only `global.css`.
  //
  // This started as the badge check and was wrong twice over. It listed the
  // variants by hand, so two were missing; and it composited them over
  // `--surface`, which happens to be the one surface where they all pass. Both
  // were fixed — and it was still wrong, in a third way that is the whole
  // reason it now walks the tree: it only ever read `global.css`. The pattern
  // repeats 44 times in the module stylesheets, and the token invented to fix
  // `.badge-warn` was used in exactly one place, because one place was all this
  // test could see. `.nearVersion` in `PlanView.module.css` is the same badge
  // by another name, declared in a module, and it shipped at 3.94:1.
  //
  // A wash mixed with `transparent` lands on whatever is behind it, and that
  // is not knowable from the rule, so every surface has to clear.
  const files = [];
  const walk = async (at) => {
    for (const entry of await readdir(at, { withFileTypes: true })) {
      const next = new URL(`${entry.name}${entry.isDirectory() ? "/" : ""}`, at);
      if (entry.isDirectory()) await walk(next);
      else if (entry.name.endsWith(".css")) files.push(next);
    }
  };
  await walk(new URL("../", import.meta.url));

  const pairs = [];
  for (const file of files) {
    const text = await readFile(file, "utf8");
    const where = file.pathname.split("/src/")[1];
    for (const block of text.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
      const body = block[2];
      const wash = body.match(
        /background(?:-color)?:\s*color-mix\(in srgb, var\((--[\w-]+)\) ([\d.]+)%,\s*transparent\)/,
      );
      const ink = body.match(/(?<!-)\bcolor:\s*var\((--[\w-]+)\)/);
      if (!wash || !ink) continue;
      pairs.push({
        where: `${where} ${block[1].trim().split("\n").pop().trim()}`,
        ink: ink[1],
        wash: wash[1],
        pct: Number(wash[2]) / 100,
      });
    }
  }
  assert.ok(pairs.length > 30, `only ${pairs.length} washed rules were found; the sweep is not reading the tree`);

  for (const [name, tokens] of Object.entries(await themes())) {
    for (const { where, ink, wash, pct } of pairs) {
      if (!tokens[ink] || !tokens[wash]) continue;
      for (const surface of SURFACES) {
        const behind = over(tokens[wash], tokens[surface], pct);
        const ratio = contrast(tokens[ink], behind);
        assert.ok(
          ratio >= AA,
          `${name}: ${where} writes ${ink} (${tokens[ink]}) on a ${pct * 100}% ` +
            `${wash} wash over ${surface} (${behind}) — ${ratio.toFixed(2)}:1, under ${AA}`,
        );
      }
    }
  }
});

// ---------------------------------------------------------------------------
// The instance hero's generated wash.
//
// Everything above is a token against a token. The hero behind an instance
// with no artwork is neither: it is an `hsl()` field built at runtime from the
// hue of the instance's id, so there are 360 of it and none of them is in
// `global.css` to be paired off. That is why this test could not see what the
// light theme was doing — a wash hardcoded dark in both themes, with the light
// theme's dark text on top of it at 2.15:1 at the worst hue.
//
// What the text actually sits on is the wash with `.heroScrim` over it, so
// that is what is measured. Every part of it is read out of the file that
// holds it — the stops and the scrim from `InstanceView.module.css`, their
// saturation and lightness from `global.css`, the second stop's hue from the
// view — so changing any of them is measured here rather than mirrored here.
//
// The taller layout a pack summary makes is covered too, and it is the reason
// two of these rows read what they read. A summary of one or two lines makes
// the header taller without moving the text down the hero, so every row above
// it sits at a smaller *fraction* of the hero and lands in less of the scrim —
// the version line went to 3.67:1 that way. The answer was not a darker scrim,
// which would have dimmed every banner in the app for a case most instances
// never reach: the version line simply stopped using the secondary token on a
// background that is a photograph, and the badges beside it were backed with a
// surface. A badge mixed with `transparent` is the one thing here that no
// scrim can rescue, because over real artwork there is no telling what is
// under it.
//
// Still not covered: a hero with real artwork, where the background is
// somebody's screenshot and the scrim is the whole answer for the two rows
// that remain on it.
// ---------------------------------------------------------------------------

/** The wash's two stops, the second stop's hue offset, and the scrim's stops. */
async function hero() {
  const url = (p) => new URL(p, import.meta.url);
  const css = await readFile(url("../views/InstanceView.module.css"), "utf8");
  const view = await readFile(url("../views/InstanceView.tsx"), "utf8");

  const stops = [...css.matchAll(/hsl\(var\(--hero-hue-\w+\)\s+var\((--[\w-]+)\)\)/g)].map(
    ([, token]) => token,
  );
  assert.equal(stops.length, 2, "the hero wash is no longer two hsl() stops of one token each");

  const offset = view.match(/"--hero-hue-to":\s*\(hue \+ (\d+)\) % 360/);
  assert.ok(offset, "InstanceView no longer derives the wash's second hue from the first");

  // The row's colour is read out of the stylesheet rather than written down
  // here. A sweep that states the token it is measuring passes just as happily
  // after somebody changes it, which is the one way this test could be worse
  // than nothing.
  const meta = css.slice(css.indexOf(".meta {"));
  const ink = meta.slice(0, meta.indexOf("}")).match(/color:\s*var\((--[\w-]+)\)/);
  assert.ok(ink, "the hero's version row no longer names a colour token");

  // And that the badges beside it are not sitting on the wash. A badge mixes
  // its tint with `transparent`; over artwork there is no telling what ends up
  // behind it, so the hero backs it with a surface and this sweep leaves it to
  // the token-against-surface check at the top of the file.
  const badge = meta.slice(meta.indexOf(".meta :global(.badge) {"));
  assert.ok(
    /background-color:\s*var\(--surface-2\)/.test(badge.slice(0, badge.indexOf("}") + 1)),
    "the hero's badges are no longer backed with a surface, so what they sit " +
      "on is the wash again — and over real artwork, a photograph",
  );

  const at = css.indexOf(".heroScrim {");
  assert.ok(at > 0, ".heroScrim is no longer in InstanceView.module.css");
  const scrim = [
    ...css
      .slice(at, css.indexOf("}", at))
      .matchAll(
        /color-mix\(in srgb, var\((--[\w-]+)\) ([\d.]+)%, transparent\) ([\d.]+)%|var\((--[\w-]+)\) ([\d.]+)%/g,
      ),
  ].map((m) =>
    m[1]
      ? { token: m[1], alpha: Number(m[2]) / 100, at: Number(m[3]) / 100 }
      : { token: m[4], alpha: 1, at: Number(m[5]) / 100 },
  );
  assert.ok(scrim.length >= 2, "the scrim is no longer a gradient of token stops");

  return { stops, offset: Number(offset[1]), scrim, ink: ink[1] };
}

/** The scrim's own colour and coverage at `y` down the hero. Interpolated
 *  straight rather than premultiplied as a browser does it: the one pair of
 *  stops that differs in both colour and alpha differs by `--bg` against
 *  `--surface`, which is under one 255th either way. */
function scrimAt(scrim, tokens, y) {
  let i = 0;
  while (i < scrim.length - 2 && y > scrim[i + 1].at) i++;
  const [a, b] = [scrim[i], scrim[i + 1]];
  const t = (y - a.at) / (b.at - a.at);
  return {
    colour: over(tokens[b.token], tokens[a.token], t),
    alpha: a.alpha + (b.alpha - a.alpha) * t,
  };
}

// Where each row of the header sits, as a fraction of the hero's height, and
// what colour it writes in. `.header` is 22px of padding, the taller of the
// icon (64px) and the text, then 18px; the text is the taller at 82.75px — the
// name at 23px over a 1.5 line height is 34.5, then 5 and a 20px badge row,
// then 6 and the stats at 11.5px is 17.25 — so the hero is 122.75px tall.
//
// Each row is measured at the middle of its line box, where its ink is, and
// that is the point that answers to the rendered article: the three ratios
// measured off the running app's pixels in the light theme — 2.90 for the
// version, 3.21 for the loader pill, 4.42 for the play time — come out of this
// arithmetic as 2.99, 3.17 and 4.27 at the hue that fits all three.
//
// A pack summary sits between the version line and the stats, at 13px over a
// 1.5 line height with 7px above it, and is clamped at two lines — so the
// header comes in three heights and each one has to carry its own text. The
// badges are not here: they are backed with `--surface-2` in both heroes, so
// what they sit on is a surface the sweep at the top of this file already
// measures, not the wash.
const LAYOUTS = [0, 19.5 + 7, 39 + 7].map((summary) => {
  const hero = 22 + 82.75 + summary + 18;
  return {
    summary,
    rows: [
      { what: "the instance name", y: (22 + 34.5 / 2) / hero, ink: "--text" },
      { what: "the version line", y: (61.5 + 20 / 2) / hero, ink: "META" },
      {
        what: "the play-time line",
        y: (87.5 + summary + 17.25 / 2) / hero,
        ink: "--text-faint",
      },
    ],
  };
});

test("the hero's generated wash carries its text at every hue, in both themes", async () => {
  const { stops, offset, scrim, ink } = await hero();
  for (const [name, tokens] of Object.entries(await themes())) {
    const [from, to] = stops.map((token) => {
      const pair = tokens[token];
      assert.ok(Array.isArray(pair), `${name}: ${token} is not a saturation and a lightness`);
      return pair;
    });
    for (let hue = 0; hue < 360; hue += 15) {
      const ends = [hslHex(hue, ...from), hslHex(hue + offset, ...to)];
      // Both stops and the nineteen points between them, in twentieths: a
      // two-stop sRGB gradient is a straight blend of its ends channel by
      // channel, and because luminance is not linear in a channel the dimmest
      // point of it can fall between the stops rather than at one of them.
      for (let step = 0; step <= 20; step++) {
        const wash = over(ends[1], ends[0], step / 20);
        for (const { summary, rows } of LAYOUTS)
        for (const row of rows) {
          const { colour, alpha } = scrimAt(scrim, tokens, row.y);
          let behind = over(colour, wash, alpha);
          if (row.tint) behind = over(tokens[row.tint], behind, 0.14);
          const ratio = contrast(tokens[row.ink === "META" ? ink : row.ink], behind);
          assert.ok(
            ratio >= AA,
            `${name}: ${row.what} on the hue-${hue} wash, ${step}/20 along ` +
              `(${behind}), with ${summary ? `a ${summary > 30 ? "two-line" : "one-line"} summary` : "no summary"}, ` +
              `is ${ratio.toFixed(2)}:1, under ${AA}`,
          );
        }
      }
    }
  }
});

// The per-theme half of that wash has to stay in the stylesheet. Put a
// lightness back into a view's inline style and the sweep above goes on
// measuring the tokens while the screen shows something else.
//
// Both headers, because they are the same wash twice: the server's was left
// inline when the instance's moved, and so stayed dark in a light theme for as
// long as nothing here was looking at it. The sweep above measures the token
// values, which both now read, so this is what ties each screen to them.
for (const view of ["InstanceView", "ServerView"]) {
  test(`${view}'s hero wash carries nothing but the hue inline`, async () => {
    const text = await readFile(new URL(`../views/${view}.tsx`, import.meta.url), "utf8");
    const at = text.indexOf("styles.heroWash");
    assert.ok(at > 0, `${view} no longer draws its hero wash with the heroWash class`);
    const style = text.slice(at, text.indexOf("/>", at));
    assert.ok(
      !/background|hsl\(/.test(style),
      `${view} is building the wash inline again; the per-theme part belongs in CSS`,
    );
  });
}

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

// ---------------------------------------------------------------------------
// The hole everything above falls through.
//
// Every check in this file compares a token with a token. A rendered element
// can be neither: `opacity` on the element composites whatever colour it was
// given toward whatever is behind it, and the result appears in no stylesheet
// for any of the sweeps above to read. So the tokens can all be correct — they
// are, that is what the rest of this file is for — while five elements built
// from them are not, and nothing says a word.
//
// It was not hypothetical. `--text-faint` is the dimmest colour in this app
// that still clears 4.5:1, and five rules took it and then asked for less:
// the console timestamp came out at 2.87:1 in the light theme, under even the
// 3:1 floor that large text gets, and it is the thing an admin reads to find
// their place. Each of the five sat beside siblings using the same token
// undimmed, so the recession bought a hierarchy that colour, family and size
// were already carrying.
//
// Hence the shape of this test: it does not try to work out which rules draw
// text, because the rule that started this carried no colour or font of its
// own at all — it dimmed a `<kbd>` that takes its colour from `global.css`,
// and any heuristic reading the block alone would have waved it through.
// Instead *every* partial opacity has to be a kind that is allowed to dim,
// and the kinds are named here with the reason each one is exempt.
const MAY_DIM = [
  [/:disabled|\[disabled\]/, "a disabled control, which WCAG 1.4.3 exempts"],
  // Narrower than it reads: `unusable` is a destination that genuinely cannot
  // be picked. `disabled` was here too and should not have been — a row whose
  // mod is off is not an inactive component, because the toggle beside it and
  // the delete at its end both work, so WCAG 1.4.3 never covered it. Both
  // Content tabs say the word instead now.
  [/\bunusable\b/, "a destination that cannot be chosen, which WCAG 1.4.3 exempts"],
  [/\bgroup\[data-empty\]/, "a group with nothing in it, likewise inactive"],
  [/\bemptyIcon\b/, "the drawing on an empty screen, not words"],
  [/\bsvg\b|\bimage\b/, "an icon rather than text"],
  [/\bcheckOff\b|\bdot\b|::before/, "a glyph standing in for punctuation"],
  [/\b(faded|alone|off)\b/, "the dependency graph saying what you are not looking at"],
];

test("nothing dims text with opacity, which no check here can see", async () => {
  const dir = new URL("../", import.meta.url);
  const files = [];
  const walk = async (at) => {
    for (const entry of await readdir(at, { withFileTypes: true })) {
      const next = new URL(`${entry.name}${entry.isDirectory() ? "/" : ""}`, at);
      if (entry.isDirectory()) await walk(next);
      else if (entry.name.endsWith(".css")) files.push(next);
    }
  };
  await walk(dir);
  assert.ok(files.length > 10, "no stylesheets were found to scan");

  for (const file of files) {
    const css = await readFile(file, "utf8");
    for (const block of css.matchAll(/([^{}]*)\{([^{}]*)\}/g)) {
      // `stroke-opacity` and `fill-opacity` paint one shape; they do not
      // composite an element and its children the way `opacity` does.
      // `0.5`, `.5` and `50%` are the same instruction; only the first two
      // were being looked for. None of the third form exists today, which is
      // exactly why it would have gone in unnoticed.
      const dim = block[2].match(/(?<![-\w])opacity:\s*(0?\.\d+|\d{1,2}%)/);
      if (!dim) continue;
      const selector = block[1].trim().split("\n").pop().trim();
      const allowed = MAY_DIM.find(([pattern]) => pattern.test(selector));
      assert.ok(
        allowed,
        `${file.pathname.split("/src/")[1]} dims \`${selector}\` to ${dim[1]}. ` +
          "If that is text, the ratio it ends up at is not measured anywhere — " +
          "say the recession in a colour instead. If it is not text, add the " +
          "kind to MAY_DIM beside the reason it is exempt.",
      );
    }
  }
});

// `.btn-danger` is the same shape as a badge — a colour written on a wash of
// itself — but it was not in the list above, and it is the button that says
// "Delete permanently". Two things make it harder than a badge. Its wash is
// mixed with `transparent`, so what it really lands on is whatever the button
// sits on, and a dialog's footer is `--surface-2` rather than the `--surface`
// the badges assume; and it has a hover state that takes the wash from 10% to
// 18%, which is the worse of the two. It read 4.39:1 at rest on a dialog
// footer and 3.66:1 under the pointer before the tokens were corrected.
test("the danger button's label is readable on its own wash, everywhere it sits", async () => {
  for (const [name, tokens] of Object.entries(await themes())) {
    for (const surface of SURFACES) {
      for (const pct of [0.1, 0.18]) {
        const wash = over(tokens["--danger"], tokens[surface], pct);
        const ratio = contrast(tokens["--danger"], wash);
        assert.ok(
          ratio >= AA,
          `${name}: --danger (${tokens["--danger"]}) on its ${pct * 100}% wash over ` +
            `${surface} (${wash}) is ${ratio.toFixed(2)}:1, under ${AA}`,
        );
      }
    }
  }
});

// The accent is a background as well as a colour to write in: `.btn-primary`
// fills with it. Nothing above measured anything against it, because it is not
// in `SURFACES` — it is user-chosen, so it cannot be, and the default is all a
// static test can see. That gap let a shortcut hint ship at 1.55:1 on the most
// prominent button in the app: `kbd` is globally `--text-faint`, which is
// picked to read on surfaces, and nothing said the primary button is not one.
test("what is written on the primary button is readable on the accent", async () => {
  for (const [name, tokens] of Object.entries(await themes())) {
    const ratio = contrast(tokens["--accent-ink"], tokens["--accent"]);
    assert.ok(
      ratio >= AA,
      `${name}: --accent-ink (${tokens["--accent-ink"]}) on --accent ` +
        `(${tokens["--accent"]}) is ${ratio.toFixed(2)}:1, under ${AA}`,
    );
  }

  // And that the key printed on that button takes the label's colour rather
  // than the one meant for surfaces.
  const css = await readFile(new URL("../styles/global.css", import.meta.url), "utf8");
  const rule = css.slice(css.indexOf(".btn-primary kbd {"));
  assert.ok(
    rule.startsWith(".btn-primary kbd {"),
    "nothing gives a printed key its own colour on the primary button, so it " +
      "falls back to `--text-faint`, which is chosen to read on a surface",
  );
  assert.match(
    rule.slice(0, rule.indexOf("}")),
    /color:\s*var\(--accent-ink\)/,
    "the key on a primary button is no longer written in --accent-ink, the one " +
      "colour that tracks whatever accent the user chose",
  );
});

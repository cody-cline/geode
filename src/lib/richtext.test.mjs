// The one thing between a mod author's prose and `dangerouslySetInnerHTML`.
//
// `renderProviderBody` renders markup written by anybody who can publish to
// Modrinth, CurseForge or Spiget, at three render sites, inside a webview that
// has `invoke` and every command behind it. It had no test of any kind, and
// the docstring on `sanitize` claimed it removed "every `on*` handler and
// `javascript:` URL" while the filter it described could be walked past three
// different ways. The CSP in `src-tauri/tauri.conf.json` was carrying it alone
// and nothing said so.
//
// What this file can and cannot do, since the difference decides what is
// worth asserting here. `sanitize` needs a `DOMParser`, and `node --test` has
// no DOM at all — calling it here fails with "ReferenceError: DOMParser is not
// defined". So the policy is split out of the loop into three things this file
// imports and really runs: `urlAllowed`, `attributeDropped` and
// `DROPPED_ELEMENTS`. The loop that applies them is read, the way
// `boundary.test.mjs` and `shortcuts.test.mjs` read the `.tsx` they hold.
//
// One link in the chain cannot be proved from here: that an attribute written
// `java&#9;script:alert(1)` reaches the loop as `java\tscript:alert(1)`, with
// a real U+0009 in the middle, because that is the HTML parser's entity
// decoding and there is no parser under `node --test`. It was confirmed in a
// real Chromium DOM — the decoded value's codepoints are
// `[106,97,118,97,9,…]`, the old filter kept the attribute, and the surviving
// anchor reported `protocol === "javascript:"`. The payloads below are those
// decoded values, so what runs here is the second half of the chain: given
// what the parser hands over, the policy refuses it.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

import { DROPPED_ELEMENTS, attributeDropped, urlAllowed } from "./richtext.ts";

async function source(path) {
  return readFile(new URL(path, import.meta.url), "utf8");
}

// Every base this window really has, read off `tauri_protocol_url` in the
// `tauri` crate: `tauri://localhost` in a packaged build everywhere except
// Windows and Android, `http://tauri.localhost` there, and `devUrl` under
// `npm run tauri dev`. A verdict on a dangerous value must not depend on which
// one the window happens to be showing.
const BASES = [
  "tauri://localhost/",
  "http://tauri.localhost/",
  "http://localhost:1420/",
];

// Attribute values as the HTML parser hands them to the loop: `&#9;` decoded,
// `&#10;` decoded, `&#1;` decoded. Each one resolves back to `javascript:`.
const SPLIT_SCHEMES = [
  ["a tab inside the scheme", "java\tscript:alert(1)"],
  ["a newline inside the scheme", "java\nscript:alert(1)"],
  ["a carriage return inside the scheme", "java\rscript:alert(1)"],
  ["a leading control character", "\u0001javascript:alert(1)"],
];

test("a scheme broken up mid-word is still the scheme the parser reads", () => {
  for (const base of BASES) {
    for (const [what, value] of SPLIT_SCHEMES) {
      // The payload is live, not merely odd-looking: the same parser the
      // webview navigates with reads it as `javascript:`. If this ever stops
      // being true the case below has stopped testing anything.
      assert.equal(
        new URL(value, base).protocol,
        "javascript:",
        `${what} no longer resolves to javascript:`,
      );
      assert.equal(urlAllowed(value, base), false, `${what}, base ${base}`);
    }
  }
});

test("a string compare cannot see any of them, which is why the check is not one", () => {
  // The shape of the filter this replaced: `attr.value.trim().toLowerCase()`
  // and then `startsWith`. `trim` takes whitespace off the ends and nothing
  // takes anything out of the middle, so all four walked past it — and so did
  // a `data:` URL with the one media type the list did not name. This is here
  // so that nobody reintroduces a string test as "equivalent".
  for (const [what, value] of SPLIT_SCHEMES) {
    const normalised = value.trim().toLowerCase();
    assert.equal(
      normalised.startsWith("javascript:"),
      false,
      `${what} would have been caught by a prefix test after all`,
    );
  }
  assert.equal(
    "data:application/xhtml+xml,<html/>".startsWith("data:text/html"),
    false,
  );
});

test("only http, https and mailto survive", () => {
  const refused = [
    "javascript:alert(1)",
    "JaVaScRiPt:alert(1)",
    "  javascript:alert(1)  ",
    "data:text/html,<script>alert(1)</script>",
    // The media type the old denylist missed, and the reason this is an
    // allowlist: `data:application/xhtml+xml` renders as a document with
    // script in it just as well.
    "data:application/xhtml+xml,<html/>",
    "vbscript:msgbox(1)",
    "blob:https://example.com/x",
    "file:///etc/passwd",
  ];
  const kept = [
    "https://modrinth.com/mod/sodium",
    "http://example.com/a?b=c#d",
    "mailto:author@example.com",
  ];

  for (const base of BASES) {
    for (const value of refused) {
      assert.equal(urlAllowed(value, base), false, `${value} under ${base}`);
    }
    for (const value of kept) {
      assert.equal(urlAllowed(value, base), true, `${value} under ${base}`);
    }
  }
});

test("a value that is not a URL at all is dropped, not kept for later", () => {
  for (const base of BASES) {
    assert.equal(urlAllowed("http://[", base), false);
    assert.equal(urlAllowed("http://ex ample.com/\u0000", base), false);
  }
});

// Pins what happens now rather than claiming it is right. A relative URL is
// judged by what it resolved to, and what it resolves to is this app's own
// origin: `tauri:` in a packaged Linux build, so refused, and `http:` on
// Windows and under the dev server, so kept. Neither can be a dangerous
// scheme, and a provider's relative URL points at nothing real either way — it
// was written against the provider's site, not this one. What a reader loses
// when it goes is the link, never the text.
test("a relative URL is judged by what it resolved to", () => {
  for (const relative of ["images/a.png", "#install", "//evil.example/x"]) {
    assert.equal(urlAllowed(relative, "tauri://localhost/"), false);
    assert.equal(urlAllowed(relative, "http://tauri.localhost/"), true);
    assert.equal(urlAllowed(relative, "http://localhost:1420/"), true);
  }
});

test("the style attribute goes with the event handlers", () => {
  // `style-src` in this window's CSP allows inline style and nothing in
  // `.prose` creates a containing block, so a `position:fixed` child out of a
  // description is laid out against the viewport: `ProjectPanel`'s `.backdrop`
  // is itself `position:fixed; inset:0`, which is the whole window. A `style`
  // attribute was enough to cover the app frame with author-chosen content and
  // no boundary saying it was not Geode.
  assert.equal(attributeDropped("style"), true);
  assert.equal(attributeDropped("STYLE"), true);
  assert.equal(attributeDropped("onclick"), true);
  assert.equal(attributeDropped("onCLICK"), true);
  assert.equal(attributeDropped("onerror"), true);

  // And the attributes a description is rendered for are still there, or this
  // would pass by dropping everything.
  for (const keep of ["href", "src", "xlink:href", "class", "alt", "title"]) {
    assert.equal(attributeDropped(keep), false, keep);
  }
});

test("SMIL is dropped as elements, because its payload is in a value nobody inspects", () => {
  // `<animate attributeName="href" values="javascript:…">` inside an
  // `<svg><a>` leaves the anchor's `href` attribute empty and its animated
  // value set to the payload, so a filter keyed on attribute names sees
  // nothing. Confirmed in a real DOM: after the old sanitiser the anchor's
  // `href.baseVal` was "" and its `animVal` was "javascript:alert(1)".
  //
  // Spelled exactly: the HTML parser gives an SVG element its camelCase local
  // name, and a type selector is case-sensitive outside HTML, so
  // `animatemotion` in that selector would match nothing.
  const dropped = DROPPED_ELEMENTS.split(",").map((s) => s.trim());
  for (const smil of ["animate", "animateMotion", "animateTransform", "set"]) {
    assert.ok(dropped.includes(smil), `${smil} is not dropped`);
  }
  // The names that were already there, so adding the four above cannot have
  // cost one of them. `style` is in both lists on purpose: the element here,
  // the attribute in `attributeDropped`.
  for (const old of [
    "script",
    "style",
    "iframe",
    "object",
    "embed",
    "form",
    "link",
    "meta",
    "base",
  ]) {
    assert.ok(dropped.includes(old), `${old} is no longer dropped`);
  }
});

test("the loop applies those three and not a copy of them", async () => {
  // The half that cannot be run here: `sanitize` needs a DOMParser, so this
  // reads it. What it checks is that the policy the cases above exercise is
  // the policy the document actually gets — three functions tested against a
  // loop that inlined its own rules would be the most convincing kind of
  // nothing.
  const src = await source("./richtext.ts");

  assert.match(src, /querySelectorAll\(DROPPED_ELEMENTS\)[\s\S]{0,60}?\.remove\(\)/);
  assert.match(src, /if \(attributeDropped\(attr\.name\)\)/);
  assert.match(src, /!urlAllowed\(attr\.value, document\.baseURI\)/);

  // Every attribute whose value is a URL reaches `urlAllowed`. `xlink:href` is
  // the one that is easy to forget and the one SVG uses.
  const branch = src.slice(src.indexOf("} else if ("), src.indexOf("!urlAllowed"));
  for (const url of ["href", "src", "xlink:href"]) {
    assert.ok(branch.includes(`name === "${url}"`), `${url} is not checked as a URL`);
  }

  // And nothing of the denylist is left in what runs. A `startsWith` on the
  // raw text is exactly the check the four payloads above walk through.
  // Measured on the body, not the file: the comments above it name the old
  // check in order to say why it went, and would otherwise be what failed
  // this.
  const body = src.slice(src.indexOf("export function sanitize"));
  assert.ok(
    !/startsWith\("javascript:"\)/.test(body),
    "the prefix test is back, and it never saw these payloads",
  );
});

test("the CSP is the backstop, and stays one", async () => {
  // This belongs as a comment beside the CSP, and cannot go there: nothing in
  // this project compiles `tauri-utils` with `config-json5` (no `json5` in
  // `src-tauri/Cargo.lock`), so `tauri.conf.json` is parsed by `serde_json`,
  // which has no comments — and every config struct is `deny_unknown_fields`,
  // so a `"_comment"` key is a build failure too. A test that fails is a
  // better note than a comment that cannot exist.
  //
  // What makes it the backstop: the payloads above all survived the old
  // sanitiser and none of them ran, because there is no `script-src` and
  // `default-src 'self'` covers scripts. Relaxing that for some library is one
  // edit; it would turn a mod description into script in this webview.
  const conf = JSON.parse(await source("../../src-tauri/tauri.conf.json"));
  const csp = conf.app.security.csp;
  const directives = Object.fromEntries(
    csp
      .split(";")
      .map((d) => d.trim())
      .filter(Boolean)
      .map((d) => {
        const [name, ...sources] = d.split(/\s+/);
        return [name.toLowerCase(), sources];
      }),
  );
  const scripts = directives["script-src"] ?? directives["default-src"];
  assert.deepEqual(
    scripts,
    ["'self'"],
    "the CSP is what stops provider HTML executing — see sanitize() in richtext.ts",
  );
});

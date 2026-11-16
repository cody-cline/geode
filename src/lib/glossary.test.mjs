// The glossary, and the one rule that stops it becoming a second copy of the
// app's own words.
//
// Every definition in `glossary.ts` is also a sentence on a screen: what an
// instance is heads the Library, what a plan is heads Plans, what a server is
// heads Servers, and each of those appears again inside that screen's empty
// state. That is the arrangement the glossary exists for — the sentence that
// used to vanish once the list stopped being empty is now readable from
// anywhere — and it is exactly the arrangement that rots if the words are
// typed in two places. So they are typed in one, and this fails if a screen
// retypes one of them.
//
// Checked by reading the files, like `shortcuts.test.mjs` beside it: there is
// no DOM under `npm test`, so the only thing that can be examined is the
// source.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";

import { GLOSSARY, term } from "./glossary.ts";

async function sources(dir = new URL("../", import.meta.url)) {
  const out = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const at = new URL(entry.name + (entry.isDirectory() ? "/" : ""), dir);
    if (entry.isDirectory()) out.push(...(await sources(at)));
    else if (entry.name.endsWith(".tsx")) out.push(at);
  }
  return out;
}

test("every term has a definition worth reading", () => {
  assert.ok(GLOSSARY.length > 0);
  for (const t of GLOSSARY) {
    assert.ok(t.term.trim().length > 0, "a term with no word");
    // One sentence, ending in one. A fragment reads as a label rather than a
    // definition, and a row of labels is a list of words nobody is any the
    // wiser for.
    assert.match(t.short, /[.!?]$/, `${t.term}: \`short\` is not a sentence`);
    assert.ok(t.short.length > 24, `${t.term}: \`short\` says too little`);
    if (t.more !== null) assert.match(t.more, /[.!?]$/, `${t.term}: \`more\` is not a sentence`);
  }
  // Every word once.
  const words = GLOSSARY.map((t) => t.term);
  assert.equal(new Set(words).size, words.length, "a term is in the list twice");
});

test("no screen retypes a definition the glossary holds", async () => {
  const files = await sources();
  assert.ok(files.length > 20, "the sweep found almost no .tsx — its path is wrong");

  for (const file of files) {
    const text = await readFile(file, "utf8");
    const where = file.pathname.split("/src/")[1];
    for (const t of GLOSSARY) {
      for (const [which, sentence] of [["short", t.short], ["more", t.more]]) {
        if (!sentence) continue;
        // Whitespace is not the question — JSX wraps these across lines, so a
        // screen that pasted one would not match character for character.
        // Both sides are squashed to single spaces first.
        const flat = (s) => s.replace(/\s+/g, " ");
        assert.ok(
          !flat(text).includes(flat(sentence)),
          `${where} types out the glossary's \`${t.term}\` ${which} instead of ` +
            `reading it from glossary.ts. Two copies of a definition is what ` +
            `the glossary was made to remove.`,
        );
      }
    }
  }
});

test("the three screens whose word this is still read it", async () => {
  // The glossary's reason for existing is that these definitions used to live
  // only in an empty state. If a screen stops reading its own word, that has
  // happened again somewhere and nothing else would say so.
  for (const [file, word] of [
    ["../views/Library.tsx", "Instance"],
    ["../views/Plans.tsx", "Plan"],
    ["../views/Servers.tsx", "Server"],
  ]) {
    const text = await readFile(new URL(file, import.meta.url), "utf8");
    assert.match(text, /from "\.\.\/lib\/glossary"/, `${file} no longer reads the glossary`);
    assert.ok(
      text.includes(`term("${word}")`),
      `${file} no longer renders the glossary's "${word}"`,
    );
    // And it is on the screen, not only in the empty state it used to hide in.
    assert.match(text, /styles\.lede/, `${file} has no persistent lede`);
  }
  assert.ok(term("Instance").short.length > 0);
});

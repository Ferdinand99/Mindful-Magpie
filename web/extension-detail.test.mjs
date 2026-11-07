// The titles and labels of detail blocks (lists, bars and the like) are the `[detail]` sections of
// the locale files. The server carries them in the page's labels as `detail.<key>`; the extension
// ships them, generated into extension/vendor/detail-labels.js by web/build-extension.mjs.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { detailSource, detailTarget, detailTexts } from "./build-extension.mjs";
import { labels as shipped, pickLabels } from "../extension/labels.js";

const repo = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("the [detail] section is read as key and text, and nothing else is", () => {
  const toml = [
    "[start]",
    'title = "Start"',
    "",
    "# a comment",
    "[detail]",
    'vv_down_now = "Down now"',
    'quoted = "a \\"b\\" c"',
    "",
    "[other]",
    'x = "no"',
  ].join("\n");
  assert.deepEqual(detailTexts(toml), { vv_down_now: "Down now", quoted: 'a "b" c' });
  assert.deepEqual(detailTexts('[start]\na = "b"\n'), {});
});

test("the generated file is current and holds both languages", () => {
  const source = detailSource();
  assert.match(source, /export const detail = \{/);
  assert.ok(source.includes('"en"') && source.includes('"nb"'));
  assert.equal(repo(detailTarget), source);
});

test("the shipped labels carry every [detail] key of both languages as detail.<key>", () => {
  for (const lang of ["en", "nb"]) {
    const texts = detailTexts(repo(`locales/${lang}.toml`));
    for (const [key, text] of Object.entries(texts)) {
      assert.equal(shipped[lang][`detail.${key}`], text, `${lang} ${key}`);
    }
    const shippedKeys = Object.keys(shipped[lang]).filter((k) => k.startsWith("detail."));
    assert.deepEqual(shippedKeys.sort(), Object.keys(texts).map((k) => `detail.${k}`).sort());
  }
  assert.ok(pickLabels("en").labels.block_more.includes("{n}"));
  assert.equal(pickLabels("nb").labels.block_more, "+ {n} til");
});

test("the two languages name the same detail keys", () => {
  assert.deepEqual(
    Object.keys(detailTexts(repo("locales/en.toml"))).sort(),
    Object.keys(detailTexts(repo("locales/nb.toml"))).sort(),
  );
});

test("a [detail] line that cannot be read is an error, not a key the extension silently lacks", () => {
  for (const bad of [
    'dash-key = "x"',
    'two_lines = "first',
    'no_value =',
    "just text",
    'single = \'quoted\'',
    'key = "unterminated',
    'key = "a" # trailing comment',
  ]) {
    assert.throws(() => detailTexts(`[detail]\n${bad}\n`), /cannot read the \[detail\] line/, bad);
  }
  // Blank lines and comments are fine, and so are lines outside the section.
  assert.deepEqual(detailTexts('[start]\nanything goes here\n\n[detail]\n\n# a note\nok_key = "fine"\n\n[other]\nlines = that are not read\n'), { ok_key: "fine" });
});

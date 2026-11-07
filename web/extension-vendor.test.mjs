// extension/vendor/ holds copies of the shared renderer and its tokens. They must equal their
// sources, or the extension would ship an old renderer. CI also runs the build script and
// `git diff --exit-code`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { build, detailTarget, files, stale } from "./build-extension.mjs";

test("extension/vendor is up to date with its sources", () => {
  assert.deepEqual(stale(), [], "run `npm run build:extension` in web/ and commit the result");
});

test("the build copies every file byte for byte", () => {
  const out = mkdtempSync(join(tmpdir(), "mm-vendor-"));
  try {
    assert.deepEqual(build(out), [...files.map(([, target]) => target), detailTarget]);
    for (const [source, target] of files) {
      assert.ok(readFileSync(new URL(`../${source}`, import.meta.url)).equals(readFileSync(join(out, target))));
    }
    assert.deepEqual(stale(out), []);
  } finally {
    rmSync(out, { recursive: true, force: true });
  }
});

test("the vendor check notices a changed or missing file", () => {
  const out = mkdtempSync(join(tmpdir(), "mm-vendor-"));
  try {
    assert.equal(stale(out).length, files.length + 1, "nothing built yet: all missing");
    build(out);
    writeFileSync(join(out, "extension/vendor/render.js"), "// edited\n");
    assert.deepEqual(stale(out), ["extension/vendor/render.js"]);
  } finally {
    rmSync(out, { recursive: true, force: true });
  }
});

test("tokens.css has every custom property that render.css reads, in light and dark", () => {
  const css = readFileSync(new URL("../static/magpie/render.css", import.meta.url), "utf8");
  const tokens = readFileSync(new URL("../extension/vendor/tokens.css", import.meta.url), "utf8");
  const used = new Set([...css.matchAll(/var\((--[a-z0-9-]+)/g)].map((m) => m[1]));
  used.delete("--mm-bg-image"); // set by the renderer, not a token
  used.delete("--mm-pct"); // a bar's fill, set by the renderer
  for (const name of used) {
    const line = tokens.match(new RegExp(`^\\s*${name}: ([^;]+);`, "m"));
    assert.ok(line, `${name} is defined in tokens.css`);
    if (/^--(bg|card|line|line-strong|text|muted|brand|fill|pal-.*)$/.test(name)) {
      assert.match(line[1], /^light-dark\(/, `${name} has a light and a dark value`);
    }
  }
});

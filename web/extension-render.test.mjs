// The new tab draws the layout with the start page's renderer (vendored into extension/vendor/),
// in the language of the browser, and loads image backgrounds with the token.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { parseHTML } from "linkedom";
import { render as pageRender } from "../static/magpie/render.js";
import { createView } from "../extension/newtab-view.js";
import { start } from "../extension/newtab-core.js";
import { fetchBackground, imageId } from "../extension/background.js";
import { labels as shipped, pickLabels } from "../extension/labels.js";

const repo = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const TOKEN = "mm_SECRET_IMAGE_TOKEN";
const SERVER = "http://10.0.3.152:8099";

const card = (id, title, url = "https://x.example/") => ({
  id,
  title,
  description: "",
  url,
  icon: { emoji: null, colour: "teal" },
});
const layout = (parts = {}) => ({
  version: 1,
  background: { kind: "preset", name: "natt" },
  groups: [],
  bookmarks: [],
  ...parts,
});
const FULL = layout({
  groups: [
    { id: "g1", title: "Work", cards: [card("a", "Wiki"), card("b", "Bad", "javascript:alert(1)")] },
    { id: "g2", title: "Home", cards: [card("c", "Router", "http://10.0.0.1/")] },
  ],
  bookmarks: [{ id: "k", title: "News", url: "https://news.example/" }],
});

function page() {
  const { document } = parseHTML(
    '<!doctype html><html lang="nb"><body><p id="status"></p><section id="groups" class="mm-root"></section></body></html>',
  );
  return {
    doc: document,
    statusEl: document.getElementById("status"),
    root: document.getElementById("groups"),
    html: document.documentElement,
  };
}
const bgImage = (html) => html.style.getPropertyValue("--mm-bg-image") || "";

function makeView(overrides = {}) {
  const p = page();
  const urls = { created: [], revoked: [] };
  const opened = [];
  const fetched = [];
  const draw = createView({
    doc: p.doc,
    statusEl: p.statusEl,
    root: p.root,
    language: "en-GB",
    fetchImpl: async (url, opts) => {
      fetched.push([url, opts]);
      return { status: 200, blob: async () => ({ type: "image/png", size: 3 }) };
    },
    openOptions: () => opened.push(1),
    objectUrl: {
      create: () => {
        const url = `blob:moz-extension://x/${urls.created.length + 1}`;
        urls.created.push(url);
        return url;
      },
      revoke: (url) => urls.revoked.push(url),
    },
    ...overrides,
  });
  return { ...p, draw, urls, opened, fetched };
}
const CONN = { serverUrl: SERVER, token: TOKEN };
const imageLayout = (id = "img_1") => layout({ background: { kind: "image", id } });

// ---- the vendored renderer ----

test("the new tab renders a layout through the vendored renderer, same as the web page", async () => {
  const v = makeView();
  await v.draw({ status: "fresh", data: FULL }, CONN);
  const web = page();
  pageRender(web.root, FULL, { lang: "en", labels: pickLabels("en").labels, imageBase: SERVER });
  assert.equal(v.root.outerHTML, web.root.outerHTML);
  assert.equal(v.html.getAttribute("data-bg"), "natt");
  assert.deepEqual(
    [...v.root.querySelectorAll("h2")].map((e) => e.textContent),
    ["Work", "Home"],
  );
  assert.equal(v.root.querySelectorAll("a.mm-card").length, 2);
  assert.equal(v.root.querySelectorAll(".mm-card-invalid").length, 1);
  assert.equal(v.root.querySelector("nav.mm-bookmarks a").textContent, "News");
  assert.equal(v.statusEl.textContent, "Oppdatert");
});

test("the vendored files are the renderer the web page uses", () => {
  assert.equal(repo("extension/vendor/render.js"), repo("static/magpie/render.js"));
  assert.equal(repo("extension/vendor/render.css"), repo("static/magpie/render.css"));
});

test("links open in the same tab: no target attribute, no script, text only", async () => {
  const v = makeView();
  const evil = '<img src=x onerror="pwned=1">';
  await v.draw(
    { status: "fresh", data: layout({ groups: [{ id: "g", title: evil, cards: [card("a", evil)] }] }) },
    CONN,
  );
  for (const a of v.root.querySelectorAll("a")) {
    assert.equal(a.getAttribute("target"), null);
    assert.ok(a.getAttribute("href").startsWith("http"));
  }
  assert.equal(v.root.querySelectorAll("img, script").length, 0);
  assert.equal(v.root.querySelector("h2").textContent, evil);
});

test("the new tab page loads the vendored styles and has the renderer's root", () => {
  const doc = parseHTML(repo("extension/newtab.html")).document;
  const hrefs = [...doc.querySelectorAll("link[rel=stylesheet]")].map((l) => l.getAttribute("href"));
  assert.deepEqual(hrefs, ["vendor/tokens.css", "shell.css", "vendor/render.css"]);
  assert.ok(doc.querySelector("#groups.mm-root"));
});

test("an empty layout draws the renderer's empty state in the browser's language", async () => {
  const v = makeView({ language: "nb-NO" });
  await v.draw({ status: "fresh", data: layout() }, CONN);
  assert.equal(v.root.querySelector(".empty-title").textContent, shipped.nb.empty);
  assert.equal(v.html.getAttribute("lang"), "nb");
});

test("no layout (not connected) draws no page and clears an earlier background", async () => {
  const v = makeView();
  await v.draw({ status: "cached", data: FULL }, CONN);
  await v.draw({ status: "empty", data: null }, {});
  assert.equal(v.root.childNodes.length, 0);
  assert.equal(v.html.getAttribute("data-bg"), null);
  assert.equal(v.statusEl.querySelector("button").textContent, "Koble til Magpie");
});

// ---- order: cache first, then the fresh layout ----

test("start draws the cached layout first, then the fresh one, through the real view", async () => {
  const v = makeView();
  const cached = layout({ groups: [{ id: "g", title: "Old", cards: [card("a", "Old card")] }] });
  const fresh = layout({ groups: [{ id: "g", title: "New", cards: [card("a", "New card")] }] });
  const titles = [];
  const store = { last: cached, serverUrl: SERVER, token: TOKEN };
  await start({
    storage: {
      local: {
        get: async () => ({ ...store }),
        set: async (values) => Object.assign(store, values),
      },
    },
    fetchImpl: async () => ({ status: 200, json: async () => fresh }),
    draw: (view, conn) => {
      const result = v.draw(view, conn);
      titles.push([view.status, v.root.querySelector("h2").textContent, v.statusEl.textContent]);
      return result;
    },
  });
  assert.deepEqual(titles, [
    ["cached", "Old", "Oppdaterer …"],
    ["fresh", "New", "Oppdatert"],
  ]);
  assert.deepEqual(store.last, fresh);
});

test("start hands the server and token to draw for the image fetch", async () => {
  const seen = [];
  await start({
    storage: { local: { get: async () => ({ serverUrl: SERVER, token: TOKEN }), set: async () => {} } },
    fetchImpl: async () => ({ status: 200, json: async () => layout() }),
    draw: (view, conn) => seen.push(conn),
  });
  assert.deepEqual(seen, [CONN, CONN]);
});

// ---- image backgrounds ----

test("an image background is fetched with the token and set as a blob: URL", async () => {
  const v = makeView();
  const done = v.draw({ status: "fresh", data: imageLayout("img_1") }, CONN);
  // Before the image is here the server url is already gone (it would be blocked and tokenless).
  assert.equal(v.html.getAttribute("data-bg"), "image");
  assert.equal(bgImage(v.html), "none");
  await done;
  assert.equal(v.fetched.length, 1);
  const [url, opts] = v.fetched[0];
  assert.equal(url, `${SERVER}/start/background/img_1`);
  assert.equal(opts.headers.Authorization, `Bearer ${TOKEN}`);
  assert.equal(opts.redirect, "error");
  assert.equal(opts.credentials, "omit");
  assert.ok(opts.signal, "has a timeout signal");
  assert.equal(v.html.getAttribute("data-bg"), "image");
  assert.equal(bgImage(v.html), `url("${v.urls.created[0]}")`);
  assert.ok(bgImage(v.html).startsWith('url("blob:'));
  assert.equal(bgImage(v.html).includes(TOKEN), false);
});

test("a failed image fetch falls back to the hedgerow preset", async () => {
  const cases = [
    async () => {
      throw new TypeError("NetworkError");
    },
    async () => ({ status: 404, blob: async () => ({ type: "image/png" }) }),
    async () => ({ status: 401, blob: async () => ({ type: "image/png" }) }),
    async () => ({ status: 200, blob: async () => ({ type: "text/html" }) }),
    async () => ({
      status: 200,
      blob: async () => {
        throw new Error("aborted");
      },
    }),
  ];
  for (const fetchImpl of cases) {
    const v = makeView({ fetchImpl });
    await v.draw({ status: "fresh", data: imageLayout() }, CONN);
    assert.equal(v.html.getAttribute("data-bg"), "hedgerow");
    assert.equal(bgImage(v.html), "");
    assert.equal(v.urls.created.length, 0);
  }
});

test("an image background without a token or server falls back without fetching", async () => {
  for (const conn of [{ serverUrl: SERVER }, { token: TOKEN }, {}]) {
    const v = makeView();
    await v.draw({ status: "cached", data: imageLayout() }, conn);
    assert.equal(v.fetched.length, 0);
    assert.equal(v.html.getAttribute("data-bg"), "hedgerow");
  }
});

test("the cached and the fresh layout with the same image load it once", async () => {
  const v = makeView();
  const first = v.draw({ status: "cached", data: imageLayout("same") }, CONN);
  const second = v.draw({ status: "fresh", data: imageLayout("same") }, CONN);
  await Promise.all([first, second]);
  assert.equal(v.fetched.length, 1);
  assert.equal(v.urls.created.length, 1);
  assert.equal(bgImage(v.html), `url("${v.urls.created[0]}")`);
  // A later draw with the same image reuses the blob URL at once.
  await v.draw({ status: "fresh", data: imageLayout("same") }, CONN);
  assert.equal(v.fetched.length, 1);
  assert.equal(bgImage(v.html), `url("${v.urls.created[0]}")`);
});

test("a late image for an older layout does not replace the newer background", async () => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const v = makeView({
    fetchImpl: async () => {
      await gate;
      return { status: 200, blob: async () => ({ type: "image/png" }) };
    },
  });
  const slow = v.draw({ status: "cached", data: imageLayout("old") }, CONN);
  await v.draw({ status: "fresh", data: layout({ background: { kind: "preset", name: "hav" } }) }, CONN);
  release();
  await slow;
  assert.equal(v.html.getAttribute("data-bg"), "hav");
  assert.equal(bgImage(v.html), "");
  assert.equal(v.urls.created.length, 0);
});

test("the blob URL is revoked when the background changes away from the image", async () => {
  const v = makeView();
  await v.draw({ status: "fresh", data: imageLayout("a") }, CONN);
  await v.draw({ status: "fresh", data: imageLayout("b") }, CONN);
  assert.deepEqual(v.urls.revoked, [v.urls.created[0]]);
  await v.draw({ status: "fresh", data: layout() }, CONN);
  assert.deepEqual(v.urls.revoked, v.urls.created);
});

test("fetchBackground: bad ids and addresses never fetch, and the token never leaks", async () => {
  assert.equal(imageId({ kind: "image", id: "ok_1-2" }), "ok_1-2");
  for (const bad of ['a"b', "a/b", "", "x".repeat(65), 5, undefined]) {
    assert.equal(imageId({ kind: "image", id: bad }), null);
  }
  assert.equal(imageId({ kind: "preset", name: "hav" }), null);
  const never = async () => {
    throw new Error("never");
  };
  assert.equal(
    await fetchBackground(
      async () => {
        throw new Error(TOKEN);
      },
      SERVER,
      TOKEN,
      "x",
    ),
    null,
  );
  assert.equal(await fetchBackground(never, "ftp://h", TOKEN, "x"), null);
  assert.equal(await fetchBackground(never, SERVER, "", "x"), null);
});

test("fetchBackground times out into null when the server never answers", async () => {
  // AbortSignal.timeout timers are unref'd in Node, so hold the event loop open until the abort lands.
  const hang = (url, { signal }) =>
    new Promise((_, reject) => {
      const keepAlive = setTimeout(() => {}, 5000);
      signal.addEventListener("abort", () => {
        clearTimeout(keepAlive);
        reject(signal.reason);
      });
    });
  assert.equal(await fetchBackground(hang, SERVER, TOKEN, "x", 20), null);
});

// ---- labels ----

test("labels: the keys are the ones render.js reads, and the texts the server's strings", () => {
  // The server's #mm-labels block also carries the editor's strings; the extension does not edit,
  // so it ships only what the renderer reads: the keys of its label(...) calls.
  const used = [...new Set([...repo("static/magpie/render.js").matchAll(/label\((?:opts\.)?labels,\s*"(\w+)"\)/g)].map((m) => m[1]))];
  assert.ok(used.length >= 4, `only ${used.length} keys found in render.js`);
  const rust = repo("src/pages.rs");
  const block = rust.slice(rust.indexOf("fn labels()"));
  const all = block.slice(0, block.indexOf("\n}"));
  // `label_map!(` lists the keys as string literals; a key the renderer reads must be one of them.
  const keys = used.map((key) => {
    assert.ok(all.includes(`"${key}",`), `the server's labels() has ${key}`);
    return [null, key, key];
  });
  for (const lang of ["en", "nb"]) {
    const toml = repo(`locales/${lang}.toml`);
    assert.deepEqual(Object.keys(shipped[lang]).sort(), [...used].sort());
    for (const [, key, tomlKey] of keys) {
      const line = toml.match(new RegExp(`^${tomlKey} = "(.*)"$`, "m"));
      assert.ok(line, `${lang}.toml has start.${tomlKey}`);
      assert.equal(shipped[lang][key], line[1], `${lang} ${key}`);
    }
  }
});

test("labels: the language comes from the browser language, other languages get English", () => {
  assert.equal(pickLabels("nb-NO").lang, "nb");
  assert.equal(pickLabels("nb").lang, "nb");
  assert.equal(pickLabels("nn-NO").lang, "nb");
  assert.equal(pickLabels("no").lang, "nb");
  assert.equal(pickLabels("en-US").lang, "en");
  assert.equal(pickLabels("de-DE").lang, "en");
  assert.equal(pickLabels("").lang, "en");
  assert.equal(pickLabels(undefined).lang, "en");
  assert.deepEqual(pickLabels("en-GB").labels, shipped.en);
  assert.deepEqual(pickLabels("nb-NO").labels, shipped.nb);
});

test("labels: a key missing in English falls back to Norwegian, never to an empty string", () => {
  const saved = shipped.en.bookmarks_label;
  delete shipped.en.bookmarks_label;
  try {
    assert.equal(pickLabels("en-US").labels.bookmarks_label, shipped.nb.bookmarks_label);
    assert.equal(pickLabels("de").labels.bookmarks_label, shipped.nb.bookmarks_label);
    assert.equal(pickLabels("en-US").labels.empty, shipped.en.empty);
  } finally {
    shipped.en.bookmarks_label = saved;
  }
});

test("a failed new image releases the old blob, and the old id is fetched again later", async () => {
  let failNext = false;
  const v = makeView({
    fetchImpl: async (url) => {
      v.fetchedUrls.push(url);
      if (failNext) throw new TypeError("NetworkError");
      return { status: 200, blob: async () => ({ type: "image/png" }) };
    },
  });
  v.fetchedUrls = [];
  await v.draw({ status: "fresh", data: imageLayout("old") }, CONN);
  assert.equal(bgImage(v.html), `url("${v.urls.created[0]}")`);
  failNext = true;
  await v.draw({ status: "fresh", data: imageLayout("new") }, CONN);
  assert.equal(v.html.getAttribute("data-bg"), "hedgerow");
  assert.equal(bgImage(v.html), "");
  assert.deepEqual(v.urls.revoked, [v.urls.created[0]], "the old blob URL is revoked");
  failNext = false;
  await v.draw({ status: "fresh", data: imageLayout("old") }, CONN);
  assert.deepEqual(v.fetchedUrls.map((u) => u.split("/").pop()), ["old", "new", "old"]);
  assert.equal(v.urls.created.length, 2);
  assert.equal(bgImage(v.html), `url("${v.urls.created[1]}")`);
});

test("the acknowledged toggle opens the list in the new tab", () => {
  const v = makeView();
  const NOW = 1_760_000_000;
  const needs = [
    { service_id: 1, service_name: "Blog", key: "down", level: "bad", label: "down", since: NOW - 60, acked: true },
  ];
  const view = {
    status: "fresh",
    data: layout({ groups: [{ id: "g1", title: "Home", cards: [card("a", "Blog")] }] }),
    services: [{ id: 1, name: "Blog", kind: "http", interval_s: 60, metrics: [] }],
    values: { now: NOW, services: {}, needs },
  };
  v.draw(view, CONN);
  const toggle = v.root.querySelector(".mm-needs-toggle");
  assert.ok(toggle, "the toggle is drawn");
  assert.equal(toggle.textContent, "1 acknowledged");
  const list = v.root.querySelector(".mm-needs-acked");
  assert.ok(list.hasAttribute("hidden"));
  const click = (el) => el.dispatchEvent(new v.doc.defaultView.Event("click", { bubbles: true, cancelable: true }));
  click(toggle);
  assert.equal(toggle.getAttribute("aria-expanded"), "true");
  assert.equal(list.hasAttribute("hidden"), false);
  // Still wired after a redraw (the listener is on the root, not on the toggle).
  v.draw({ ...view, data: layout({ ...view.data }) }, CONN);
  const again = v.root.querySelector(".mm-needs-toggle");
  click(again);
  assert.equal(again.getAttribute("aria-expanded"), "true");
  click(again);
  assert.equal(again.getAttribute("aria-expanded"), "false");
  assert.ok(v.root.querySelector(".mm-needs-acked").hasAttribute("hidden"));
});

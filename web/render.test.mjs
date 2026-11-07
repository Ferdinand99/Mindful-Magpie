// Tests for static/magpie/render.js, run with `node --test` in web/. The
// renderer is DOM-only, so linkedom (a small DOM for Node) is enough.
import assert from "node:assert/strict";
import { test } from "node:test";
import { parseHTML } from "linkedom";

import { render, tileLetter } from "../static/magpie/render.js";

const LABELS = {
  empty: "Your start page is empty.",
  invalid_link: "invalid link",
  group_label: "Group: {title}",
  bookmarks_label: "Bookmarks",
};
const OPTS = { lang: "en", labels: LABELS };

function page() {
  const { document } = parseHTML(
    '<!doctype html><html><body><section id="groups"></section></body></html>',
  );
  return { document, root: document.getElementById("groups") };
}

// linkedom answers undefined for a property that is not set; a browser answers "".
function bgImage(html) {
  return html.style.getPropertyValue("--mm-bg-image") || "";
}

function card(id, title, url, extra = {}) {
  return {
    id,
    title,
    description: "",
    url,
    icon: { emoji: null, colour: "teal" },
    ...extra,
  };
}

function layout(parts = {}) {
  return {
    version: 1,
    background: { kind: "preset", name: "hedgerow" },
    groups: [],
    bookmarks: [],
    ...parts,
  };
}

test("renders_groups_cards_and_bookmarks", () => {
  const { root } = page();
  render(
    root,
    layout({
      groups: [
        {
          id: "g1",
          title: "Media",
          cards: [
            card("c1", "Sonarr", "https://sonarr.example/", { description: "Series" }),
            card("c2", "Radarr", "http://radarr.example/"),
          ],
        },
        { id: "g2", title: "Dev", cards: [card("c3", "GitHub", "https://github.com/")] },
      ],
      bookmarks: [
        { id: "b1", title: "Hedgerow", url: "https://hedgerow.tools/" },
        { id: "b2", title: "UniFi", url: "https://unifi.example/" },
      ],
    }),
    OPTS,
  );

  const groups = [...root.querySelectorAll("section.mm-group")];
  assert.equal(groups.length, 2);
  assert.deepEqual(
    groups.map((g) => g.querySelector("h2").textContent),
    ["Media", "Dev"],
  );
  // The heading's accessible name comes from the label.
  assert.equal(groups[0].querySelector("h2").getAttribute("aria-label"), "Group: Media");

  const first = [...groups[0].querySelectorAll(".mm-card")];
  assert.ok(first.every((el) => el.tagName === "DIV"), "a card is a box, its title is the link");
  assert.deepEqual(
    first.map((a) => a.querySelector(".mm-card-title").textContent),
    ["Sonarr", "Radarr"],
  );
  const sonarr = first[0].querySelector("a.mm-card-link");
  assert.equal(sonarr.textContent, "Sonarr");
  assert.equal(sonarr.getAttribute("href"), "https://sonarr.example/");
  assert.equal(sonarr.getAttribute("rel"), "noreferrer");
  assert.equal(sonarr.hasAttribute("target"), false);
  assert.ok(first[0].classList.contains("glass"));
  assert.equal(first[0].querySelector(".mm-card-desc").textContent, "Series");
  // No empty description element.
  assert.equal(first[1].querySelector(".mm-card-desc"), null);
  assert.equal(groups[1].querySelectorAll(".mm-card-link").length, 1);

  // Text that may be right-to-left is isolated, and the tile is decoration.
  assert.equal(first[0].querySelector(".mm-card-title").getAttribute("dir"), "auto");
  assert.equal(first[0].querySelector(".mm-card-desc").getAttribute("dir"), "auto");
  assert.equal(groups[0].querySelector("h2").getAttribute("dir"), "auto");
  assert.equal(first[0].querySelector(".mm-tile").getAttribute("aria-hidden"), "true");

  const pills = [...root.querySelectorAll("nav.mm-bookmarks a.mm-pill")];
  assert.deepEqual(
    pills.map((a) => a.textContent),
    ["Hedgerow", "UniFi"],
  );
  assert.equal(pills[0].getAttribute("href"), "https://hedgerow.tools/");
  assert.equal(pills[0].getAttribute("dir"), "auto");
  assert.equal(root.querySelector("nav.mm-bookmarks").getAttribute("aria-label"), "Bookmarks");
  // Groups come before the bookmarks, and there is no empty state.
  const order = [...root.children].map((c) => c.className);
  assert.deepEqual(order, ["mm-group", "mm-group", "mm-bookmarks"]);
  assert.equal(root.querySelector(".mm-empty"), null);
});

test("render_replaces_what_was_there_and_is_repeatable", () => {
  const { root } = page();
  root.innerHTML = "<p>old</p>";
  const data = layout({
    groups: [{ id: "g", title: "A", cards: [card("c", "X", "https://x.example/")] }],
  });
  render(root, data, OPTS);
  render(root, data, OPTS);
  assert.equal(root.querySelectorAll("section.mm-group").length, 1);
  assert.equal(root.querySelector("p"), null);
});

test("text_is_never_html", () => {
  const { document, root } = page();
  const hostile = "<img src=x onerror=alert(1)>";
  render(
    root,
    layout({
      groups: [
        {
          id: "g",
          title: hostile,
          cards: [card("c", hostile, "https://x.example/", { description: hostile })],
        },
      ],
      bookmarks: [{ id: "b", title: hostile, url: "https://x.example/" }],
    }),
    OPTS,
  );
  assert.equal(document.querySelectorAll("img").length, 0);
  assert.equal(root.querySelector(".mm-card-title").textContent, hostile);
  assert.equal(root.querySelector(".mm-card-desc").textContent, hostile);
  assert.equal(root.querySelector("h2").textContent, hostile);
  assert.equal(root.querySelector(".mm-pill").textContent, hostile);
  // No attribute anywhere carries a handler.
  for (const el of root.querySelectorAll("*")) {
    for (const name of el.getAttributeNames()) {
      assert.ok(!name.startsWith("on"), `${el.localName} has ${name}`);
    }
  }
});

test("only_http_links_are_links", () => {
  const { root } = page();
  const bad = [
    "javascript:alert(1)",
    "JaVaScRiPt:alert(1)",
    " javascript:alert(1)",
    "data:text/html,<script>alert(1)</script>",
    "vbscript:x",
    "file:///etc/passwd",
    "ftp://x.example/",
    "/relative/path",
    "not a url",
    "",
  ];
  render(
    root,
    layout({
      groups: [{ id: "g", title: "Bad", cards: bad.map((u, i) => card(`c${i}`, `T${i}`, u)) }],
      bookmarks: bad.map((u, i) => ({ id: `b${i}`, title: `B${i}`, url: u })),
    }),
    OPTS,
  );
  assert.equal(root.querySelectorAll("a").length, 0);
  assert.equal(root.querySelectorAll("[href]").length, 0);

  const cards = [...root.querySelectorAll(".mm-card")];
  assert.equal(cards.length, bad.length);
  for (const c of cards) {
    assert.equal(c.localName, "div");
    assert.ok(c.classList.contains("mm-card-invalid"));
    assert.equal(c.querySelector(".mm-invalid").textContent, "invalid link");
  }
  for (const p of root.querySelectorAll(".mm-pill")) {
    assert.equal(p.localName, "span");
    assert.equal(p.querySelector(".mm-invalid").textContent, "invalid link");
  }

  // http and https are links.
  const ok = page();
  render(
    ok.root,
    layout({
      bookmarks: [
        { id: "a", title: "A", url: "http://a.example/" },
        { id: "b", title: "B", url: "https://b.example/x?y=1" },
      ],
    }),
    OPTS,
  );
  assert.equal(ok.root.querySelectorAll("a[href]").length, 2);
});

test("empty_layout_shows_the_empty_state", () => {
  const { root } = page();
  render(root, layout(), OPTS);
  const empty = root.querySelector(".mm-empty");
  assert.ok(empty);
  assert.equal(empty.querySelector(".empty-title").textContent, "Your start page is empty.");
  assert.equal(root.querySelector(".mm-group"), null);
  assert.equal(root.querySelector(".mm-bookmarks"), null);

  // Localized: the text is whatever the labels say.
  const nb = page();
  render(
    nb.root,
    layout(),
    { lang: "nb", labels: { ...LABELS, empty: "Startsiden er tom." } },
  );
  assert.equal(nb.root.querySelector(".empty-title").textContent, "Startsiden er tom.");

  // Bookmarks alone are not empty.
  const some = page();
  render(
    some.root,
    layout({ bookmarks: [{ id: "b", title: "B", url: "https://b.example/" }] }),
    OPTS,
  );
  assert.equal(some.root.querySelector(".mm-empty"), null);
});

test("tile_letter", () => {
  assert.equal(tileLetter("netflix"), "N");
  assert.equal(tileLetter("Ærfugl"), "Æ");
  assert.equal(tileLetter("æble"), "Æ");
  assert.equal(tileLetter(""), "?");
  assert.equal(tileLetter("   "), "?");
  assert.equal(tileLetter("  vole"), "V");
  // A grapheme, not a UTF-16 unit or a code point.
  assert.equal(tileLetter("😀 smile"), "😀");
  assert.equal(tileLetter("👨‍👩‍👧 family"), "👨‍👩‍👧");
  assert.equal(tileLetter("école"), "É");
  // One grapheme even when upper-casing makes two letters.
  assert.equal(tileLetter("ßeta"), "S");
  assert.equal(tileLetter("izmir", "tr"), "İ");
  assert.equal(tileLetter("x", "not a language"), "X");

  // In the DOM: the emoji icon wins over the letter; colour is a data attribute.
  const { root } = page();
  render(
    root,
    layout({
      groups: [
        {
          id: "g",
          title: "G",
          cards: [
            card("c1", "netflix", "https://n.example/", { icon: { emoji: "🎬", colour: "red" } }),
            card("c2", "netflix", "https://n.example/", { icon: { emoji: null, colour: "teal" } }),
            card("c3", "x", "https://n.example/", { icon: { emoji: null, colour: "nonsense" } }),
          ],
        },
      ],
    }),
    OPTS,
  );
  const tiles = [...root.querySelectorAll(".mm-tile")];
  assert.deepEqual(
    tiles.map((t) => t.textContent),
    ["🎬", "N", "X"],
  );
  assert.deepEqual(
    tiles.map((t) => t.getAttribute("data-colour")),
    ["red", "teal", "gray"],
  );
});

test("background_preset_and_image", () => {
  const { document, root } = page();
  const html = document.documentElement;

  render(root, layout({ background: { kind: "preset", name: "natt" } }), OPTS);
  assert.equal(html.getAttribute("data-bg"), "natt");
  assert.equal(bgImage(html), "");

  render(root, layout({ background: { kind: "image", id: "abc123" } }), OPTS);
  assert.equal(html.getAttribute("data-bg"), "image");
  assert.equal(bgImage(html), 'url("/start/background/abc123")');
  // The page never gets a style attribute in its markup.
  assert.equal(root.querySelectorAll("[style]").length, 0);

  // Back to a preset: the image property goes away.
  render(root, layout({ background: { kind: "preset", name: "hav" } }), OPTS);
  assert.equal(html.getAttribute("data-bg"), "hav");
  assert.equal(bgImage(html), "");

  // An unknown preset falls back to the default.
  render(root, layout({ background: { kind: "preset", name: "nope" } }), OPTS);
  assert.equal(html.getAttribute("data-bg"), "hedgerow");

  // An id that is not a plain id cannot break out of the url("...").
  render(root, layout({ background: { kind: "image", id: '");x:y;("' } }), OPTS);
  assert.equal(html.getAttribute("data-bg"), "hedgerow");
  assert.equal(bgImage(html), "");

  // The extension serves the image from another origin.
  render(root, layout({ background: { kind: "image", id: "abc" } }), {
    ...OPTS,
    imageBase: "https://magpie.example",
  });
  assert.equal(bgImage(html), 'url("https://magpie.example/start/background/abc")');
});

test("odd_input_does_not_throw", () => {
  const { root } = page();
  for (const bad of [
    null,
    undefined,
    {},
    { groups: "x", bookmarks: 3 },
    { groups: [null, 3], bookmarks: [null, "x"] },
  ]) {
    render(root, bad, OPTS);
    assert.ok(root.querySelector(".mm-empty"), JSON.stringify(bad));
  }
  // Missing labels give empty strings, never "undefined".
  render(
    root,
    layout({ groups: [{ id: "g", title: "G", cards: [card("c", "C", "nope")] }] }),
    { lang: "en", labels: {} },
  );
  assert.ok(!root.textContent.includes("undefined"));
});

test("image_base_is_an_origin_or_nothing", () => {
  const { document, root } = page();
  const html = document.documentElement;
  const image = { background: { kind: "image", id: "abc" } };
  const cases = [
    // [imageBase, expected url]
    ['x");a:b;("', 'url("/start/background/abc")'],
    ["javascript:x", 'url("/start/background/abc")'],
    ['https://a.example");a:b;("', 'url("/start/background/abc")'],
    ["data:text/html,x", 'url("/start/background/abc")'],
    ["", 'url("/start/background/abc")'],
    [42, 'url("/start/background/abc")'],
    ["https://magpie.example/", 'url("https://magpie.example/start/background/abc")'],
    ["https://magpie.example:8443/some/path?q=1", 'url("https://magpie.example:8443/start/background/abc")'],
    ["http://10.0.3.152:3010", 'url("http://10.0.3.152:3010/start/background/abc")'],
    ["http://[::1]:3010/x", 'url("http://[::1]:3010/start/background/abc")'],
    // Hosts the URL parser accepts but that would end the url("...").
    ['https://x"y.example', 'url("/start/background/abc")'],
    ['https://x"),red,url("y.example', 'url("/start/background/abc")'],
    ["https://x;y.example", 'url("/start/background/abc")'],
    ["https://x(y).example", 'url("/start/background/abc")'],
    ["https://x'y.example", 'url("/start/background/abc")'],
  ];
  for (const [imageBase, expected] of cases) {
    render(root, layout(image), { ...OPTS, imageBase });
    assert.equal(bgImage(html), expected, String(imageBase));
  }
});

test("the_root_gets_the_hook_class", () => {
  const { root } = page();
  render(root, layout(), OPTS);
  assert.ok(root.classList.contains("mm-root"));
});

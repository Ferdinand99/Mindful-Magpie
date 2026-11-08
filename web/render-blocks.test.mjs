// Tests for the detail blocks and the card sizes of static/magpie/render.js: lists, bars, strips
// and facts next to the numbers, normal and wide cards, and live updates that keep a focused link.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { parseHTML } from "linkedom";

import { render, updateValues } from "../static/magpie/render.js";

const LABELS = {
  empty: "Empty",
  invalid_link: "invalid link",
  group_label: "Group: {title}",
  bookmarks_label: "Bookmarks",
  service_missing: "Gone",
  service_waiting: "Waiting",
  status_ok: "Status: OK",
  status_warn: "Status: warning",
  status_bad: "Status: problem",
  status_unknown: "Status: unknown",
  updated: "Last updated {when}",
  block_more: "+ {n} more",
  spark_line: "Now {value}, from {min} to {max}",
  spark_counter: "{n} in the last 24 hours",
  spark_updown: "{n} half hours with something down",
  "detail.down_now": "Down now",
  "detail.uptime": "Last 24 hours",
  "detail.disks": "Disks",
  "detail.lib": "Libraries",
  "detail.pct": "Percent",
  "detail.since": "Since",
  "detail.badge": "Transcoding",
};
const EN = { lang: "en", labels: LABELS };
const NOW = "2026-11-07T12:00:00Z";
const NOW_MS = Date.parse(NOW);

const metric = (id, label) => ({ id, label, format: "number" });
const SERVICES = [
  {
    id: 1,
    name: "Plain",
    kind: "http",
    interval_s: 60,
    size: "normal",
    link: "https://plain.example",
    metrics: [metric("a", "A"), metric("b", "B"), metric("c", "C"), metric("d", "D"), metric("e", "E"), metric("f", "F")],
  },
  {
    id: 2,
    name: "Rich",
    kind: "github",
    interval_s: 60,
    size: "wide",
    link: "https://rich.example",
    metrics: [metric("a", "A"), metric("b", "B"), metric("c", "C")],
  },
];

const item = (text, extra = {}) => ({ text, ...extra });
const LIST = (n, extra = {}) => ({
  kind: "list",
  title_key: "down_now",
  items: Array.from({ length: n }, (_, i) =>
    item(`line ${i + 1}`, { sub: `sub ${i + 1}`, status: "warn", right: `${i + 1} min`, href: `https://x.example/${i + 1}`, ...extra }),
  ),
});
const BARS = {
  kind: "bars",
  title_key: "disks",
  bars: [
    { label: "disk1", pct: 88, text: "88 %" },
    { label: "disk2", pct: 61, text: "61 %" },
  ],
};
const STRIP = { kind: "strip", title_key: "uptime", cells: ["ok", "ok", "bad", "warn", "unknown"] };
const FACTS = {
  kind: "facts",
  title_key: "lib",
  rows: [
    { label_key: "pct", value: "42 %" },
    { label: "Plain label", value: "7" },
  ],
};

function entry(details, over = {}) {
  return {
    name: "x",
    kind: "http",
    interval_s: 60,
    status: "ok",
    values: { a: { num: 1 }, b: { num: 2 } },
    details,
    fetched_at: NOW,
    ...over,
  };
}
const valuesFor = (id, e) => ({ now: NOW, services: { [String(id)]: e } });

function boundCard(id, serviceId, extra = {}) {
  return {
    id,
    title: `Card ${id}`,
    description: "",
    url: "",
    icon: { emoji: null, colour: "teal" },
    service: { id: serviceId, metrics: ["a", "b"] },
    ...extra,
  };
}
const layoutOf = (...cards) => ({
  version: 1,
  background: { kind: "preset", name: "hedgerow" },
  groups: [{ id: "g1", title: "G", cards }],
  bookmarks: [],
});
function page() {
  const { document } = parseHTML('<!doctype html><html><body><section id="groups"></section></body></html>');
  return { document, root: document.getElementById("groups") };
}
function draw(card, details, opts = {}) {
  const p = page();
  const layout = layoutOf(card);
  const serviceId = card.service.id;
  const o = { ...EN, services: SERVICES, values: valuesFor(serviceId, entry(details)), ...opts };
  render(p.root, layout, o);
  return { ...p, layout, opts: o, card: p.root.querySelector(".mm-card") };
}
const texts = (root, selector) => [...root.querySelectorAll(selector)].map((el) => el.textContent);

test("a_normal_card_cuts_the_first_list_at_three_lines_and_says_how_many_more", () => {
  const { card } = draw(boundCard("c1", 1), [LIST(6)]);
  assert.equal(card.querySelectorAll(".mm-li").length, 3);
  assert.deepEqual(texts(card, ".mm-li-text"), ["line 1", "line 2", "line 3"]);
  assert.deepEqual(texts(card, ".mm-block-more"), ["+ 3 more"]);
  assert.deepEqual(texts(card, ".mm-block-title"), ["Down now"]);
  // A list of three or fewer is shown whole with no note.
  const short = draw(boundCard("c1", 1), [LIST(3)]).card;
  assert.equal(short.querySelectorAll(".mm-li").length, 3);
  assert.equal(short.querySelector(".mm-block-more"), null);
});

test("a_normal_card_shows_the_first_list_and_the_first_bars_or_strip_and_no_more", () => {
  const { card } = draw(boundCard("c1", 1), [LIST(2), LIST(2), STRIP, BARS, FACTS]);
  const kinds = [...card.querySelectorAll(".mm-block")].map((b) => b.getAttribute("class").replace("mm-block ", ""));
  assert.deepEqual(kinds, ["mm-block-list", "mm-block-strip"], "one list, one visual, no facts");
  const barsFirst = draw(boundCard("c1", 1), [FACTS, BARS, STRIP]).card;
  assert.deepEqual(
    [...barsFirst.querySelectorAll(".mm-block")].map((b) => b.getAttribute("class").replace("mm-block ", "")),
    ["mm-block-bars"],
  );
});

test("a_wide_card_shows_every_block_whole_in_order", () => {
  const { card } = draw(boundCard("c1", 1, { size: "wide" }), [LIST(8), BARS, STRIP, FACTS, LIST(1)]);
  assert.equal(card.querySelectorAll(".mm-li").length, 9, "no cut");
  assert.equal(card.querySelector(".mm-block-more"), null);
  assert.deepEqual(
    [...card.querySelectorAll(".mm-block")].map((b) => b.getAttribute("class").replace("mm-block ", "")),
    ["mm-block-list", "mm-block-bars", "mm-block-strip", "mm-block-facts", "mm-block-list"],
  );
});

test("a_card_is_wide_by_its_own_size_or_else_by_its_services_default", () => {
  const cls = (card) => card.getAttribute("class");
  // The card names none: the service's size decides.
  assert.match(cls(draw(boundCard("c1", 2), []).card), /\bmm-card-wide\b/);
  assert.doesNotMatch(cls(draw(boundCard("c1", 1), []).card), /mm-card-wide/);
  // Its own size wins over the service's.
  assert.doesNotMatch(cls(draw(boundCard("c1", 2, { size: "normal" }), []).card), /mm-card-wide/);
  assert.match(cls(draw(boundCard("c1", 1, { size: "wide" }), []).card), /\bmm-card-wide\b/);
  assert.equal(draw(boundCard("c1", 2), []).card.getAttribute("data-size"), "wide");
  // Without a size from the server (an older one) it is normal; a nonsense size is ignored.
  const old = SERVICES.map(({ size, ...rest }) => rest);
  assert.doesNotMatch(cls(draw(boundCard("c1", 2), [], { services: old }).card), /mm-card-wide/);
  assert.doesNotMatch(cls(draw(boundCard("c1", 1, { size: "huge" }), []).card), /mm-card-wide/);
  // A plain card has no size.
  const p = page();
  render(p.root, layoutOf({ id: "p", title: "P", description: "", url: "https://p.example/", icon: { emoji: null, colour: "teal" } }), EN);
  assert.equal(p.root.querySelector(".mm-card").hasAttribute("data-size"), false);
});

test("the_css_spans_two_columns_for_a_wide_card_when_the_grid_is_wide_enough", () => {
  const css = readFileSync(new URL("../static/magpie/render.css", import.meta.url), "utf8");
  // The grid is the container; two columns of it (2 x 260px and the gap, with a few px to spare) fit in
  // 536px, in px so that a larger root font size does not move it.
  assert.match(css, /\.mm-cards \{[^}]*container-type: inline-size;/);
  const query = css.match(/@container \(min-width: 536px\) \{([\s\S]*?)\n\}\n/)[1];
  assert.match(query, /\.mm-card-wide \{\s*grid-column: span 2;/);
  assert.doesNotMatch(css.replace(query, ""), /grid-column: span 2/, "the span is inside the container query only");
  assert.match(query, /\.mm-card-wide \.mm-metrics \{\s*grid-template-columns: repeat\(5, /);
  assert.match(query, /column-count: 2;/);
  assert.doesNotMatch(css, /@media \(min-width: 700px\)/);
  assert.match(css, /\.mm-metrics \{[^}]*grid-template-columns: repeat\(2, /, "two columns on a normal card");
});

test("the_css_stretches_the_title_link_over_the_card_and_keeps_block_links_above_it", () => {
  const css = readFileSync(new URL("../static/magpie/render.css", import.meta.url), "utf8");
  assert.match(css, /\.mm-card \{\s*position: relative;/);
  assert.match(css, /\.mm-card-link\[href\]::after \{\s*content: "";\s*position: absolute;\s*(?:\/\*[^*]*\*\/\s*)?inset: -1px;\s*border-radius: var\(--r-lg\);/);
  // The ring is the card's, and the hover is the card's too.
  assert.match(css, /\.mm-card-link:focus-visible::after \{\s*outline: 2px solid var\(--brand\)/);
  assert.match(css, /\.mm-card:has\(\.mm-card-link\[href\]:hover\)/);
  assert.match(css, /\.mm-li-link \{\s*position: relative;\s*z-index: 1;/);
  // No rule is left for a card that is a link.
  assert.doesNotMatch(css, /a\.mm-card/);
});

test("a_card_is_a_box_whose_title_is_the_link_and_no_link_holds_another", () => {
  const { card, root } = draw(boundCard("c1", 2, { url: "https://own.example/" }), [LIST(3), BARS]);
  assert.equal(card.tagName, "DIV");
  const title = card.querySelector(".mm-card-text > .mm-card-title");
  assert.equal(title.tagName, "A");
  assert.equal(title.getAttribute("href"), "https://own.example/");
  assert.equal(title.getAttribute("rel"), "noreferrer");
  assert.ok(title.classList.contains("mm-card-link"));
  assert.equal(card.querySelector(".mm-svc").tagName, "DIV");
  const links = [...root.querySelectorAll("a")];
  assert.ok(links.length >= 4);
  for (const a of links) assert.equal(a.parentNode.closest?.("a") ?? null, null, "no link inside a link");
  // A card with no link has a title that is not one.
  const none = draw(boundCard("c2", 2, { url: "" }), [], { services: [{ ...SERVICES[1], link: "" }] }).card;
  assert.equal(none.querySelector(".mm-card-title").tagName, "SPAN");
  assert.equal(none.querySelector("a"), null);
});

test("lines_show_a_status_shape_a_second_line_a_right_value_and_a_same_tab_link", () => {
  const { card } = draw(boundCard("c1", 1), [
    {
      kind: "list",
      title_key: "down_now",
      items: [
        item("with all", { sub: "second", status: "bad", right: "3 min", href: "https://x.example/a?b=1" }),
        item("plain"),
        item("unsafe", { href: "javascript:alert(1)" }),
      ],
    },
  ]);
  const [first, second, third] = card.querySelectorAll(".mm-li");
  assert.equal(first.querySelector(".mm-status").getAttribute("data-status"), "bad");
  assert.equal(first.querySelector(".mm-status").getAttribute("aria-label"), "Status: problem");
  assert.equal(first.querySelector(".mm-li-sub").textContent, "second");
  assert.equal(first.querySelector(".mm-li-right").textContent, "3 min");
  const link = first.querySelector("a.mm-li-text");
  assert.equal(link.getAttribute("href"), "https://x.example/a?b=1");
  assert.equal(link.hasAttribute("target"), false, "same tab");
  assert.equal(link.getAttribute("rel"), "noreferrer");
  assert.equal(second.querySelector(".mm-status"), null, "no status, no dot");
  assert.equal(second.querySelector("a"), null);
  assert.equal(third.querySelector("a"), null, "a javascript: link is text");
  assert.equal(third.querySelector(".mm-li-text").textContent, "unsafe");
});

test("bars_have_their_width_in_a_custom_property_and_a_clamped_percentage", () => {
  const { card } = draw(boundCard("c1", 1), [
    {
      kind: "bars",
      title_key: "disks",
      bars: [
        { label: "ok", pct: 40.5, text: "40 %" },
        { label: "over", pct: 1e9, text: "x" },
        { label: "nan", pct: Number.NaN, text: "y" },
        { label: "neg", pct: -7, text: "z" },
      ],
    },
  ]);
  const fills = [...card.querySelectorAll(".mm-bar-fill")].map((f) => f.style.getPropertyValue("--mm-pct"));
  assert.deepEqual(fills, ["40.5%", "100%", "0%", "0%"]);
  const bar = card.querySelector(".mm-bar");
  assert.equal(bar.getAttribute("role"), "progressbar");
  assert.equal(bar.getAttribute("aria-valuenow"), "41");
  assert.equal(bar.getAttribute("aria-label"), "ok");
  assert.deepEqual(texts(card, ".mm-bar-label"), ["ok", "over", "nan", "neg"]);
  assert.deepEqual(texts(card, ".mm-bar-text"), ["40 %", "x", "y", "z"]);
});

test("a_strip_has_a_cell_for_each_status_and_is_named_as_one_picture", () => {
  const { card } = draw(boundCard("c1", 1), [STRIP]);
  const cells = [...card.querySelectorAll(".mm-cell")].map((c) => c.getAttribute("data-status"));
  assert.deepEqual(cells, ["ok", "ok", "bad", "warn", "unknown"]);
  const strip = card.querySelector(".mm-strip");
  assert.equal(strip.getAttribute("role"), "img");
  assert.equal(strip.getAttribute("aria-label"), "Last 24 hours, Status: problem: 1, Status: warning: 1");
  // A status the renderer does not know reads unknown.
  const odd = draw(boundCard("c1", 1), [{ kind: "strip", title_key: "uptime", cells: ["ok", "boom", 3, null] }]).card;
  assert.deepEqual(
    [...odd.querySelectorAll(".mm-cell")].map((c) => c.getAttribute("data-status")),
    ["ok", "unknown", "unknown", "unknown"],
  );
});

test("facts_take_their_label_from_a_key_or_from_text", () => {
  const { card } = draw(boundCard("c1", 2), [FACTS]);
  assert.deepEqual(texts(card, ".mm-fact-label"), ["Percent", "Plain label"]);
  assert.deepEqual(texts(card, ".mm-fact-value"), ["42 %", "7"]);
});

test("an_unknown_key_shows_nothing_and_a_key_of_the_wrong_shape_is_never_looked_up", () => {
  const labels = { ...LABELS, "detail.Bad Key": "LEAK" };
  const { card } = draw(
    boundCard("c1", 2),
    [
      { kind: "list", title_key: "no_such_key", items: [item("a")] },
      { kind: "list", title_key: "Bad Key", items: [item("b")] },
      { kind: "facts", title_key: "__proto__", rows: [{ label_key: "no_such_key", value: "v" }] },
    ],
    { labels },
  );
  assert.equal(card.querySelector(".mm-block-title"), null);
  assert.equal(card.textContent.includes("LEAK"), false);
  // The lists are drawn without a title; the facts have no row with a usable label, so they are not.
  assert.equal(card.querySelectorAll(".mm-block-list").length, 2);
  assert.equal(card.querySelector(".mm-block-facts"), null);
  assert.equal(card.querySelectorAll(".mm-block").length, 2);
});

test("a_fact_without_a_usable_label_is_left_out_and_so_is_a_block_with_nothing_to_draw", () => {
  const { card } = draw(boundCard("c1", 2), [
    {
      kind: "facts",
      title_key: "lib",
      rows: [
        { value: "no label" },
        { label_key: "no_such_key", value: "unknown key, no text" },
        { label_key: "no_such_key", label: "Own text", value: "kept" },
        { label_key: "pct", value: "named by key" },
      ],
    },
    { kind: "list", title_key: "down_now", items: [{ text: "" }, { sub: "no text" }] },
    { kind: "bars", title_key: "disks", bars: [] },
    { kind: "strip", title_key: "uptime", cells: [] },
  ]);
  assert.deepEqual(texts(card, ".mm-fact-label"), ["Own text", "Percent"]);
  assert.deepEqual(texts(card, ".mm-fact-value"), ["kept", "named by key"]);
  assert.equal(card.querySelectorAll(".mm-block").length, 1, "the empty blocks are not drawn, not even a title");
});

test("hostile_text_is_drawn_as_text_and_the_counts_are_capped_again", () => {
  const evil = '<img src=x onerror="alert(1)"><script>alert(2)</script>‮';
  const { card } = draw(boundCard("c1", 2), [
    {
      kind: "list",
      title_key: "down_now",
      items: [
        ...Array.from({ length: 30 }, (_, i) => item(i === 0 ? evil : `i${i}`, { sub: evil, right: evil })),
      ],
    },
    { kind: "strip", title_key: "uptime", cells: Array(500).fill("ok") },
    ...Array.from({ length: 20 }, () => FACTS),
    { kind: "nonsense" },
    null,
    "text",
  ]);
  assert.equal(card.querySelectorAll("img, script").length, 0);
  assert.equal(card.querySelector(".mm-li-text").textContent, evil);
  assert.equal(card.querySelectorAll(".mm-li").length, 8);
  assert.equal(card.querySelectorAll(".mm-cell").length, 48);
  assert.equal(card.querySelectorAll(".mm-block").length, 6, "at most six blocks");
  for (const el of card.querySelectorAll(".mm-li-text, .mm-li-sub, .mm-li-right")) {
    assert.equal(el.getAttribute("dir"), "auto");
  }
});

test("no_inline_style_is_written_as_an_attribute_by_the_renderer", () => {
  const source = readFileSync(new URL("../static/magpie/render.js", import.meta.url), "utf8");
  assert.equal(/setAttribute\(\s*["']style["']\s*,/.test(source), false, "no setAttribute('style', ...)");
  assert.equal(/\.style\s*\.\s*cssText|\.style\s*=[^=]/.test(source), false, "no cssText or style assignment");
  assert.equal(/innerHTML|outerHTML|insertAdjacentHTML/.test(source), false);
  // Custom properties only, through the CSS object model.
  const used = [...source.matchAll(/style\??\.setProperty\(\s*"([^"]+)"/g)].map((m) => m[1]);
  assert.deepEqual([...new Set(used)].sort(), ["--mm-bg-image", "--mm-pct"]);
  // The markup of a card with every kind of block has no style attribute but the bar's property.
  const { card } = draw(boundCard("c1", 2), [LIST(3), BARS, STRIP, FACTS]);
  for (const el of card.querySelectorAll("[style]")) {
    assert.equal(el.getAttribute("class"), "mm-bar-fill");
    assert.match(el.getAttribute("style"), /^--mm-pct: ?[\d.]+%;?$/);
  }
});

test("a_card_without_details_or_with_an_old_server_draws_as_before", () => {
  for (const details of [undefined, null, [], "x", {}]) {
    const { card } = draw(boundCard("c1", 1), details);
    assert.equal(card.querySelector(".mm-blocks"), null, String(details));
    assert.equal(card.querySelectorAll(".mm-metric").length, 2);
  }
  // A waiting service draws no blocks.
  const p = page();
  render(p.root, layoutOf(boundCard("c1", 1)), { ...EN, services: SERVICES, values: { now: NOW, services: {} } });
  assert.equal(p.root.querySelector(".mm-blocks"), null);
});

// ---- live updates -----------------------------------------------------------------------------

function liveCard(details, cardExtra = {}) {
  const p = page();
  const layout = layoutOf(boundCard("c1", 2, cardExtra));
  const base = { ...EN, services: SERVICES };
  render(p.root, layout, { ...base, values: valuesFor(2, entry(details)) });
  const update = (next, over = {}) =>
    updateValues(p.root, layout, { ...base, values: valuesFor(2, entry(next, over)), ...over.opts });
  return { ...p, layout, update };
}

test("a_focused_link_inside_a_block_survives_a_live_update", () => {
  const t = liveCard([LIST(4), BARS]);
  const calls = [];
  for (const el of t.root.querySelectorAll("*")) el.focus = () => calls.push("focus");
  const link = t.root.querySelectorAll("a.mm-li-text")[1];
  const row = link.closest(".mm-li");
  const block = t.root.querySelector(".mm-block-list");
  const svc = t.root.querySelector(".mm-svc");
  const bar = t.root.querySelector(".mm-bar-fill");

  // New numbers, a new text and a new value on the same line, a changed bar.
  const next = LIST(4);
  next.items[1] = item("line 2 changed", { sub: "new sub", status: "bad", right: "9 min", href: "https://x.example/2" });
  const bars = { ...BARS, bars: [{ label: "disk1", pct: 95, text: "95 %" }, BARS.bars[1]] };
  const ok = t.update([next, bars], { values: { a: { num: 5 }, b: { num: 6 } } });
  assert.equal(ok, true);

  assert.ok(t.root.querySelectorAll("a.mm-li-text")[1] === link, "the very same <a>");
  assert.equal(link.isConnected, true);
  assert.ok(link.closest(".mm-li") === row);
  assert.ok(t.root.querySelector(".mm-block-list") === block);
  assert.ok(t.root.querySelector(".mm-svc") === svc);
  assert.ok(t.root.querySelector(".mm-bar-fill") === bar, "the bar element is kept");
  assert.equal(link.textContent, "line 2 changed");
  assert.equal(row.querySelector(".mm-li-sub").textContent, "new sub");
  assert.equal(row.querySelector(".mm-li-right").textContent, "9 min");
  assert.equal(row.querySelector(".mm-status").getAttribute("data-status"), "bad");
  assert.equal(bar.style.getPropertyValue("--mm-pct"), "95%");
  assert.equal(t.root.querySelector(".mm-bar-text").textContent, "95 %");
  assert.deepEqual(texts(t.root, ".mm-metric-value"), ["5", "6"]);
  assert.deepEqual(calls, [], "no focus call");
});

test("a_block_whose_shape_changed_is_drawn_again_alone_and_the_others_stay", () => {
  const t = liveCard([LIST(3), STRIP]);
  const list = t.root.querySelector(".mm-block-list");
  const link = t.root.querySelector("a.mm-li-text");
  const strip = t.root.querySelector(".mm-block-strip");
  // The strip becomes facts: that block is new, the list (and its focused link) is not.
  t.update([LIST(3), FACTS]);
  assert.ok(t.root.querySelector(".mm-block-list") === list);
  assert.ok(t.root.querySelector("a.mm-li-text") === link);
  assert.equal(t.root.querySelector(".mm-block-strip"), null);
  assert.equal(strip.isConnected, false);
  assert.equal(t.root.querySelectorAll(".mm-fact").length, 2);
  // Lines come and go: the ones that stay are kept.
  t.update([LIST(5), FACTS]);
  assert.ok(t.root.querySelector("a.mm-li-text") === link);
  assert.equal(t.root.querySelectorAll(".mm-li").length, 5);
  t.update([LIST(2), FACTS]);
  assert.equal(t.root.querySelectorAll(".mm-li").length, 2);
  assert.ok(t.root.querySelector("a.mm-li-text") === link);
  // Blocks that go away entirely.
  t.update([]);
  assert.equal(t.root.querySelector(".mm-blocks"), null);
  assert.equal(t.root.querySelectorAll(".mm-metric").length, 2);
  t.update(undefined);
  assert.equal(t.root.querySelector(".mm-blocks"), null);
});

test("a_card_whose_size_changed_is_drawn_again_by_update_values", () => {
  const p = page();
  const base = { ...EN, services: SERVICES, values: valuesFor(2, entry([LIST(5), BARS])) };
  render(p.root, layoutOf(boundCard("c1", 2, { size: "normal" })), base);
  const el = p.root.querySelector(".mm-card");
  assert.equal(el.querySelectorAll(".mm-li").length, 3);
  const wide = layoutOf(boundCard("c1", 2, { size: "wide" }));
  updateValues(p.root, wide, base);
  const after = p.root.querySelector(".mm-card");
  assert.equal(after.getAttribute("data-size"), "wide");
  assert.equal(after.querySelectorAll(".mm-li").length, 5);
});

test("the_ages_still_tick_with_blocks_on_the_card", async () => {
  const { tickAges } = await import("../static/magpie/render.js");
  const { root } = draw(boundCard("c1", 2), [LIST(2)]);
  tickAges(root, { ...EN, now: NOW_MS + 600_000 });
  assert.equal(root.querySelectorAll(".mm-li").length, 2);
  assert.ok(root.querySelector(".mm-age").textContent.length > 0);
});

// ---- bars, strips, ages, badges, titles ----------------------------------------------------------

test("a_bar_may_have_a_status_that_colours_its_fill_and_says_its_text_in_words", () => {
  const { card } = draw(boundCard("c1", 2), [
    {
      kind: "bars",
      title_key: "disks",
      bars: [
        { label: "disk1", pct: 88, text: "88 % 12.3 of 14 TB", status: "warn" },
        { label: "disk2", pct: 99, text: "99 %", status: "bad" },
        { label: "disk3", pct: 10, text: "10 %", status: "ok" },
        { label: "disk4", pct: 10, text: "", status: "boom" },
        { label: "disk5", pct: 10, text: "10 %" },
      ],
    },
  ]);
  const fills = [...card.querySelectorAll(".mm-bar-fill")].map((f) => f.getAttribute("data-status"));
  assert.deepEqual(fills, ["warn", "bad", "ok", null, null]);
  const bars = [...card.querySelectorAll(".mm-bar")];
  assert.equal(bars[0].getAttribute("aria-valuetext"), "88 % 12.3 of 14 TB");
  assert.equal(bars[3].hasAttribute("aria-valuetext"), false, "no text, no valuetext");
  const css = readFileSync(new URL("../static/magpie/render.css", import.meta.url), "utf8");
  for (const status of ["ok", "warn", "bad"]) assert.match(css, new RegExp(`\\.mm-bar-fill\\[data-status="${status}"\\] \\{\\s*background: var\\(--${status}-text\\)`));
});

test("a_strip_is_a_picture_only_when_it_has_a_name", () => {
  const named = draw(boundCard("c1", 1), [STRIP]).card.querySelector(".mm-strip");
  assert.equal(named.getAttribute("role"), "img");
  assert.ok(named.getAttribute("aria-label").startsWith("Last 24 hours"));
  assert.equal(named.hasAttribute("aria-hidden"), false);
  // No title and nothing wrong in it: nothing to say, so it is hidden from a screen reader.
  const quiet = draw(boundCard("c1", 1), [{ kind: "strip", title_key: "no_such_key", cells: ["ok", "ok"] }]).card.querySelector(".mm-strip");
  assert.equal(quiet.hasAttribute("role"), false);
  assert.equal(quiet.hasAttribute("aria-label"), false);
  assert.equal(quiet.getAttribute("aria-hidden"), "true");
  // Without a title but with a problem in it, it still has something to say.
  const bad = draw(boundCard("c1", 1), [{ kind: "strip", title_key: "", cells: ["ok", "bad"] }]).card.querySelector(".mm-strip");
  assert.equal(bad.getAttribute("role"), "img");
  assert.equal(bad.getAttribute("aria-label"), "Status: problem: 1");
});

test("a_block_is_named_by_its_visible_title_with_an_id_that_is_its_own", () => {
  const p = page();
  const layout = layoutOf(boundCard("c1", 2), boundCard("c2", 2));
  const values = { now: NOW, services: { 2: entry([LIST(2), BARS, { kind: "list", title_key: "no_such_key", items: [item("untitled")] }]) } };
  render(p.root, layout, { ...EN, services: SERVICES, values });
  const blocks = [...p.root.querySelectorAll(".mm-block")];
  assert.equal(blocks.length, 6);
  const ids = [...p.root.querySelectorAll(".mm-block-title")].map((t) => t.getAttribute("id"));
  assert.equal(ids.length, 4, "the block without a known title has none");
  assert.equal(new Set(ids).size, ids.length, "every id is unique on the page");
  for (const block of blocks) {
    const title = block.querySelector(".mm-block-title");
    if (title) {
      assert.equal(block.getAttribute("role"), "group");
      assert.equal(block.getAttribute("aria-labelledby"), title.getAttribute("id"));
    } else {
      assert.equal(block.hasAttribute("aria-labelledby"), false);
    }
  }
});

const ago = (seconds) => NOW_MS / 1000 - seconds;
const relative = (value, unit) => new Intl.RelativeTimeFormat("en", { numeric: "auto", style: "short" }).format(value, unit);

test("a_moment_is_shown_as_an_age_in_place_of_the_text_and_is_kept_current", async () => {
  const { tickAges } = await import("../static/magpie/render.js");
  const { root } = draw(boundCard("c1", 2), [
    {
      kind: "list",
      title_key: "down_now",
      items: [
        item("with a moment", { right: "fallback text", at: ago(720) }),
        item("not a moment", { right: "kept text", at: "yesterday" }),
        item("no moment", { right: "plain" }),
      ],
    },
    { kind: "facts", title_key: "lib", rows: [{ label: "Last check", value: "fallback", at: ago(7200) }, { label: "Other", value: "plain value" }] },
  ]);
  const rights = texts(root, ".mm-li-right");
  assert.equal(rights[0], relative(-12, "minute"));
  assert.deepEqual(rights.slice(1), ["kept text", "plain"], "no usable moment: the text");
  assert.equal(root.querySelector(".mm-li-right").getAttribute("data-since"), String(ago(720) * 1000));
  assert.deepEqual(texts(root, ".mm-fact-value"), [relative(-2, "hour"), "plain value"]);
  // The ages move on with the clock, and only they.
  tickAges(root, { ...EN, now: NOW_MS + 3600 * 1000 });
  assert.equal(texts(root, ".mm-li-right")[0], relative(-1, "hour"));
  assert.deepEqual(texts(root, ".mm-li-right").slice(1), ["kept text", "plain"]);
  assert.equal(texts(root, ".mm-fact-value")[0], relative(-3, "hour"));
  assert.equal(texts(root, ".mm-fact-value")[1], "plain value");
});

test("a_moment_in_a_live_update_is_counted_again_and_in_the_readers_language", () => {
  const t = liveCard([{ kind: "list", title_key: "down_now", items: [item("x", { right: "t", at: ago(720) })] }]);
  assert.equal(texts(t.root, ".mm-li-right")[0], relative(-12, "minute"));
  t.update([{ kind: "list", title_key: "down_now", items: [item("x", { right: "t", at: ago(7200) })] }]);
  assert.equal(texts(t.root, ".mm-li-right")[0], relative(-2, "hour"));
  // Norwegian, through the same helper the card's own age uses.
  const nb = draw(boundCard("c1", 2), [{ kind: "list", title_key: "down_now", items: [item("x", { at: ago(720) })] }], { lang: "nb" });
  assert.equal(
    nb.root.querySelector(".mm-li-right").textContent,
    new Intl.RelativeTimeFormat("nb", { numeric: "auto", style: "short" }).format(-12, "minute"),
  );
});

test("a_badge_is_a_detail_key_and_an_unknown_key_shows_nothing", () => {
  const { card } = draw(boundCard("c1", 2), [
    {
      kind: "list",
      title_key: "down_now",
      items: [item("known", { badge_key: "badge" }), item("unknown", { badge_key: "no_such_key" }), item("wrong shape", { badge_key: "Bad Key" }), item("none")],
    },
  ]);
  assert.deepEqual(texts(card, ".mm-badge"), ["Transcoding"]);
  const first = card.querySelector(".mm-li");
  assert.equal(first.querySelector(".mm-li-main .mm-badge").textContent, "Transcoding");
});

test("every_line_has_a_status_slot_so_that_positions_do_not_move", () => {
  const { card } = draw(boundCard("c1", 2), [
    { kind: "list", title_key: "down_now", items: [item("with", { status: "bad" }), item("without")] },
  ]);
  const [a, b] = card.querySelectorAll(".mm-li");
  assert.equal(a.firstChild.getAttribute("class"), "mm-li-slot");
  assert.equal(b.firstChild.getAttribute("class"), "mm-li-slot");
  assert.ok(a.querySelector(".mm-li-slot .mm-status"));
  assert.equal(b.querySelector(".mm-li-slot").childNodes.length, 0);
  assert.equal(a.getAttribute("data-key"), "with");
  // The key is the link when the line has one.
  const linked = draw(boundCard("c1", 2), [{ kind: "list", title_key: "down_now", items: [item("t", { href: "https://x.example/1" })] }]);
  assert.equal(linked.card.querySelector(".mm-li").getAttribute("data-key"), "https://x.example/1");
});

test("a_list_that_is_reordered_keeps_every_line_and_never_moves_the_one_with_the_focus", () => {
  const t = liveCard([LIST(4)]);
  const rows = [...t.root.querySelectorAll(".mm-li")];
  const ul = t.root.querySelector(".mm-list");
  const focused = rows[1].querySelector("a.mm-li-text");
  // The page's focus is on the link of line 2.
  Object.defineProperty(t.document, "activeElement", { get: () => focused, configurable: true });
  const moved = [];
  const insertBefore = Object.getPrototypeOf(ul).insertBefore;
  ul.insertBefore = (node, ref) => {
    moved.push(node);
    return insertBefore.call(ul, node, ref);
  };

  const next = LIST(4);
  next.items = [next.items[3], next.items[0], next.items[2], next.items[1]];
  next.items[3].right = "changed";
  t.update([next]);

  const after = [...t.root.querySelectorAll(".mm-li")];
  assert.deepEqual(
    after.map((li) => li.querySelector("a").textContent),
    ["line 4", "line 1", "line 3", "line 2"],
  );
  // The same elements, in the new order, and the focused one stays the same line.
  assert.deepEqual(after, [rows[3], rows[0], rows[2], rows[1]]);
  assert.ok(after[3].querySelector("a.mm-li-text") === focused);
  assert.equal(after[3].querySelector(".mm-li-right").textContent, "changed");
  assert.equal(moved.includes(rows[1]), false, "the line with the focus was not moved");
  assert.equal(focused.isConnected, true);
  // A line that goes and one that comes: the others are kept.
  const again = LIST(5);
  again.items = [again.items[4], again.items[1]];
  t.update([again]);
  const last = [...t.root.querySelectorAll(".mm-li")];
  assert.deepEqual(last.map((li) => li.querySelector("a").textContent), ["line 5", "line 2"]);
  assert.ok(last[1] === rows[1]);
});

test("lines_that_repeat_a_key_are_matched_in_order", () => {
  const dup = (text) => ({ kind: "list", title_key: "down_now", items: [item(text), item(text), item("other")] });
  const t = liveCard([dup("same")]);
  const rows = [...t.root.querySelectorAll(".mm-li")];
  t.update([dup("same")]);
  assert.deepEqual([...t.root.querySelectorAll(".mm-li")], rows);
});

// ---- curves (spark blocks) -------------------------------------------------------------------------

const NOW_S = NOW_MS / 1000;
const SVG = "http://www.w3.org/2000/svg";
const SPARK = { kind: "spark", title_key: "uptime", metric: "lat", style: "line" };
const UPDOWN = { kind: "spark", title_key: "uptime", metric: "down", style: "up_down" };
/** Points of a series, `ago` seconds before NOW each. */
const series = (...pairs) => pairs.map(([ago, v]) => [NOW_S - ago, v]);
function drawSpark(details, history, card = boundCard("c1", 2)) {
  const p = page();
  const layout = layoutOf(card);
  const o = { ...EN, services: SERVICES, values: valuesFor(card.service.id, entry(details, { history })) };
  render(p.root, layout, o);
  return { ...p, layout, opts: o };
}
const yOf = (points) => points.split(" ").map((pair) => Number(pair.split(",")[1]));

test("a_line_spark_is_an_svg_made_with_the_dom_and_scaled_to_the_lowest_and_highest_value", () => {
  const { root } = drawSpark([SPARK], { lat: series([600, 10], [300, 30], [0, 20]), other: series([0, 1]) });
  const block = root.querySelector(".mm-block-spark");
  assert.ok(block, "a spark block");
  const wrap = block.querySelector(".mm-spark");
  assert.equal(wrap.getAttribute("role"), "img");
  // The block's title names the group; the picture says what the line shows.
  assert.equal(wrap.getAttribute("aria-label"), "Now 20, from 10 to 30");
  assert.equal(block.getAttribute("aria-labelledby"), block.querySelector(".mm-block-title").id);
  const pic = wrap.querySelector("svg");
  assert.equal(pic.namespaceURI, SVG);
  assert.equal(pic.getAttribute("aria-hidden"), "true");
  const line = pic.querySelector("polyline");
  assert.equal(line.namespaceURI, SVG);
  const ys = yOf(line.getAttribute("points"));
  assert.equal(ys.length, 3);
  // The highest value is at the top (small y), the lowest at the bottom.
  assert.ok(ys[1] < ys[2] && ys[2] < ys[0], ys.join(" "));
  // x by time: the last point is at the right end.
  const xs = line.getAttribute("points").split(" ").map((pair) => Number(pair.split(",")[0]));
  assert.equal(xs[2], 288);
  assert.ok(xs[0] < xs[1]);
});

test("one_point_or_equal_values_give_a_flat_line_and_bad_numbers_never_throw", () => {
  const flat = (history) => {
    const { root } = drawSpark([SPARK], history);
    const line = root.querySelector(".mm-spark polyline");
    return line ? line.getAttribute("points") : null;
  };
  // One point: a line across the whole width, in the middle.
  assert.equal(flat({ lat: series([60, 5]) }), "0,16 288,16");
  // All the same: flat, in the middle.
  assert.deepEqual(new Set(yOf(flat({ lat: series([600, 7], [300, 7], [0, 7]) }))), new Set([16]));
  // Too far apart to subtract (the span is not finite): flat too.
  assert.deepEqual(new Set(yOf(flat({ lat: series([600, -1.7e308], [0, 1.7e308]) }))), new Set([16]));
  // Negative numbers are numbers.
  assert.equal(yOf(flat({ lat: series([600, -5], [0, -10]) })).length, 2);
  // What is not a finite number, or older than a day, or not a point, is left out.
  const mixed = flat({
    lat: [
      [NOW_S - 600, Number.NaN],
      [NOW_S - 500, Infinity],
      "x",
      [NOW_S - 400],
      [NOW_S - 200_000, 3],
      [NOW_S - 300, 4],
      [NOW_S, "9"],
      null,
      [NOW_S - 100, 6],
    ],
  });
  assert.equal(yOf(mixed).length, 2);
  assert.doesNotMatch(mixed, /NaN|Infinity/);
  // Nothing to draw: no block at all, not even its title.
  for (const history of [undefined, null, {}, { lat: [] }, { lat: "junk" }, { lat: series([200_000, 1]) }, { other: series([0, 1]) }, []]) {
    const { root } = drawSpark([SPARK], history);
    assert.equal(root.querySelector(".mm-block-spark"), null, JSON.stringify(history));
  }
  // A metric named like a property of every object is not looked up on the prototype.
  const { root } = drawSpark([{ ...SPARK, metric: "__proto__" }, { ...SPARK, metric: "constructor" }], {});
  assert.equal(root.querySelector(".mm-block-spark"), null);
  // A spark with no metric, or an odd style, does not throw; the odd style draws a line.
  const odd = drawSpark([{ kind: "spark", title_key: "uptime" }, { ...SPARK, style: "pie" }], { lat: series([0, 1]) });
  assert.equal(odd.root.querySelectorAll(".mm-block-spark").length, 1);
  assert.equal(odd.root.querySelector(".mm-spark").getAttribute("data-style"), "line");
});

test("an_up_down_spark_is_a_strip_of_half_hours_bad_where_something_was_down", () => {
  const { root } = drawSpark(
    [UPDOWN],
    // Now: 0; 45 minutes ago: 2 down; 50 minutes and 5 hours ago: 0; a negative value is no reading.
    { down: series([0, 0], [2700, 2], [3000, 0], [18_000, 0], [36_000, -1]) },
  );
  const wrap = root.querySelector(".mm-spark");
  assert.equal(wrap.getAttribute("data-style"), "up_down");
  const cells = [...wrap.querySelectorAll("rect")];
  assert.equal(cells.length, 48);
  assert.ok(cells.every((c) => c.namespaceURI === SVG));
  const states = cells.map((c) => c.getAttribute("data-status"));
  assert.equal(states.filter((s) => s === "bad").length, 1);
  assert.equal(states[46], "bad", "the half hour 30 to 60 minutes ago");
  assert.equal(states[47], "ok");
  assert.equal(states.filter((s) => s === "ok").length, 2);
  assert.equal(states.filter((s) => s === "none").length, 45);
  assert.equal(wrap.getAttribute("aria-label"), "1 half hours with something down");
  // Without a known title the block has no heading, and the picture still says its count.
  const plain = drawSpark([{ ...UPDOWN, title_key: "nope" }], { down: series([0, 0]) });
  assert.equal(plain.root.querySelector(".mm-block-title"), null);
  assert.equal(plain.root.querySelector(".mm-spark").getAttribute("aria-label"), "0 half hours with something down");
});

test("a_normal_card_counts_a_spark_as_its_one_visual", () => {
  const { root } = drawSpark([LIST(4), SPARK, BARS, STRIP], { lat: series([600, 1], [0, 2]) }, boundCard("c1", 1));
  const kinds = [...root.querySelectorAll(".mm-block")].map((b) => b.getAttribute("class"));
  assert.deepEqual(kinds, ["mm-block mm-block-list", "mm-block mm-block-spark"]);
  // Without history the spark is not drawn, and the next visual takes its place.
  const empty = drawSpark([LIST(4), SPARK, BARS], {}, boundCard("c1", 1));
  const after = [...empty.root.querySelectorAll(".mm-block")].map((b) => b.getAttribute("class"));
  assert.deepEqual(after, ["mm-block mm-block-list", "mm-block mm-block-bars"]);
});

test("a_spark_is_changed_in_place_when_a_point_is_added_and_no_focus_moves", () => {
  const first = { lat: series([600, 1], [300, 2]), down: series([600, 0]) };
  const t = liveCard([LIST(2), SPARK, UPDOWN]);
  // The first draw had no history; the next update brings some.
  t.update([LIST(2), SPARK, UPDOWN], { history: first });
  const calls = [];
  for (const el of t.root.querySelectorAll("*")) el.focus = () => calls.push("focus");
  const link = t.root.querySelector("a.mm-li-text");
  const pic = t.root.querySelector(".mm-spark-line");
  const line = pic.querySelector("polyline");
  const before = line.getAttribute("points");
  const cells = [...t.root.querySelectorAll(".mm-spark-updown rect")];
  // A point is appended (the page merged a history_append).
  const next = { lat: [...first.lat, [NOW_S, 9]], down: [...first.down, [NOW_S, 1]] };
  assert.equal(t.update([LIST(2), SPARK, UPDOWN], { history: next }), true);
  assert.ok(t.root.querySelector(".mm-spark-line") === pic, "the same svg");
  assert.ok(pic.querySelector("polyline") === line, "the same polyline");
  assert.notEqual(line.getAttribute("points"), before);
  assert.equal(yOf(line.getAttribute("points")).length, 3);
  const after = [...t.root.querySelectorAll(".mm-spark-updown rect")];
  assert.ok(after.length === 48 && after.every((c, i) => c === cells[i]), "the same cells");
  assert.equal(after[47].getAttribute("data-status"), "bad");
  assert.ok(t.root.querySelector("a.mm-li-text") === link);
  assert.deepEqual(calls, []);
});

test("the_css_draws_the_spark_line_and_cells_in_the_status_colours", () => {
  const css = readFileSync(new URL("../static/magpie/render.css", import.meta.url), "utf8");
  assert.match(css, /\.mm-spark-path \{[^}]*fill: none;[^}]*stroke: var\(--brand\);/);
  assert.match(css, /\.mm-spark-cell\[data-status="ok"\] \{\s*fill: var\(--ok-text\);/);
  assert.match(css, /\.mm-spark-cell\[data-status="bad"\] \{\s*fill: var\(--bad-text\);/);
  const source = readFileSync(new URL("../static/magpie/render.js", import.meta.url), "utf8");
  assert.match(source, /createElementNS\(SVG_NS/);
});

const COUNTER = { kind: "spark", title_key: "uptime", metric: "visits", style: "counter" };
const barHeights = (root) => [...root.querySelectorAll(".mm-spark-counter rect")].map((r) => Number(r.getAttribute("height")));

test("a_counter_spark_draws_what_the_count_grew_by_in_each_hour_and_a_reset_is_a_new_start", () => {
  const { root } = drawSpark(
    [COUNTER],
    {
      // Two hours ago 10, then 20 and 25 in the hour before last; at midnight (an hour ago) it
      // starts again at 3, then 5 now.
      visits: series([7200, 10], [6600, 20], [6000, 25], [3000, 3], [0, 5]),
    },
  );
  const wrap = root.querySelector(".mm-spark");
  assert.equal(wrap.getAttribute("data-style"), "counter");
  const bars = [...root.querySelectorAll(".mm-spark-counter rect")];
  assert.equal(bars.length, 24);
  assert.ok(bars.every((b) => b.namespaceURI === SVG));
  const h = barHeights(root);
  assert.ok(h.every((x) => Number.isFinite(x) && x >= 0), "no negative bar");
  // 10 + 5 in the hour before last; in the last hour the reset counts as 3 (not -22), then 2.
  assert.equal(wrap.getAttribute("aria-label"), "20 in the last 24 hours");
  assert.equal(h[22], 30, "the fullest hour is the full height");
  assert.ok(h[23] > 0 && h[23] < h[22]);
  assert.equal(h.filter((x) => x > 0).length, 2);
  // Every bar sits on the bottom edge.
  for (const b of bars) assert.equal(Number(b.getAttribute("y")) + Number(b.getAttribute("height")), 32);
});

test("a_counter_spark_takes_gaps_and_hostile_values_without_a_negative_bar_or_a_throw", () => {
  const cases = [
    // A gap of hours: the step after it counts in the hour that ends it.
    [series([20 * 3600, 5], [3600, 9]), "4 in the last 24 hours"],
    // Values that fall below 0, a huge jump, numbers too big to subtract.
    [series([3000, 5], [2700, -4], [2400, 2], [2100, 1.7e308], [1800, -1.7e308], [1500, 1.7e308]), null],
    // One point: nothing grew.
    [series([0, 50]), "0 in the last 24 hours"],
  ];
  for (const [visits, label] of cases) {
    const { root } = drawSpark([COUNTER], { visits });
    const h = barHeights(root);
    assert.equal(h.length, 24);
    assert.ok(h.every((x) => Number.isFinite(x) && x >= 0 && x <= 32), JSON.stringify(h));
    for (const r of root.querySelectorAll(".mm-spark-counter rect")) {
      for (const a of ["x", "y", "width", "height"]) assert.doesNotMatch(r.getAttribute(a), /NaN|Infinity/);
    }
    if (label) assert.equal(root.querySelector(".mm-spark").getAttribute("aria-label"), label);
  }
  // A page that does not know a style draws a line; an old page does the same with "counter".
  const odd = drawSpark([{ ...COUNTER, style: "pie" }], { visits: series([300, 1], [0, 2]) });
  assert.equal(odd.root.querySelector(".mm-spark").getAttribute("data-style"), "line");
});

test("a_line_is_broken_where_readings_are_missing", () => {
  // A service read every minute keeps a point per 5 minutes: a gap of more than 10 minutes breaks
  // the line, and a lone reading between two gaps is a short dash of its own.
  const { root } = drawSpark([SPARK], { lat: series([7200, 1], [6900, 2], [3600, 5], [600, 3], [300, 4], [0, 2]) });
  const lines = [...root.querySelectorAll(".mm-spark-line polyline")];
  assert.equal(lines.length, 3);
  assert.deepEqual(lines.map((l) => l.getAttribute("points").split(" ").length), [2, 2, 3]);
  const dash = lines[1].getAttribute("points").split(" ").map((p) => p.split(",").map(Number));
  assert.equal(dash[0][1], dash[1][1]);
  assert.ok(dash[1][0] - dash[0][0] <= 2);
  // A service read every 15 minutes: points 15 minutes apart are not a gap.
  const slow = drawSpark([SPARK], { lat: series([1800, 1], [900, 2], [0, 3]) }, boundCard("c1", 2));
  const t = liveCard([SPARK]);
  t.update([SPARK], { interval_s: 900, history: { lat: series([1800, 1], [900, 2], [0, 3]) } });
  assert.equal(t.root.querySelectorAll(".mm-spark-line polyline").length, 1);
  assert.equal(slow.root.querySelectorAll(".mm-spark-line polyline").length, 3, "at 60 s they are");
  // The label says the latest value and the range of all of them.
  assert.equal(root.querySelector(".mm-spark").getAttribute("aria-label"), "Now 2, from 1 to 5");
});

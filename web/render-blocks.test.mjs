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
  "detail.down_now": "Down now",
  "detail.uptime": "Last 24 hours",
  "detail.disks": "Disks",
  "detail.lib": "Libraries",
  "detail.pct": "Percent",
  "detail.since": "Since",
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

test("the_css_spans_two_columns_for_a_wide_card_on_wide_screens_only", () => {
  const css = readFileSync(new URL("../static/magpie/render.css", import.meta.url), "utf8");
  const media = css.match(/@media \(min-width: 700px\) \{([\s\S]*?)\n\}\n/)[1];
  assert.match(media, /\.mm-card-wide \{\s*grid-column: span 2;/);
  assert.doesNotMatch(css.replace(media, ""), /grid-column: span 2/, "the span is inside the wide-screen query only");
  assert.match(media, /\.mm-card-wide \.mm-metrics \{\s*grid-template-columns: repeat\(5, /);
  assert.match(css, /\.mm-metrics \{[^}]*grid-template-columns: repeat\(2, /, "two columns on a normal card");
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
  assert.equal(card.querySelectorAll(".mm-block").length, 3, "the blocks are still drawn");
  assert.deepEqual(texts(card, ".mm-fact-label"), [""]);
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

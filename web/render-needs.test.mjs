// Tests for the «Trenger deg» row (static/magpie/render.js: the needs of `values.needs`, drawn
// between the top of the page and the groups), run with `node --test` in web/. Fixture needs stand
// in for what the server sends.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { parseHTML } from "linkedom";

import { render, tickAges, toggleAckedNeeds, updateValues } from "../static/magpie/render.js";

function startStrings(path) {
  const strings = {};
  let inStart = false;
  for (const line of readFileSync(new URL(path, import.meta.url), "utf8").split(/\r?\n/)) {
    if (/^\[.*\]\s*$/.test(line)) inStart = line.trim() === "[start]";
    else if (inStart) {
      const m = /^(\w+)\s*=\s*"(.*)"\s*$/.exec(line);
      if (m) strings[m[1]] = JSON.parse(`"${m[2]}"`);
    }
  }
  return strings;
}
const EN = { lang: "en", labels: startStrings("../locales/en.toml") };
const NB = { lang: "nb", labels: startStrings("../locales/nb.toml") };

const NOW = 1_760_000_000; // epoch seconds, the server's clock

const SERVICES = [
  { id: 1, name: "Blog", kind: "http", interval_s: 60, metrics: [{ id: "status", label: "HTTP status", format: "number" }] },
  { id: 2, name: "Wiki", kind: "http", interval_s: 60, metrics: [{ id: "status", label: "HTTP status", format: "number" }] },
];

function need(over = {}) {
  return {
    service_id: 1,
    service_name: "Blog",
    key: "down",
    level: "bad",
    label: "down",
    since: NOW - 120,
    acked: false,
    ...over,
  };
}

function layout(cards) {
  return {
    version: 1,
    background: { kind: "preset", name: "hedgerow" },
    groups: [{ id: "g1", title: "Home", cards }],
    bookmarks: [],
  };
}

const blog = {
  id: "c1",
  title: "Blog",
  description: "",
  url: "https://blog.example/",
  icon: { emoji: null, colour: "teal" },
  service: { id: 1, metrics: ["status"] },
};
const wiki = { ...blog, id: "c2", title: "Wiki", url: "https://wiki.example/", service: { id: 2, metrics: ["status"] } };

function values(needs) {
  return { now: NOW, services: {}, needs };
}

function page() {
  const { document } = parseHTML(
    '<!doctype html><html><body><button id="keep">keep</button><section id="groups"></section></body></html>',
  );
  return { document, root: document.getElementById("groups") };
}

function draw(needs, { cards = [blog, wiki], lang = EN, canAck = true, extra = {} } = {}) {
  const p = page();
  const opts = { ...lang, services: SERVICES, values: values(needs), canAck, ...extra };
  const l = layout(cards);
  render(p.root, l, opts);
  return { ...p, l, opts };
}

const row = (root) => root.querySelector(".mm-needs");
const chips = (root) => [...root.querySelectorAll(".mm-needs-list:not(.mm-needs-acked) .mm-need")];

test("the_row_is_absent_when_there_is_nothing_to_show", () => {
  const { root } = draw([]);
  assert.equal(row(root), null);
  assert.equal(root.querySelector(".mm-need"), null);
  // Without a needs list in the values (an older server, or none given) nothing is drawn either.
  const q = page();
  render(q.root, layout([blog]), { ...EN, services: SERVICES, values: { now: NOW, services: {} } });
  assert.equal(row(q.root), null);
  assert.equal(q.root.querySelector(".mm-needs-live"), null);
  const r = page();
  render(r.root, layout([blog]), EN);
  assert.equal(row(r.root), null);
});

test("the_row_sits_before_the_groups_and_chips_read_service_text_age", () => {
  const { root } = draw([need(), need({ service_id: 2, service_name: "Wiki", key: "disk", level: "warn", label: "disk almost full", since: NOW - 3 * 3600 })]);
  const section = row(root);
  assert.ok(section, "the row is there");
  const first = [...root.children].find((el) => el.classList.contains("mm-needs") || el.classList.contains("mm-group"));
  assert.equal(first, section, "before the first group");
  assert.equal(section.querySelector(".mm-needs-title").textContent, EN.labels.needs);
  const items = chips(root);
  assert.equal(items.length, 2);
  assert.equal(items[0].querySelector(".mm-need-chip").textContent, "Blog · down · 2 min. ago");
  assert.equal(items[1].querySelector(".mm-need-chip").textContent, "Wiki · disk almost full · 3 hr. ago");
  // The same in Norwegian.
  const nb = draw([need({ label: "nede" })], { lang: NB });
  assert.match(chips(nb.root)[0].querySelector(".mm-need-chip").textContent, /^Blog · nede · for 2 min siden$/);
});

test("a_chip_breaks_between_its_parts_never_inside_the_name", () => {
  const { root } = draw([need({ service_name: "KK", label: "needs to be set up again" })]);
  const chip = root.querySelector(".mm-need-chip");
  // The dot, the name and its separator are one part; the text and its separator another; then the age.
  assert.deepEqual([...chip.children].map((el) => el.className), ["mm-need-lead", "mm-need-main", "mm-need-age"]);
  const lead = chip.querySelector(".mm-need-lead");
  assert.deepEqual([...lead.children].map((el) => el.className), ["mm-status", "mm-need-name", "mm-need-sep"]);
  assert.deepEqual([...chip.querySelector(".mm-need-main").children].map((el) => el.className), ["mm-need-text", "mm-need-sep"]);
  assert.equal(chip.textContent, "KK · needs to be set up again · 2 min. ago");
  // Without a time there is nothing to separate the text from.
  const bare = draw([need({ since: undefined })]);
  assert.deepEqual([...bare.root.querySelector(".mm-need-main").children].map((el) => el.className), ["mm-need-text"]);
  // The style that keeps the name whole (a one-letter name must not wrap letter by letter).
  const css = readFileSync(new URL("../static/magpie/render.css", import.meta.url), "utf8");
  const rule = (selector) => {
    const at = css.indexOf(`\n${selector} {`);
    return at === -1 ? "" : css.slice(at, css.indexOf("}", at));
  };
  assert.match(rule(".mm-need-lead"), /white-space:\s*nowrap/);
  assert.match(rule(".mm-need-lead"), /flex:\s*none/);
});

test("the_shape_tells_the_level_as_well_as_the_colour", () => {
  const { root } = draw([need(), need({ service_id: 2, key: "disk", level: "warn", label: "w" })]);
  const [red, yellow] = chips(root);
  assert.equal(red.getAttribute("data-level"), "bad");
  assert.equal(yellow.getAttribute("data-level"), "warn");
  const redDot = red.querySelector(".mm-status");
  const yellowDot = yellow.querySelector(".mm-status");
  assert.equal(redDot.getAttribute("data-status"), "bad");
  assert.equal(yellowDot.getAttribute("data-status"), "warn");
  // The same shapes as the card's dots (render.css), and a name for a screen reader.
  assert.equal(redDot.getAttribute("aria-label"), EN.labels.status_bad);
  assert.equal(yellowDot.getAttribute("aria-label"), EN.labels.status_warn);
});

test("a_chip_opens_the_bound_cards_address_in_the_same_tab", () => {
  const { root } = draw([need(), need({ service_id: 2, service_name: "Wiki", key: "disk", label: "w" })]);
  const [a, b] = chips(root).map((li) => li.querySelector(".mm-need-chip"));
  assert.equal(a.tagName, "A");
  assert.equal(a.getAttribute("href"), "https://blog.example/");
  assert.equal(b.getAttribute("href"), "https://wiki.example/");
  for (const el of [a, b]) {
    assert.equal(el.getAttribute("target"), null, "no new tab");
    assert.equal(el.getAttribute("rel"), "noreferrer");
  }
});

test("a_chip_without_a_usable_card_address_is_text", () => {
  // No card is bound to the service, or its address is not http(s).
  const unbound = draw([need({ service_id: 5, service_name: "Gone", label: "down" })]);
  const el = chips(unbound.root)[0].querySelector(".mm-need-chip");
  assert.equal(el.tagName, "SPAN");
  assert.equal(el.getAttribute("href"), null);
  const evil = { ...blog, url: "javascript:alert(1)" };
  const odd = draw([need()], { cards: [evil] });
  assert.equal(chips(odd.root)[0].querySelector(".mm-need-chip").tagName, "SPAN");
  assert.equal(odd.root.querySelector("[href^='javascript']"), null);
});

test("hostile_names_and_texts_stay_text", () => {
  const hostile = '<img src=x onerror="alert(1)"><script>alert(2)</script>';
  const { root } = draw([need({ service_name: hostile, label: hostile })]);
  const section = row(root);
  assert.equal(section.querySelector("img"), null);
  assert.equal(section.querySelector("script"), null);
  assert.ok(section.textContent.includes(hostile));
  // Right to left text is isolated by the browser.
  for (const el of section.querySelectorAll(".mm-need-name, .mm-need-text")) {
    assert.equal(el.getAttribute("dir"), "auto");
  }
  const labelled = section.querySelector(".mm-need-ack");
  assert.ok(labelled.getAttribute("aria-label").includes(hostile), "an attribute value is data, not markup");
});

test("each_chip_has_a_kvitter_button_after_it_and_the_order_is_chips_then_their_buttons", () => {
  const { root } = draw([need(), need({ service_id: 2, service_name: "Wiki", key: "disk", level: "warn", label: "disk" })]);
  const tab = [...row(root).querySelectorAll("a, button")].map((el) => [el.tagName, el.className.split(" ")[0], el.getAttribute("data-key") ?? el.closest(".mm-need")?.getAttribute("data-key")]);
  assert.deepEqual(tab, [
    ["A", "mm-need-chip", "down"],
    ["BUTTON", "mm-need-ack", "down"],
    ["A", "mm-need-chip", "disk"],
    ["BUTTON", "mm-need-ack", "disk"],
  ]);
  const button = row(root).querySelector(".mm-need-ack");
  assert.equal(button.getAttribute("type"), "button");
  assert.equal(button.textContent, EN.labels.need_ack);
  assert.equal(button.getAttribute("aria-label"), "Acknowledge Blog: down");
  assert.ok(button.getAttribute("aria-label").startsWith(button.textContent), "the name holds the visible text");
  const nb = draw([need()], { lang: NB });
  assert.equal(nb.root.querySelector(".mm-need-ack").textContent, "Kvitter");
});

test("without_canAck_there_is_no_kvitter_button", () => {
  const { root } = draw([need(), need({ service_id: 2, key: "disk", label: "w", acked: true })], { canAck: false });
  assert.equal(root.querySelector("button.mm-need-ack"), null);
  assert.equal(root.querySelector("button.mm-need-unack"), null);
  // The chips and the toggle stay.
  assert.equal(chips(root).length, 1);
  assert.ok(root.querySelector(".mm-needs-toggle"));
  const q = page();
  render(q.root, layout([blog]), { ...EN, services: SERVICES, values: values([need()]) });
  assert.equal(q.root.querySelector(".mm-need-ack"), null, "off unless asked for");
});

test("acknowledged_needs_are_hidden_behind_a_toggle_with_a_count", () => {
  const { root } = draw([need(), need({ service_id: 2, service_name: "Wiki", key: "disk", level: "warn", label: "disk", acked: true })]);
  assert.equal(chips(root).length, 1, "only the one that still needs you");
  const toggle = row(root).querySelector(".mm-needs-toggle");
  assert.equal(toggle.tagName, "BUTTON");
  assert.equal(toggle.getAttribute("type"), "button");
  assert.equal(toggle.textContent, "1 acknowledged");
  assert.equal(toggle.getAttribute("title"), EN.labels.needs_show_acked);
  assert.equal(toggle.getAttribute("aria-expanded"), "false");
  const hidden = row(root).querySelector(".mm-needs-acked");
  assert.ok(hidden.hasAttribute("hidden"));
  assert.equal(hidden.querySelectorAll(".mm-need").length, 1);
  assert.equal(hidden.querySelector(".mm-need-chip").textContent.startsWith("Wiki · disk"), true);
  // The toggle shows and hides them.
  toggleAckedNeeds(toggle);
  assert.equal(toggle.getAttribute("aria-expanded"), "true");
  assert.equal(hidden.hasAttribute("hidden"), false);
  toggleAckedNeeds(toggle);
  assert.equal(toggle.getAttribute("aria-expanded"), "false");
  assert.ok(hidden.hasAttribute("hidden"));
  // In Norwegian.
  const nb = draw([need({ acked: true })], { lang: NB });
  assert.equal(nb.root.querySelector(".mm-needs-toggle").textContent, "1 kvittert");
  assert.equal(nb.root.querySelector(".mm-needs-toggle").getAttribute("title"), "Vis kvitterte");
});

test("an_acknowledged_need_has_an_undo_button_not_a_kvitter_button", () => {
  const { root } = draw([need({ acked: true })]);
  const item = root.querySelector(".mm-needs-acked .mm-need");
  assert.equal(item.querySelector(".mm-need-ack"), null);
  const undo = item.querySelector(".mm-need-unack");
  assert.equal(undo.textContent, EN.labels.need_unack);
  assert.equal(undo.getAttribute("type"), "button");
  assert.equal(item.getAttribute("data-service"), "1");
  assert.equal(item.getAttribute("data-key"), "down");
});

test("when_every_need_is_acknowledged_only_the_toggle_is_left", () => {
  const { root } = draw([need({ acked: true }), need({ service_id: 2, key: "x", label: "y", acked: true })]);
  assert.ok(row(root), "the row stays so the toggle can be reached");
  assert.equal(chips(root).length, 0);
  assert.equal(row(root).querySelector(".mm-needs-toggle").textContent, "2 acknowledged");
});

test("bad_entries_are_not_drawn", () => {
  const { root } = draw([null, "x", need({ level: "weird" }), need({ service_id: "1" }), need({ label: 5 }), need({ key: "ok" })]);
  assert.equal(chips(root).length, 1);
});

test("the_live_region_starts_empty_and_a_new_need_is_announced_without_moving_focus", () => {
  const p = page();
  const focused = [];
  const keep = p.document.getElementById("keep");
  const opts = { ...EN, services: SERVICES, canAck: true };
  const l = layout([blog, wiki]);
  render(p.root, l, { ...opts, values: values([]) });
  const live = p.root.querySelector(".mm-needs-live");
  assert.ok(live, "the region is there before anything appears");
  assert.equal(live.getAttribute("aria-live"), "polite");
  assert.equal(live.textContent, "");
  assert.equal(row(p.root), null);
  for (const el of [keep, p.root, live]) el.focus = () => focused.push(el.id || el.className);

  // A need appears.
  assert.equal(updateValues(p.root, l, { ...opts, values: values([need()]) }), true);
  assert.ok(row(p.root));
  assert.equal(p.root.querySelector(".mm-needs-live"), live, "the same region");
  assert.equal(live.textContent, "Blog: down");
  // Nothing new in the next update: the same row stays, and the text is not touched again.
  const section = row(p.root);
  updateValues(p.root, l, { ...opts, values: values([need()]) });
  assert.equal(row(p.root), section, "same element when nothing changed");
  assert.equal(live.textContent, "Blog: down");
  // Another change (an acknowledgement) announces nothing new.
  updateValues(p.root, l, { ...opts, values: values([need({ acked: true })]) });
  assert.equal(live.textContent, "");
  assert.equal(chips(p.root).length, 0);
  // It goes: the row goes.
  updateValues(p.root, l, { ...opts, values: values([]) });
  assert.equal(row(p.root), null);
  assert.deepEqual(focused, [], "no focus call");
  // Drawn on load: nothing is announced.
  const q = page();
  render(q.root, l, { ...opts, values: values([need()]) });
  assert.equal(q.root.querySelector(".mm-needs-live").textContent, "");
});

/** Makes `target` the document's active element and records the focus calls. */
function trackFocus(p, target) {
  const calls = [];
  Object.defineProperty(p.document, "activeElement", { get: () => target, configurable: true });
  const proto = p.document.defaultView.Element.prototype;
  const original = proto.focus;
  proto.focus = function () {
    calls.push(this.closest?.(".mm-need")?.getAttribute("data-key") ?? this.className);
  };
  return {
    calls,
    restore: () => {
      // Not an own property left behind that hides the element's own method.
      if (original === undefined) delete proto.focus;
      else proto.focus = original;
    },
  };
}

test("a_live_update_keeps_the_focus_on_a_chip_that_stays", () => {
  const p = page();
  const opts = { ...EN, services: SERVICES, canAck: true };
  const l = layout([blog, wiki]);
  render(p.root, l, { ...opts, values: values([need(), need({ service_id: 2, service_name: "Wiki", key: "disk", label: "disk" })]) });
  // The reader stands on the second Kvitter button; a new need arrives before it.
  const t = trackFocus(p, chips(p.root)[1].querySelector(".mm-need-ack"));
  try {
    updateValues(p.root, l, {
      ...opts,
      values: values([need(), need({ service_id: 2, service_name: "Wiki", key: "disk", label: "disk" }), need({ key: "cert", level: "warn", label: "cert" })]),
    });
  } finally {
    t.restore();
  }
  // It is given back to the same button of the rebuilt row, and to nothing else.
  assert.deepEqual(t.calls, ["disk"]);
});

test("update_values_replaces_the_row_in_place_and_leaves_the_cards", () => {
  const { root, l, opts } = draw([need()]);
  const card = root.querySelector(".mm-card");
  const group = root.querySelector(".mm-group");
  updateValues(root, l, { ...opts, values: values([need({ level: "warn", label: "Blog is slow" })]) });
  assert.equal(root.querySelector(".mm-card"), card);
  assert.equal(root.querySelector(".mm-group"), group);
  assert.equal(chips(root)[0].getAttribute("data-level"), "warn");
  assert.match(chips(root)[0].textContent, /Blog is slow/);
});

test("the_open_toggle_stays_open_through_an_update", () => {
  const { root, l, opts } = draw([need({ acked: true })]);
  toggleAckedNeeds(root.querySelector(".mm-needs-toggle"));
  updateValues(root, l, { ...opts, values: values([need({ acked: true }), need({ service_id: 2, key: "b", label: "b", acked: true })]) });
  assert.equal(root.querySelector(".mm-needs-toggle").getAttribute("aria-expanded"), "true");
  assert.equal(root.querySelector(".mm-needs-acked").hasAttribute("hidden"), false);
  assert.equal(root.querySelector(".mm-needs-toggle").textContent, "2 acknowledged");
});

test("ages_on_the_chips_are_ticked_without_drawing_again", () => {
  const { root, opts } = draw([need({ since: NOW - 120 })]);
  const age = root.querySelector(".mm-need-age");
  assert.equal(age.textContent, "2 min. ago");
  tickAges(root, { now: (NOW + 3600) * 1000, lang: "en", labels: opts.labels });
  assert.equal(root.querySelector(".mm-need-age"), age);
  assert.equal(age.textContent, "1 hr. ago");
});

test("render_after_render_does_not_double_the_row", () => {
  const p = page();
  const l = layout([blog]);
  const opts = { ...EN, services: SERVICES, values: values([need()]), canAck: true };
  render(p.root, l, opts);
  render(p.root, l, opts);
  assert.equal(p.root.querySelectorAll(".mm-needs").length, 1);
  assert.equal(p.root.querySelectorAll(".mm-needs-live").length, 1);
});

test("render_css_styles_the_row_and_hides_the_live_region_visually", () => {
  const css = readFileSync(new URL("../static/magpie/render.css", import.meta.url), "utf8");
  for (const selector of [".mm-needs", ".mm-needs-list", ".mm-need-chip", ".mm-need-ack", ".mm-needs-toggle", ".mm-vh"]) {
    assert.ok(css.includes(selector), `${selector} in render.css`);
  }
  assert.ok(!/style\s*=/.test(readFileSync(new URL("../static/magpie/render.js", import.meta.url), "utf8").replace(/\/\/.*$/gm, "").replace(/setAttribute\("style"/g, "")), "no style attributes");
});

test("the_row_is_named_by_its_heading_and_the_toggle_controls_the_acknowledged_list", () => {
  const { root } = draw([need(), need({ service_id: 2, key: "b", label: "b", acked: true })]);
  const section = row(root);
  const heading = section.querySelector(".mm-needs-title");
  assert.ok(heading.getAttribute("id"), "the heading has an id");
  assert.equal(section.getAttribute("aria-labelledby"), heading.getAttribute("id"));
  assert.equal(section.getAttribute("aria-label"), null, "not named twice");
  const toggle = section.querySelector(".mm-needs-toggle");
  const list = section.querySelector(".mm-needs-acked");
  assert.ok(list.getAttribute("id"));
  assert.equal(toggle.getAttribute("aria-controls"), list.getAttribute("id"));
  assert.equal(root.ownerDocument.querySelectorAll(`[id="${heading.getAttribute("id")}"]`).length, 1);
});

test("the_buttons_name_the_service_as_well_as_the_text", () => {
  const { root } = draw([need({ service_name: "Wiki", service_id: 2 }), need({ key: "x", label: "slow", acked: true })]);
  assert.equal(root.querySelector(".mm-need-ack").getAttribute("aria-label"), "Acknowledge Wiki: down");
  assert.equal(root.querySelector(".mm-need-unack").getAttribute("aria-label"), "Undo the acknowledgement of Blog: slow");
  const nb = draw([need()], { lang: NB });
  assert.equal(nb.root.querySelector(".mm-need-ack").getAttribute("aria-label"), "Kvitter Blog: down");
  assert.equal(nb.root.querySelector(".mm-need-ack").textContent, "Kvitter");
});

test("a_focused_chip_that_goes_gives_its_focus_to_the_next_chip_or_the_toggle_never_the_body", () => {
  const two = [need(), need({ service_id: 2, service_name: "Wiki", key: "disk", label: "disk" })];
  const opts = { ...EN, services: SERVICES, canAck: true };
  const l = layout([blog, wiki]);

  // The second need goes: the focus (on its Kvitter button) goes to the chip before it.
  let p = page();
  render(p.root, l, { ...opts, values: values(two) });
  let t = trackFocus(p, chips(p.root)[1].querySelector(".mm-need-ack"));
  try {
    updateValues(p.root, l, { ...opts, values: values([two[0]]) });
  } finally {
    t.restore();
  }
  assert.deepEqual(t.calls, ["down"]);

  // The first goes: the next one takes its place.
  p = page();
  render(p.root, l, { ...opts, values: values(two) });
  t = trackFocus(p, chips(p.root)[0].querySelector(".mm-need-chip"));
  try {
    updateValues(p.root, l, { ...opts, values: values([two[1]]) });
  } finally {
    t.restore();
  }
  assert.deepEqual(t.calls, ["disk"]);

  // The only one is acknowledged elsewhere: the toggle.
  p = page();
  render(p.root, l, { ...opts, values: values([two[0]]) });
  t = trackFocus(p, chips(p.root)[0].querySelector(".mm-need-ack"));
  try {
    updateValues(p.root, l, { ...opts, values: values([{ ...two[0], acked: true }]) });
  } finally {
    t.restore();
  }
  assert.deepEqual(t.calls, ["mm-needs-toggle"]);

  // The row goes altogether: the first link of the page, not the body.
  p = page();
  render(p.root, l, { ...opts, values: values([two[0]]) });
  t = trackFocus(p, chips(p.root)[0].querySelector(".mm-need-chip"));
  try {
    updateValues(p.root, l, { ...opts, values: values([]) });
  } finally {
    t.restore();
  }
  assert.equal(t.calls.length, 1, "one call, to a link of the page");
});

test("focus_outside_the_row_is_never_touched_by_an_update", () => {
  const p = page();
  const opts = { ...EN, services: SERVICES, canAck: true };
  const l = layout([blog, wiki]);
  render(p.root, l, { ...opts, values: values([need()]) });
  const t = trackFocus(p, p.document.getElementById("keep"));
  try {
    updateValues(p.root, l, { ...opts, values: values([]) });
    updateValues(p.root, l, { ...opts, values: values([need()]) });
  } finally {
    t.restore();
  }
  assert.deepEqual(t.calls, []);
});

// Tests for the live numbers on the start page (static/magpie/start.js: initLive,
// mergeValues and the wiring). linkedom stands in for the DOM; the EventSource,
// the timers and the clock are fakes the tests drive by hand.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { parseHTML } from "linkedom";

import { startEditing } from "../static/magpie/edit.js";
import { LIVE_URL, RETRY_MS, TICK_MS, draw, initLive, mergeServices, mergeValues, wirePage } from "../static/magpie/start.js";

function readStartStrings(path) {
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
const LABELS = readStartStrings("../locales/en.toml");

const NOW = 1_760_000_000; // the server's clock, epoch seconds

const plainCard = { id: "c1", title: "Sonarr", description: "", url: "https://sonarr.example/", icon: { emoji: null, colour: "teal" } };
const boundCard = {
  id: "c2",
  title: "Blog",
  description: "",
  url: "https://blog.example/",
  icon: { emoji: null, colour: "orange" },
  service: { id: 1, metrics: ["status", "latency_ms"] },
};

function layoutFixture(cards = [plainCard, boundCard]) {
  return {
    version: 3,
    background: { kind: "preset", name: "hedgerow" },
    groups: [{ id: "g1", title: "Media", cards }],
    bookmarks: [],
  };
}

const SERVICES = [
  {
    id: 1,
    name: "Blog",
    kind: "http",
    interval_s: 60,
    metrics: [
      { id: "status", label: "HTTP status", format: "number" },
      { id: "latency_ms", label: "Response time (ms)", format: "number" },
    ],
  },
];

function entry(latency, { fetchedAt = NOW - 30, status = "ok", name = "Blog" } = {}) {
  return {
    name,
    kind: "http",
    interval_s: 60,
    status,
    values: { status: { num: 200, text: null }, latency_ms: { num: latency, text: null } },
    needs: [],
    fetched_at: fetchedAt,
  };
}

function valuesFixture(latency = 84, extra = {}) {
  return { now: NOW, services: { 1: entry(latency, extra) } };
}


/** A fake EventSource class that remembers every instance. */
function fakeEventSource() {
  const all = [];
  class FakeEventSource {
    constructor(url) {
      this.url = url;
      this.readyState = 1;
      this.closed = false;
      this.listeners = {};
      all.push(this);
    }
    addEventListener(type, fn) {
      (this.listeners[type] ??= []).push(fn);
    }
    close() {
      this.closed = true;
      this.readyState = 2;
    }
    emit(type, data) {
      const text = typeof data === "string" ? data : JSON.stringify(data);
      for (const fn of this.listeners[type] ?? []) fn({ data: text });
    }
  }
  FakeEventSource.all = all;
  return FakeEventSource;
}

function fakeTimers() {
  const intervals = new Map();
  const timeouts = new Map();
  let n = 0;
  return {
    intervals,
    timeouts,
    setInterval: (fn, ms) => (intervals.set(++n, { fn, ms }), n),
    clearInterval: (id) => intervals.delete(id),
    setTimeout: (fn, ms) => (timeouts.set(++n, { fn, ms }), n),
    clearTimeout: (id) => timeouts.delete(id),
    fireInterval() {
      for (const { fn } of [...intervals.values()]) fn();
    },
    fireTimeouts() {
      const due = [...timeouts.entries()];
      timeouts.clear();
      for (const [, { fn }] of due) fn();
    },
  };
}

function page({ layout = layoutFixture(), values = valuesFixture(), services = SERVICES, visible = true, lang = "en", blocks = true } = {}) {
  const { document, window } = parseHTML(
    `<!doctype html><html lang="${lang}"><body><main>
       <div class="start-actions"><button type="button" id="mm-edit" aria-pressed="false">Edit</button></div>
       <section id="groups" class="mm-root" data-user="7"></section>
       <script type="application/json" id="mm-layout">${JSON.stringify(layout)}</script>
       <script type="application/json" id="mm-labels">${JSON.stringify(LABELS)}</script>
       ${blocks ? `<script type="application/json" id="mm-services">${JSON.stringify(services)}</script>` : ""}
       ${blocks ? `<script type="application/json" id="mm-values">${JSON.stringify(values)}</script>` : ""}
     </main></body></html>`,
  );
  const state = { visibility: visible ? "visible" : "hidden" };
  Object.defineProperty(document, "visibilityState", { get: () => state.visibility, configurable: true });
  const focus = { count: 0, active: null };
  window.Element.prototype.focus = function () {
    focus.count++;
    focus.active = this;
  };
  // linkedom does not track focus: a getter that follows the stubbed focus().
  Object.defineProperty(document, "activeElement", { get: () => focus.active ?? document.body, configurable: true });
  // What the probe after a refused stream gets back.
  const answer = { type: "basic", status: 503 };
  const ES = fakeEventSource();
  const timers = fakeTimers();
  const clock = { ms: NOW * 1000 };
  const deps = {
    EventSource: ES,
    now: () => clock.ms,
    setInterval: timers.setInterval,
    clearInterval: timers.clearInterval,
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout,
    fetch: async () => ({ ...answer }),
  };
  const root = document.getElementById("groups");
  const setVisibility = (v) => {
    state.visibility = v;
    document.dispatchEvent(new window.Event("visibilitychange", { bubbles: true }));
  };
  return { document, window, root, ES, timers, clock, deps, focus, setVisibility, answer };
}

const block = (p, id) => JSON.parse(p.document.getElementById(id).textContent);
const cardEl = (p) => p.root.querySelector('.mm-card[data-service="1"]');
const valueTexts = (p) => [...cardEl(p).querySelectorAll(".mm-metric-value")].map((el) => el.textContent);
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
/** htmx's event before a swap, with shouldSwap as htmx sets it. */
const beforeSwap = (p, shouldSwap = true) =>
  p.document.dispatchEvent(new p.window.CustomEvent("htmx:beforeSwap", { bubbles: true, detail: { shouldSwap } }));

// ---- opening and closing ----------------------------------------------------

test("the_stream_is_open_only_while_the_page_is_visible", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  assert.equal(p.ES.all.length, 1);
  assert.equal(p.ES.all[0].url, LIVE_URL);
  assert.equal(p.timers.intervals.size, 1);
  assert.equal([...p.timers.intervals.values()][0].ms, TICK_MS);

  p.setVisibility("hidden");
  assert.equal(p.ES.all[0].closed, true, "closed when hidden");
  assert.equal(p.timers.intervals.size, 0, "the timer goes with it");
  assert.equal(live.isOpen(), false);

  p.setVisibility("visible");
  assert.equal(p.ES.all.length, 2, "opened again");
  assert.equal(p.ES.all[1].closed, false);
  assert.equal(p.timers.intervals.size, 1);

  // Sync again changes nothing: one stream at a time.
  live.sync();
  live.sync();
  assert.equal(p.ES.all.length, 2);
});

test("a_page_that_starts_hidden_opens_nothing_until_it_is_shown", () => {
  const p = page({ visible: false });
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  assert.equal(p.ES.all.length, 0);
  p.setVisibility("visible");
  assert.equal(p.ES.all.length, 1);
});

test("no_stream_without_bound_cards_values_or_an_event_source", () => {
  let p = page({ layout: layoutFixture([plainCard]) });
  draw(p.document);
  initLive(p.document, p.deps).sync();
  assert.equal(p.ES.all.length, 0, "no card shows a service");

  p = page({ blocks: false });
  draw(p.document);
  initLive(p.document, p.deps).sync();
  assert.equal(p.ES.all.length, 0, "an older page without #mm-values");

  p = page();
  draw(p.document);
  initLive(p.document, { ...p.deps, EventSource: undefined }).sync();
  assert.equal(p.timers.intervals.size, 0, "a browser without EventSource");
});

test("the_stream_is_closed_when_the_body_is_swapped_and_when_the_root_is_gone", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  assert.equal(p.ES.all[0].closed, false);

  // htmx is about to swap the body (a boosted visit).
  beforeSwap(p);
  assert.equal(p.ES.all[0].closed, true);
  assert.equal(p.timers.intervals.size, 0);

  // The page is still there after a swap that was only a fragment: it opens again.
  live.sync();
  assert.equal(p.ES.all.length, 2);

  // The body is replaced without any event we hear: the next tick notices.
  p.document.body.innerHTML = "<main><h1>Settings</h1></main>";
  p.timers.fireInterval();
  assert.equal(p.ES.all[1].closed, true);
  assert.equal(p.timers.intervals.size, 0);
});

test("a_refused_stream_is_asked_again_later_and_a_reconnecting_one_is_left_alone", async () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  const first = p.ES.all[0];

  // The browser is reconnecting by itself (readyState 0): nothing to do.
  first.readyState = 0;
  first.emit("error", "");
  assert.equal(p.ES.all.length, 1);
  assert.equal(p.timers.timeouts.size, 0);

  // A refusal is final for an EventSource (readyState 2).
  first.readyState = 2;
  first.emit("error", "");
  await tick();
  assert.equal(live.isOpen(), false);
  assert.equal(p.ES.all.length, 1, "not at once");
  assert.equal([...p.timers.timeouts.values()][0].ms, RETRY_MS);
  p.timers.fireTimeouts();
  assert.equal(p.ES.all.length, 2, "asked again");
});

test("events_from_a_stream_that_was_closed_are_ignored", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  const old = p.ES.all[0];
  p.setVisibility("hidden");
  old.emit("values", { now: NOW + 5, full: true, services: { 1: entry(999) }, removed: [] });
  assert.deepEqual(block(p, "mm-values").services[1].values.latency_ms.num, 84);
});

test("after_a_401_the_stream_is_not_asked_for_again_until_the_page_is_shown_again", async () => {
  for (const answer of [
    { type: "basic", status: 401 },
    { type: "opaqueredirect", status: 0 },
  ]) {
    const p = page();
    draw(p.document);
    const live = initLive(p.document, p.deps);
    live.sync();
    Object.assign(p.answer, answer);
    p.ES.all[0].readyState = 2;
    p.ES.all[0].emit("error", "");
    await tick();
    assert.equal(p.timers.timeouts.size, 0, "no retry is planned");
    live.sync();
    live.tick();
    assert.equal(p.ES.all.length, 1, "nothing is opened by a sync either");

    // The next visibility change tries again.
    p.setVisibility("hidden");
    p.setVisibility("visible");
    assert.equal(p.ES.all.length, 2);
  }
});

test("after_a_401_a_swapped_body_tries_again", async () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  Object.assign(p.answer, { type: "basic", status: 401 });
  p.ES.all[0].readyState = 2;
  p.ES.all[0].emit("error", "");
  await tick();
  beforeSwap(p);
  live.sync();
  assert.equal(p.ES.all.length, 2);
});

test("a_network_failure_of_the_probe_is_retried_like_a_refusal", async () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, {
    ...p.deps,
    fetch: async () => {
      throw new Error("offline");
    },
  });
  live.sync();
  p.ES.all[0].readyState = 2;
  p.ES.all[0].emit("error", "");
  await tick();
  assert.equal([...p.timers.timeouts.values()][0].ms, RETRY_MS);
});

test("a_swap_htmx_does_not_make_leaves_the_stream_and_the_timer_alone", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  beforeSwap(p, false);
  assert.equal(p.ES.all[0].closed, false);
  assert.equal(p.timers.intervals.size, 1);
  p.document.dispatchEvent(new p.window.Event("htmx:beforeSwap", { bubbles: true }));
  assert.equal(p.ES.all[0].closed, false, "an event without detail is not a swap either");
  beforeSwap(p, true);
  assert.equal(p.ES.all[0].closed, true);
});

// ---- updates ----------------------------------------------------------------

test("an_event_is_merged_and_the_cards_change_in_place_without_moving_focus", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  const link = cardEl(p);
  assert.deepEqual(valueTexts(p), ["200", "84"]);
  // The person is standing on the card's link.
  link.focus();
  assert.ok(p.document.activeElement === link);
  const focusBefore = p.focus.count;

  p.ES.all[0].emit("values", { now: NOW + 5, full: false, services: { 1: entry(450, { status: "warn", fetchedAt: NOW + 2 }) }, removed: [] });

  assert.deepEqual(valueTexts(p), ["200", "450"]);
  // (Not assert.equal: a failing comparison of DOM nodes tries to print them.)
  assert.ok(cardEl(p) === link, "the card's own element is the same one");
  assert.ok(link.isConnected && link.querySelector(".mm-card-title, .mm-card-text, span") !== null);
  assert.equal(p.root.querySelector(".mm-status").getAttribute("data-status"), "warn");
  assert.equal(p.focus.count, focusBefore, "focus() was never called");
  assert.ok(p.document.activeElement === link, "focus is still on the link");
  assert.ok(link.isConnected, "and the link is still in the page");
  assert.equal(block(p, "mm-values").now, NOW + 5);
  assert.equal(block(p, "mm-values").services[1].values.latency_ms.num, 450);
  // The plain card was not touched.
  assert.ok(p.root.textContent.includes("Sonarr"));
});

test("a_partial_event_keeps_the_entries_it_does_not_mention", () => {
  const two = { now: NOW, services: { 1: entry(10), 2: entry(20, { name: "Wiki" }) } };
  const layout = layoutFixture([plainCard, boundCard, { ...boundCard, id: "c3", service: { id: 2, metrics: ["status"] } }]);
  const services = [...SERVICES, { ...SERVICES[0], id: 2, name: "Wiki" }];
  const p = page({ layout, values: two, services });
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  p.ES.all[0].emit("values", { now: NOW + 1, full: false, services: { 2: entry(25, { name: "Wiki" }) }, removed: [] });
  const values = block(p, "mm-values");
  assert.equal(values.services[1].values.latency_ms.num, 10);
  assert.equal(values.services[2].values.latency_ms.num, 25);
});

test("a_removed_service_is_dropped_from_the_values_and_the_services_and_its_card_says_so", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  p.ES.all[0].emit("values", { now: NOW + 5, full: false, services: {}, removed: [1] });
  assert.deepEqual(block(p, "mm-values").services, {});
  assert.deepEqual(block(p, "mm-services"), []);
  assert.ok(p.root.querySelector(".mm-svc-notice")?.textContent.includes(LABELS.service_missing));
  assert.equal(p.root.querySelector(".mm-metric"), null);
});

const VIEW = { ...SERVICES[0] };

test("a_service_that_comes_back_is_drawn_with_numbers_not_the_missing_notice", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  p.ES.all[0].emit("values", { now: NOW + 5, full: false, services: {}, removed: [1], services_add: [] });
  assert.ok(p.root.querySelector(".mm-svc-notice")?.textContent.includes(LABELS.service_missing));
  assert.deepEqual(block(p, "mm-services"), []);

  // It is visible again: the event carries its view and its entry.
  p.ES.all[0].emit("values", { now: NOW + 9, full: false, services: { 1: entry(31) }, removed: [], services_add: [VIEW] });
  assert.deepEqual(
    block(p, "mm-services").map((s) => s.id),
    [1],
  );
  assert.deepEqual(valueTexts(p), ["200", "31"]);
  assert.equal(p.root.querySelector(".mm-svc-notice"), null, "no notice");
});

test("a_service_that_was_hidden_at_load_arrives_with_its_view", () => {
  // The page was made without the service: no entry, no view.
  const p = page({ values: { now: NOW, services: {} }, services: [] });
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  assert.ok(p.root.querySelector(".mm-svc-notice")?.textContent.includes(LABELS.service_missing));
  p.ES.all[0].emit("values", { now: NOW + 3, full: false, services: { 1: entry(12) }, removed: [], services_add: [VIEW] });
  assert.deepEqual(valueTexts(p), ["200", "12"]);
  // The edit form's list sees it too.
  assert.deepEqual(
    block(p, "mm-services").map((s) => s.name),
    ["Blog"],
  );
});

test("mergeServices_adds_replaces_removes_and_keeps_the_order_by_name", () => {
  const list = [
    { id: 1, name: "Blog" },
    { id: 3, name: "Zed" },
  ];
  const values = { services: {} };
  const merged = mergeServices(list, values, [3], [{ id: 2, name: "alpha" }, { id: 1, name: "Blog 2" }, null, {}]);
  assert.deepEqual(
    merged.map((s) => [s.id, s.name]),
    [
      [2, "alpha"],
      [1, "Blog 2"],
    ],
  );
  assert.equal(list.length, 2, "the list is not changed");
  assert.deepEqual(
    mergeServices(list, values, [], undefined).map((s) => s.id),
    [1, 3],
  );
});

test("a_full_event_replaces_what_the_page_had", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  p.ES.all[0].emit("values", { now: NOW + 9, full: true, services: { 1: entry(7) }, removed: [] });
  assert.equal(block(p, "mm-values").services[1].values.latency_ms.num, 7);
  // A full event without the service: it is gone.
  p.ES.all[0].emit("values", { now: NOW + 10, full: true, services: {}, removed: [] });
  assert.deepEqual(block(p, "mm-values").services, {});
  assert.deepEqual(block(p, "mm-services"), []);
});

test("a_renamed_service_shows_its_new_name_in_the_services_list", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  p.ES.all[0].emit("values", { now: NOW + 5, full: false, services: { 1: entry(5, { name: "Blog 2" }) }, removed: [] });
  assert.equal(block(p, "mm-services")[0].name, "Blog 2");
});

test("when_the_page_and_the_layout_differ_in_more_than_numbers_it_is_drawn_again", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  // A card was bound since the page was drawn (the layout block moved on).
  const layout = layoutFixture([plainCard, boundCard, { ...plainCard, id: "c9", title: "Newcomer", service: { id: 1, metrics: ["status"] } }]);
  p.document.getElementById("mm-layout").textContent = JSON.stringify(layout);
  p.ES.all[0].emit("values", { now: NOW + 5, full: false, services: { 1: entry(11) }, removed: [] });
  assert.ok(p.root.textContent.includes("Newcomer"));
  assert.equal(p.root.querySelectorAll(".mm-bound").length, 2);
});

test("nothing_is_drawn_while_edit_mode_is_open_and_the_stream_waits_for_its_end", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  const before = p.root.innerHTML;

  p.root.setAttribute("data-editing", "");
  live.sync();
  assert.equal(p.ES.all[0].closed, true, "closed while editing");
  assert.equal(p.timers.intervals.size, 0);

  // An event that was already on its way: kept, not drawn.
  live.apply({ now: NOW + 5, full: false, services: { 1: entry(321) }, removed: [] });
  assert.equal(p.root.innerHTML, before);
  assert.equal(block(p, "mm-values").services[1].values.latency_ms.num, 321);

  p.root.removeAttribute("data-editing");
  live.sync();
  assert.equal(p.ES.all.length, 2, "opens again when editing ends");
});

test("garbage_on_the_stream_is_ignored", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  const before = p.document.getElementById("mm-values").textContent;
  p.ES.all[0].emit("values", "not json");
  p.ES.all[0].emit("values", "42");
  assert.equal(p.document.getElementById("mm-values").textContent, before);
  assert.equal(valueTexts(p)[1], "84");
});

test("an_event_never_ends_the_values_block_early", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  p.ES.all[0].emit("values", { now: NOW + 1, full: false, services: { 1: entry(1, { name: "</script><b>x" }) }, removed: [] });
  assert.ok(!p.document.getElementById("mm-values").textContent.includes("<"));
  assert.equal(block(p, "mm-values").services[1].name, "</script><b>x");
});

// ---- ages and the server's clock --------------------------------------------

test("the_age_timer_counts_on_the_servers_clock_not_the_browsers", () => {
  const p = page({ values: valuesFixture(84, { fetchedAt: NOW - 600 }) });
  // The browser's clock is an hour behind the server's.
  p.clock.ms = NOW * 1000 - 3_600_000;
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  const age = () => cardEl(p).querySelector(".mm-age").textContent;
  assert.match(age(), /10/, age());
  assert.ok(cardEl(p).hasAttribute("data-stale"), "ten minutes is stale at a 60 s interval");

  // 30 minutes pass on the browser's clock: 40 minutes since the reading.
  p.clock.ms += 30 * 60 * 1000;
  p.timers.fireInterval();
  assert.match(age(), /40/, age());

  // A new event brings a new offset (the browser's clock was set right meanwhile).
  p.clock.ms = (NOW + 1800) * 1000;
  p.ES.all[0].emit("values", { now: NOW + 1800, full: false, services: { 1: entry(5, { fetchedAt: NOW + 1790 }) }, removed: [] });
  p.clock.ms += 60 * 1000;
  p.timers.fireInterval();
  assert.doesNotMatch(age(), /40/);
  assert.ok(!cardEl(p).hasAttribute("data-stale"), "a fresh reading is not stale");
});

test("showing_the_page_again_brings_the_ages_up_to_date_at_once", () => {
  const p = page({ values: valuesFixture(84, { fetchedAt: NOW - 30 }) });
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  p.setVisibility("hidden");
  p.clock.ms += 3 * 60 * 60 * 1000;
  p.setVisibility("visible");
  assert.match(cardEl(p).querySelector(".mm-age").textContent, /3/);
  assert.ok(cardEl(p).hasAttribute("data-stale"));
});

// ---- the wiring around it ---------------------------------------------------

test("mergeValues_does_not_change_its_arguments_and_reports_what_went", () => {
  const current = { now: 1, services: { 1: { name: "a" }, 2: { name: "b" } } };
  const frozen = JSON.stringify(current);
  const update = { now: 2, full: false, services: { 3: { name: "c" } }, removed: [2] };
  const merged = mergeValues(current, update);
  assert.deepEqual(merged.values, { now: 2, services: { 1: { name: "a" }, 3: { name: "c" } } });
  assert.deepEqual(merged.removed, ["2"]);
  assert.equal(JSON.stringify(current), frozen);

  const full = mergeValues(current, { now: 3, full: true, services: { 1: { name: "a2" } }, removed: [] });
  assert.deepEqual(full.values, { now: 3, services: { 1: { name: "a2" } } });
  assert.deepEqual(full.removed, ["2"], "an id the full document no longer has is removed");

  // Missing or odd input does not throw.
  assert.deepEqual(mergeValues(null, {}).values.services, {});
  assert.deepEqual(mergeValues({ services: null }, { services: [] }).values.services, {});
});

test("wirePage_opens_the_stream_closes_it_for_edit_mode_and_opens_it_after_done", async () => {
  const p = page();
  const reloads = [];
  const wired = wirePage(p.document, {
    storage: { getItem: () => null, setItem() {}, removeItem() {} },
    reload: () => reloads.push(1),
    load: async () => ({ startEditing }),
    save: async (next) => ({ status: 200, body: { version: next.version + 1 } }),
    live: p.deps,
  });
  wired.start();
  assert.equal(p.ES.all.length, 1, "opened by the first draw");
  assert.deepEqual(valueTexts(p), ["200", "84"], "drawn from the embedded values");

  p.document.getElementById("mm-edit").dispatchEvent(new p.window.Event("click", { bubbles: true, cancelable: true }));
  await tick();
  assert.ok(p.root.hasAttribute("data-editing"));
  assert.equal(p.ES.all[0].closed, true, "closed while editing");

  // An event that was on its way when editing started does not redraw.
  const editingHtml = p.root.innerHTML;
  wired.live.apply({ now: NOW + 7, full: false, services: { 1: entry(555) }, removed: [] });
  assert.equal(p.root.innerHTML, editingHtml);

  // «Done» with nothing changed: the plain page comes back with the numbers the page holds.
  p.document.getElementById("mm-edit").dispatchEvent(new p.window.Event("click", { bubbles: true, cancelable: true }));
  await tick();
  await tick();
  assert.ok(!p.root.hasAttribute("data-editing"));
  assert.deepEqual(valueTexts(p), ["200", "555"], "the numbers are on the page as soon as editing ends");
  assert.equal(p.ES.all.length, 2, "a new stream, whose first event is the whole document");
  assert.equal(p.ES.all[1].closed, false);
});

test("wirePage_closes_the_stream_on_beforeSwap_and_opens_it_after_the_settle_when_the_page_is_still_there", () => {
  const p = page();
  const wired = wirePage(p.document, {
    storage: { getItem: () => null, setItem() {}, removeItem() {} },
    reload: () => {},
    load: async () => ({ startEditing }),
    live: p.deps,
  });
  wired.start();
  beforeSwap(p);
  assert.equal(p.ES.all[0].closed, true);
  p.document.dispatchEvent(new p.window.Event("htmx:afterSettle", { bubbles: true }));
  assert.equal(p.ES.all.length, 2);

  // Boosted navigation to another page: the stream stays closed.
  beforeSwap(p);
  p.document.body.innerHTML = "<main><h1>Settings</h1></main>";
  p.document.dispatchEvent(new p.window.Event("htmx:afterSettle", { bubbles: true }));
  assert.equal(p.ES.all.length, 2);
  assert.equal(p.ES.all[1].closed, true);
  assert.equal(p.timers.intervals.size, 0);
});

test("draw_hands_the_embedded_values_to_the_renderer", () => {
  const p = page();
  draw(p.document);
  assert.deepEqual(valueTexts(p), ["200", "84"]);
  assert.ok(cardEl(p).querySelector(".mm-age"));
  // Without the block the bound card is waiting, not broken.
  const q = page({ blocks: true });
  q.document.getElementById("mm-values").remove();
  draw(q.document);
  assert.equal(q.root.querySelector(".mm-metric"), null);
});


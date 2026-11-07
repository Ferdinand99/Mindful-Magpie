// Tests for the live numbers on the start page (static/magpie/start.js: initLive,
// mergeValues and the wiring). linkedom stands in for the DOM; the EventSource,
// the timers and the clock are fakes the tests drive by hand.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { parseHTML } from "linkedom";

import { startEditing } from "../static/magpie/edit.js";
import { LIVE_URL, RETRY_MS, TICK_MS, draw, initLive, initNeeds, mergeServices, mergeValues, wirePage } from "../static/magpie/start.js";

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

test("a_removed_service_is_dropped_from_the_values_and_its_card_says_so_and_from_the_services_when_not_visible", () => {
  const p = page();
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  // Unbound or hidden, its values go. It leaves the list the editor offers only when it is not visible.
  p.ES.all[0].emit("values", { now: NOW + 5, full: false, services: {}, removed: [1], services_visible: [] });
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
  p.ES.all[0].emit("values", { now: NOW + 5, full: false, services: {}, removed: [1], services_visible: [], services_add: [] });
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
  const merged = mergeServices(list, values, [1], [{ id: 2, name: "alpha" }, { id: 1, name: "Blog 2" }, null, {}]);
  assert.deepEqual(
    merged.map((s) => [s.id, s.name]),
    [
      [2, "alpha"],
      [1, "Blog 2"],
    ],
  );
  assert.equal(list.length, 2, "the list is not changed");
  // Without a list of visible ids (an event that did not change it) nobody is dropped.
  assert.deepEqual(
    mergeServices(list, values, undefined, undefined).map((s) => s.id),
    [1, 3],
  );
  assert.deepEqual(
    mergeServices(list, values, null, undefined).map((s) => s.id),
    [1, 3],
  );
  // An empty list is a list: nobody is visible.
  assert.deepEqual(mergeServices(list, values, [], undefined), []);
  // Ids compare as numbers or text.
  assert.deepEqual(
    mergeServices(list, values, ["3"], undefined).map((s) => s.id),
    [3],
  );
});

test("an_unbound_service_stays_in_the_editors_list_after_a_removed_event", () => {
  const wiki = { id: 2, name: "Wiki", kind: "http", interval_s: 60, metrics: [] };
  const p = page({ services: [SERVICES[0], wiki] });
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  // The card is unbound elsewhere (Blog's values go), and an unbound Wiki was never in the values.
  p.ES.all[0].emit("values", { now: NOW + 5, full: false, services: {}, removed: [1], services_add: [] });
  assert.deepEqual(
    block(p, "mm-services").map((s) => s.id),
    [1, 2],
    "both are still offered",
  );
  // A set of visible ids that leaves Wiki out takes it away, and only it.
  p.ES.all[0].emit("values", { now: NOW + 6, full: false, services: {}, removed: [], services_visible: [1], services_add: [] });
  assert.deepEqual(
    block(p, "mm-services").map((s) => s.id),
    [1],
  );
  // A service that became visible (an unbound one too) is offered.
  p.ES.all[0].emit("values", { now: NOW + 7, full: false, services: {}, removed: [], services_visible: [1, 2], services_add: [wiki] });
  assert.deepEqual(
    block(p, "mm-services").map((s) => s.id).sort(),
    [1, 2],
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
  p.ES.all[0].emit("values", { now: NOW + 10, full: true, services: {}, removed: [], services_visible: [] });
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

test("an_ack_answer_does_not_move_the_ages_back", async () => {
  // The browser's clock is an hour behind the server's, and the reading is ten minutes old.
  const p = page({ values: { ...valuesFixture(84, { fetchedAt: NOW - 600 }), needs: [NEED] } });
  p.clock.ms = NOW * 1000 - 3_600_000;
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  const age = () => cardEl(p).querySelector(".mm-age").textContent;
  // 30 minutes pass: 40 minutes since the reading.
  p.clock.ms += 30 * 60 * 1000;
  p.timers.fireInterval();
  assert.match(age(), /40/, age());
  assert.ok(cardEl(p).hasAttribute("data-stale"));
  // The answer to an acknowledgement carries the needs and no `now`.
  const needs = initNeeds(p.document, {
    apply: live.apply,
    fetch: async () => ({ status: 200, json: async () => ({ needs: [{ ...NEED, acked: true }] }) }),
  });
  assert.ok(needs);
  p.root.querySelector("button.mm-need-ack").dispatchEvent(new p.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(p.root.querySelector(".mm-needs-toggle").textContent, "1 acknowledged", "the answer was applied");
  assert.match(age(), /40/, `the age stays on the server's clock: ${age()}`);
  assert.ok(cardEl(p).hasAttribute("data-stale"), "and the card is still stale");
  // The next tick agrees, and a full redraw (the data block) does too.
  p.timers.fireInterval();
  assert.match(age(), /40/, age());
  draw(p.document);
  assert.match(age(), /40/, age());
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
  assert.deepEqual(merged.values, { now: 2, services: { 1: { name: "a" }, 3: { name: "c" } }, needs: [] });
  assert.deepEqual(merged.removed, ["2"]);
  assert.equal(JSON.stringify(current), frozen);

  const full = mergeValues(current, { now: 3, full: true, services: { 1: { name: "a2" } }, removed: [] });
  assert.deepEqual(full.values, { now: 3, services: { 1: { name: "a2" } }, needs: [] });
  // The needs list is replaced when an event brings one and kept when it does not.
  const withNeeds = { now: 1, services: {}, needs: [{ key: "a" }] };
  assert.deepEqual(mergeValues(withNeeds, { now: 2, services: {} }).values.needs, [{ key: "a" }]);
  assert.deepEqual(mergeValues(withNeeds, { now: 2, services: {}, needs: [] }).values.needs, []);
  assert.deepEqual(mergeValues(withNeeds, { now: 2, services: {}, needs: [{ key: "b" }] }).values.needs, [{ key: "b" }]);
  assert.deepEqual(mergeValues(withNeeds, { now: 2, services: {}, needs: "junk" }).values.needs, [{ key: "a" }]);
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


// ---- «Trenger deg» ----------------------------------------------------------

const NEED = {
  service_id: 1,
  service_name: "Blog",
  key: "down",
  level: "bad",
  label: "down",
  since: NOW - 120,
  acked: false,
};
const NEED2 = { ...NEED, key: "disk", level: "warn", label: "disk almost full" };

const needsPage = (needs, extra = {}) => page({ values: { ...valuesFixture(), needs }, ...extra });
const chipKeys = (p) => [...p.root.querySelectorAll(".mm-needs-list:not(.mm-needs-acked) .mm-need")].map((li) => li.getAttribute("data-key"));

test("the_page_draws_the_needs_of_the_embedded_values_with_kvitter_buttons", () => {
  const p = needsPage([NEED, NEED2]);
  draw(p.document);
  assert.deepEqual(chipKeys(p), ["down", "disk"]);
  assert.equal(p.root.querySelectorAll("button.mm-need-ack").length, 2, "the page can acknowledge");
});

test("an_event_brings_a_need_into_the_row_in_place_and_announces_it_without_moving_focus", () => {
  const p = needsPage([]);
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  assert.equal(p.root.querySelector(".mm-needs"), null);
  const before = p.focus.count;
  p.ES.all[0].emit("values", { now: NOW + 3, full: false, services: {}, removed: [], needs: [NEED] });
  assert.deepEqual(chipKeys(p), ["down"]);
  assert.equal(p.root.querySelector(".mm-needs-live").textContent, "Blog: down");
  assert.equal(p.focus.count, before, "no focus call");
  assert.deepEqual(block(p, "mm-values").needs, [NEED], "the page's data block follows");
  // An event without a needs list leaves the row alone.
  p.ES.all[0].emit("values", { now: NOW + 4, full: false, services: { 1: entry(9) }, removed: [] });
  assert.deepEqual(chipKeys(p), ["down"]);
  // A full event replaces it.
  p.ES.all[0].emit("values", { now: NOW + 5, full: true, services: { 1: entry(9) }, removed: [], needs: [] });
  assert.equal(p.root.querySelector(".mm-needs"), null);
});

function needsDeps(p, answer) {
  const calls = [];
  const live = initLive(p.document, p.deps);
  live.sync();
  const fetchFn = async (url, init) => {
    calls.push({ url, init });
    return typeof answer === "function" ? answer(url, init) : answer;
  };
  const needs = initNeeds(p.document, { apply: live.apply, fetch: fetchFn });
  return { calls, live, needs };
}

const ok = (needs) => ({ status: 200, json: async () => ({ needs }) });

test("kvitter_posts_the_service_and_key_with_the_pages_proof_and_the_row_follows_the_answer", async () => {
  const p = needsPage([NEED, NEED2]);
  draw(p.document);
  const { calls } = needsDeps(p, ok([NEED, { ...NEED2, acked: true }]));
  const button = p.root.querySelectorAll("button.mm-need-ack")[1];
  button.dispatchEvent(new p.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "/start/needs/ack");
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls[0].init.credentials, "same-origin");
  assert.equal(calls[0].init.headers["HX-Request"], "true");
  assert.equal(calls[0].init.headers["Content-Type"], "application/json");
  assert.deepEqual(JSON.parse(calls[0].init.body), { service_id: 1, key: "disk" });
  // The row now has one chip and the toggle.
  assert.deepEqual(chipKeys(p), ["down"]);
  assert.equal(p.root.querySelector(".mm-needs-toggle").textContent, "1 acknowledged");
  assert.deepEqual(block(p, "mm-values").needs.map((n) => n.acked), [false, true]);
});

test("after_kvitter_the_focus_goes_to_the_next_chip_not_to_the_top_of_the_page", async () => {
  const p = needsPage([NEED, NEED2]);
  draw(p.document);
  needsDeps(p, ok([NEED2]));
  p.root.querySelector("button.mm-need-ack").dispatchEvent(new p.window.Event("click", { bubbles: true }));
  await tick();
  // The first chip went; the one that took its place has the focus (the person asked for the change).
  assert.equal(p.focus.active?.className, "mm-need-chip");
  assert.equal(p.focus.active?.closest(".mm-need")?.getAttribute("data-key"), "disk");
});

test("after_kvitter_of_the_last_chip_the_focus_goes_to_the_toggle", async () => {
  const p = needsPage([NEED]);
  draw(p.document);
  needsDeps(p, ok([{ ...NEED, acked: true }]));
  p.root.querySelector("button.mm-need-ack").dispatchEvent(new p.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(p.focus.active?.className, "mm-needs-toggle");
});

test("undo_posts_to_unack_and_a_failed_ack_is_said_in_the_live_region", async () => {
  const p = needsPage([{ ...NEED, acked: true }]);
  draw(p.document);
  const { calls } = needsDeps(p, ok([NEED]));
  const toggle = p.root.querySelector(".mm-needs-toggle");
  toggle.dispatchEvent(new p.window.Event("click", { bubbles: true }));
  assert.equal(toggle.getAttribute("aria-expanded"), "true", "the toggle works through the page's handler");
  p.root.querySelector("button.mm-need-unack").dispatchEvent(new p.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(calls[0].url, "/start/needs/unack");
  assert.deepEqual(chipKeys(p), ["down"]);

  // A refusal (the need is gone, or the session): nothing changes, and the person is told.
  const q = needsPage([NEED]);
  draw(q.document);
  const failing = needsDeps(q, { status: 404, json: async () => ({ error: "not_found" }) });
  q.root.querySelector("button.mm-need-ack").dispatchEvent(new q.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(failing.calls.length, 1);
  assert.deepEqual(chipKeys(q), ["down"]);
  // A need that is gone is said so, not "try again".
  assert.equal(q.root.querySelector(".mm-needs-live").textContent, LABELS.need_gone);
  assert.equal(LABELS.need_gone, "This has already cleared");
  // Any other refusal asks for another try.
  const busy = needsPage([NEED]);
  draw(busy.document);
  needsDeps(busy, { status: 500, json: async () => ({}) });
  busy.root.querySelector("button.mm-need-ack").dispatchEvent(new busy.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(busy.root.querySelector(".mm-needs-live").textContent, LABELS.need_ack_failed);
  // A network failure too.
  const r = needsPage([NEED]);
  draw(r.document);
  needsDeps(r, () => {
    throw new TypeError("network");
  });
  r.root.querySelector("button.mm-need-ack").dispatchEvent(new p.window.Event("click", { bubbles: true }));
  await tick();
  assert.equal(r.root.querySelector(".mm-needs-live").textContent, LABELS.need_ack_failed);
});

test("a_second_click_while_the_first_is_on_its_way_posts_nothing_more", async () => {
  const p = needsPage([NEED]);
  draw(p.document);
  let release;
  const gate = new Promise((resolve) => (release = resolve));
  const { calls } = needsDeps(p, async () => {
    await gate;
    return ok([{ ...NEED, acked: true }]);
  });
  const button = p.root.querySelector("button.mm-need-ack");
  button.dispatchEvent(new p.window.Event("click", { bubbles: true }));
  button.dispatchEvent(new p.window.Event("click", { bubbles: true }));
  release();
  await tick();
  assert.equal(calls.length, 1);
});

test("wirePage_wires_the_needs_buttons", async () => {
  const p = needsPage([NEED]);
  const calls = [];
  const wired = wirePage(p.document, {
    live: p.deps,
    needs: { fetch: async (url) => (calls.push(url), ok([{ ...NEED, acked: true }])) },
  });
  wired.start();
  p.root.querySelector("button.mm-need-ack").dispatchEvent(new p.window.Event("click", { bubbles: true }));
  await tick();
  assert.deepEqual(calls, ["/start/needs/ack"]);
  assert.equal(chipKeys(p).length, 0);
});

test("a_renamed_service_that_no_card_uses_is_offered_under_its_new_name", () => {
  const wiki = { id: 2, name: "Wiki", kind: "http", interval_s: 60, metrics: [] };
  const p = page({ services: [SERVICES[0], wiki] });
  draw(p.document);
  const live = initLive(p.document, p.deps);
  live.sync();
  // The stream sends the changed view again (an unbound service has no entry to carry its name).
  p.ES.all[0].emit("values", { now: NOW + 5, full: false, services: {}, removed: [], services_add: [{ ...wiki, name: "Handbook" }] });
  assert.deepEqual(
    block(p, "mm-services").map((s) => s.name),
    ["Blog", "Handbook"],
  );
});

// Live service numbers on the new tab (extension v0.3.0): the values fetch (state.js), the
// start-up and refresh sequence (newtab-core.js) and the in-place update (newtab-view.js).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { parseHTML } from "linkedom";
import {
  fetchValues,
  isValidValues,
  keepCurves,
  nextValues,
  serverNowMs,
  statusText,
  statusWithValues,
  valuesUrl,
} from "../extension/state.js";
import { HISTORY_MS, REFRESH_MS, connectionChanged, start } from "../extension/newtab-core.js";
import { createView } from "../extension/newtab-view.js";
import { render, updateValues, tickAges } from "../extension/vendor/render.js";

const TOKEN = "mm_SECRET_VALUES_TOKEN";
const SERVER = "http://10.0.3.152:8099";
const read = (name) => readFileSync(new URL(`../extension/${name}`, import.meta.url), "utf8");

// Real-world magnitudes: the server clock in epoch seconds (about 1.7e9), the device clock in epoch
// milliseconds (about 1.7e12), and a device that runs 5 s ahead of the server.
const SERVER_NOW = 1_700_000_000;
const DEVICE_T = 1_700_000_005_000;
const OFFSET = SERVER_NOW * 1000 - DEVICE_T;
const cache = (v = values(), offsetMs = OFFSET, serverUrl = SERVER) => ({ serverUrl, values: v, offsetMs });
const missing = (at, serverUrl = SERVER) => ({ serverUrl, at });

const SERVICES = [
  {
    id: 1,
    name: "Blog",
    kind: "http",
    interval_s: 60,
    metrics: [
      { id: "status", label: "HTTP status", format: "number" },
      { id: "latency_ms", label: "Latency", format: "number" },
    ],
  },
];
const bound = (id, title, service) => ({
  id,
  title,
  description: "",
  url: `https://${id}.example/`,
  icon: { emoji: null, colour: "teal" },
  ...(service ? { service } : {}),
});
const layout = (over = {}) => ({
  version: 1,
  background: { kind: "preset", name: "natt" },
  groups: [
    {
      id: "g1",
      title: "Work",
      cards: [bound("blog", "Blog", { id: 1, metrics: ["status", "latency_ms"] }), bound("wiki", "Wiki")],
    },
  ],
  bookmarks: [],
  services: SERVICES,
  ...over,
});
const values = (over = {}) => ({
  now: SERVER_NOW,
  services: {
    1: {
      name: "Blog",
      kind: "http",
      interval_s: 60,
      status: "ok",
      values: { status: { num: 200, text: null }, latency_ms: { num: 84, text: null } },
      needs: [],
      fetched_at: SERVER_NOW - 10,
      ...over,
    },
  },
});

const reply = (status, body) => ({ status, json: async () => body });

// ---------------------------------------------------------------- state.js

test("valuesUrl joins the server and the values endpoint once", () => {
  assert.equal(valuesUrl("http://h:1/"), "http://h:1/api/v1/me/values");
  assert.equal(valuesUrl("https://h/magpie//"), "https://h/magpie/api/v1/me/values");
  assert.equal(valuesUrl("http://h:1/", { history: false }), "http://h:1/api/v1/me/values?history=none");
  assert.equal(valuesUrl("http://h:1/", { history: true }), "http://h:1/api/v1/me/values");
});

test("isValidValues accepts the values document and rejects other shapes", () => {
  assert.equal(isValidValues(values()), true);
  assert.equal(isValidValues({ now: 1, services: {} }), true);
  assert.equal(isValidValues({ now: "2025-10-01T00:00:00Z", services: {} }), true);
  for (const bad of [null, undefined, "x", [], {}, { now: 1 }, { services: {} }, { now: 1, services: [] }, { now: 1, services: null }, { now: "soon", services: {} }]) {
    assert.equal(isValidValues(bad), false, JSON.stringify(bad));
  }
});

test("serverNowMs reads epoch seconds, epoch milliseconds and ISO text", () => {
  assert.equal(serverNowMs({ now: 1000 }), 1_000_000);
  assert.equal(serverNowMs({ now: SERVER_NOW }), 1_700_000_000_000);
  assert.equal(serverNowMs({ now: 1_700_000_000_000 }), 1_700_000_000_000);
  assert.equal(serverNowMs({ now: "1970-01-01T00:16:40Z" }), 1_000_000);
  assert.equal(serverNowMs({ now: "soon" }), null);
  assert.equal(serverNowMs(null), null);
});

test("fetchValues sends the token in the header only and keeps the request private", async () => {
  let seen;
  const result = await fetchValues(async (url, opts) => {
    seen = { url, opts };
    return reply(200, values());
  }, `${SERVER}/`, TOKEN);
  assert.deepEqual(result, { ok: true, values: values() });
  assert.equal(seen.url, `${SERVER}/api/v1/me/values`);
  assert.equal(seen.opts.headers.Authorization, `Bearer ${TOKEN}`);
  assert.equal(seen.url.includes(TOKEN), false);
  assert.equal(seen.opts.redirect, "error");
  assert.equal(seen.opts.credentials, "omit");
  assert.equal(seen.opts.cache, "no-store");
  assert.ok(seen.opts.signal instanceof AbortSignal);
});

test("fetchValues maps 401 and 403 to unauthorized, 404 to missing, the rest to network", async () => {
  const kind = async (impl) => (await fetchValues(impl, SERVER, TOKEN)).kind;
  assert.equal(await kind(async () => reply(401, {})), "unauthorized");
  assert.equal(await kind(async () => reply(403, {})), "unauthorized");
  assert.equal(await kind(async () => reply(404, {})), "missing");
  assert.equal(await kind(async () => reply(500, null)), "network");
  assert.equal(await kind(async () => reply(200, { now: 1, services: [] })), "network");
  assert.equal(await kind(async () => ({ status: 200, json: async () => { throw new SyntaxError("bad"); } })), "network");
  assert.equal(await kind(async () => { throw new TypeError("NetworkError"); }), "network");
});

test("fetchValues without a server or token does not fetch", async () => {
  let called = false;
  const impl = async () => { called = true; return reply(200, values()); };
  assert.deepEqual(await fetchValues(impl, "", TOKEN), { ok: false, kind: "network" });
  assert.deepEqual(await fetchValues(impl, SERVER, ""), { ok: false, kind: "network" });
  assert.equal(called, false);
});

test("the token never appears in a values result, an error or the console", async () => {
  const logged = [];
  const originals = {};
  for (const level of ["log", "info", "warn", "error", "debug"]) {
    originals[level] = console[level];
    console[level] = (...args) => logged.push(args.map(String).join(" "));
  }
  const results = [];
  try {
    results.push(await fetchValues(async (url, opts) => { throw new TypeError(`${url} ${opts.headers.Authorization}`); }, SERVER, TOKEN));
    results.push(await fetchValues(async () => ({ status: 200, json: async () => { throw new SyntaxError(`bad ${TOKEN}`); } }), SERVER, TOKEN));
    results.push(await fetchValues(async () => reply(401, { error: TOKEN }), SERVER, TOKEN));
    results.push(await fetchValues(async () => reply(404, { error: TOKEN }), SERVER, TOKEN));
  } finally {
    Object.assign(console, originals);
  }
  assert.equal(JSON.stringify(results).includes(TOKEN), false);
  assert.equal(logged.join("\n").includes(TOKEN), false);
});

test("nextValues keeps the cache on a failure and drops it for a server without values", () => {
  const cached = values();
  const fresh = values({ status: "warn" });
  assert.deepEqual(nextValues(cached, undefined), { values: cached, kind: "cached" });
  assert.deepEqual(nextValues(cached, { ok: true, values: fresh }), { values: fresh, kind: "fresh" });
  assert.deepEqual(nextValues(cached, { ok: false, kind: "network" }), { values: cached, kind: "network" });
  assert.deepEqual(nextValues(cached, { ok: false, kind: "unauthorized" }), { values: cached, kind: "unauthorized" });
  assert.deepEqual(nextValues(cached, { ok: false, kind: "missing" }), { values: null, kind: "missing" });
  assert.deepEqual(nextValues(null, { ok: false, kind: "network" }), { values: null, kind: "network" });
});

test("statusWithValues marks a failed values fetch, and a token problem wins", () => {
  assert.equal(statusWithValues("fresh", "network"), "offline");
  assert.equal(statusWithValues("fresh", "fresh"), "fresh");
  assert.equal(statusWithValues("fresh", "missing"), "fresh");
  assert.equal(statusWithValues("fresh", "unauthorized"), "unauthorized");
  assert.equal(statusWithValues("offline", "fresh"), "offline");
  assert.equal(statusWithValues("cached", "cached"), "cached");
  assert.equal(statusText.offline, "Frakoblet – viser sist lagrede");
  assert.equal(statusText.unauthorized, "Ugyldig token – se innstillingene");
});

// ---------------------------------------------------------------- newtab-core.js

function env({ stored = {}, startReply, valuesReply, hidden = false, t = DEVICE_T, connection = true } = {}) {
  const store = { serverUrl: SERVER, token: TOKEN, ...stored };
  const clock = { t };
  const fetches = [];
  const draws = [];
  const updates = [];
  const ticks = [];
  const sets = [];
  const removes = [];
  const timers = {
    next: 0,
    active: new Map(),
    cleared: 0,
    set(fn, ms) {
      this.next += 1;
      this.active.set(this.next, { fn, ms });
      return this.next;
    },
    clear(id) {
      this.cleared += 1;
      this.active.delete(id);
    },
    fire() {
      return Promise.all([...this.active.values()].map((x) => x.fn()));
    },
  };
  const visibility = {
    hidden,
    listeners: [],
    isHidden() {
      return this.hidden;
    },
    onChange(cb) {
      this.listeners.push(cb);
    },
    set(h) {
      this.hidden = h;
      return Promise.all(this.listeners.map((cb) => cb()));
    },
  };
  const changes = {
    listeners: [],
    onChange(cb) {
      this.listeners.push(cb);
    },
    fire() {
      for (const cb of this.listeners) cb();
    },
  };
  const e = {
    changes, store, clock, fetches, draws, updates, ticks, sets, removes, timers, visibility,
    startReply: startReply ?? (() => reply(200, layout())),
    valuesReply: valuesReply ?? (() => reply(200, values())),
    deps: {
      storage: {
        local: {
          get: async () => ({ ...store }),
          set: async (v) => { sets.push(v); Object.assign(store, v); },
          remove: async (keys) => { removes.push(keys); for (const k of [].concat(keys)) delete store[k]; },
        },
      },
      fetchImpl: async (url) => {
        fetches.push(url);
        const r = VALUES_PATH.test(url) ? e.valuesReply(url) : e.startReply();
        if (r instanceof Error) throw r;
        return r;
      },
      draw: (view, conn) => draws.push({ view, conn, fetchesSoFar: fetches.length }),
      update: (view, conn) => updates.push({ view, conn }),
      tick: (nowMs) => ticks.push(nowMs),
      visibility,
      ...(connection ? { connection: changes } : {}),
      timers,
      now: () => clock.t,
    },
  };
  return e;
}
const VALUES_PATH = /\/api\/v1\/me\/values(\?history=none)?$/;
const valuesFetches = (e) => e.fetches.filter((u) => VALUES_PATH.test(u)).length;

test("cached values are drawn before any fetch, with ages counted from the estimated server clock", async () => {
  const e = env({ stored: { last: layout(), lastValues: cache() }, t: DEVICE_T + 300_000 });
  await start(e.deps);
  const first = e.draws[0];
  assert.equal(first.fetchesSoFar, 0);
  assert.equal(first.view.status, "cached");
  assert.deepEqual(first.view.services, SERVICES);
  // 300 s after the cache was made the server is at SERVER_NOW + 300, whatever the document says.
  assert.equal(first.view.values.now, SERVER_NOW + 300);
  assert.deepEqual(first.view.values.services, values().services);
});

test("a fresh fetch draws the new layout and values and stores both with the clock offset", async () => {
  const e = env();
  await start(e.deps);
  const last = e.draws.at(-1).view;
  assert.equal(last.status, "fresh");
  assert.deepEqual(last.values.services, values().services);
  assert.deepEqual(last.services, SERVICES);
  assert.deepEqual(e.store.lastValues, { serverUrl: SERVER, values: values(), offsetMs: OFFSET });
  assert.deepEqual(e.store.last, layout());
});

test("the offset clock: ages tick against the server's clock, not the device's", async () => {
  const e = env();
  await start(e.deps);
  e.valuesReply = () => new TypeError("offline");
  e.clock.t = DEVICE_T + 60_000;
  await e.timers.fire();
  assert.deepEqual(e.ticks, [SERVER_NOW * 1000 + 60_000]);
  assert.equal(e.updates.at(-1).view.values.now, SERVER_NOW + 60);
});

test("a values fetch failure keeps the cached values and marks the page offline", async () => {
  const cached = values({ fetched_at: SERVER_NOW - 5000 });
  const e = env({
    stored: { last: layout(), lastValues: cache(cached) },
    valuesReply: () => new TypeError("NetworkError"),
  });
  await start(e.deps);
  const view = e.draws.at(-1).view;
  assert.equal(view.status, "offline");
  assert.deepEqual(view.values.services, cached.services);
  assert.equal("lastValues" in e.sets.reduce((a, s) => ({ ...a, ...s }), {}), false);
  assert.deepEqual(e.store.lastValues.values, cached);
});

test("a values fetch that answers 500 or junk is a failure too", async () => {
  for (const r of [() => reply(500, null), () => reply(200, { nope: true })]) {
    const e = env({ stored: { lastValues: cache() }, valuesReply: r });
    await start(e.deps);
    assert.equal(e.draws.at(-1).view.status, "offline");
    assert.ok(e.draws.at(-1).view.values);
  }
});

test("a layout with no bound cards ignores a failing values fetch", async () => {
  const plain = layout({ groups: [{ id: "g", title: "Plain", cards: [bound("wiki", "Wiki")] }], services: [] });
  const e = env({ startReply: () => reply(200, plain), valuesReply: () => new TypeError("x") });
  await start(e.deps);
  assert.equal(e.draws.at(-1).view.status, "fresh");
  assert.equal(e.timers.active.size, 0);
});

test("a 401 or 403 from values reads like a 401 from start and stops the refresh", async () => {
  for (const code of [401, 403]) {
    const e = env({ stored: { lastValues: cache() }, valuesReply: () => reply(code, {}) });
    await start(e.deps);
    const view = e.draws.at(-1).view;
    assert.equal(view.status, "unauthorized");
    assert.ok(view.values, "the cache stays");
    assert.equal(e.timers.active.size, 0, "no polling with a rejected token");
  }
});

test("old servers: 404 draws without values and is not asked again for five minutes", async () => {
  const e = env({
    stored: { lastValues: cache() },
    valuesReply: () => reply(404, { error: "not_found" }),
  });
  await start(e.deps);
  const view = e.draws.at(-1).view;
  assert.equal(view.values, null);
  assert.equal(view.status, "fresh");
  assert.equal("lastValues" in e.store, false);
  assert.deepEqual(e.store.valuesMissing, missing(DEVICE_T));
  assert.equal(valuesFetches(e), 1);

  e.clock.t += 60_000;
  await e.timers.fire();
  e.clock.t += 60_000;
  await e.timers.fire();
  assert.equal(valuesFetches(e), 1, "still inside the back-off");

  e.clock.t = DEVICE_T + 5 * 60_000;
  await e.timers.fire();
  assert.equal(valuesFetches(e), 2, "asked again after five minutes");
});

test("a new tab inside the back-off does not ask an old server at all", async () => {
  const e = env({ stored: { valuesMissing: missing(DEVICE_T - 100_000) } });
  await start(e.deps);
  assert.equal(valuesFetches(e), 0);
  assert.equal(e.draws.at(-1).view.values, null);
  assert.equal(e.draws.at(-1).view.status, "fresh");
});

test("an old server that gets values later: the back-off ends and the values are kept", async () => {
  const e = env({ stored: { valuesMissing: missing(1) } });
  await start(e.deps);
  assert.equal(valuesFetches(e), 1);
  assert.equal("valuesMissing" in e.store, false);
  assert.ok(e.draws.at(-1).view.values);
});

test("refresh runs every 30 s while visible, uses update (not draw) and stops while hidden", async () => {
  const e = env();
  await start(e.deps);
  assert.equal(e.timers.active.size, 1);
  assert.equal([...e.timers.active.values()][0].ms, 30_000);
  const draws = e.draws.length;

  await e.timers.fire();
  assert.equal(valuesFetches(e), 2);
  assert.equal(e.updates.length, 1);
  assert.equal(e.draws.length, draws, "a refresh updates in place, it does not redraw");

  await e.visibility.set(true);
  assert.equal(e.timers.active.size, 0, "the timer is gone while hidden");
  const before = e.fetches.length;
  await e.timers.fire();
  assert.equal(e.fetches.length, before, "nothing is fetched while hidden");

  e.clock.t += 60_000;
  await e.visibility.set(false);
  assert.equal(valuesFetches(e), 3, "shown again: refresh at once");
  assert.equal(e.timers.active.size, 1);
});

test("a tab that starts hidden does not poll until it is shown", async () => {
  const e = env({ hidden: true });
  await start(e.deps);
  assert.equal(e.timers.active.size, 0);
  await e.visibility.set(false);
  assert.equal(e.timers.active.size, 1);
});

test("stop() ends the polling and ignores later visibility changes", async () => {
  const e = env();
  const ctl = await start(e.deps);
  ctl.stop();
  assert.equal(e.timers.active.size, 0);
  await e.visibility.set(true);
  await e.visibility.set(false);
  assert.equal(e.timers.active.size, 0);
});

test("a slow refresh is not started twice", async () => {
  const e = env();
  await start(e.deps);
  let release;
  e.valuesReply = () => new Promise((r) => { release = r; });
  const first = e.timers.fire();
  await new Promise((r) => setImmediate(r));
  const second = e.timers.fire();
  release(reply(200, values()));
  await Promise.all([first, second]);
  assert.equal(valuesFetches(e), 2);
});

test("the token is never in a draw, an update or a stored value", async () => {
  const e = env({ valuesReply: () => new TypeError(`failed ${TOKEN}`) });
  await start(e.deps);
  await e.timers.fire();
  const seen = JSON.stringify([e.draws.map((d) => d.view), e.updates.map((u) => u.view), e.sets, e.ticks]);
  assert.equal(seen.includes(TOKEN), false);
});

test("a layout with no bound cards still records an old server's 404, and the next tab does not ask", async () => {
  const plain = layout({ groups: [{ id: "g", title: "Plain", cards: [bound("wiki", "Wiki")] }], services: undefined });
  delete plain.services;
  const e = env({ startReply: () => reply(200, plain), valuesReply: () => reply(404, { error: "not_found" }) });
  await start(e.deps);
  assert.equal(valuesFetches(e), 1);
  assert.deepEqual(e.store.valuesMissing, missing(DEVICE_T));
  assert.equal(e.draws.at(-1).view.status, "fresh");
  assert.equal(e.timers.active.size, 0, "nothing bound, nothing to poll");

  e.clock.t += 4 * 60_000;
  await start(e.deps);
  assert.equal(valuesFetches(e), 1, "inside the five minutes");
  e.clock.t += 61_000;
  await start(e.deps);
  assert.equal(valuesFetches(e), 2, "asked again after five minutes");
});

test("a values cache or back-off from another server is never drawn or obeyed", async () => {
  const e = env({
    stored: {
      last: layout(),
      lastValues: cache(values(), OFFSET, "http://other.example:1"),
      valuesMissing: missing(DEVICE_T, "http://other.example:1"),
    },
    valuesReply: () => new TypeError("down"),
  });
  await start(e.deps);
  assert.equal(e.draws[0].view.values, null, "the first paint has no foreign numbers");
  assert.equal(e.draws.at(-1).view.values, null);
  assert.equal(valuesFetches(e), 1, "the foreign back-off does not apply");
});

test("a server or token that changes between the fetch and the write leaves no write", async () => {
  for (const change of [{ serverUrl: "http://other.example:1" }, { token: "another-token" }]) {
    const e = env({
      valuesReply: () => {
        Object.assign(e.store, change);
        return reply(200, values());
      },
    });
    await start(e.deps);
    assert.deepEqual(e.sets, [], JSON.stringify(change));
    assert.deepEqual(e.removes, []);
    assert.equal(e.timers.active.size, 0);
    assert.equal(e.draws.length, 1, "only the cached first paint: the answers were for the old connection");
  }
});

test("a 404 for the old connection is not recorded for the new one", async () => {
  const e = env({
    valuesReply: () => {
      e.store.serverUrl = "http://new.example:1";
      return reply(404, {});
    },
  });
  await start(e.deps);
  assert.equal("valuesMissing" in e.store, false);
  assert.deepEqual(e.sets, []);
});

test("a refresh that finishes after a server change writes nothing and updates nothing", async () => {
  const e = env();
  await start(e.deps);
  const sets = e.sets.length;
  const updates = e.updates.length;
  e.valuesReply = () => {
    e.store.serverUrl = "http://other.example:1";
    return reply(200, values({ status: "bad" }));
  };
  await e.timers.fire();
  assert.equal(e.sets.length, sets);
  assert.equal(e.updates.length, updates);
  assert.equal(e.timers.active.size, 0);
});

test("a storage change of the server or token stops the refresh at once", async () => {
  const e = env();
  const ctl = await start(e.deps);
  assert.equal(e.timers.active.size, 1);
  e.changes.fire();
  assert.equal(e.timers.active.size, 0);
  const before = e.fetches.length;
  await ctl.refresh();
  await e.visibility.set(true);
  await e.visibility.set(false);
  assert.equal(e.fetches.length, before);
  assert.equal(e.timers.active.size, 0);
});

test("a values document over 256 KiB is shown but not cached, and drops the older copy", async () => {
  const big = values();
  big.services[1].error_label = "x".repeat(300 * 1024);
  const e = env({ stored: { lastValues: cache() }, valuesReply: () => reply(200, big) });
  await start(e.deps);
  assert.ok(e.draws.at(-1).view.values);
  assert.equal("lastValues" in e.store, false);
  const small = env({ valuesReply: () => reply(200, values()) });
  await start(small.deps);
  assert.ok(small.store.lastValues);
});

test("an exception inside a refresh or a visibility callback does not escape", async () => {
  const e = env();
  e.deps.update = () => {
    throw new Error("renderer broke");
  };
  await start(e.deps);
  await assert.doesNotReject(e.timers.fire());
  e.clock.t += 60_000;
  await assert.doesNotReject(e.visibility.set(true));
  await assert.doesNotReject(e.visibility.set(false));
});

test("through the real view: a cached page ten minutes later reads Last updated, with exact text", async () => {
  const v = makeView();
  const e = env({
    stored: { last: layout(), lastValues: cache() },
    t: DEVICE_T + 600_000,
    startReply: () => new TypeError("offline"),
    valuesReply: () => new TypeError("offline"),
  });
  e.deps.draw = v.draw;
  e.deps.update = v.draw.update;
  e.deps.tick = v.draw.tick;
  await start(e.deps);
  const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto", style: "short" });
  // Fetched 10 s before the cache was made, plus 600 s since: 610 s is 10 whole minutes.
  assert.equal(v.root.querySelector(".mm-age").textContent, `Last updated ${rtf.format(-10, "minute")}`);
  assert.equal(v.root.querySelector(".mm-bound").hasAttribute("data-stale"), true);
  assert.equal(v.statusEl.textContent, statusText.offline);
});

test("a token or server change clears the values cache (options page)", async () => {
  const { saveConnection } = await import("../extension/options-core.js");
  const store = { serverUrl: SERVER, token: "old", last: layout(), lastValues: cache(), valuesMissing: missing(5) };
  const storage = {
    local: {
      get: async (keys) => Object.fromEntries([].concat(keys).map((k) => [k, store[k]])),
      set: async (v) => Object.assign(store, v),
      remove: async (keys) => { for (const k of [].concat(keys)) delete store[k]; },
    },
  };
  const result = await saveConnection(
    {
      storage,
      fetchImpl: async () => reply(200, layout()),
      permissions: { contains: async () => true, request: async () => true },
    },
    { serverUrl: SERVER, token: "new" },
  );
  assert.equal(result.ok, true, JSON.stringify(result));
  for (const key of ["last", "lastValues", "valuesMissing"]) assert.equal(key in store, false, key);
});

// ---------------------------------------------------------------- newtab-view.js

function page() {
  const { document } = parseHTML(
    '<!doctype html><html lang="nb"><body><p id="status"></p><section id="groups" class="mm-root"></section></body></html>',
  );
  return { doc: document, statusEl: document.getElementById("status"), root: document.getElementById("groups") };
}
function makeView(renderer) {
  const p = page();
  const draw = createView({
    doc: p.doc,
    statusEl: p.statusEl,
    root: p.root,
    language: "en-GB",
    fetchImpl: async () => { throw new Error(`no network ${TOKEN}`); },
    openOptions: () => {},
    objectUrl: { create: () => "blob:x", revoke: () => {} },
    ...(renderer ? { renderer } : {}),
  });
  return { ...p, draw };
}
const CONN = { serverUrl: SERVER, token: TOKEN };
const DATA = layout();
const viewOf = (v, status = "fresh") => ({ data: DATA, services: SERVICES, values: v, status });
const metricTexts = (root) => [...root.querySelectorAll(".mm-metric-value")].map((n) => n.textContent);

test("the view draws bound cards with numbers", () => {
  const v = makeView();
  v.draw(viewOf(values()), CONN);
  assert.equal(v.root.querySelectorAll(".mm-bound").length, 1);
  assert.deepEqual(metricTexts(v.root), ["200", "84"]);
});

test("update swaps new numbers in and keeps the same <a> elements (so the address bar keeps focus)", () => {
  const v = makeView();
  v.draw(viewOf(values()), CONN);
  const before = [...v.root.querySelectorAll("a")];
  assert.ok(before.length >= 2);
  const fresh = values({ values: { status: { num: 503, text: null }, latency_ms: { num: 9, text: null } }, status: "bad" });
  v.draw.update(viewOf({ ...fresh, now: 1030 }, "fresh"), CONN);
  const after = [...v.root.querySelectorAll("a")];
  assert.equal(after.length, before.length);
  after.forEach((a, i) => assert.equal(a === before[i], true, `link ${i} is the same element`));
  assert.deepEqual(metricTexts(v.root), ["503", "9"]);
});

test("update falls back to a full render when updateValues says the page differs", () => {
  const calls = [];
  const renderer = {
    render: (...a) => { calls.push("render"); return render(...a); },
    updateValues: () => { calls.push("updateValues"); return false; },
    tickAges,
  };
  const v = makeView(renderer);
  v.draw(viewOf(values()), CONN);
  v.draw.update(viewOf(values({ status: "warn" })), CONN);
  assert.deepEqual(calls, ["render", "updateValues", "render"]);
  assert.equal(v.root.querySelectorAll(".mm-bound").length, 1);
});

test("update with a different layout draws again instead of updating", () => {
  const calls = [];
  const renderer = {
    render: (...a) => { calls.push("render"); return render(...a); },
    updateValues: (...a) => { calls.push("updateValues"); return updateValues(...a); },
    tickAges,
  };
  const v = makeView(renderer);
  v.draw(viewOf(values()), CONN);
  v.draw.update({ ...viewOf(values()), data: layout() }, CONN);
  assert.deepEqual(calls, ["render", "render"]);
});

test("the renderer gets services and values, and canAck is false (read-only token)", () => {
  const seen = [];
  const renderer = { render: (root, data, opts) => { seen.push(opts); return render(root, data, opts); }, updateValues, tickAges };
  const v = makeView(renderer);
  v.draw(viewOf(values()), CONN);
  assert.deepEqual(seen[0].services, SERVICES);
  assert.equal(seen[0].values.now, SERVER_NOW);
  // The extension token is read-only: no acknowledge button.
  assert.equal(seen[0].canAck, false);
});

test("tick moves the ages without drawing again", () => {
  const calls = [];
  const renderer = { render, updateValues, tickAges: (...a) => { calls.push(a[1].now); return tickAges(...a); } };
  const v = makeView(renderer);
  v.draw(viewOf(values()), CONN);
  const age = () => v.root.querySelector(".mm-age").textContent;
  const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto", style: "short" });
  assert.equal(age(), rtf.format(-10, "second"));
  assert.equal(v.root.querySelector(".mm-bound").hasAttribute("data-stale"), false);
  // Ten minutes later: fetched 610 s ago, more than three intervals of 60 s, so stale.
  v.draw.tick(SERVER_NOW * 1000 + 10 * 60_000);
  assert.deepEqual(calls, [SERVER_NOW * 1000 + 600_000]);
  assert.equal(age(), `Last updated ${rtf.format(-10, "minute")}`);
  assert.equal(v.root.querySelector(".mm-bound").hasAttribute("data-stale"), true);
});

test("the token is not in the DOM, the status line or any thrown error", async () => {
  const v = makeView();
  await v.draw(viewOf(values()), CONN);
  v.draw.update(viewOf(values(), "offline"), CONN);
  v.draw.update(viewOf(values(), "unauthorized"), CONN);
  const dom = v.doc.documentElement.outerHTML + v.doc.documentElement.getAttribute("style");
  assert.equal(dom.includes(TOKEN), false);
  assert.equal(v.statusEl.textContent, statusText.unauthorized);
  // Image background fetch fails with an error that carries the token: still nothing leaks.
  const img = { ...viewOf(values()), data: layout({ background: { kind: "image", id: "img_1" } }) };
  await v.draw(img, CONN);
  assert.equal(v.doc.documentElement.outerHTML.includes(TOKEN), false);
});

test("update on a view with no layout draws the empty state", () => {
  const v = makeView();
  v.draw.update({ data: null, services: undefined, values: null, status: "empty" }, CONN);
  assert.equal(v.root.children.length, 0);
  assert.ok(v.statusEl.textContent.startsWith(statusText.empty));
});

// ---------------------------------------------------------------- files

test("v0.4.0 is the manifest version, Firefox 140 the floor, and Android needs 142 (data collection keys)", () => {
  const m = JSON.parse(read("manifest.json"));
  assert.equal(m.version, "0.4.0");
  assert.equal(m.browser_specific_settings.gecko.strict_min_version, "140.0");
  assert.deepEqual(m.browser_specific_settings.gecko_android, { strict_min_version: "142.0" });
  assert.deepEqual(m.browser_specific_settings.gecko.data_collection_permissions, { required: ["none"] });
});

test("the new-tab code still never calls focus()", () => {
  for (const file of ["newtab.js", "newtab-core.js", "newtab-view.js", "state.js"]) {
    assert.equal(/\.focus\s*\(/.test(read(file)), false, `${file} must not call focus()`);
  }
});

test("the page script wires the refresh to the page's visibility", () => {
  const src = read("newtab.js");
  assert.ok(src.includes("visibilitychange"));
  assert.ok(src.includes("document.hidden"));
});

test("the page script stops the refresh when the saved server or token changes", () => {
  const src = read("newtab.js");
  assert.ok(src.includes("storage.onChanged"));
  assert.ok(src.includes("connectionChanged"));
  assert.ok(src.includes("connection"));
});

test("only a real change of the server or token stops the tabs, not saving the same values again", () => {
  const same = { serverUrl: { oldValue: "http://h:1", newValue: "http://h:1" }, token: { oldValue: "t", newValue: "t" } };
  assert.equal(connectionChanged(same, "local"), false);
  assert.equal(connectionChanged({ serverUrl: { oldValue: "http://h:1", newValue: "http://h:2" } }, "local"), true);
  assert.equal(connectionChanged({ token: { oldValue: "a", newValue: "b" } }, "local"), true);
  // A first connection (nothing before) and a disconnect (nothing after) are changes.
  assert.equal(connectionChanged({ token: { newValue: "b" } }, "local"), true);
  assert.equal(connectionChanged({ token: { oldValue: "a" } }, "local"), true);
  // Other keys (the cache) and other areas are not the connection.
  assert.equal(connectionChanged({ last: { oldValue: 1, newValue: 2 } }, "local"), false);
  assert.equal(connectionChanged({ token: { oldValue: "a", newValue: "b" } }, "sync"), false);
  assert.equal(connectionChanged(undefined, "local"), false);
});

// ---------------------------------------------------------------- curves (history)

test("keepCurves puts the previous curves back into a document fetched without them", () => {
  const curve = { lat: [[SERVER_NOW - 300, 1], [SERVER_NOW, 2]] };
  const days = { lat: [["2023-11-14", 1, 2, 1.5]] };
  const previous = values({ history: curve, history_daily: days });
  const frozen = JSON.stringify(previous);
  const light = { now: SERVER_NOW + 30, services: { 1: { ...values().services[1], status: "warn" }, 2: { name: "New" } } };
  const kept = keepCurves(previous, light);
  assert.deepEqual(kept.services[1].history, curve);
  assert.deepEqual(kept.services[1].history_daily, days);
  assert.equal(kept.services[1].status, "warn");
  assert.deepEqual(kept.services[2], { name: "New" });
  assert.equal(kept.now, SERVER_NOW + 30);
  assert.equal(JSON.stringify(previous), frozen);
  // A document that brings its own curves keeps them; odd input does not throw.
  const own = { now: 1, services: { 1: { history: { lat: [[1, 9]] } } } };
  assert.deepEqual(keepCurves(previous, own).services[1].history, { lat: [[1, 9]] });
  assert.deepEqual(keepCurves(null, light).services[2], { name: "New" });
  assert.deepEqual(keepCurves({ services: { __proto__: null } }, light).services[1].status, "warn");
  assert.equal(keepCurves(previous, { now: 1, services: null }).services, null);
});

test("the 30 s refresh leaves the curves out and keeps them; every 5 minutes they come again", async () => {
  const curve = (v) => ({ lat: [[SERVER_NOW, v]] });
  const e = env({
    valuesReply: (url) => reply(200, url.endsWith("?history=none") ? values() : values({ history: curve(e.clock.t) })),
  });
  await start(e.deps);
  const urls = () => e.fetches.filter((u) => VALUES_PATH.test(u));
  assert.equal(urls().length, 1);
  assert.equal(urls()[0], `${SERVER}/api/v1/me/values`, "the first fetch takes the curves");
  const first = e.draws.at(-1).view.values.services[1].history;
  assert.deepEqual(first, curve(DEVICE_T));

  // Refreshes inside 5 minutes: without the curves, and the page keeps the ones it has.
  for (let i = 1; i < HISTORY_MS / REFRESH_MS; i += 1) {
    e.clock.t = DEVICE_T + i * REFRESH_MS;
    await e.timers.fire();
    assert.ok(urls().at(-1).endsWith("?history=none"), `refresh ${i}`);
    assert.deepEqual(e.updates.at(-1).view.values.services[1].history, first, `refresh ${i}`);
  }
  // The cache keeps them too.
  assert.deepEqual(e.store.lastValues.values.services[1].history, first);

  // 5 minutes after the last curves: they come again.
  e.clock.t = DEVICE_T + HISTORY_MS;
  await e.timers.fire();
  assert.equal(urls().at(-1), `${SERVER}/api/v1/me/values`);
  assert.deepEqual(e.updates.at(-1).view.values.services[1].history, curve(DEVICE_T + HISTORY_MS));
});

// ---------------------------------------------------------------- one language on the whole page

test("a Norwegian browser gets Norwegian metric labels from the server and Norwegian detail titles", async () => {
  const asked = [];
  // A server that names the metrics in the language the request asks for, as Magpie does.
  const server = async (url, opts = {}) => {
    const lang = opts.headers?.["Accept-Language"];
    asked.push([url, lang]);
    assert.equal(opts.headers.Authorization, `Bearer ${TOKEN}`, "the token still goes");
    if (VALUES_PATH.test(url)) {
      return reply(200, values({ details: [{ kind: "list", title_key: "vv_down_now", items: [{ text: "Sylo", status: "bad" }] }] }));
    }
    const label = lang === "nb" ? "HTTP-status" : "HTTP status";
    const services = [{ ...SERVICES[0], metrics: [{ id: "status", label, format: "number" }, SERVICES[0].metrics[1]] }];
    return reply(200, layout({ services }));
  };
  for (const [language, metric, title] of [
    ["nb-NO", "HTTP-status", "Nede nå"],
    ["en-GB", "HTTP status", "Down now"],
  ]) {
    asked.length = 0;
    const p = page();
    const draw = createView({
      doc: p.doc,
      statusEl: p.statusEl,
      root: p.root,
      language,
      fetchImpl: server,
      openOptions: () => {},
      objectUrl: { create: () => "blob:x", revoke: () => {} },
    });
    const e = env();
    await start({ ...e.deps, fetchImpl: server, draw, update: draw.update, language });
    const want = language.startsWith("nb") ? "nb" : "en";
    assert.ok(asked.length >= 2 && asked.every(([, lang]) => lang === want), JSON.stringify(asked));
    const labels = [...p.root.querySelectorAll(".mm-metric-label")].map((n) => n.textContent);
    assert.equal(labels[0], metric, language);
    assert.equal(p.root.querySelector(".mm-block-title").textContent, title, language);
    assert.equal(p.doc.documentElement.getAttribute("lang"), want);
  }
  // Without a language the request is as it was: no header of the page's own.
  const e = env();
  const seen = [];
  await start({ ...e.deps, fetchImpl: async (url, opts) => (seen.push(opts.headers), e.deps.fetchImpl(url)) });
  assert.ok(seen.every((h) => !("Accept-Language" in h)));
});

test("the new tab uses the width of the screen", () => {
  const html = read("newtab.html");
  assert.match(html, /<main class="shell newtab">/);
  const css = read("shell.css");
  const rule = css.match(/\.shell\.newtab \{([^}]*)\}/)[1];
  assert.match(rule, /max-width: 2400px;/);
  assert.match(rule, /padding-inline: clamp\(16px, 3vw, 48px\);/);
});

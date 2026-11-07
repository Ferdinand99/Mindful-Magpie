import { test } from "node:test";
import assert from "node:assert/strict";
import {
  classifyResponse,
  fetchData,
  isValidData,
  nextView,
  startUrl,
  statusText,
} from "../extension/state.js";

const layout = (over = {}) => ({
  version: 1,
  background: { kind: "preset", name: "dusk" },
  groups: [],
  bookmarks: [],
  ...over,
});
const C = layout();
const D = layout({
  groups: [
    {
      id: "g1",
      title: "Work",
      cards: [
        { id: "c1", title: "Wiki", description: "", url: "https://wiki.example", icon: { emoji: "W", colour: "mint" } },
      ],
    },
  ],
  bookmarks: [{ id: "b1", title: "Docs", url: "https://docs.example" }],
});

test("no cache and network failure is empty", () => {
  assert.deepEqual(nextView(null, { ok: false, kind: "network" }), { data: null, status: "empty" });
});

test("cache and network failure is offline", () => {
  assert.deepEqual(nextView(C, { ok: false, kind: "network" }), { data: C, status: "offline" });
});

test("cache and rejected token is unauthorized", () => {
  assert.deepEqual(nextView(C, { ok: false, kind: "unauthorized" }), { data: C, status: "unauthorized" });
});

test("successful fetch is fresh", () => {
  assert.deepEqual(nextView(C, { ok: true, data: D }), { data: D, status: "fresh" });
});

test("fetch not finished shows cached", () => {
  assert.deepEqual(nextView(C, undefined), { data: C, status: "cached" });
});

test("no cache and fetch not finished is empty", () => {
  assert.deepEqual(nextView(null, undefined), { data: null, status: "empty" });
});

test("no cache and rejected token is unauthorized with no data", () => {
  assert.deepEqual(nextView(null, { ok: false, kind: "unauthorized" }), { data: null, status: "unauthorized" });
});

test("no cache and successful fetch is fresh", () => {
  assert.deepEqual(nextView(null, { ok: true, data: D }), { data: D, status: "fresh" });
});

test("isValidData accepts the layout document and rejects corrupt values", () => {
  assert.equal(isValidData(D), true);
  assert.equal(isValidData(C), true);
  assert.equal(isValidData(null), false);
  assert.equal(isValidData(undefined), false);
  assert.equal(isValidData("x"), false);
  assert.equal(isValidData([]), false);
  assert.equal(isValidData({ ...D, version: "1" }), false);
  assert.equal(isValidData({ ...D, version: undefined }), false);
  assert.equal(isValidData({ ...D, groups: "nope" }), false);
  assert.equal(isValidData({ ...D, groups: null }), false);
  assert.equal(isValidData({ ...D, bookmarks: {} }), false);
  assert.equal(isValidData({ ...D, bookmarks: undefined }), false);
});

test("classifyResponse maps status and body", () => {
  assert.deepEqual(classifyResponse(200, D), { ok: true, data: D });
  assert.deepEqual(classifyResponse(401, { error: "unauthorized" }), { ok: false, kind: "unauthorized" });
  assert.deepEqual(classifyResponse(403, { error: "forbidden" }), { ok: false, kind: "unauthorized" });
  assert.deepEqual(classifyResponse(500, null), { ok: false, kind: "network" });
  assert.deepEqual(classifyResponse(200, { ...D, groups: "nope" }), { ok: false, kind: "network" });
  assert.deepEqual(classifyResponse(200, null), { ok: false, kind: "network" });
});

const stubResponse = (status, body) => ({ status, json: async () => body });

test("startUrl joins the server and the endpoint path once", () => {
  assert.equal(startUrl("http://h:1/"), "http://h:1/api/v1/me/start");
  assert.equal(startUrl("https://h/magpie//"), "https://h/magpie/api/v1/me/start");
});

test("fetchData returns data on 200 and sends the token in the header only, plus a signal", async () => {
  let seen;
  const stub = async (url, opts) => {
    seen = { url, opts };
    return stubResponse(200, D);
  };
  assert.deepEqual(await fetchData(stub, "http://h:1/", "tok-secret"), { ok: true, data: D });
  assert.equal(seen.url, "http://h:1/api/v1/me/start");
  assert.equal(seen.opts.headers.Authorization, "Bearer tok-secret");
  assert.ok(!seen.url.includes("tok-secret"));
  assert.ok(seen.opts.signal instanceof AbortSignal);
});

test("fetchData maps 401, 403, 500, wrong shape and bad JSON", async () => {
  assert.deepEqual(await fetchData(async () => stubResponse(401, {}), "http://h", "t"), { ok: false, kind: "unauthorized" });
  assert.deepEqual(await fetchData(async () => stubResponse(403, {}), "http://h", "t"), { ok: false, kind: "unauthorized" });
  assert.deepEqual(await fetchData(async () => stubResponse(500, null), "http://h", "t"), { ok: false, kind: "network" });
  assert.deepEqual(
    await fetchData(async () => stubResponse(200, { ...D, groups: "x" }), "http://h", "t"),
    { ok: false, kind: "network" },
  );
  const badJson = async () => ({ status: 200, json: async () => { throw new SyntaxError("bad"); } });
  assert.deepEqual(await fetchData(badJson, "http://h", "t"), { ok: false, kind: "network" });
});

test("fetchData maps a network error to network", async () => {
  const stub = async () => { throw new TypeError("NetworkError"); };
  assert.deepEqual(await fetchData(stub, "http://h", "t"), { ok: false, kind: "network" });
});

test("fetchData maps a hanging server to network once the timeout fires", async () => {
  // AbortSignal.timeout timers are unref'd in Node, so hold the event loop open until the abort lands.
  const hang = (url, { signal }) =>
    new Promise((_, reject) => {
      const keepAlive = setTimeout(() => {}, 5000);
      signal.addEventListener("abort", () => {
        clearTimeout(keepAlive);
        reject(signal.reason);
      });
    });
  const started = Date.now();
  assert.deepEqual(await fetchData(hang, "http://h", "t", 30), { ok: false, kind: "network" });
  assert.ok(Date.now() - started < 2000);
});

test("fetchData with missing url or token does not fetch", async () => {
  let called = false;
  const stub = async () => { called = true; return stubResponse(200, D); };
  assert.deepEqual(await fetchData(stub, "", "t"), { ok: false, kind: "network" });
  assert.deepEqual(await fetchData(stub, "http://h", ""), { ok: false, kind: "network" });
  assert.deepEqual(await fetchData(stub, undefined, undefined), { ok: false, kind: "network" });
  assert.equal(called, false);
});

test("the token never appears in a result, even when the fetch error carries it", async () => {
  const token = "mm_SECRET_TOKEN_123";
  const leaky = async (url, opts) => {
    throw new TypeError(`request to ${url} failed: ${opts.headers.Authorization}`);
  };
  const result = await fetchData(leaky, "http://h", token);
  assert.equal(JSON.stringify(result).includes(token), false);
  const badJson = async () => ({ status: 200, json: async () => { throw new SyntaxError(`bad ${token}`); } });
  assert.equal(JSON.stringify(await fetchData(badJson, "http://h", token)).includes(token), false);
});

test("the token never reaches the console", async () => {
  const token = "mm_SECRET_TOKEN_456";
  const logged = [];
  const originals = {};
  for (const level of ["log", "info", "warn", "error", "debug"]) {
    originals[level] = console[level];
    console[level] = (...args) => logged.push(args.map(String).join(" "));
  }
  try {
    await fetchData(async () => { throw new Error(token); }, "http://h", token);
    await fetchData(async () => stubResponse(401, {}), "http://h", token);
  } finally {
    Object.assign(console, originals);
  }
  assert.equal(logged.join("\n").includes(token), false);
});

test("status texts are the four Norwegian lines and carry no secrets", () => {
  assert.equal(statusText.fresh, "Oppdatert");
  assert.equal(statusText.offline, "Frakoblet – viser sist lagrede");
  assert.equal(statusText.unauthorized, "Ugyldig token – se innstillingene");
  assert.equal(statusText.empty, "Ikke koblet til – åpne innstillingene");
  assert.equal(typeof statusText.cached, "string");
  for (const text of Object.values(statusText)) assert.equal(text.includes("Bearer"), false);
});

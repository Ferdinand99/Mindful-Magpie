import { test } from "node:test";
import assert from "node:assert/strict";
import { classifyResponse, fetchData, isValidData, nextView } from "./state.js";

const C = { generated_at: "2026-01-01T00:00:00Z", counter: 1, items: [] };
const D = { generated_at: "2026-01-01T00:00:05Z", counter: 2, items: [{ label: "a", value: "b" }] };

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

test("isValidData accepts the documented shape and rejects corrupt values", () => {
  assert.equal(isValidData(D), true);
  assert.equal(isValidData(null), false);
  assert.equal(isValidData("x"), false);
  assert.equal(isValidData({ ...D, items: "nope" }), false);
  assert.equal(isValidData({ ...D, counter: "1" }), false);
  assert.equal(isValidData({ ...D, generated_at: 5 }), false);
  assert.equal(isValidData({ ...D, items: [{ label: "a" }] }), false);
  assert.equal(isValidData({ ...D, items: [null] }), false);
});

test("classifyResponse maps status and body", () => {
  assert.deepEqual(classifyResponse(200, D), { ok: true, data: D });
  assert.deepEqual(classifyResponse(401, { error: "unauthorized" }), { ok: false, kind: "unauthorized" });
  assert.deepEqual(classifyResponse(500, null), { ok: false, kind: "network" });
  assert.deepEqual(classifyResponse(200, { ...D, items: "nope" }), { ok: false, kind: "network" });
  assert.deepEqual(classifyResponse(200, null), { ok: false, kind: "network" });
});

const stubResponse = (status, body) => ({ status, json: async () => body });

test("fetchData returns data on 200 and sends token, url and a signal", async () => {
  let seen;
  const stub = async (url, opts) => {
    seen = { url, opts };
    return stubResponse(200, D);
  };
  assert.deepEqual(await fetchData(stub, "http://h:1/", "tok"), { ok: true, data: D });
  assert.equal(seen.url, "http://h:1/api/spike/data");
  assert.equal(seen.opts.headers.Authorization, "Bearer tok");
  assert.ok(seen.opts.signal instanceof AbortSignal);
});

test("fetchData maps 401, 500, wrong shape and bad JSON", async () => {
  assert.deepEqual(await fetchData(async () => stubResponse(401, {}), "http://h", "t"), { ok: false, kind: "unauthorized" });
  assert.deepEqual(await fetchData(async () => stubResponse(500, null), "http://h", "t"), { ok: false, kind: "network" });
  assert.deepEqual(
    await fetchData(async () => stubResponse(200, { ...D, items: "x" }), "http://h", "t"),
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

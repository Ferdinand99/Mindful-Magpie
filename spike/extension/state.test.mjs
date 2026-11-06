import { test } from "node:test";
import assert from "node:assert/strict";
import { nextView } from "./state.js";

const C = { generated_at: "2026-01-01T00:00:00Z", counter: 1, items: [] };
const D = { generated_at: "2026-01-01T00:00:05Z", counter: 2, items: [] };

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

import { test } from "node:test";
import assert from "node:assert/strict";
import { checkConnection, parseServerUrl, saveConnection } from "../extension/options-core.js";

const TOKEN = "mm_SECRET_TOKEN_789";
const GOOD = {
  version: 1,
  background: { kind: "preset", name: "dusk" },
  groups: [],
  bookmarks: [],
};
const response = (status, body) => ({ status, json: async () => body });

function makeEnv({ granted = true, fetchImpl, stored = {} } = {}) {
  const calls = [];
  const store = { ...stored };
  return {
    calls,
    store,
    deps: {
      permissions: {
        request: (arg) => {
          calls.push(["permissions.request", arg]);
          return Promise.resolve(granted);
        },
      },
      storage: {
        local: {
          get: async (keys) => {
            calls.push(["storage.get", keys]);
            return Object.fromEntries([].concat(keys).map((k) => [k, store[k]]));
          },
          set: async (values) => {
            calls.push(["storage.set", Object.keys(values)]);
            Object.assign(store, values);
          },
          remove: async (keys) => {
            calls.push(["storage.remove", keys]);
            for (const k of [].concat(keys)) delete store[k];
          },
        },
      },
      fetchImpl:
        fetchImpl ??
        (async (url, opts) => {
          calls.push(["fetch", url, opts.headers.Authorization]);
          return response(200, GOOD);
        }),
    },
  };
}

test("parseServerUrl accepts http and https and builds a port-less host pattern", () => {
  assert.deepEqual(parseServerUrl("http://10.0.3.152:8099/"), {
    ok: true,
    serverUrl: "http://10.0.3.152:8099",
    pattern: "http://10.0.3.152/*",
  });
  assert.deepEqual(parseServerUrl("  https://magpie.example.org/sub/  "), {
    ok: true,
    serverUrl: "https://magpie.example.org/sub",
    pattern: "https://magpie.example.org/*",
  });
});

test("parseServerUrl refuses javascript:, ftp:, data:, file:, empty and junk", () => {
  for (const bad of [
    "javascript:alert(1)",
    "JaVaScRiPt:alert(1)",
    "ftp://example.org",
    "data:text/html,<script>1</script>",
    "file:///etc/passwd",
    "moz-extension://abc/x",
    "",
    "   ",
    undefined,
    null,
    "not a url",
    "example.org",
    "http://",
    "http://user:pw@example.org",
  ]) {
    assert.equal(parseServerUrl(bad).ok, false, `must refuse ${String(bad)}`);
  }
});

test("checkConnection refuses a bad address without a permission request or a fetch", async () => {
  for (const bad of ["javascript:alert(1)", "ftp://x.example", ""]) {
    const env = makeEnv();
    const result = await checkConnection(env.deps, { serverUrl: bad, token: TOKEN });
    assert.equal(result.ok, false);
    assert.equal(result.kind, "invalid-url");
    assert.deepEqual(env.calls, []);
  }
});

test("checkConnection refuses an empty token", async () => {
  const env = makeEnv();
  const result = await checkConnection(env.deps, { serverUrl: "http://h:1", token: "  " });
  assert.equal(result.ok, false);
  assert.equal(result.kind, "no-token");
  assert.deepEqual(env.calls, []);
});

test("checkConnection requests host permission synchronously, before any other await", () => {
  const env = makeEnv();
  const pending = checkConnection(env.deps, { serverUrl: "http://10.0.3.152:8099", token: TOKEN });
  // Nothing awaited yet: the request must already have been made, inside the user gesture.
  assert.deepEqual(env.calls, [["permissions.request", { origins: ["http://10.0.3.152/*"] }]]);
  return pending;
});

test("saveConnection also requests host permission synchronously", () => {
  const env = makeEnv();
  const pending = saveConnection(env.deps, { serverUrl: "http://10.0.3.152:8099", token: TOKEN });
  assert.deepEqual(env.calls, [["permissions.request", { origins: ["http://10.0.3.152/*"] }]]);
  return pending;
});

test("checkConnection reports success, calls the endpoint with the bearer header and saves nothing", async () => {
  const env = makeEnv();
  const result = await checkConnection(env.deps, { serverUrl: "http://h:1/", token: ` ${TOKEN} ` });
  assert.equal(result.ok, true);
  assert.deepEqual(env.calls.find((c) => c[0] === "fetch"), ["fetch", "http://h:1/api/v1/me/start", `Bearer ${TOKEN}`]);
  assert.equal(env.calls.some((c) => c[0].startsWith("storage")), false);
});

test("checkConnection reports denied permission without fetching", async () => {
  const env = makeEnv({ granted: false });
  const result = await checkConnection(env.deps, { serverUrl: "http://h:1", token: TOKEN });
  assert.equal(result.ok, false);
  assert.equal(result.kind, "denied");
  assert.equal(env.calls.some((c) => c[0] === "fetch"), false);
});

test("checkConnection maps 401 to unauthorized and a failure to network", async () => {
  const unauthorized = makeEnv({ fetchImpl: async () => response(401, {}) });
  assert.equal((await checkConnection(unauthorized.deps, { serverUrl: "http://h", token: TOKEN })).kind, "unauthorized");
  const down = makeEnv({ fetchImpl: async () => { throw new TypeError("NetworkError"); } });
  assert.equal((await checkConnection(down.deps, { serverUrl: "http://h", token: TOKEN })).kind, "network");
});

test("saveConnection saves only when the test succeeds", async () => {
  for (const fetchImpl of [
    async () => response(401, {}),
    async () => response(500, null),
    async () => { throw new TypeError("NetworkError"); },
  ]) {
    const env = makeEnv({ fetchImpl, stored: { serverUrl: "http://old:1", token: "old", last: GOOD } });
    const result = await saveConnection(env.deps, { serverUrl: "http://h:1", token: TOKEN });
    assert.equal(result.ok, false);
    assert.equal(env.calls.some((c) => c[0] === "storage.set" || c[0] === "storage.remove"), false);
    assert.equal(env.store.serverUrl, "http://old:1");
    assert.equal(env.store.token, "old");
  }
});

test("saveConnection saves the normalised url and the trimmed token on success", async () => {
  const env = makeEnv();
  const result = await saveConnection(env.deps, { serverUrl: "http://h:1///", token: ` ${TOKEN} ` });
  assert.equal(result.ok, true);
  assert.equal(env.store.serverUrl, "http://h:1");
  assert.equal(env.store.token, TOKEN);
});

test("saveConnection clears the cache when the server changes, keeps it when it does not", async () => {
  const changed = makeEnv({ stored: { serverUrl: "http://old:1", token: "old", last: GOOD } });
  await saveConnection(changed.deps, { serverUrl: "http://new:1", token: TOKEN });
  assert.equal("last" in changed.store, false);

  const first = makeEnv({ stored: { last: GOOD } });
  await saveConnection(first.deps, { serverUrl: "http://new:1", token: TOKEN });
  assert.equal("last" in first.store, false);

  const same = makeEnv({ stored: { serverUrl: "http://h:1", token: "old", last: GOOD } });
  await saveConnection(same.deps, { serverUrl: "http://h:1/", token: TOKEN });
  assert.deepEqual(same.store.last, GOOD);
  assert.equal(same.store.token, TOKEN);
});

test("saveConnection refuses non-http(s) urls and never touches storage", async () => {
  for (const bad of ["javascript:alert(1)", "ftp://h", ""]) {
    const env = makeEnv();
    const result = await saveConnection(env.deps, { serverUrl: bad, token: TOKEN });
    assert.equal(result.ok, false);
    assert.deepEqual(env.calls, []);
  }
});

test("the token never appears in a result message, whatever fails", async () => {
  const failures = [
    { serverUrl: "javascript:alert(1)" },
    { serverUrl: "http://h", granted: false },
    { serverUrl: "http://h", fetchImpl: async () => response(401, {}) },
    { serverUrl: "http://h", fetchImpl: async () => { throw new TypeError(`leak ${TOKEN}`); } },
    { serverUrl: "http://h", fetchImpl: async () => ({ status: 200, json: async () => { throw new Error(TOKEN); } }) },
    { serverUrl: "http://h" },
  ];
  for (const f of failures) {
    const env = makeEnv({ granted: f.granted ?? true, fetchImpl: f.fetchImpl });
    for (const fn of [checkConnection, saveConnection]) {
      const result = await fn(env.deps, { serverUrl: f.serverUrl, token: TOKEN });
      assert.equal(JSON.stringify(result).includes(TOKEN), false);
    }
  }
});

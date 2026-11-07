import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { parseHTML } from "linkedom";
import {
  challengeS256,
  connectUrl,
  deviceName,
  exchangeCode,
  makeState,
  makeVerifier,
  parseRedirect,
} from "../extension/connect.js";
import { connectMagpie, messages } from "../extension/options-core.js";

const REDIRECT = "https://abc123.extensions.allizom.org/";
const CODE = "ONE_TIME_CODE_xyz";
const TOKEN = "mm_SECRET_NEW_TOKEN";
const GOOD = { version: 1, background: { kind: "preset", name: "dusk" }, groups: [], bookmarks: [] };
const response = (status, body) => ({ status, json: async () => body });

test("makeVerifier gives 43 to 128 URL-safe characters and is random", () => {
  const a = makeVerifier();
  assert.match(a, /^[A-Za-z0-9_-]{43,128}$/);
  assert.notEqual(a, makeVerifier());
});

test("makeState is URL-safe and random", () => {
  assert.match(makeState(), /^[A-Za-z0-9_-]{16,}$/);
  assert.notEqual(makeState(), makeState());
});

test("challengeS256 matches the RFC 7636 appendix B vector", async () => {
  assert.equal(
    await challengeS256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
    "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
  );
});

test("challengeS256 has no padding or non-URL-safe characters", async () => {
  for (let i = 0; i < 20; i += 1) {
    assert.match(await challengeS256(makeVerifier()), /^[A-Za-z0-9_-]{43}$/);
  }
});

test("connectUrl builds the consent url with every value encoded", () => {
  const url = connectUrl("http://10.0.3.152:8099", REDIRECT, "st&te=1", "ch_-", "Firefox på Windows");
  assert.equal(
    url,
    "http://10.0.3.152:8099/connect/extension?redirect_uri=https%3A%2F%2Fabc123.extensions.allizom.org%2F" +
      "&state=st%26te%3D1&code_challenge=ch_-&code_challenge_method=S256&device=Firefox%20p%C3%A5%20Windows",
  );
  const parsed = new URL(url);
  assert.equal(parsed.pathname, "/connect/extension");
  assert.equal(parsed.searchParams.get("redirect_uri"), REDIRECT);
  assert.equal(parsed.searchParams.get("state"), "st&te=1");
  assert.equal(parsed.searchParams.get("device"), "Firefox på Windows");
  assert.equal(parsed.searchParams.get("code_challenge_method"), "S256");
});

test("connectUrl tolerates a trailing slash on the server", () => {
  assert.ok(
    connectUrl("https://m.example/", REDIRECT, "s", "c", "d").startsWith("https://m.example/connect/extension?"),
  );
});

test("parseRedirect returns the code for a good redirect", () => {
  assert.deepEqual(parseRedirect(`${REDIRECT}?code=${CODE}&state=S1`, "S1"), { code: CODE });
});

function thrownCode(fn) {
  try {
    fn();
  } catch (error) {
    assert.equal(error.message, error.code, "message is the fixed code only");
    return error.code;
  }
  return null;
}

test("parseRedirect rejects a wrong or missing state", () => {
  assert.equal(thrownCode(() => parseRedirect(`${REDIRECT}?code=${CODE}&state=OTHER`, "S1")), "state_mismatch");
  assert.equal(thrownCode(() => parseRedirect(`${REDIRECT}?code=${CODE}`, "S1")), "state_mismatch");
  assert.equal(thrownCode(() => parseRedirect(`${REDIRECT}?code=${CODE}&state=`, "S1")), "state_mismatch");
});

test("parseRedirect reports a denied approval", () => {
  assert.equal(thrownCode(() => parseRedirect(`${REDIRECT}?error=access_denied&state=S1`, "S1")), "denied");
  // A forged denial with the wrong state is a mismatch, not a denial.
  assert.equal(thrownCode(() => parseRedirect(`${REDIRECT}?error=access_denied&state=X`, "S1")), "state_mismatch");
});

test("parseRedirect reports a missing code or an unreadable url", () => {
  assert.equal(thrownCode(() => parseRedirect(`${REDIRECT}?state=S1`, "S1")), "no_code");
  assert.equal(thrownCode(() => parseRedirect(`${REDIRECT}?code=&state=S1`, "S1")), "no_code");
  assert.equal(thrownCode(() => parseRedirect("not a url", "S1")), "no_code");
  assert.equal(thrownCode(() => parseRedirect(undefined, "S1")), "no_code");
});

test("parseRedirect errors never contain the code or the url", () => {
  for (const url of [`${REDIRECT}?code=${CODE}&state=OTHER`, `${REDIRECT}?code=${CODE}`]) {
    try {
      parseRedirect(url, "S1");
      assert.fail("must throw");
    } catch (error) {
      assert.equal(String(error).includes(CODE), false);
      assert.equal(JSON.stringify(error).includes(CODE), false);
    }
  }
});

test("exchangeCode posts code and verifier as JSON without cookies or redirects", async () => {
  let seen;
  const result = await exchangeCode(
    async (url, opts) => {
      seen = { url, opts };
      return response(200, { token: TOKEN });
    },
    "http://h:1/",
    CODE,
    "VERIFIER",
  );
  assert.deepEqual(result, { ok: true, token: TOKEN });
  assert.equal(seen.url, "http://h:1/api/v1/extension/token");
  assert.equal(seen.opts.method, "POST");
  assert.equal(seen.opts.credentials, "omit");
  assert.equal(seen.opts.redirect, "error");
  assert.equal(seen.opts.headers["Content-Type"], "application/json");
  assert.deepEqual(JSON.parse(seen.opts.body), { code: CODE, code_verifier: "VERIFIER" });
  assert.ok(seen.opts.signal instanceof AbortSignal);
});

test("exchangeCode maps 400, other statuses, bad bodies and network errors to fixed kinds", async () => {
  const kind = async (fetchImpl) => (await exchangeCode(fetchImpl, "http://h", CODE, "V", 50)).kind;
  assert.equal(await kind(async () => response(400, { error: "invalid_grant" })), "rejected");
  assert.equal(await kind(async () => response(500, null)), "server");
  assert.equal(await kind(async () => response(200, {})), "server");
  assert.equal(await kind(async () => response(200, { token: "" })), "server");
  assert.equal(
    await kind(async () => ({
      status: 200,
      json: async () => {
        throw new Error("x");
      },
    })),
    "server",
  );
  assert.equal(
    await kind(async () => {
      throw new TypeError("NetworkError");
    }),
    "network",
  );
});

test("exchangeCode times out into a network failure when the server never answers", async () => {
  const hanging = (url, opts) =>
    new Promise((resolve, reject) => {
      opts.signal.addEventListener("abort", () => reject(opts.signal.reason));
    });
  // AbortSignal.timeout timers are unref'd; keep the event loop alive while the request hangs.
  const keepAlive = setTimeout(() => {}, 2000);
  try {
    assert.deepEqual(await exchangeCode(hanging, "http://h", CODE, "V", 30), { ok: false, kind: "network" });
  } finally {
    clearTimeout(keepAlive);
  }
});

test("exchangeCode never puts the code, verifier or token in a failure", async () => {
  const secrets = [CODE, "VERIFIER_SECRET", TOKEN];
  for (const fetchImpl of [
    async () => response(400, { error: CODE }),
    async () => response(500, { token: TOKEN }),
    async () => {
      throw new TypeError(`leak ${CODE} ${TOKEN} VERIFIER_SECRET`);
    },
    async () => ({
      status: 200,
      json: async () => {
        throw new Error(TOKEN);
      },
    }),
  ]) {
    const result = await exchangeCode(fetchImpl, "http://h", CODE, "VERIFIER_SECRET");
    assert.equal(result.ok, false);
    for (const secret of secrets) assert.equal(JSON.stringify(result).includes(secret), false);
  }
});

test("deviceName names the browser and the system", () => {
  assert.equal(deviceName({ os: "win" }), "Firefox på Windows");
  assert.equal(deviceName({ os: "mac" }), "Firefox på macOS");
  assert.equal(deviceName({ os: "linux" }), "Firefox på Linux");
  assert.equal(deviceName({ os: "android" }), "Firefox på Android");
  assert.equal(deviceName({ os: "plan9" }), "Firefox");
  assert.equal(deviceName(undefined), "Firefox");
  assert.equal(deviceName(null), "Firefox");
});

// ---- the connect flow of the options page, with stubbed browser.* ----

function makeEnv({ granted = true, flow, exchange, stored = {}, setFails = false } = {}) {
  const calls = [];
  const store = { ...stored };
  const deps = {
    permissions: {
      request: (arg) => {
        calls.push(["permissions.request", arg]);
        return Promise.resolve(granted);
      },
    },
    identity: {
      getRedirectURL: () => REDIRECT,
      launchWebAuthFlow: async (details) => {
        calls.push(["launchWebAuthFlow", details]);
        const state = new URL(details.url).searchParams.get("state");
        return flow ? flow(state, details) : `${REDIRECT}?code=${CODE}&state=${state}`;
      },
    },
    runtime: { getPlatformInfo: async () => ({ os: "win", arch: "x86-64" }) },
    storage: {
      local: {
        get: async (keys) => Object.fromEntries([].concat(keys).map((k) => [k, store[k]])),
        set: async (values) => {
          if (setFails) throw new Error(`disk full ${TOKEN}`);
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
      exchange ??
      (async (url, opts) => {
        calls.push(["fetch", url, JSON.parse(opts.body)]);
        return response(200, { token: TOKEN });
      }),
  };
  return { calls, store, deps };
}

test("connect: success opens the consent page, exchanges the code, saves and clears the cache", async () => {
  const env = makeEnv({ stored: { serverUrl: "http://old:1", token: "old", last: GOOD } });
  const result = await connectMagpie(env.deps, { serverUrl: "http://10.0.3.152:8099/" });
  assert.equal(result.ok, true);
  assert.equal(result.message, "Koblet til ✓");
  assert.equal(env.store.serverUrl, "http://10.0.3.152:8099");
  assert.equal(env.store.token, TOKEN);
  assert.equal("last" in env.store, false);

  const flow = env.calls.find((c) => c[0] === "launchWebAuthFlow")[1];
  assert.equal(flow.interactive, true);
  const consent = new URL(flow.url);
  assert.equal(consent.origin + consent.pathname, "http://10.0.3.152:8099/connect/extension");
  assert.equal(consent.searchParams.get("redirect_uri"), REDIRECT);
  assert.equal(consent.searchParams.get("device"), "Firefox på Windows");
  assert.equal(consent.searchParams.get("code_challenge_method"), "S256");

  // The verifier sent to the token endpoint belongs to the challenge on the consent page.
  const exchange = env.calls.find((c) => c[0] === "fetch");
  assert.equal(exchange[1], "http://10.0.3.152:8099/api/v1/extension/token");
  assert.equal(exchange[2].code, CODE);
  assert.equal(await challengeS256(exchange[2].code_verifier), consent.searchParams.get("code_challenge"));
  assert.equal(JSON.stringify(result).includes(TOKEN), false);
});

test("connect: asks for the host permission synchronously, before any other await", () => {
  const env = makeEnv();
  const pending = connectMagpie(env.deps, { serverUrl: "http://10.0.3.152:8099" });
  assert.deepEqual(env.calls, [["permissions.request", { origins: ["http://10.0.3.152/*"] }]]);
  return pending;
});

test("connect: keeps the cache when neither server nor token changes", async () => {
  const env = makeEnv({ stored: { serverUrl: "http://h:1", token: TOKEN, last: GOOD } });
  await connectMagpie(env.deps, { serverUrl: "http://h:1" });
  assert.deepEqual(env.store.last, GOOD);
});

test("connect: a new token for the same server clears the cache", async () => {
  const env = makeEnv({ stored: { serverUrl: "http://h:1", token: "old", last: GOOD } });
  await connectMagpie(env.deps, { serverUrl: "http://h:1" });
  assert.equal("last" in env.store, false);
  assert.equal(env.store.token, TOKEN);
});

function assertNothingSaved(env, before) {
  assert.equal(env.calls.some((c) => c[0] === "storage.set" || c[0] === "storage.remove"), false);
  assert.deepEqual(env.store, before);
}

test("connect: a cancelled flow saves nothing and never calls the token endpoint", async () => {
  const before = { serverUrl: "http://old:1", token: "old", last: GOOD };
  const env = makeEnv({
    stored: before,
    flow: () => {
      throw new Error("The user did not approve access.");
    },
  });
  const result = await connectMagpie(env.deps, { serverUrl: "http://h:1" });
  assert.equal(result.ok, false);
  assert.equal(result.kind, "cancelled");
  assert.equal(result.message, messages.cancelled);
  assert.equal(env.calls.some((c) => c[0] === "fetch"), false);
  assertNothingSaved(env, before);
});

test("connect: denied approval, state mismatch and missing code save nothing", async () => {
  const cases = [
    [(state) => `${REDIRECT}?error=access_denied&state=${state}`, "declined"],
    [() => `${REDIRECT}?code=${CODE}&state=forged`, "state-mismatch"],
    [(state) => `${REDIRECT}?state=${state}`, "no-code"],
    [() => undefined, "no-code"],
  ];
  for (const [flow, kind] of cases) {
    const before = { serverUrl: "http://old:1", token: "old", last: GOOD };
    const env = makeEnv({ stored: before, flow });
    const result = await connectMagpie(env.deps, { serverUrl: "http://h:1" });
    assert.equal(result.kind, kind);
    assert.equal(env.calls.some((c) => c[0] === "fetch"), false, "code must not be exchanged");
    assertNothingSaved(env, before);
  }
});

test("connect: a refused host permission or a bad address never opens the flow", async () => {
  const denied = makeEnv({ granted: false });
  assert.equal((await connectMagpie(denied.deps, { serverUrl: "http://h:1" })).kind, "denied");
  assert.equal(denied.calls.some((c) => c[0] === "launchWebAuthFlow"), false);

  for (const bad of ["javascript:alert(1)", "ftp://h", "", undefined]) {
    const env = makeEnv();
    assert.equal((await connectMagpie(env.deps, { serverUrl: bad })).kind, "invalid-url");
    assert.deepEqual(env.calls, []);
  }
});

test("connect: an exchange failure saves nothing and shows a fixed message", async () => {
  for (const [exchange, kind] of [
    [async () => response(400, {}), "rejected"],
    [async () => response(500, {}), "server"],
    [
      async () => {
        throw new TypeError(`leak ${CODE}`);
      },
      "network",
    ],
  ]) {
    const before = { serverUrl: "http://old:1", token: "old" };
    const env = makeEnv({ stored: before, exchange });
    const result = await connectMagpie(env.deps, { serverUrl: "http://h:1" });
    assert.equal(result.kind, kind);
    assert.equal(result.message, messages[kind]);
    assert.equal(JSON.stringify(result).includes(CODE), false);
    assertNothingSaved(env, before);
  }
});

test("connect: a storage failure gives a fixed message without the token", async () => {
  const env = makeEnv({ setFails: true });
  const result = await connectMagpie(env.deps, { serverUrl: "http://h:1" });
  assert.equal(result.kind, "storage");
  assert.equal(JSON.stringify(result).includes(TOKEN), false);
});

test("connect: no fixed message contains the code or the token", () => {
  for (const text of Object.values(messages)) {
    assert.equal(text.includes(CODE) || text.includes(TOKEN), false);
  }
});

test("connect: works when getPlatformInfo fails, with a plain device name", async () => {
  const env = makeEnv();
  env.deps.runtime.getPlatformInfo = async () => {
    throw new Error("nope");
  };
  assert.equal((await connectMagpie(env.deps, { serverUrl: "http://h:1" })).ok, true);
  const flow = env.calls.find((c) => c[0] === "launchWebAuthFlow")[1];
  assert.equal(new URL(flow.url).searchParams.get("device"), "Firefox");
});

// ---- the options page itself (options.html + options.js), with linkedom and browser stubs ----

const read = (name) => readFileSync(new URL(`../extension/${name}`, import.meta.url), "utf8");

test("options page: «Koble til Magpie» is the primary button, above the Avansert paste fields", () => {
  const doc = parseHTML(read("options.html")).document;
  const connect = doc.getElementById("connect");
  assert.equal(connect.textContent.trim(), "Koble til Magpie");
  assert.match(connect.getAttribute("class"), /primary/);
  const details = doc.querySelector("details");
  assert.equal(details.querySelector("summary").textContent.trim(), "Avansert");
  assert.ok(details.querySelector("#token"));
  assert.ok(details.querySelector("#test"));
  assert.ok(details.querySelector("#save"));
  assert.equal(details.contains(connect), false);
  const order = [...doc.querySelectorAll("#connect, details")].map((e) => e.localName);
  assert.deepEqual(order, ["button", "details"]);
});

test("options page: clicking «Koble til Magpie» runs the flow, saves and shows «Koblet til ✓»", async () => {
  const doc = parseHTML(read("options.html")).document;
  const env = makeEnv({ stored: { serverUrl: "http://10.0.3.152:8099", token: "old", last: GOOD } });
  const callsAtRequest = [];
  const realRequest = env.deps.permissions.request;
  env.deps.permissions.request = (arg) => {
    callsAtRequest.push(env.calls.length);
    return realRequest(arg);
  };
  const stubs = {
    document: doc,
    fetch: env.deps.fetchImpl,
    browser: {
      permissions: env.deps.permissions,
      storage: env.deps.storage,
      identity: env.deps.identity,
      runtime: env.deps.runtime,
    },
  };
  const previous = Object.fromEntries(Object.keys(stubs).map((k) => [k, Object.getOwnPropertyDescriptor(globalThis, k)]));
  for (const [k, value] of Object.entries(stubs)) {
    Object.defineProperty(globalThis, k, { value, configurable: true, writable: true });
  }
  try {
    await import("../extension/options.js?connect-ui");
    await new Promise((r) => setTimeout(r, 0)); // let the address field fill from storage
    assert.equal(doc.getElementById("serverUrl").value, "http://10.0.3.152:8099");
    const button = doc.getElementById("connect");
    const click = doc.createEvent("Event");
    click.initEvent("click", true, true);
    button.dispatchEvent(click);
    // The permission request was already made, synchronously, inside the click.
    assert.deepEqual(callsAtRequest, [0]);
    assert.equal(button.disabled, true);
    const result = doc.getElementById("result");
    for (let i = 0; i < 100 && !result.textContent.includes("✓"); i += 1) {
      await new Promise((r) => setTimeout(r, 5));
    }
    assert.equal(result.textContent, "Koblet til ✓");
    assert.match(result.getAttribute("class"), /ok/);
    assert.equal(env.store.token, TOKEN);
    assert.equal("last" in env.store, false);
    await new Promise((r) => setTimeout(r, 5));
    assert.equal(doc.getElementById("token").value, TOKEN);
    assert.equal(button.disabled, false);
  } finally {
    for (const [k, descriptor] of Object.entries(previous)) {
      if (descriptor) Object.defineProperty(globalThis, k, descriptor);
      else delete globalThis[k];
    }
  }
});

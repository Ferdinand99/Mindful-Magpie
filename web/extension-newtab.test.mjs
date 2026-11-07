import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { parseHTML } from "linkedom";
import { renderPlaceholder, renderStatus } from "../extension/placeholder.js";
import { statusText } from "../extension/state.js";
import { start } from "../extension/newtab-core.js";

const newDoc = () =>
  parseHTML('<!DOCTYPE html><html><body><p id="status"></p><div id="body"></div></body></html>').document;

const layout = (groups) => ({ version: 1, background: { kind: "preset", name: "dusk" }, groups, bookmarks: [] });
const card = (title) => ({ id: title, title, description: "", url: "https://x.example", icon: { colour: "mint" } });
const read = (name) => readFileSync(new URL(`../extension/${name}`, import.meta.url), "utf8");

test("placeholder lists group titles and card titles", () => {
  const doc = newDoc();
  const root = doc.getElementById("body");
  renderPlaceholder(root, layout([
    { id: "g1", title: "Work", cards: [card("Wiki"), card("Mail")] },
    { id: "g2", title: "Home", cards: [card("Router")] },
  ]));
  assert.deepEqual([...root.querySelectorAll("h2")].map((e) => e.textContent), ["Work", "Home"]);
  assert.deepEqual([...root.querySelectorAll("li")].map((e) => e.textContent), ["Wiki", "Mail", "Router"]);
});

test("placeholder renders titles as text only", () => {
  const doc = newDoc();
  const root = doc.getElementById("body");
  const evil = '<script>window.pwned=1</script><img src=x onerror="pwned=2">';
  renderPlaceholder(root, layout([{ id: "g", title: evil, cards: [card(evil)] }]));
  assert.equal(root.querySelectorAll("script, img").length, 0);
  assert.equal(root.querySelector("h2").textContent, evil);
  assert.equal(root.querySelector("li").textContent, evil);
  assert.equal(root.querySelector("a"), null);
});

test("placeholder with no data or no groups renders nothing and replaces earlier content", () => {
  const doc = newDoc();
  const root = doc.getElementById("body");
  renderPlaceholder(root, layout([{ id: "g", title: "Old", cards: [] }]));
  renderPlaceholder(root, null);
  assert.equal(root.childNodes.length, 0);
  renderPlaceholder(root, layout([]));
  assert.equal(root.childNodes.length, 0);
});

test("status line shows the Norwegian text for each status", () => {
  const doc = newDoc();
  const el = doc.getElementById("status");
  for (const status of ["fresh", "cached", "offline", "unauthorized"]) {
    renderStatus(el, status, () => {});
    assert.equal(el.textContent, statusText[status]);
    assert.equal(el.querySelector("a"), null);
  }
});

test("empty status is a link that opens the options page on click", () => {
  const doc = newDoc();
  const el = doc.getElementById("status");
  let opened = 0;
  renderStatus(el, "empty", () => { opened += 1; });
  const link = el.querySelector("a");
  assert.ok(link);
  assert.equal(link.textContent, statusText.empty);
  let prevented = false;
  const event = doc.createEvent("Event");
  event.initEvent("click", true, true);
  const original = event.preventDefault.bind(event);
  event.preventDefault = () => { prevented = true; original(); };
  link.dispatchEvent(event);
  assert.equal(opened, 1);
  assert.equal(prevented, true);
});

test("new-tab page keeps address-bar focus: no inputs, no autofocus, no inline script or style, no focus()", () => {
  const doc = parseHTML(read("newtab.html")).document;
  assert.equal(doc.querySelectorAll("input, textarea, select").length, 0);
  assert.equal(doc.querySelectorAll("[autofocus]").length, 0);
  assert.equal(doc.querySelectorAll("style, [style], [onclick], [onload]").length, 0);
  assert.ok(doc.querySelectorAll("script").length > 0);
  for (const s of doc.querySelectorAll("script")) {
    assert.ok(s.getAttribute("src"), "script must be external");
  }
  for (const file of ["newtab.js", "newtab-core.js", "placeholder.js"]) {
    assert.equal(/\.focus\s*\(/.test(read(file)), false, `${file} must not call focus()`);
  }
});

test("manifest meets the shell requirements", () => {
  const m = JSON.parse(read("manifest.json"));
  assert.equal(m.manifest_version, 3);
  assert.equal(m.name, "Mindful Magpie");
  assert.equal(m.version, "0.2.0");
  assert.equal(m.browser_specific_settings.gecko.id, "mindful-magpie@hedgerow.tools");
  assert.equal(m.browser_specific_settings.gecko.strict_min_version, "128.0");
  assert.equal(m.chrome_url_overrides.newtab, "newtab.html");
  assert.deepEqual(m.options_ui, { page: "options.html", open_in_tab: true });
  assert.deepEqual(m.permissions, ["storage", "identity"]);
  assert.deepEqual(m.optional_host_permissions, ["*://*/*"]);
  assert.equal(m.content_security_policy.extension_pages, "script-src 'self'; object-src 'self';");
  assert.equal(m.content_security_policy.extension_pages.includes("upgrade-insecure-requests"), false);
  assert.deepEqual(Object.keys(m.icons).sort(), ["128", "48", "96"]);
  assert.equal(m.host_permissions, undefined);
});

test("options page: password token field, no inline script or style", () => {
  const doc = parseHTML(read("options.html")).document;
  assert.equal(doc.getElementById("token").getAttribute("type"), "password");
  assert.equal(doc.getElementById("token").getAttribute("autocomplete"), "off");
  assert.equal(doc.querySelectorAll("style, [style], [onclick]").length, 0);
  assert.ok(doc.querySelectorAll("script").length > 0);
  for (const s of doc.querySelectorAll("script")) assert.ok(s.getAttribute("src"));
});

// ---- start(): the start-up sequence of the new-tab page ----

function startEnv({ stored = {}, getFails = false, setFails = false, fetchImpl } = {}) {
  const store = { ...stored };
  const draws = [];
  const sets = [];
  return {
    store,
    draws,
    sets,
    deps: {
      storage: {
        local: {
          get: async () => {
            if (getFails) throw new Error("storage unavailable");
            return { ...store };
          },
          set: async (values) => {
            if (setFails) throw new Error("quota");
            sets.push(values);
            Object.assign(store, values);
          },
        },
      },
      fetchImpl: fetchImpl ?? (async () => ({ status: 200, json: async () => layout([]) })),
      draw: (view) => draws.push(view),
    },
  };
}

test("start draws the cache first, then the fresh data, and stores it", async () => {
  const cached = layout([{ id: "g", title: "Old", cards: [] }]);
  const env = startEnv({ stored: { last: cached, serverUrl: "http://h:1", token: "t" } });
  await start(env.deps);
  assert.deepEqual(env.draws.map((v) => v.status), ["cached", "fresh"]);
  assert.deepEqual(env.draws[0].data, cached);
  assert.deepEqual(env.sets, [{ last: layout([]) }]);
});

test("start shows the not-connected state when nothing is saved", async () => {
  const env = startEnv();
  await start(env.deps);
  assert.deepEqual(env.draws.map((v) => v.status), ["empty", "empty"]);
});

test("start shows the not-connected state, not a blank page, when reading storage fails", async () => {
  let fetched = false;
  const env = startEnv({
    getFails: true,
    fetchImpl: async () => {
      fetched = true;
      return { status: 200, json: async () => layout([]) };
    },
  });
  await start(env.deps);
  assert.ok(env.draws.length >= 1);
  assert.equal(env.draws.at(-1).status, "empty");
  assert.equal(fetched, false);
});

test("start shows the offline state with the cache when the server is down", async () => {
  const cached = layout([{ id: "g", title: "Old", cards: [] }]);
  const env = startEnv({
    stored: { last: cached, serverUrl: "http://h:1", token: "t" },
    fetchImpl: async () => {
      throw new TypeError("NetworkError");
    },
  });
  await start(env.deps);
  assert.equal(env.draws.at(-1).status, "offline");
  assert.deepEqual(env.draws.at(-1).data, cached);
});

test("start keeps the fresh page when writing the cache fails", async () => {
  const env = startEnv({ stored: { serverUrl: "http://h:1", token: "t" }, setFails: true });
  await start(env.deps);
  assert.equal(env.draws.at(-1).status, "fresh");
});

test("start ignores a cache with the wrong shape", async () => {
  const env = startEnv({ stored: { last: { nope: true } } });
  await start(env.deps);
  assert.equal(env.draws[0].status, "empty");
});

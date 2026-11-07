import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { parseHTML } from "linkedom";
import { renderPlaceholder, renderStatus } from "../extension/placeholder.js";
import { statusText } from "../extension/state.js";

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
  for (const file of ["newtab.js", "placeholder.js"]) {
    assert.equal(/\.focus\s*\(/.test(read(file)), false, `${file} must not call focus()`);
  }
});

test("manifest meets the shell requirements", () => {
  const m = JSON.parse(read("manifest.json"));
  assert.equal(m.manifest_version, 3);
  assert.equal(m.name, "Mindful Magpie");
  assert.equal(m.version, "0.1.0");
  assert.equal(m.browser_specific_settings.gecko.id, "mindful-magpie@hedgerow.tools");
  assert.equal(m.browser_specific_settings.gecko.strict_min_version, "128.0");
  assert.equal(m.chrome_url_overrides.newtab, "newtab.html");
  assert.deepEqual(m.options_ui, { page: "options.html", open_in_tab: true });
  assert.deepEqual(m.permissions, ["storage"]);
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

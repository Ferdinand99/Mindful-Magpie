// Tests for static/magpie/service-form.js: the button that puts a presented
// certificate fingerprint into the form. linkedom stands in for the DOM.
import assert from "node:assert/strict";
import { test } from "node:test";
import { parseHTML } from "linkedom";

import { init, useFingerprint } from "../static/magpie/service-form.js";

const FP = "ab".repeat(32);

function page() {
  const { document } = parseHTML(`<!doctype html><html><body>
    <form><input id="f-cert_sha256" name="cert_sha256" value="old"><input id="f-name" name="name" value="Router"></form>
    <div id="result"><button type="button" id="use" data-use-fingerprint="${FP}" data-target="f-cert_sha256"><span id="label">Use</span></button></div>
  </body></html>`);
  return document;
}

test("the button fills the field with the fingerprint", () => {
  const doc = page();
  assert.equal(useFingerprint(doc, doc.getElementById("use")), true);
  assert.equal(doc.getElementById("f-cert_sha256").value, FP);
  assert.equal(doc.getElementById("f-name").value, "Router", "nothing else changes");
});

test("a click anywhere in the button fills it, through one listener on the page", () => {
  const doc = page();
  init(doc);
  doc.getElementById("label").click();
  assert.equal(doc.getElementById("f-cert_sha256").value, FP);
});

test("a click elsewhere changes nothing", () => {
  const doc = page();
  init(doc);
  doc.getElementById("f-name").click();
  assert.equal(doc.getElementById("f-cert_sha256").value, "old");
});

test("a button without a fingerprint or without its field does nothing", () => {
  const doc = page();
  const button = doc.getElementById("use");
  button.setAttribute("data-target", "missing");
  assert.equal(useFingerprint(doc, button), false);
  button.setAttribute("data-target", "f-cert_sha256");
  button.setAttribute("data-use-fingerprint", "");
  assert.equal(useFingerprint(doc, button), false);
  assert.equal(doc.getElementById("f-cert_sha256").value, "old");
});

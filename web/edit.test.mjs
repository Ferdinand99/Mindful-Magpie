// Tests for static/magpie/edit.js (edit mode) and the wiring in start.js.
// linkedom stands in for the DOM: it has no layout, no focus tracking and no
// <dialog> behaviour, so these tests spy on focus() and read the markup.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { parseHTML } from "linkedom";

import { startEditing } from "../static/magpie/edit.js";
import {
  clearStash,
  draw,
  drawIfNeeded,
  initEditing,
  makeSave,
  readStash,
  reloadIfDataLost,
  stashDraft,
} from "../static/magpie/start.js";

// The real English strings, so that the tests and the locale file cannot drift.
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
const NB = readStartStrings("../locales/nb.toml");

const card = (id, title, colour = "teal") => ({
  id,
  title,
  description: "",
  url: `https://${id}.example/`,
  icon: { emoji: null, colour },
});

function layoutFixture() {
  return {
    version: 3,
    background: { kind: "preset", name: "hedgerow" },
    groups: [
      { id: "g1", title: "Media", cards: [card("c1", "Sonarr"), card("c2", "Radarr")] },
      { id: "g2", title: "Tools", cards: [card("c3", "Grafana", "orange")] },
    ],
    bookmarks: [{ id: "b1", title: "Hedgerow", url: "https://hedgerow.tools/" }],
  };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

function setup({ layout = layoutFixture(), saveResult } = {}) {
  const { document, window } = parseHTML(
    `<!doctype html><html lang="en"><body><main>
       <div class="start-actions"><button type="button" id="mm-edit" aria-pressed="false">Edit</button></div>
       <section id="groups" class="mm-root" data-user="7"></section>
       <script type="application/json" id="mm-layout">${JSON.stringify(layout)}</script>
       <script type="application/json" id="mm-labels">${JSON.stringify(LABELS)}</script>
     </main></body></html>`,
  );
  const focus = { last: null, count: 0 };
  window.Element.prototype.focus = function () {
    focus.last = this;
    focus.count++;
  };
  const root = document.getElementById("groups");
  const saved = [];
  const reloads = [];
  const save = async (next) => {
    saved.push(structuredClone(next));
    const result = typeof saveResult === "function" ? saveResult(next, saved.length) : saveResult;
    if (result instanceof Error) throw result;
    return result ?? { status: 200, body: { version: next.version + 1 } };
  };
  const current = { layout };
  const start = () =>
    startEditing(root, () => current.layout, save, {
      labels: LABELS,
      lang: "en",
      editButton: document.getElementById("mm-edit"),
      reload: () => reloads.push(1),
    });
  return { document, window, root, focus, saved, reloads, start, current };
}

// ---- tiny DOM helpers ----------------------------------------------------

const click = (el) => el.dispatchEvent(new el.ownerDocument.defaultView.Event("click", { bubbles: true, cancelable: true }));
const item = (root, kind, id) => root.querySelector(`[data-kind="${kind}"][data-id="${id}"]`);
const act = (el, name) => el.querySelector(`[data-act="${name}"]`);
const sheet = (document) => document.querySelector("dialog.sheet[open]");
const sheets = (document) => [...document.querySelectorAll("dialog.sheet[open]")];
const bar = (document) => document.querySelector(".mm-edit-bar");
const alertText = (document) => document.querySelector(".mm-edit-alert")?.textContent ?? "";

function fill(dialog, values) {
  for (const [name, value] of Object.entries(values)) {
    if (name === "colour") {
      for (const radio of dialog.querySelectorAll('input[name="colour"]')) radio.checked = radio.value === value;
    } else {
      dialog.querySelector(`[name="${name}"]`).value = value;
    }
  }
}

function submit(dialog) {
  const form = dialog.querySelector("form");
  form.dispatchEvent(new dialog.ownerDocument.defaultView.Event("submit", { bubbles: true, cancelable: true }));
}

function press(el, key) {
  const event = new el.ownerDocument.defaultView.Event("keydown", { bubbles: true, cancelable: true });
  event.key = key;
  el.dispatchEvent(event);
  return event;
}

const cardIds = (layout, groupId) => layout.groups.find((g) => g.id === groupId).cards.map((c) => c.id);

// ---- tests -----------------------------------------------------------------

test("entering_edit_mode_adds_the_controls", () => {
  const t = setup();
  const ed = t.start();
  assert.ok(t.root.hasAttribute("data-editing"));
  assert.equal(t.document.getElementById("mm-edit").getAttribute("aria-pressed"), "true");

  for (const id of ["g1", "g2"]) {
    const group = item(t.root, "group", id);
    for (const name of ["rename", "move", "delete", "add-card"]) {
      const control = act(group, name);
      assert.ok(control, `${id} ${name}`);
      assert.equal(control.tagName, "BUTTON");
      assert.equal(control.getAttribute("type"), "button");
      assert.ok(control.getAttribute("aria-label") || control.textContent.trim(), `${id} ${name} has a name`);
    }
  }
  for (const id of ["c1", "c2", "c3"]) {
    const el = item(t.root, "card", id);
    for (const name of ["edit", "move", "delete"]) assert.equal(act(el, name)?.tagName, "BUTTON", `${id} ${name}`);
    assert.equal(el.getAttribute("draggable"), "true");
    // Edit mode never follows a link.
    assert.equal(el.querySelector("a[href]"), null);
  }
  const pill = item(t.root, "bookmark", "b1");
  for (const name of ["edit", "move", "delete"]) assert.equal(act(pill, name)?.tagName, "BUTTON", name);

  const toolbar = bar(t.document);
  for (const name of ["add-group", "add-bookmark", "done", "cancel"]) assert.equal(act(toolbar, name)?.tagName, "BUTTON", name);
  // Rendering still comes from render.js: the tiles and titles are there.
  assert.equal(item(t.root, "card", "c1").querySelector(".mm-card-title").textContent, "Sonarr");
  assert.equal(ed.isDirty(), false);
});

test("the_page_never_calls_focus_before_edit_mode_is_opened", () => {
  const t = setup();
  draw(t.document);
  drawIfNeeded(t.document);
  assert.equal(t.focus.count, 0);
  assert.equal(t.root.querySelectorAll("[data-act]").length, 0);

  t.start();
  assert.ok(t.focus.count > 0, "opening edit mode may move focus");
});

test("start_js_does_not_redraw_over_edit_mode", () => {
  const t = setup();
  draw(t.document);
  t.start();
  act(item(t.root, "card", "c1"), "delete").dispatchEvent(new t.window.Event("click", { bubbles: true }));
  const before = t.root.innerHTML;
  assert.ok(before.includes('data-act="edit"'));
  t.document.documentElement.removeAttribute("data-bg");
  drawIfNeeded(t.document);
  assert.equal(t.root.innerHTML, before);
});

test("the_move_menu_moves_a_card_and_focus_returns_to_that_card", async () => {
  const t = setup();
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "move"));
  const menu = sheet(t.document);
  assert.ok(menu, "the move menu is a sheet");
  const entries = [...menu.querySelectorAll("button[data-move]")].map((b) => b.getAttribute("data-move"));
  assert.deepEqual(entries, ["down", "group:g2"]);

  click(menu.querySelector('[data-move="group:g2"]'));
  await tick();
  assert.equal(sheet(t.document), null, "the sheet closed");
  assert.deepEqual(cardIds(ed.draft(), "g1"), ["c2"]);
  assert.deepEqual(cardIds(ed.draft(), "g2"), ["c3", "c1"]);
  // Drawn in the new group, and focus is on that card's control.
  assert.ok(item(item(t.root, "group", "g2"), "card", "c1"));
  assert.equal(t.focus.last.closest('[data-kind="card"]').getAttribute("data-id"), "c1");
  assert.equal(t.focus.last.getAttribute("data-act"), "move");
  assert.equal(ed.isDirty(), true);

  // Up and down inside the group.
  click(act(item(t.root, "card", "c1"), "move"));
  const again = sheet(t.document);
  const next = [...again.querySelectorAll("button[data-move]")].map((b) => b.getAttribute("data-move"));
  assert.deepEqual(next, ["up", "group:g1"]);
  click(again.querySelector('[data-move="up"]'));
  await tick();
  assert.deepEqual(cardIds(ed.draft(), "g2"), ["c1", "c3"]);
});

test("a_group_and_a_bookmark_can_be_moved_from_the_keyboard_too", async () => {
  const t = setup();
  const ed = t.start();
  click(act(item(t.root, "group", "g1"), "move"));
  let menu = sheet(t.document);
  assert.deepEqual([...menu.querySelectorAll("button[data-move]")].map((b) => b.getAttribute("data-move")), ["down", "last"]);
  click(menu.querySelector('[data-move="last"]'));
  await tick();
  assert.deepEqual(ed.draft().groups.map((g) => g.id), ["g2", "g1"]);
  assert.equal(t.focus.last.closest('[data-kind="group"]').getAttribute("data-id"), "g1");

  // One bookmark: nothing to move to, so the control is not offered.
  assert.equal(act(item(t.root, "bookmark", "b1"), "move").hasAttribute("disabled"), true);
});

test("a_keyboard_only_flow_adds_a_group_and_a_card_moves_it_and_saves", async () => {
  const t = setup();
  const ed = t.start();

  click(act(bar(t.document), "add-group"));
  let dialog = sheet(t.document);
  assert.ok(dialog);
  fill(dialog, { title: "Games" });
  submit(dialog);
  await tick();
  assert.equal(sheet(t.document), null);
  const games = ed.draft().groups[2];
  assert.equal(games.title, "Games");
  // Focus lands on the new group's "add card".
  assert.equal(t.focus.last.getAttribute("data-act"), "add-card");
  assert.equal(t.focus.last.closest('[data-kind="group"]').getAttribute("data-id"), games.id);

  click(t.focus.last);
  dialog = sheet(t.document);
  fill(dialog, { title: "Steam", url: "store.steampowered.com", description: "", emoji: "", colour: "indigo" });
  submit(dialog);
  await tick();
  const added = ed.draft().groups[2].cards[0];
  assert.equal(added.url, "https://store.steampowered.com");
  assert.deepEqual(added.icon, { emoji: null, colour: "indigo" });
  assert.equal(t.focus.last.closest('[data-kind="card"]').getAttribute("data-id"), added.id);

  click(act(item(t.root, "card", added.id), "move"));
  click(sheet(t.document).querySelector('[data-move="group:g1"]'));
  await tick();

  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(t.saved.length, 1);
  const sent = t.saved[0];
  assert.equal(sent.version, 3);
  assert.deepEqual(sent.groups.map((g) => g.title), ["Media", "Tools", "Games"]);
  assert.deepEqual(sent.groups[0].cards.map((c) => c.title), ["Sonarr", "Radarr", "Steam"]);
  assert.deepEqual(sent.groups[2].cards, []);
  // Edit mode is over.
  assert.equal(t.root.hasAttribute("data-editing"), false);
  assert.equal(t.document.getElementById("mm-edit").getAttribute("aria-pressed"), "false");
  assert.equal(bar(t.document), null);
  assert.equal(t.root.querySelectorAll("[data-act]").length, 0);
  assert.equal(t.focus.last, t.document.getElementById("mm-edit"));
});

test("done_without_changes_leaves_without_saving", async () => {
  const t = setup();
  t.start();
  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(t.saved.length, 0);
  assert.equal(t.root.hasAttribute("data-editing"), false);
});

test("a_409_shows_the_reload_message_and_keeps_the_draft", async () => {
  const t = setup({ saveResult: { status: 409, body: { error: "conflict", current: 9 } } });
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "delete"));
  click(act(bar(t.document), "done"));
  await tick();
  assert.match(alertText(t.document), /The page was changed in another tab/);
  assert.ok(t.root.hasAttribute("data-editing"), "still editing");
  assert.equal(ed.isDirty(), true);
  const reload = act(t.document.querySelector(".mm-edit-alert"), "reload");
  assert.equal(reload.textContent.trim(), "Reload");
  click(reload);
  assert.equal(t.reloads.length, 1);
});

test("a_network_error_keeps_edit_mode_open_and_a_retry_works", async () => {
  const t = setup({ saveResult: (next, n) => (n === 1 ? new TypeError("Failed to fetch") : undefined) });
  const ed = t.start();
  click(act(item(t.root, "bookmark", "b1"), "delete"));
  click(act(bar(t.document), "done"));
  await tick();
  assert.match(alertText(t.document), /Could not save – try again/);
  assert.ok(t.root.hasAttribute("data-editing"));
  assert.equal(ed.isDirty(), true);

  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(t.saved.length, 2);
  assert.equal(t.root.hasAttribute("data-editing"), false);
});

test("a_422_is_listed_and_opens_the_form_with_the_error_under_the_field", async () => {
  const t = setup({
    saveResult: { status: 422, body: { error: "invalid", fields: [{ path: "groups[0].cards[0].url", code: "bad_url" }] } },
  });
  t.start();
  click(act(item(t.root, "card", "c1"), "delete"));
  click(act(bar(t.document), "done"));
  await tick();
  const summary = t.document.querySelector(".mm-edit-alert");
  assert.match(summary.textContent, /Radarr/);
  const entry = summary.querySelector("button[data-path]");
  assert.ok(entry);
  click(entry);
  const dialog = sheet(t.document);
  assert.ok(dialog, "the card's form is open");
  assert.equal(dialog.querySelector('[name="title"]').value, "Radarr");
  const message = dialog.querySelector('.field-error[data-for="url"]');
  assert.match(message.textContent, /full link/);
  assert.equal(dialog.querySelector('[name="url"]').getAttribute("aria-invalid"), "true");
});

test("a_422_that_names_no_form_shows_a_summary", async () => {
  const t = setup({ saveResult: { status: 422, body: { error: "invalid", fields: [{ path: "groups", code: "too_many" }] } } });
  t.start();
  click(act(item(t.root, "bookmark", "b1"), "delete"));
  click(act(bar(t.document), "done"));
  await tick();
  assert.match(alertText(t.document), /could not be saved/i);
  assert.equal(sheet(t.document), null);
});

test("the_card_form_checks_the_fields_and_marks_them", async () => {
  const t = setup();
  const ed = t.start();
  click(act(item(t.root, "group", "g2"), "add-card"));
  const dialog = sheet(t.document);
  fill(dialog, { title: "   ", url: "javascript:alert(1)", emoji: "ab", description: "d".repeat(161) });
  submit(dialog);
  await tick();
  assert.equal(sheet(t.document), dialog, "the form stays open");
  const errors = Object.fromEntries([...dialog.querySelectorAll(".field-error[data-for]")].map((e) => [e.getAttribute("data-for"), e.textContent]));
  assert.match(errors.title, /Fill this in/);
  assert.match(errors.url, /http/);
  assert.match(errors.emoji, /one emoji/i);
  assert.match(errors.description, /160/);
  for (const name of ["title", "url", "emoji", "description"]) {
    assert.equal(dialog.querySelector(`[name="${name}"]`).getAttribute("aria-invalid"), "true", name);
  }
  assert.equal(ed.isDirty(), false, "nothing was added");
  // Focus goes to the first field with a problem.
  assert.equal(t.focus.last.getAttribute("name"), "title");
});

test("an_empty_emoji_field_is_sent_as_null", async () => {
  const t = setup();
  const ed = t.start();
  click(act(item(t.root, "card", "c3"), "edit"));
  const dialog = sheet(t.document);
  assert.equal(dialog.querySelector('[name="title"]').value, "Grafana");
  fill(dialog, { title: "Grafana 2", emoji: "" });
  submit(dialog);
  await tick();
  assert.deepEqual(ed.draft().groups[1].cards[0].icon, { emoji: null, colour: "orange" });
  assert.equal(ed.draft().groups[1].cards[0].title, "Grafana 2");

  click(act(item(t.root, "card", "c3"), "edit"));
  fill(sheet(t.document), { emoji: "\u{1f4ca}", colour: "mint" });
  submit(sheet(t.document));
  await tick();
  assert.deepEqual(ed.draft().groups[1].cards[0].icon, { emoji: "\u{1f4ca}", colour: "mint" });
});

test("the_colour_picker_has_thirteen_swatches_with_names", () => {
  const t = setup();
  t.start();
  click(act(item(t.root, "group", "g1"), "add-card"));
  const radios = [...sheet(t.document).querySelectorAll('input[type="radio"][name="colour"]')];
  assert.deepEqual(radios.map((r) => r.value), [
    "red", "orange", "yellow", "green", "mint", "teal", "cyan", "blue", "indigo", "purple", "pink", "brown", "gray",
  ]);
  for (const radio of radios) {
    const label = radio.closest("label");
    assert.ok(label.textContent.trim().length > 0, `${radio.value} has a name`);
  }
  assert.ok(sheet(t.document).querySelector('[role="radiogroup"]'));
});

test("escape_closes_a_sheet_and_focus_returns_to_the_control", async () => {
  const t = setup();
  t.start();
  const edit = act(item(t.root, "card", "c2"), "edit");
  click(edit);
  const dialog = sheet(t.document);
  assert.ok(dialog);
  press(dialog, "Escape");
  await tick();
  assert.equal(sheet(t.document), null);
  assert.equal(t.document.querySelector("dialog"), null, "the sheet is removed");
  assert.equal(t.focus.last.getAttribute("data-act"), "edit");
  assert.equal(t.focus.last.closest('[data-kind="card"]').getAttribute("data-id"), "c2");
});

test("cancel_with_changes_asks_first", async () => {
  const t = setup();
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "delete"));
  assert.equal(ed.isDirty(), true);

  click(act(bar(t.document), "cancel"));
  let confirm = sheet(t.document);
  assert.ok(confirm);
  assert.match(confirm.textContent, /Discard/);
  click(confirm.querySelector('[data-answer="no"]'));
  await tick();
  assert.ok(t.root.hasAttribute("data-editing"), "kept editing");
  assert.equal(ed.isDirty(), true);

  click(act(bar(t.document), "cancel"));
  confirm = sheet(t.document);
  click(confirm.querySelector('[data-answer="yes"]'));
  await tick();
  assert.equal(t.root.hasAttribute("data-editing"), false);
  assert.equal(t.saved.length, 0);
  // The page shows the stored layout again.
  assert.ok(t.root.textContent.includes("Sonarr"));
  assert.equal(t.root.querySelectorAll("[data-act]").length, 0);
});

test("cancel_without_changes_leaves_at_once", async () => {
  const t = setup();
  t.start();
  click(act(bar(t.document), "cancel"));
  await tick();
  assert.equal(sheet(t.document), null);
  assert.equal(t.root.hasAttribute("data-editing"), false);
});

test("deleting_a_group_with_cards_asks_but_a_card_does_not", async () => {
  const t = setup();
  const ed = t.start();
  click(act(item(t.root, "group", "g1"), "delete"));
  const confirm = sheet(t.document);
  assert.ok(confirm);
  assert.match(confirm.textContent, /Media/);
  click(confirm.querySelector('[data-answer="no"]'));
  await tick();
  assert.equal(ed.draft().groups.length, 2);
  click(act(item(t.root, "group", "g1"), "delete"));
  click(sheet(t.document).querySelector('[data-answer="yes"]'));
  await tick();
  assert.deepEqual(ed.draft().groups.map((g) => g.id), ["g2"]);
  assert.equal(t.focus.last.getAttribute("data-act"), "add-group");

  click(act(item(t.root, "card", "c3"), "delete"));
  assert.equal(sheet(t.document), null);
  assert.deepEqual(ed.draft().groups[0].cards, []);
  assert.equal(t.focus.last.getAttribute("data-act"), "add-card");
});

test("a_dragged_card_lands_in_another_group_at_the_pointer", () => {
  const t = setup();
  const ed = t.start();
  const rect = (el, box) => {
    el.getBoundingClientRect = () => ({ left: box[0], top: box[1], right: box[0] + box[2], bottom: box[1] + box[3], width: box[2], height: box[3] });
  };
  const c3 = item(t.root, "card", "c3");
  rect(c3, [0, 200, 260, 60]);
  const event = (type, target, props = {}) => {
    const e = new t.window.Event(type, { bubbles: true, cancelable: true });
    e.dataTransfer = { setData() {}, setDragImage() {}, effectAllowed: "", dropEffect: "" };
    Object.assign(e, props);
    target.dispatchEvent(e);
    return e;
  };
  const c1 = item(t.root, "card", "c1");
  event("dragstart", c1);
  // Over the left half of Grafana: drop before it.
  const over = event("dragover", c3, { clientX: 20, clientY: 220 });
  assert.equal(over.defaultPrevented, true, "a drop is allowed here");
  event("drop", c3, { clientX: 20, clientY: 220 });
  event("dragend", c1);
  assert.deepEqual(cardIds(ed.draft(), "g1"), ["c2"]);
  assert.deepEqual(cardIds(ed.draft(), "g2"), ["c1", "c3"]);
});

test("a_dragged_card_can_be_dropped_on_an_empty_group_and_a_group_can_be_dragged", () => {
  const layout = layoutFixture();
  layout.groups[1].cards = [];
  const t = setup({ layout });
  const ed = t.start();
  const make = (type, target, props = {}) => {
    const e = new t.window.Event(type, { bubbles: true, cancelable: true });
    e.dataTransfer = { setData() {}, setDragImage() {}, effectAllowed: "", dropEffect: "" };
    Object.assign(e, props);
    target.dispatchEvent(e);
    return e;
  };
  const cards = item(t.root, "group", "g2").querySelector(".mm-cards");
  make("dragstart", item(t.root, "card", "c2"));
  make("dragover", cards, { clientX: 5, clientY: 5 });
  make("drop", cards, { clientX: 5, clientY: 5 });
  make("dragend", item(t.root, "card", "c2"));
  assert.deepEqual(cardIds(ed.draft(), "g2"), ["c2"]);

  const g2 = item(t.root, "group", "g2");
  g2.getBoundingClientRect = () => ({ left: 0, top: 100, right: 500, bottom: 200, width: 500, height: 100 });
  make("dragstart", item(t.root, "group", "g1").querySelector(".mm-grip"));
  make("dragover", g2, { clientX: 10, clientY: 190 });
  make("drop", g2, { clientX: 10, clientY: 190 });
  assert.deepEqual(ed.draft().groups.map((g) => g.id), ["g2", "g1"]);
});

test("leaving_the_page_asks_only_when_something_is_unsaved", () => {
  const t = setup();
  const ed = t.start();
  const ask = () => {
    const e = new t.window.Event("beforeunload", { cancelable: true });
    t.window.dispatchEvent(e);
    return e.defaultPrevented;
  };
  assert.equal(ask(), false);
  click(act(item(t.root, "card", "c1"), "delete"));
  assert.equal(ask(), true);
  ed.dispose();
  assert.equal(ask(), false, "the handler is gone");
});

test("a_link_click_with_unsaved_changes_asks_first", async () => {
  const t = setup();
  t.start();
  const link = t.document.createElement("a");
  link.setAttribute("href", "/settings");
  t.document.body.appendChild(link);
  const clean = new t.window.Event("click", { bubbles: true, cancelable: true });
  link.dispatchEvent(clean);
  assert.equal(clean.defaultPrevented, false, "nothing to lose");

  click(act(item(t.root, "card", "c1"), "delete"));
  const e = new t.window.Event("click", { bubbles: true, cancelable: true });
  link.dispatchEvent(e);
  assert.equal(e.defaultPrevented, true);
  const confirm = sheet(t.document);
  assert.ok(confirm, "the confirm sheet is open");
  // "Keep editing" puts focus back on the link that was clicked.
  click(confirm.querySelector('[data-answer="no"]'));
  await tick();
  assert.equal(t.focus.last, link);
  assert.ok(t.root.hasAttribute("data-editing"));
});

test("the_limits_stop_an_add_with_a_message", () => {
  const layout = layoutFixture();
  layout.groups = Array.from({ length: 30 }, (_, i) => ({ id: `g${i}`, title: `G${i}`, cards: [] }));
  const t = setup({ layout });
  t.start();
  click(act(bar(t.document), "add-group"));
  assert.equal(sheet(t.document), null);
  assert.match(alertText(t.document), /at most 30 groups/);
});

test("a_bookmark_is_added_edited_and_deleted", async () => {
  const t = setup();
  const ed = t.start();
  click(act(bar(t.document), "add-bookmark"));
  fill(sheet(t.document), { title: "Docs", url: "docs.example.com" });
  submit(sheet(t.document));
  await tick();
  assert.deepEqual(ed.draft().bookmarks.map((b) => [b.title, b.url]), [
    ["Hedgerow", "https://hedgerow.tools/"],
    ["Docs", "https://docs.example.com"],
  ]);
  const id = ed.draft().bookmarks[1].id;
  click(act(item(t.root, "bookmark", id), "edit"));
  fill(sheet(t.document), { title: "Documentation" });
  submit(sheet(t.document));
  await tick();
  assert.equal(ed.draft().bookmarks[1].title, "Documentation");
  assert.equal(t.focus.last.closest('[data-kind="bookmark"]').getAttribute("data-id"), id);
  click(act(item(t.root, "bookmark", id), "delete"));
  assert.equal(ed.draft().bookmarks.length, 1);
});

test("a_group_is_renamed_in_a_sheet", async () => {
  const t = setup();
  const ed = t.start();
  click(act(item(t.root, "group", "g2"), "rename"));
  const dialog = sheet(t.document);
  assert.equal(dialog.querySelector('[name="title"]').value, "Tools");
  fill(dialog, { title: "  Utilities  " });
  submit(dialog);
  await tick();
  assert.equal(ed.draft().groups[1].title, "Utilities");
  assert.equal(t.focus.last.getAttribute("data-act"), "rename");
});

test("hostile_text_stays_text_in_edit_mode", () => {
  const layout = layoutFixture();
  layout.groups[0].title = "<img src=x onerror=alert(1)>";
  layout.groups[0].cards[0].title = "</script><b>x</b>";
  const t = setup({ layout });
  t.start();
  assert.equal(t.root.querySelector("img"), null);
  assert.equal(t.root.querySelector("b"), null);
  assert.ok(act(item(t.root, "group", "g1"), "rename").getAttribute("aria-label").includes("<img"));
});

test("the_edit_button_opens_and_closes_edit_mode", async () => {
  const t = setup();
  initEditing(t.document, {
    load: async () => ({ startEditing }),
    save: async (next) => ({ status: 200, body: { version: next.version + 1 } }),
    reload: () => {},
  });
  const button = t.document.getElementById("mm-edit");
  click(button);
  await tick();
  assert.ok(t.root.hasAttribute("data-editing"));
  assert.equal(button.getAttribute("aria-pressed"), "true");
  // The button finishes edit mode too (nothing changed, so nothing is saved).
  click(button);
  await tick();
  assert.equal(t.root.hasAttribute("data-editing"), false);
  assert.equal(button.getAttribute("aria-pressed"), "false");
});

test("makeSave_posts_json_with_the_csrf_header_and_keeps_the_embedded_layout_current", async () => {
  const t = setup();
  const calls = [];
  const fetchFn = async (url, init) => {
    calls.push({ url, init });
    return { status: 200, json: async () => ({ version: 4 }) };
  };
  const save = makeSave(t.document, fetchFn);
  const next = { ...layoutFixture(), bookmarks: [] };
  const result = await save(next);
  assert.deepEqual(result, { status: 200, body: { version: 4 } });
  assert.equal(calls[0].url, "/start/layout");
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls[0].init.headers["Content-Type"], "application/json");
  assert.equal(calls[0].init.headers["HX-Request"], "true");
  assert.equal(calls[0].init.credentials, "same-origin");
  assert.deepEqual(JSON.parse(calls[0].init.body), next);
  // The block now holds the saved layout at its new version, and it is safe in HTML.
  const stored = t.document.getElementById("mm-layout").textContent;
  assert.equal(JSON.parse(stored).version, 4);
  assert.equal(JSON.parse(stored).bookmarks.length, 0);

  const failing = makeSave(t.document, async () => ({ status: 409, json: async () => ({ error: "conflict", current: 9 }) }));
  assert.deepEqual(await failing(next), { status: 409, body: { error: "conflict", current: 9 } });
  assert.equal(JSON.parse(t.document.getElementById("mm-layout").textContent).version, 4, "a failed save changes nothing");

  const broken = makeSave(t.document, async () => ({ status: 502, json: async () => { throw new Error("html"); } }));
  assert.deepEqual(await broken(next), { status: 502, body: null });
});

test("the_embedded_layout_block_cannot_end_early", async () => {
  const t = setup();
  const save = makeSave(t.document, async () => ({ status: 200, json: async () => ({ version: 5 }) }));
  const next = layoutFixture();
  next.groups[0].title = "</script><!-- x";
  await save(next);
  const stored = t.document.getElementById("mm-layout").textContent;
  assert.ok(!stored.includes("<"), stored);
  assert.equal(JSON.parse(stored).groups[0].title, "</script><!-- x");
});

test("every_string_edit_mode_uses_is_in_both_locales", () => {
  const source = readFileSync(new URL("../static/magpie/edit.js", import.meta.url), "utf8");
  const used = new Set([...source.matchAll(/\bL\("(\w+)"/g)].map((m) => m[1]));
  for (const colour of ["red", "orange", "yellow", "green", "mint", "teal", "cyan", "blue", "indigo", "purple", "pink", "brown", "gray"]) {
    used.add(`colour_${colour}`);
  }
  assert.ok(used.size > 40, `only ${used.size} keys found`);
  for (const key of used) {
    assert.ok(LABELS[key], `en is missing start.${key}`);
    assert.ok(NB[key], `nb is missing start.${key}`);
  }
  const listed = source.match(/export const LABEL_KEYS = \[([\s\S]*?)\];/);
  assert.ok(listed, "edit.js lists its label keys");
  const keys = new Set([...listed[1].matchAll(/"(\w+)"/g)].map((m) => m[1]));
  for (const key of used) assert.ok(keys.has(key), `LABEL_KEYS is missing ${key}`);
  for (const key of keys) {
    assert.ok(LABELS[key], `en is missing start.${key}`);
    assert.ok(NB[key], `nb is missing start.${key}`);
  }
  // The page's own strings too (the Edit button).
  assert.ok(LABELS.edit && NB.edit);
  assert.deepEqual(Object.keys(NB).sort(), Object.keys(LABELS).sort(), "nb and en have the same keys");
});

test("a_form_submit_with_unsaved_changes_asks_first_and_goes_on_after_yes", async () => {
  const t = setup();
  t.start();
  const form = t.document.createElement("form");
  form.setAttribute("action", "/logout");
  form.setAttribute("method", "post");
  const submitter = t.document.createElement("button");
  submitter.setAttribute("type", "submit");
  form.appendChild(submitter);
  t.document.body.appendChild(form);
  const submits = [];
  form.requestSubmit = (who) => submits.push(who ?? null);
  const fire = () => {
    const event = new t.window.Event("submit", { bubbles: true, cancelable: true });
    event.submitter = submitter;
    form.dispatchEvent(event);
    return event;
  };
  assert.equal(fire().defaultPrevented, false, "nothing to lose");

  click(act(item(t.root, "card", "c1"), "delete"));
  assert.equal(fire().defaultPrevented, true);
  assert.equal(submits.length, 0);
  const confirm = sheet(t.document);
  assert.ok(confirm, "the leave sheet is open");
  assert.match(confirm.textContent, /Leave without saving/);

  // Keep editing: focus goes back to the button that was pressed.
  click(confirm.querySelector('[data-answer="no"]'));
  await tick();
  assert.equal(t.focus.last, submitter);
  assert.equal(submits.length, 0);
  assert.ok(t.root.hasAttribute("data-editing"));

  fire();
  click(sheet(t.document).querySelector('[data-answer="yes"]'));
  await tick();
  assert.deepEqual(submits, [submitter], "the form is submitted again, with its submitter");
  assert.equal(t.root.hasAttribute("data-editing"), false);
  assert.equal(t.root.querySelectorAll("[data-act]").length, 0, "the history gets the plain page");
  assert.equal(fire().defaultPrevented, false, "the guard is gone");
});

test("the_forms_of_the_editor_itself_are_not_guarded", async () => {
  const t = setup();
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "delete"));
  click(act(bar(t.document), "add-bookmark"));
  fill(sheet(t.document), { title: "Docs", url: "docs.example.com" });
  submit(sheet(t.document));
  await tick();
  assert.equal(ed.draft().bookmarks.length, 2);
});

test("input_is_refused_while_a_save_is_under_way", async () => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const t = setup({ saveResult: () => gate });
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "delete"));
  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(t.saved.length, 1);

  // The controls and the drag and drop do nothing now.
  click(act(item(t.root, "card", "c2"), "delete"));
  click(act(item(t.root, "group", "g1"), "rename"));
  click(act(bar(t.document), "add-group"));
  assert.equal(sheet(t.document), null);
  assert.equal(ed.draft().groups[0].cards.length, 1);
  const start = new t.window.Event("dragstart", { bubbles: true, cancelable: true });
  start.dataTransfer = { setData() {}, setDragImage() {}, effectAllowed: "" };
  item(t.root, "card", "c2").dispatchEvent(start);
  assert.equal(start.defaultPrevented, true, "no drag starts");

  release({ status: 200, body: { version: 4 } });
  await tick();
  assert.equal(t.root.hasAttribute("data-editing"), false);
  // What was drawn is what was saved: Radarr (c2) is still there.
  assert.deepEqual(t.saved[0].groups[0].cards.map((c) => c.id), ["c2"]);
  assert.ok(t.root.textContent.includes("Radarr"));
});

test("reload_gives_up_the_page_guards_so_the_browser_does_not_ask_twice", async () => {
  const t = setup({ saveResult: { status: 409, body: { error: "conflict", current: 9 } } });
  t.start();
  click(act(item(t.root, "card", "c1"), "delete"));
  click(act(bar(t.document), "done"));
  await tick();
  const unload = () => {
    const event = new t.window.Event("beforeunload", { cancelable: true });
    t.window.dispatchEvent(event);
    return event.defaultPrevented;
  };
  assert.equal(unload(), true, "the guard is on while the draft is open");
  click(act(t.document.querySelector(".mm-edit-alert"), "reload"));
  assert.equal(t.reloads.length, 1);
  assert.equal(unload(), false, "and off when the page reloads");
  assert.equal(t.root.hasAttribute("data-editing"), false);
});

// ---- a draft kept across Back and Forward ----------------------------------

function fakeStorage() {
  const map = new Map();
  return {
    map,
    getItem: (key) => (map.has(key) ? map.get(key) : null),
    setItem: (key, value) => void map.set(key, String(value)),
    removeItem: (key) => void map.delete(key),
  };
}

const denied = {
  getItem() {
    throw new Error("SecurityError");
  },
  setItem() {
    throw new Error("QuotaExceededError");
  },
  removeItem() {
    throw new Error("SecurityError");
  },
};

function wired(t, storage, save, now) {
  return initEditing(t.document, {
    now,
    load: async () => ({ startEditing }),
    save: save ?? (async (next) => ({ status: 200, body: { version: next.version + 1 } })),
    reload: () => {},
    navigate: () => {},
    storage,
  });
}

const keptDraft = () => {
  const draft = layoutFixture();
  draft.groups[0].cards.shift();
  draft.groups[0].title = "Kept";
  return draft;
};

test("history_save_while_dirty_keeps_the_draft_and_shows_the_plain_page", async () => {
  const t = setup();
  const storage = fakeStorage();
  const editing = wired(t, storage);
  click(t.document.getElementById("mm-edit"));
  await tick();
  click(act(item(t.root, "card", "c1"), "delete"));

  editing.leave();
  assert.equal(t.root.hasAttribute("data-editing"), false, "the history gets the plain page");
  assert.equal(t.root.querySelectorAll("[data-act]").length, 0);
  assert.ok(t.root.textContent.includes("Sonarr"), "the page shows what is stored");
  const kept = readStash(storage, 7);
  assert.deepEqual(kept.groups[0].cards.map((c) => c.id), ["c2"], "the draft is not lost");
  assert.equal(kept.version, 3);
  assert.equal(JSON.parse([...storage.map.values()][0]).version, 3);
});

test("history_save_without_changes_keeps_nothing", async () => {
  const t = setup();
  const storage = fakeStorage();
  const editing = wired(t, storage);
  click(t.document.getElementById("mm-edit"));
  await tick();
  editing.leave();
  assert.equal(t.root.hasAttribute("data-editing"), false);
  assert.equal(storage.map.size, 0);
});

test("the_restore_notice_reopens_edit_mode_on_the_kept_draft", async () => {
  const t = setup();
  const storage = fakeStorage();
  stashDraft(storage, keptDraft(), 7);
  const editing = wired(t, storage);
  editing.offerRestore();
  editing.offerRestore();
  const notice = t.document.getElementById("mm-restore");
  assert.ok(notice);
  assert.equal(t.document.querySelectorAll("#mm-restore").length, 1, "offered once");
  assert.match(notice.textContent, /You have unsaved changes from before/);
  const [restore, forget] = notice.querySelectorAll("button");
  assert.equal(restore.textContent, "Restore");
  assert.equal(forget.textContent, "Discard");
  assert.equal(t.focus.count, 0, "the notice does not take focus");

  click(restore);
  await tick();
  assert.ok(t.root.hasAttribute("data-editing"));
  assert.equal(t.document.getElementById("mm-restore"), null);
  assert.equal(item(t.root, "card", "c1"), null, "the kept draft is on screen");
  assert.ok(item(t.root, "card", "c2"));
  assert.ok(t.root.textContent.includes("Kept"));
  assert.ok(readStash(storage, 7), "kept until it is saved or given up");
});

test("a_restored_draft_saves_with_its_version_and_clears_the_kept_copy", async () => {
  const t = setup();
  const storage = fakeStorage();
  stashDraft(storage, keptDraft(), 7);
  const sent = [];
  const editing = wired(t, storage, async (next) => {
    sent.push(next);
    return { status: 200, body: { version: next.version + 1 } };
  });
  editing.offerRestore();
  click(t.document.querySelector("#mm-restore button"));
  await tick();
  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(sent.length, 1);
  assert.equal(sent[0].version, 3);
  assert.equal(sent[0].groups[0].title, "Kept");
  assert.equal(readStash(storage, 7), null);
  assert.equal(storage.map.size, 0);
});

test("a_restored_draft_over_a_newer_page_takes_the_normal_409_path", async () => {
  const t = setup();
  const storage = fakeStorage();
  stashDraft(storage, { ...keptDraft(), version: 1 }, 7);
  const editing = wired(t, storage, async () => ({ status: 409, body: { error: "conflict", current: 3 } }));
  editing.offerRestore();
  click(t.document.querySelector("#mm-restore button"));
  await tick();
  click(act(bar(t.document), "done"));
  await tick();
  assert.match(alertText(t.document), /changed in another tab/);
  assert.ok(readStash(storage, 7), "a failed save keeps the draft");
  assert.ok(t.root.hasAttribute("data-editing"));
});

test("cancel_and_discard_delete_the_kept_draft", async () => {
  // Cancel in a restored edit session.
  let t = setup();
  let storage = fakeStorage();
  stashDraft(storage, keptDraft(), 7);
  let editing = wired(t, storage);
  editing.offerRestore();
  click(t.document.querySelector("#mm-restore button"));
  await tick();
  click(act(bar(t.document), "cancel"));
  click(sheet(t.document).querySelector('[data-answer="yes"]'));
  await tick();
  assert.equal(storage.map.size, 0);
  assert.equal(t.root.hasAttribute("data-editing"), false);

  // The notice's Discard.
  t = setup();
  storage = fakeStorage();
  stashDraft(storage, keptDraft(), 7);
  editing = wired(t, storage);
  editing.offerRestore();
  click(t.document.querySelectorAll("#mm-restore button")[1]);
  assert.equal(storage.map.size, 0);
  assert.equal(t.document.getElementById("mm-restore"), null);
  assert.equal(t.root.hasAttribute("data-editing"), false);

  // Leaving through a link after "Leave" also gives the changes up.
  t = setup();
  storage = fakeStorage();
  stashDraft(storage, keptDraft(), 7);
  editing = wired(t, storage);
  editing.offerRestore();
  click(t.document.querySelector("#mm-restore button"));
  await tick();
  const link = t.document.createElement("a");
  link.setAttribute("href", "/settings");
  t.document.body.appendChild(link);
  link.dispatchEvent(new t.window.Event("click", { bubbles: true, cancelable: true }));
  click(sheet(t.document).querySelector('[data-answer="yes"]'));
  await tick();
  assert.equal(storage.map.size, 0);
});

test("a_kept_draft_that_is_not_a_layout_is_ignored", () => {
  const t = setup();
  const storage = fakeStorage();
  for (const junk of ["garbage", "null", "{}", JSON.stringify({ version: 1, draft: { groups: [1], bookmarks: [] } }), JSON.stringify({ draft: keptDraft() })]) {
    storage.setItem("mm-unsaved-draft", junk);
    assert.equal(readStash(storage, 7), null, junk);
    wired(t, storage).offerRestore();
    assert.equal(t.document.getElementById("mm-restore"), null, junk);
  }
  clearStash(storage);
  assert.equal(storage.map.size, 0);
});

test("storage_that_throws_does_not_break_the_page", async () => {
  const t = setup();
  const editing = wired(t, denied);
  assert.doesNotThrow(() => editing.offerRestore());
  assert.equal(t.document.getElementById("mm-restore"), null);
  click(t.document.getElementById("mm-edit"));
  await tick();
  click(act(item(t.root, "card", "c1"), "delete"));
  assert.doesNotThrow(() => editing.leave(), "the draft is lost, as before, but nothing breaks");
  assert.equal(t.root.hasAttribute("data-editing"), false);

  // Saving still works, and so does a restored session without storage.
  click(t.document.getElementById("mm-edit"));
  await tick();
  click(act(item(t.root, "card", "c1"), "delete"));
  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(t.root.hasAttribute("data-editing"), false);

  // sessionStorage itself refusing to be read (blocked cookies).
  const blocked = setup();
  Object.defineProperty(blocked.window, "sessionStorage", {
    get() {
      throw new Error("SecurityError");
    },
  });
  const other = initEditing(blocked.document, {
    load: async () => ({ startEditing }),
    save: async (next) => ({ status: 200, body: { version: next.version + 1 } }),
  });
  assert.doesNotThrow(() => other.offerRestore());
  click(blocked.document.getElementById("mm-edit"));
  await tick();
  click(act(item(blocked.root, "card", "c1"), "delete"));
  assert.doesNotThrow(() => other.leave());
  assert.equal(blocked.root.hasAttribute("data-editing"), false);
});

// ---- a start page that htmx swaps in has no data blocks ---------------------

test("a_start_page_swapped_in_without_its_data_blocks_is_reloaded", () => {
  // htmx does not keep script elements: after Back (fetched from the server, as
  // the page is not kept in htmx's history cache) the groups element is there
  // but #mm-layout and #mm-labels are not.
  const t = setup();
  draw(t.document);
  t.document.getElementById("mm-layout").remove();
  t.document.getElementById("mm-labels").remove();
  const storage = fakeStorage();
  const reloads = [];
  const deps = { storage, reload: () => reloads.push(1), now: () => 1_000_000 };
  assert.equal(reloadIfDataLost(t.document, deps), true);
  assert.equal(reloads.length, 1);
  // Still without data right after the reload: left alone, no loop.
  assert.equal(reloadIfDataLost(t.document, { ...deps, now: () => 1_000_000 + 5_000 }), false);
  assert.equal(reloads.length, 1);
  assert.equal(reloadIfDataLost(t.document, { ...deps, now: () => 1_000_000 + 11_000 }), true);
  assert.equal(reloads.length, 2);
  // Once a start page with its data has been seen, the next loss reloads at once.
  assert.equal(reloadIfDataLost(t.document, { ...deps, now: () => 1_000_000 + 12_000 }), false, "guarded again");
  const whole = setup();
  assert.equal(reloadIfDataLost(whole.document, { ...deps, now: () => 1_000_000 + 12_500 }), false, "has its data");
  assert.equal(reloadIfDataLost(t.document, { ...deps, now: () => 1_000_000 + 13_000 }), true, "the mark is gone");
  // Without storage there is no guard, and still no error.
  assert.equal(reloadIfDataLost(t.document, { storage: denied, reload: () => reloads.push(1) }), true);
});

test("a_page_with_its_data_or_without_a_groups_element_is_not_reloaded", () => {
  const t = setup();
  const reloads = [];
  const deps = { storage: fakeStorage(), reload: () => reloads.push(1) };
  assert.equal(reloadIfDataLost(t.document, deps), false, "the data blocks are there");
  t.root.remove();
  t.document.getElementById("mm-layout").remove();
  assert.equal(reloadIfDataLost(t.document, deps), false, "another page");
  assert.equal(reloads.length, 0);
});

test("a_body_for_another_user_never_shows_the_layout_of_the_one_before", async () => {
  // Nothing of the page is remembered in the script: a body without data
  // blocks draws as the empty page, it never takes another user's layout.
  const a = setup();
  draw(a.document);
  assert.ok(a.root.textContent.includes("Sonarr"));

  const b = setup({ layout: { version: 1, background: { kind: "preset", name: "hedgerow" }, groups: [], bookmarks: [] } });
  b.root.setAttribute("data-user", "8");
  b.document.getElementById("mm-layout").remove();
  b.document.getElementById("mm-labels").remove();
  drawIfNeeded(b.document);
  assert.ok(!b.root.textContent.includes("Sonarr"));
  assert.ok(!b.root.textContent.includes("Media"));

  // And the Edit button has nothing to edit without data.
  const editing = wired(b, fakeStorage());
  void editing;
  click(b.document.getElementById("mm-edit"));
  await tick();
  assert.equal(b.root.hasAttribute("data-editing"), false);
});

// ---- Back and Forward --------------------------------------------------------

function popstate(t, state) {
  const event = new t.window.Event("popstate");
  event.state = state;
  t.window.dispatchEvent(event);
}

test("popstate_into_an_htmx_entry_keeps_a_dirty_draft_and_closes_edit_mode", async () => {
  const t = setup();
  const storage = fakeStorage();
  wired(t, storage);
  click(t.document.getElementById("mm-edit"));
  await tick();
  click(act(item(t.root, "card", "c1"), "delete"));
  popstate(t, { htmx: true });
  assert.equal(t.root.hasAttribute("data-editing"), false);
  assert.deepEqual(readStash(storage, 7).groups[0].cards.map((c) => c.id), ["c2"]);
});

test("a_hash_change_keeps_edit_mode_and_keeps_nothing", async () => {
  const t = setup();
  const storage = fakeStorage();
  wired(t, storage);
  click(t.document.getElementById("mm-edit"));
  await tick();
  click(act(item(t.root, "card", "c1"), "delete"));
  popstate(t, null);
  assert.ok(t.root.hasAttribute("data-editing"));
  assert.equal(storage.map.size, 0);
});

test("a_failed_load_of_edit_mode_keeps_the_kept_draft", async () => {
  const t = setup();
  const storage = fakeStorage();
  stashDraft(storage, keptDraft(), 7);
  const errors = [];
  const original = globalThis.console.error;
  globalThis.console.error = (...args) => errors.push(args);
  try {
    const editing = initEditing(t.document, {
      load: async () => {
        throw new TypeError("Failed to fetch dynamically imported module");
      },
      save: async () => ({ status: 200, body: { version: 4 } }),
      storage,
    });
    editing.offerRestore();
    click(t.document.querySelector("#mm-restore button"));
    await tick();
    assert.equal(errors.length, 1, "reported, not thrown");
    assert.ok(readStash(storage, 7), "the draft survives");
    assert.ok(t.document.getElementById("mm-restore"), "and is still offered");
    assert.equal(t.root.hasAttribute("data-editing"), false);
    // A second try works once the module loads.
    const again = initEditing(t.document, {
      load: async () => ({ startEditing }),
      save: async () => ({ status: 200, body: { version: 4 } }),
      storage,
    });
    void again;
  } finally {
    globalThis.console.error = original;
  }
});

test("a_kept_draft_that_edit_mode_cannot_use_is_dropped", async () => {
  const t = setup();
  const storage = fakeStorage();
  stashDraft(storage, keptDraft(), 7);
  const editing = initEditing(t.document, {
    load: async () => ({
      startEditing() {
        throw new Error("cannot use this draft");
      },
    }),
    save: async () => ({ status: 200, body: { version: 4 } }),
    storage,
  });
  editing.offerRestore();
  click(t.document.querySelector("#mm-restore button"));
  await tick();
  assert.equal(storage.map.size, 0);
});

const HOUR = 60 * 60 * 1000;

test("the_kept_draft_is_tied_to_the_user_and_the_time_it_was_kept", async () => {
  const t = setup();
  const storage = fakeStorage();
  const clock = { now: 1_000_000 };
  const editing = wired(t, storage, undefined, () => clock.now);
  click(t.document.getElementById("mm-edit"));
  await tick();
  click(act(item(t.root, "card", "c1"), "delete"));
  editing.leave();
  const kept = JSON.parse([...storage.map.values()][0]);
  assert.equal(kept.user, 7, "the number of the signed-in user");
  assert.equal(kept.at, 1_000_000);
  assert.deepEqual(Object.keys(kept).sort(), ["at", "draft", "user", "version"], "no user name, only the number");
});

test("a_draft_kept_for_another_user_is_deleted_and_not_offered", () => {
  const t = setup();
  const storage = fakeStorage();
  stashDraft(storage, keptDraft(), 8);
  wired(t, storage).offerRestore();
  assert.equal(t.document.getElementById("mm-restore"), null);
  assert.equal(storage.map.size, 0, "deleted without being offered");
});

test("a_draft_older_than_a_day_is_deleted_and_not_offered", () => {
  const t = setup();
  const storage = fakeStorage();
  stashDraft(storage, keptDraft(), 7, 5 * HOUR);
  wired(t, storage, undefined, () => 5 * HOUR + 24 * HOUR).offerRestore();
  assert.equal(t.document.getElementById("mm-restore"), null);
  assert.equal(storage.map.size, 0);

  // A clock that went backwards is not trusted either.
  stashDraft(storage, keptDraft(), 7, 100 * HOUR);
  wired(t, storage, undefined, () => 5 * HOUR).offerRestore();
  assert.equal(t.document.getElementById("mm-restore"), null);
  assert.equal(storage.map.size, 0);
});

test("a_fresh_draft_of_the_same_user_is_offered", () => {
  const t = setup();
  const storage = fakeStorage();
  stashDraft(storage, keptDraft(), 7, 5 * HOUR);
  wired(t, storage, undefined, () => 5 * HOUR + 23 * HOUR + 59 * 60 * 1000).offerRestore();
  assert.ok(t.document.getElementById("mm-restore"));
  assert.equal(storage.map.size, 1, "kept until it is used");
});

test("without_a_user_number_on_the_page_nothing_is_kept_or_offered", async () => {
  const t = setup();
  t.root.removeAttribute("data-user");
  const storage = fakeStorage();
  stashDraft(storage, keptDraft(), 7);
  const editing = wired(t, storage);
  editing.offerRestore();
  assert.equal(t.document.getElementById("mm-restore"), null);
  assert.equal(storage.map.size, 0);

  click(t.document.getElementById("mm-edit"));
  await tick();
  click(act(item(t.root, "card", "c1"), "delete"));
  editing.leave();
  assert.equal(storage.map.size, 0, "a draft that cannot be tied to a user is not kept");

  t.root.setAttribute("data-user", "ada");
  stashDraft(storage, keptDraft(), 7);
  wired(t, storage).offerRestore();
  assert.equal(t.document.getElementById("mm-restore"), null, "only a number counts");
});

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
  makeBackground,
  makeSave,
  readStash,
  reloadIfDataLost,
  stashDraft,
  wirePage,
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

function setup({ layout = layoutFixture(), saveResult, background, services } = {}) {
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
      background,
      services,
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
  // Two groups: "down" is the last place already, so there is no "last".
  assert.deepEqual([...menu.querySelectorAll("button[data-move]")].map((b) => b.getAttribute("data-move")), ["down"]);
  click(menu.querySelector('[data-move="down"]'));
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

test("a_failed_load_of_edit_mode_keeps_the_kept_draft_and_a_second_try_works", async () => {
  const t = setup();
  const storage = fakeStorage();
  stashDraft(storage, keptDraft(), 7);
  const errors = [];
  const original = globalThis.console.error;
  globalThis.console.error = (...args) => errors.push(args);
  try {
    let tries = 0;
    const editing = initEditing(t.document, {
      load: async () => {
        tries += 1;
        if (tries === 1) throw new TypeError("Failed to fetch dynamically imported module");
        return { startEditing };
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

    // The module loads the second time: Restore works from the same notice.
    click(t.document.querySelector("#mm-restore button"));
    await tick();
    assert.equal(tries, 2);
    assert.ok(t.root.hasAttribute("data-editing"));
    assert.equal(item(t.root, "card", "c1"), null, "the kept draft is on screen");
    assert.equal(t.document.getElementById("mm-restore"), null);
    assert.equal(errors.length, 1);
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

// ---- the background picker ---------------------------------------------------

/** The two background calls, recording what they were asked. */
function fakeBackground(overrides = {}) {
  const calls = { upload: [], remove: [] };
  return {
    calls,
    upload:
      overrides.upload ??
      (async (file) => {
        calls.upload.push(file);
        return { status: 200, body: { id: "up1" } };
      }),
    remove:
      overrides.remove ??
      (async (id) => {
        calls.remove.push(id);
        return { status: 204, body: null };
      }),
  };
}

const PNG = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
const picture = (name = "bg.png", bytes = PNG) => new File([new Uint8Array(bytes)], name, { type: "image/png" });

function openPicker(t) {
  click(act(bar(t.document), "background"));
  return sheet(t.document);
}

function choosePicture(dialog, file) {
  const input = dialog.querySelector('input[type="file"]');
  Object.defineProperty(input, "files", { value: [file], configurable: true });
  input.dispatchEvent(new dialog.ownerDocument.defaultView.Event("change", { bubbles: true }));
}

function choosePreset(dialog, name) {
  for (const radio of dialog.querySelectorAll('input[name="preset"]')) radio.checked = radio.value === name;
  dialog
    .querySelector(`input[name="preset"][value="${name}"]`)
    .dispatchEvent(new dialog.ownerDocument.defaultView.Event("change", { bubbles: true }));
}

const withImage = (id = "old1") => ({ ...layoutFixture(), background: { kind: "image", id } });
const dataBg = (t) => t.document.documentElement.getAttribute("data-bg");

test("the_toolbar_opens_the_picker_with_focus_inside_and_back_on_close", async () => {
  const t = setup({ background: fakeBackground() });
  t.start();
  const button = act(bar(t.document), "background");
  assert.equal(button.textContent.trim(), "Background");
  const dialog = openPicker(t);
  assert.ok(dialog);
  assert.equal(dialog.querySelector(".sheet-title").textContent, "Background");
  // The current preset (Hedgerow) has focus.
  assert.equal(t.focus.last.getAttribute("name"), "preset");
  assert.equal(t.focus.last.getAttribute("value"), "hedgerow");

  press(dialog, "Escape");
  await tick();
  assert.equal(sheet(t.document), null);
  assert.equal(t.focus.last, button, "focus is back on the Background control");

  openPicker(t);
  click(sheet(t.document).querySelector(".sheet-foot button"));
  await tick();
  assert.equal(t.focus.last, button);
});

test("the_picker_offers_four_presets_with_names_and_marks_the_current_one", () => {
  const layout = layoutFixture();
  layout.background = { kind: "preset", name: "hav" };
  const t = setup({ layout, background: fakeBackground() });
  t.start();
  const dialog = openPicker(t);
  assert.ok(dialog.querySelector('[role="radiogroup"]'));
  const radios = [...dialog.querySelectorAll('input[type="radio"][name="preset"]')];
  assert.deepEqual(radios.map((r) => r.value), ["hedgerow", "natt", "skumring", "hav"]);
  assert.deepEqual(radios.map((r) => r.closest("label").textContent.trim()), ["Hedgerow", "Night", "Dusk", "Sea"]);
  assert.deepEqual(radios.map((r) => Boolean(r.checked)), [false, false, false, true]);
  assert.equal(t.focus.last.getAttribute("value"), "hav", "focus goes to the current one");
  for (const dot of dialog.querySelectorAll(".mm-bg-dot")) assert.ok(dot.getAttribute("data-preset"), "drawn from a data attribute");
  assert.equal(dialog.querySelector("[style]"), null, "no style attributes");
});

test("choosing_a_preset_changes_the_draft_and_the_page_and_is_saved_with_done", async () => {
  const t = setup({ background: fakeBackground() });
  const ed = t.start();
  assert.equal(ed.isDirty(), false);
  const dialog = openPicker(t);
  choosePreset(dialog, "natt");
  assert.deepEqual(ed.draft().background, { kind: "preset", name: "natt" });
  assert.equal(dataBg(t), "natt", "the page changed at once");
  assert.equal(ed.isDirty(), true);
  choosePreset(dialog, "hav");
  assert.equal(dataBg(t), "hav");
  press(dialog, "Escape");
  await tick();

  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(t.saved.length, 1);
  assert.deepEqual(t.saved[0].background, { kind: "preset", name: "hav" });
});

test("going_back_to_the_stored_preset_is_not_a_change", () => {
  const t = setup({ background: fakeBackground() });
  const ed = t.start();
  const dialog = openPicker(t);
  choosePreset(dialog, "natt");
  choosePreset(dialog, "hedgerow");
  assert.equal(ed.isDirty(), false);
});

test("the_keyboard_order_is_presets_upload_remove_close", () => {
  const t = setup({ layout: withImage(), background: fakeBackground() });
  t.start();
  const dialog = openPicker(t);
  const order = [...dialog.querySelectorAll("input, button")]
    .filter((el) => !el.hasAttribute("hidden"))
    .map((el) => el.getAttribute("name") ?? el.getAttribute("data-act") ?? el.textContent.trim());
  // The head close button (no name of its own), the four presets (one tab stop
  // in a browser), the upload, "Remove picture" (a picture is set), the footer Close.
  assert.deepEqual(order, ["", "preset", "preset", "preset", "preset", "image", "remove-picture", "Close"]);
  // With an image set, no preset is marked.
  assert.deepEqual([...dialog.querySelectorAll('input[name="preset"]')].map((r) => Boolean(r.checked)), [false, false, false, false]);
});

test("without_a_picture_there_is_no_remove_button", () => {
  const t = setup({ background: fakeBackground() });
  t.start();
  const dialog = openPicker(t);
  assert.equal(dialog.querySelector('[data-act="remove-picture"]').hasAttribute("hidden"), true);
});

test("a_file_over_5_mb_is_refused_on_the_client_with_a_clear_message", async () => {
  const bg = fakeBackground();
  const t = setup({ background: bg });
  const ed = t.start();
  const dialog = openPicker(t);
  choosePicture(dialog, new File([new Uint8Array(6 * 1024 * 1024)], "big.png", { type: "image/png" }));
  await tick();
  const message = dialog.querySelector('.field-error[data-for="image"]');
  assert.equal(message.hasAttribute("hidden"), false);
  assert.match(message.textContent, /too big \(at most 5 MB\)/);
  assert.equal(dialog.querySelector('input[type="file"]').getAttribute("aria-invalid"), "true");
  assert.equal(bg.calls.upload.length, 0, "nothing was sent");
  assert.equal(ed.isDirty(), false);
  // Exactly 5 MiB passes the size check.
  const exact = new Uint8Array(5 * 1024 * 1024);
  exact.set(PNG);
  choosePicture(dialog, new File([exact], "ok.png"));
  await tick();
  assert.equal(bg.calls.upload.length, 1);
});

test("a_gif_or_a_renamed_file_is_refused_on_the_client_by_its_bytes", async () => {
  const bg = fakeBackground();
  const t = setup({ background: bg });
  t.start();
  const dialog = openPicker(t);
  const gif = [...new TextEncoder().encode("GIF89a"), 1, 0, 1, 0, 0, 0];
  choosePicture(dialog, new File([new Uint8Array(gif)], "anim.gif", { type: "image/gif" }));
  await tick();
  assert.match(dialog.querySelector('.field-error[data-for="image"]').textContent, /PNG, JPEG or WebP/);
  // A .png that is really text.
  choosePicture(dialog, new File(["<html>not a picture</html>"], "fake.png", { type: "image/png" }));
  await tick();
  assert.match(dialog.querySelector('.field-error[data-for="image"]').textContent, /PNG, JPEG or WebP/);
  assert.equal(bg.calls.upload.length, 0);
  // JPEG and WebP pass.
  const jpeg = [0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0, 0, 0, 0, 0];
  const webp = [...new TextEncoder().encode("RIFF"), 1, 2, 3, 4, ...new TextEncoder().encode("WEBP")];
  choosePicture(dialog, picture("a.jpg", jpeg));
  await tick();
  choosePicture(dialog, picture("a.webp", webp));
  await tick();
  assert.equal(bg.calls.upload.length, 2);
});

test("a_finished_upload_sets_the_image_background_and_redraws", async () => {
  const bg = fakeBackground();
  const t = setup({ background: bg });
  const ed = t.start();
  const dialog = openPicker(t);
  choosePicture(dialog, picture());
  await tick();
  assert.equal(bg.calls.upload.length, 1);
  assert.deepEqual(ed.draft().background, { kind: "image", id: "up1" });
  assert.equal(dataBg(t), "image");
  assert.equal(ed.isDirty(), true);
  assert.equal(dialog.querySelector('[data-act="remove-picture"]').hasAttribute("hidden"), false, "the picture can be removed now");
  assert.match(dialog.querySelector(".mm-bg-status").textContent, /Your picture is the background/);
  assert.equal(t.focus.last, dialog.querySelector('input[type="file"]'));
  press(dialog, "Escape");
  await tick();

  // The layout points at the picture only when it is saved.
  assert.equal(t.saved.length, 0);
  click(act(bar(t.document), "done"));
  await tick();
  assert.deepEqual(t.saved[0].background, { kind: "image", id: "up1" });
  assert.deepEqual(bg.calls.remove, [], "a used picture stays");
});

test("an_upload_shows_a_busy_state_and_blocks_the_other_controls", async () => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const bg = fakeBackground({ upload: () => gate });
  const t = setup({ background: bg });
  const ed = t.start();
  const dialog = openPicker(t);
  const input = dialog.querySelector('input[type="file"]');
  input.focus();
  choosePicture(dialog, picture());
  await tick();
  assert.equal(dialog.querySelector(".sheet-body").getAttribute("aria-busy"), "true");
  assert.match(dialog.querySelector(".mm-bg-status").textContent, /Uploading…/);
  // Blocked with aria-disabled, never `disabled`: a disabled control that has
  // focus drops it to the page, and Escape would stop working.
  for (const control of [...dialog.querySelectorAll('input[name="preset"]'), input]) {
    assert.equal(control.getAttribute("aria-disabled"), "true");
    assert.equal(control.hasAttribute("disabled"), false);
  }
  // Choosing a preset while busy changes nothing.
  choosePreset(dialog, "natt");
  assert.deepEqual(ed.draft().background, { kind: "preset", name: "hedgerow" });
  // The file input keeps focus, and Escape still closes the sheet.
  assert.ok(t.focus.last === input);
  press(dialog, "Escape");
  await tick();
  assert.equal(sheet(t.document), null);
  release({ status: 200, body: { id: "up2" } });
  await tick();
  // The upload finished after the sheet closed: it still counts.
  assert.deepEqual(ed.draft().background, { kind: "image", id: "up2" });
});

test("focus_stays_in_the_sheet_while_it_is_busy_and_after", async () => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const t = setup({ background: fakeBackground({ upload: () => gate }) });
  t.start();
  const dialog = openPicker(t);
  const input = dialog.querySelector('input[type="file"]');
  input.focus();
  choosePicture(dialog, picture());
  await tick();
  assert.ok(t.focus.last === input, "nothing took focus away while busy");
  assert.equal(input.hasAttribute("disabled"), false);
  release({ status: 200, body: { id: "up2" } });
  await tick();
  assert.equal(input.getAttribute("aria-disabled"), null);
  assert.ok(t.focus.last === input);
});

test("the_file_input_sits_hidden_inside_a_secondary_button_label_and_shares_the_busy_state", async () => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const t = setup({ background: fakeBackground({ upload: () => gate }) });
  t.start();
  const dialog = openPicker(t);
  const input = dialog.querySelector('input[type="file"]');
  const label = input.parentElement;
  assert.equal(label.localName, "label", "the label is the button");
  assert.ok(label.classList.contains("btn") && label.classList.contains("btn-secondary"));
  assert.ok(input.classList.contains("vh"), "the real input stays, visually hidden");
  assert.equal(label.textContent.trim(), "Upload picture", "the button text names the input");
  const name = dialog.querySelector(".mm-file-name");
  assert.equal(name.textContent, "");
  choosePicture(dialog, picture("holiday.png"));
  await tick();
  assert.equal(name.textContent, "holiday.png", "the file on its way is named");
  assert.equal(label.getAttribute("aria-disabled"), "true", "the button is busy with the input");
  assert.equal(input.getAttribute("aria-disabled"), "true");
  release({ status: 200, body: { id: "up3" } });
  await tick();
  assert.equal(name.textContent, "");
  assert.equal(label.getAttribute("aria-disabled"), null);
});

test("server_refusals_and_network_errors_show_a_message_and_change_nothing", async () => {
  for (const [result, expected] of [
    [{ status: 413, body: { error: "too_large" } }, /too big/],
    [{ status: 422, body: { error: "bad_image" } }, /PNG, JPEG or WebP/],
    [{ status: 400, body: { error: "bad_request" } }, /Could not upload/],
    [new TypeError("Failed to fetch"), /Could not upload/],
  ]) {
    const bg = fakeBackground({
      upload: async () => {
        if (result instanceof Error) throw result;
        return result;
      },
    });
    const t = setup({ background: bg });
    const ed = t.start();
    const dialog = openPicker(t);
    choosePicture(dialog, picture());
    await tick();
    assert.match(dialog.querySelector('.field-error[data-for="image"]').textContent, expected);
    assert.equal(ed.isDirty(), false);
    assert.equal(dialog.querySelector('input[type="file"]').hasAttribute("disabled"), false, "usable again");
  }
});

test("remove_picture_is_a_change_of_the_draft_only", async () => {
  const bg = fakeBackground();
  const t = setup({ layout: withImage("old1"), background: bg });
  const ed = t.start();
  const dialog = openPicker(t);
  click(dialog.querySelector('[data-act="remove-picture"]'));
  await tick();
  assert.deepEqual(bg.calls.remove, [], "nothing is sent to the server");
  assert.deepEqual(ed.draft().background, { kind: "preset", name: "hedgerow" });
  assert.equal(ed.draft().version, 3, "the version is never changed by the picker");
  assert.equal(dataBg(t), "hedgerow");
  assert.equal(ed.isDirty(), true, "it is a change like the others");
  assert.equal(dialog.querySelector('[data-act="remove-picture"]').hasAttribute("hidden"), true);
  // Focus goes to a control that is still there.
  assert.equal(t.focus.last.getAttribute("name"), "preset");
});

test("cancel_after_remove_picture_brings_the_picture_back_and_asks_nothing_of_the_server", async () => {
  const bg = fakeBackground();
  const t = setup({ layout: withImage("old1"), background: bg });
  t.start();
  const dialog = openPicker(t);
  click(dialog.querySelector('[data-act="remove-picture"]'));
  press(dialog, "Escape");
  await tick();
  click(act(bar(t.document), "cancel"));
  click(sheet(t.document).querySelector('[data-answer="yes"]'));
  await tick();
  assert.deepEqual(bg.calls.remove, []);
  assert.equal(dataBg(t), "image", "the stored picture is on the page again");
});

test("done_after_remove_picture_saves_hedgerow_and_then_removes_the_old_picture_by_id", async () => {
  const bg = fakeBackground();
  const t = setup({ layout: withImage("old1"), background: bg });
  t.start();
  const dialog = openPicker(t);
  click(dialog.querySelector('[data-act="remove-picture"]'));
  press(dialog, "Escape");
  await tick();
  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(t.saved[0].version, 3, "saved at the version it was loaded at");
  assert.deepEqual(t.saved[0].background, { kind: "preset", name: "hedgerow" });
  assert.deepEqual(bg.calls.remove, ["old1"]);
});

test("a_saved_picture_that_the_layout_still_uses_is_not_removed", async () => {
  const bg = fakeBackground();
  const t = setup({ layout: withImage("old1"), background: bg });
  t.start();
  click(act(item(t.root, "card", "c1"), "delete"));
  click(act(bar(t.document), "done"));
  await tick();
  assert.deepEqual(t.saved[0].background, { kind: "image", id: "old1" });
  assert.deepEqual(bg.calls.remove, []);
});

test("two_tabs_a_remove_in_a_stale_tab_ends_in_a_conflict_not_in_a_lost_update", async () => {
  // Tab B saved a preset after tab A loaded the page with picture old1. A
  // removes the picture in its draft and presses Done: the version A sends is
  // the one it loaded, so the save is a conflict, and nothing the picker did
  // has changed any layout or version.
  const bg = fakeBackground();
  const t = setup({
    layout: withImage("old1"),
    background: bg,
    saveResult: { status: 409, body: { error: "conflict", current: 4 } },
  });
  const ed = t.start();
  const dialog = openPicker(t);
  click(dialog.querySelector('[data-act="remove-picture"]'));
  press(dialog, "Escape");
  await tick();
  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(t.saved[0].version, 3);
  assert.match(alertText(t.document), /changed in another tab/);
  assert.ok(t.root.hasAttribute("data-editing"));
  assert.equal(ed.draft().version, 3);
  // The stored layout block is written only from a successful save.
  assert.equal(JSON.parse(t.document.getElementById("mm-layout").textContent).version, 3);
  assert.deepEqual(bg.calls.remove, [], "no clean-up after a refused save");
  // Reload gives the draft up; the old picture is not touched either.
  click(act(t.document.querySelector(".mm-edit-alert"), "reload"));
  assert.deepEqual(bg.calls.remove, []);
});

test("cancel_after_an_upload_takes_the_unused_picture_off_the_server_by_id", async () => {
  const bg = fakeBackground();
  const t = setup({ background: bg });
  t.start();
  const dialog = openPicker(t);
  choosePicture(dialog, picture());
  await tick();
  press(dialog, "Escape");
  await tick();
  click(act(bar(t.document), "cancel"));
  click(sheet(t.document).querySelector('[data-answer="yes"]'));
  await tick();
  assert.deepEqual(bg.calls.remove, ["up1"], "the new file, by its id");
  assert.equal(t.root.hasAttribute("data-editing"), false);
  assert.equal(dataBg(t), "hedgerow", "the old (preset) background stays");
});

test("the_clean_up_after_cancel_names_only_this_sessions_upload", async () => {
  // Another tab saved a layout with its own picture meanwhile. This tab never
  // asks for that picture to go: it names the id it uploaded, nothing else.
  const bg = fakeBackground();
  const t = setup({ layout: withImage("old1"), background: bg });
  t.start();
  const dialog = openPicker(t);
  choosePicture(dialog, picture());
  await tick();
  press(dialog, "Escape");
  await tick();
  click(act(bar(t.document), "cancel"));
  click(sheet(t.document).querySelector('[data-answer="yes"]'));
  await tick();
  assert.deepEqual(bg.calls.remove, ["up1"]);
  assert.ok(!bg.calls.remove.includes("old1"));
});

test("an_upload_that_finishes_after_cancel_is_taken_off_the_server_by_id", async () => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const bg = fakeBackground({ upload: () => gate });
  const t = setup({ background: bg });
  const ed = t.start();
  const dialog = openPicker(t);
  choosePicture(dialog, picture());
  await tick();
  press(dialog, "Escape");
  await tick();
  click(act(bar(t.document), "cancel"));
  await tick();
  assert.equal(ed.isActive(), false, "the editor has ended");
  assert.deepEqual(bg.calls.remove, [], "nothing to remove yet");
  release({ status: 200, body: { id: "late1" } });
  await tick();
  assert.deepEqual(bg.calls.remove, ["late1"]);
  assert.equal(t.root.hasAttribute("data-editing"), false);
});

test("cancel_after_an_upload_that_replaced_a_stored_picture_shows_the_old_picture_again", async () => {
  // The server keeps the picture the stored layout uses when an upload comes,
  // and gives it back when the upload is removed: after Cancel the page is as
  // it was, picture and all.
  const toasts = [];
  const t = setup({ layout: withImage("old1") });
  t.window.VV = { toast: (message, kind) => toasts.push([message, kind]) };
  const bg = fakeBackground();
  const ed = startEditing(t.root, () => t.current.layout, async () => ({ status: 200, body: { version: 5 } }), {
    labels: LABELS,
    lang: "en",
    editButton: t.document.getElementById("mm-edit"),
    background: bg,
  });
  const dialog = openPicker(t);
  choosePicture(dialog, picture());
  await tick();
  assert.equal(dataBg(t), "image");
  press(dialog, "Escape");
  await tick();
  click(act(bar(t.document), "cancel"));
  click(sheet(t.document).querySelector('[data-answer="yes"]'));
  await tick();
  assert.equal(ed.isActive(), false);
  assert.equal(dataBg(t), "image", "the old picture is drawn again");
  assert.match(t.document.documentElement.style.getPropertyValue("--mm-bg-image"), /old1/);
  assert.equal(toasts.length, 0, "nothing was lost, so nothing to say");
  assert.deepEqual(bg.calls.remove, ["up1"], "only the session's upload is asked to go");
  assert.equal(JSON.parse(t.document.getElementById("mm-layout").textContent).background.id, "old1");
});

test("saving_with_another_background_than_the_upload_takes_the_upload_off_the_server", async () => {
  const bg = fakeBackground();
  const t = setup({ background: bg });
  t.start();
  const dialog = openPicker(t);
  choosePicture(dialog, picture());
  await tick();
  choosePreset(dialog, "hav");
  press(dialog, "Escape");
  await tick();
  click(act(bar(t.document), "done"));
  await tick();
  assert.deepEqual(t.saved[0].background, { kind: "preset", name: "hav" });
  assert.deepEqual(bg.calls.remove, ["up1"]);
});

test("saving_a_new_picture_over_a_stored_one_removes_the_old_one_by_id", async () => {
  const bg = fakeBackground();
  const t = setup({ layout: withImage("old1"), background: bg });
  t.start();
  const dialog = openPicker(t);
  choosePicture(dialog, picture());
  await tick();
  press(dialog, "Escape");
  await tick();
  click(act(bar(t.document), "done"));
  await tick();
  assert.deepEqual(t.saved[0].background, { kind: "image", id: "up1" });
  // The new picture is used and stays; the old one is asked to go (the server
  // has replaced it already, so this is usually a no-op there).
  assert.deepEqual(bg.calls.remove, ["old1"]);
});

test("a_draft_that_is_kept_for_Back_keeps_its_picture_on_the_server", async () => {
  const bg = fakeBackground();
  const t = setup({ background: bg });
  const ed = t.start();
  const dialog = openPicker(t);
  choosePicture(dialog, picture());
  await tick();
  press(dialog, "Escape");
  await tick();
  ed.stop({ focus: false });
  await tick();
  assert.deepEqual(bg.calls.remove, []);
});

test("a_save_refused_for_a_dead_picture_falls_back_to_hedgerow_with_a_message", async () => {
  const t = setup({
    layout: withImage("gone1"),
    background: fakeBackground(),
    saveResult: (next, n) =>
      n === 1 ? { status: 422, body: { error: "invalid", fields: [{ path: "background.id", code: "bad_image" }] } } : undefined,
  });
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "delete"));
  click(act(bar(t.document), "done"));
  await tick();
  assert.match(alertText(t.document), /The background picture is gone/);
  assert.ok(t.root.hasAttribute("data-editing"), "no dead end: still editing");
  assert.deepEqual(ed.draft().background, { kind: "preset", name: "hedgerow" });
  assert.equal(dataBg(t), "hedgerow");
  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(t.saved.length, 2);
  assert.deepEqual(t.saved[1].background, { kind: "preset", name: "hedgerow" });
  assert.equal(t.root.hasAttribute("data-editing"), false);
});

test("without_background_calls_the_picker_offers_only_the_presets", () => {
  const t = setup();
  t.start();
  const dialog = openPicker(t);
  assert.equal(dialog.querySelectorAll('input[name="preset"]').length, 4);
  assert.equal(dialog.querySelector('input[type="file"]'), null);
});

test("makeBackground_posts_the_file_and_deletes_one_picture_by_id_with_the_csrf_header", async () => {
  const t = setup();
  const calls = [];
  const fetchFn = async (url, init) => {
    calls.push({ url, init });
    return init.method === "POST"
      ? { status: 200, json: async () => ({ id: "abc" }) }
      : { status: 204, json: async () => { throw new Error("no body"); } };
  };
  const background = makeBackground(t.document, fetchFn);
  const file = picture();
  assert.deepEqual(await background.upload(file), { status: 200, body: { id: "abc" } });
  assert.equal(calls[0].url, "/start/background");
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls[0].init.headers["HX-Request"], "true");
  assert.equal(calls[0].init.headers["Content-Type"], undefined, "the browser writes the boundary");
  assert.equal(calls[0].init.credentials, "same-origin");
  assert.equal(calls[0].init.body.get("image").name, "bg.png");

  // The layout block is left alone: only a save writes it.
  const block = t.document.getElementById("mm-layout");
  block.textContent = JSON.stringify(withImage("old1"));
  const before = block.textContent;
  assert.deepEqual(await background.remove("old1"), { status: 204, body: null });
  assert.equal(calls[1].url, "/start/background?id=old1");
  assert.equal(calls[1].init.method, "DELETE");
  assert.equal(calls[1].init.headers["HX-Request"], "true");
  assert.equal(calls[1].init.credentials, "same-origin");
  assert.equal(block.textContent, before);
  // An id is written into the query safely.
  await background.remove("a b&c");
  assert.equal(calls[2].url, "/start/background?id=a%20b%26c");
});

test("a_422_with_fields_of_an_unexpected_shape_does_not_break_the_editor", async () => {
  const t = setup({
    background: fakeBackground(),
    saveResult: { status: 422, body: { error: "invalid", fields: [null, { code: "bad_image" }, { path: 7, code: "bad_image" }, {}] } },
  });
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "delete"));
  click(act(bar(t.document), "done"));
  await tick();
  assert.ok(t.root.hasAttribute("data-editing"), "still open, no exception");
  assert.equal(ed.isActive(), true);
  assert.ok(alertText(t.document).length > 0);
});

test("first_and_last_are_offered_only_when_up_and_down_do_not_reach_them", async () => {
  const layout = layoutFixture();
  layout.groups = ["a", "b", "c", "d"].map((id) => ({ id, title: id.toUpperCase(), cards: [] }));
  const t = setup({ layout });
  t.start();
  const options = (id) => {
    click(act(item(t.root, "group", id), "move"));
    const found = [...sheet(t.document).querySelectorAll("button[data-move]")].map((b) => b.getAttribute("data-move"));
    press(sheet(t.document), "Escape");
    return found;
  };
  assert.deepEqual(options("a"), ["down", "last"]);
  assert.deepEqual(options("b"), ["up", "down", "last"]);
  assert.deepEqual(options("c"), ["up", "down", "first"]);
  assert.deepEqual(options("d"), ["up", "first"]);
  await tick();
  // Three items: the only other places are one step away.
  const three = layoutFixture();
  three.bookmarks = ["x", "y", "z"].map((id) => ({ id, title: id, url: `https://${id}.example/` }));
  const u = setup({ layout: three });
  u.start();
  click(act(item(u.root, "bookmark", "y"), "move"));
  assert.deepEqual([...sheet(u.document).querySelectorAll("button[data-move]")].map((b) => b.getAttribute("data-move")), ["up", "down"]);
});

test("a_restored_draft_that_conflicts_is_not_offered_again_after_reload", async () => {
  const t = setup();
  const storage = fakeStorage();
  // Kept at version 1, while the page is at version 3: it will conflict.
  stashDraft(storage, { ...keptDraft(), version: 1 }, 7);
  const first = wired(t, storage, async () => ({ status: 409, body: { error: "conflict", current: 3 } }));
  first.offerRestore();
  const notice = t.document.getElementById("mm-restore");
  assert.match(notice.textContent, /The page has changed since – restoring may cause a conflict/, "the note is honest");
  click(notice.querySelector("button"));
  await tick();
  click(act(bar(t.document), "done"));
  await tick();
  assert.match(alertText(t.document), /changed in another tab/);
  assert.ok(readStash(storage, 7), "kept while the person decides");

  // Reload gives the draft up, and the kept copy with it.
  click(act(t.document.querySelector(".mm-edit-alert"), "reload"));
  assert.equal(storage.map.size, 0, "the stash is cleared");

  // The page after the reload does not offer it again.
  const after = setup();
  wired(after, storage).offerRestore();
  assert.equal(after.document.getElementById("mm-restore"), null);
});

test("a_kept_draft_at_the_pages_own_version_is_offered_without_the_stale_note", () => {
  const t = setup();
  const storage = fakeStorage();
  stashDraft(storage, { ...keptDraft(), version: 3 }, 7);
  wired(t, storage).offerRestore();
  const notice = t.document.getElementById("mm-restore");
  assert.ok(notice);
  assert.doesNotMatch(notice.textContent, /may cause a conflict/);
  assert.equal(notice.querySelectorAll("p").length, 1);
});

// ---- the page wiring: what htmx does to the body ----------------------------

function wiredPage(t, extra = {}) {
  const reloads = [];
  const storage = fakeStorage();
  const page = wirePage(t.document, {
    storage,
    reload: () => reloads.push(1),
    load: async () => ({ startEditing }),
    save: async (next) => ({ status: 200, body: { version: next.version + 1 } }),
    ...extra,
  });
  return { ...page, reloads, storage };
}

const fire = (t, type) => t.document.dispatchEvent(new t.window.Event(type, { bubbles: true }));

test("htmx_afterSettle_on_a_body_without_data_blocks_reloads_and_draws_nothing", () => {
  const t = setup();
  const page = wiredPage(t);
  page.start();
  assert.ok(t.root.textContent.includes("Sonarr"));
  // htmx swapped in a fresh start page body: no script blocks, #groups empty.
  t.document.getElementById("mm-layout").remove();
  t.document.getElementById("mm-labels").remove();
  t.root.innerHTML = "";
  fire(t, "htmx:afterSettle");
  assert.equal(page.reloads.length, 1, "the page is loaded in full");
  assert.equal(t.root.innerHTML, "", "nothing is drawn from the missing data");
});

test("htmx_historyRestore_on_a_body_without_data_blocks_reloads", () => {
  const t = setup();
  const page = wiredPage(t);
  page.start();
  t.document.getElementById("mm-layout").remove();
  t.document.getElementById("mm-labels").remove();
  t.document.documentElement.removeAttribute("data-bg");
  fire(t, "htmx:historyRestore");
  assert.equal(page.reloads.length, 1);
  assert.equal(t.document.documentElement.hasAttribute("data-bg"), false, "not drawn empty over the page");
});

test("htmx_afterSettle_and_historyRestore_on_a_body_with_data_draw_it_and_offer_a_kept_draft", () => {
  for (const type of ["htmx:afterSettle", "htmx:historyRestore"]) {
    const t = setup();
    const page = wiredPage(t);
    stashDraft(page.storage, keptDraft(), 7);
    t.root.innerHTML = "";
    t.document.documentElement.removeAttribute("data-bg");
    fire(t, type);
    assert.equal(page.reloads.length, 0, type);
    assert.ok(t.root.textContent.includes("Sonarr"), `${type}: drawn`);
    assert.ok(t.document.documentElement.hasAttribute("data-bg"), `${type}: the background is back`);
    assert.ok(t.document.getElementById("mm-restore"), `${type}: the kept draft is offered`);
  }
});

test("htmx_afterSettle_on_another_page_cleans_up_and_does_not_reload", () => {
  const t = setup();
  const page = wiredPage(t);
  page.start();
  // A boosted visit to another page: the body is replaced, <html> keeps its data-bg.
  t.document.body.innerHTML = "<main><h1>Settings</h1></main>";
  fire(t, "htmx:afterSettle");
  assert.equal(page.reloads.length, 0);
  assert.equal(t.document.documentElement.hasAttribute("data-bg"), false, "the background goes with the page");
});

test("an_editor_whose_page_was_swapped_away_is_dropped_by_afterSettle", async () => {
  const t = setup();
  const page = wiredPage(t);
  page.start();
  click(t.document.getElementById("mm-edit"));
  await tick();
  assert.ok(t.root.hasAttribute("data-editing"));
  // The body is replaced by a fresh start page (new #groups, with its data).
  const fresh = t.document.createElement("section");
  fresh.setAttribute("id", "groups");
  fresh.setAttribute("class", "mm-root");
  fresh.setAttribute("data-user", "7");
  t.root.replaceWith(fresh);
  fire(t, "htmx:afterSettle");
  assert.ok(fresh.textContent.includes("Sonarr"), "the new body is drawn");
  assert.equal(fresh.hasAttribute("data-editing"), false);
});

// ---- cards bound to services (Plan 4, task 3b) ---------------------------------

const metric = (id, label, format = "number") => ({ id, label, format });
const service = (id, name, metrics) => ({ id, name, kind: "http", interval_s: 60, metrics });
const SERVICES = [
  service(1, "Blog", [metric("status", "HTTP status"), metric("latency_ms", "Response time (ms)"), metric("checked", "Last checked", "since")]),
  service(2, "Big one", ["a", "b", "c", "d", "e"].map((id) => metric(id, `Number ${id}`))),
];

/** Picks the service in the form's select, as a person (or their keyboard) does. */
function chooseService(dialog, value) {
  const select = dialog.querySelector('select[name="service"]');
  for (const option of select.querySelectorAll("option")) {
    const on = option.value === value;
    if (on) option.setAttribute("selected", "");
    else option.removeAttribute("selected");
  }
  select.dispatchEvent(new dialog.ownerDocument.defaultView.Event("change", { bubbles: true }));
}

const boxes = (dialog) => [...dialog.querySelectorAll('input[name="metric"]')];
const boxOn = (box) => box.checked ?? box.hasAttribute("checked");

function setBox(box, on) {
  box.checked = on;
  if (on) box.setAttribute("checked", "");
  else box.removeAttribute("checked");
  box.dispatchEvent(new box.ownerDocument.defaultView.Event("change", { bubbles: true }));
}

const metricsPicker = (dialog) => dialog.querySelector("[data-metrics]");

function bound(id, metrics, extra = {}) {
  return { ...card("c1", "Blog card"), service: { id, metrics }, ...extra };
}

function layoutWith(c1) {
  const layout = layoutFixture();
  layout.groups[0].cards[0] = c1;
  return layout;
}

test("the_card_form_offers_a_labelled_service_select_with_none_first_in_the_tab_order", () => {
  const t = setup({ services: SERVICES });
  t.start();
  click(act(item(t.root, "card", "c1"), "edit"));
  const dialog = sheet(t.document);
  const select = dialog.querySelector('select[name="service"]');
  assert.ok(select, "a «Tjeneste» select");
  const label = dialog.querySelector(`label[for="${select.getAttribute("id")}"]`);
  assert.equal(label.firstChild.textContent, LABELS.field_service);
  assert.equal(NB.field_service, "Tjeneste");
  assert.equal(NB.service_none, "Ingen");
  const options = [...select.querySelectorAll("option")].map((o) => [o.value, o.textContent]);
  assert.deepEqual(options, [["", LABELS.service_none], ["1", "Blog"], ["2", "Big one"]]);
  // The select describes itself with the help text and the error line.
  for (const id of select.getAttribute("aria-describedby").split(" ")) assert.ok(dialog.querySelector(`#${id}`), id);
  // Nothing is chosen on a card without a service, and no boxes are shown.
  assert.ok(metricsPicker(dialog).hasAttribute("hidden"));
  assert.equal(boxes(dialog).length, 0);
  // Reachable by Tab in the order of the form: after the colours, before the buttons.
  const order = [...dialog.querySelectorAll("input, select, button, textarea, a[href]")];
  assert.ok(order.indexOf(dialog.querySelector('[name="title"]')) < order.indexOf(select));
  assert.ok(order.indexOf(dialog.querySelector('input[name="colour"]')) < order.indexOf(select));
  assert.ok(order.indexOf(select) < order.indexOf(dialog.querySelector('button[type="submit"]')));
  assert.equal(dialog.querySelector("[tabindex]"), null, "no tabindex: the natural order stands");
  assert.equal(select.hasAttribute("disabled"), false);
});

test("a_keyboard_flow_binds_a_card_to_a_service_with_two_numbers_and_saves_the_payload", async () => {
  const t = setup({ services: SERVICES });
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "edit"));
  const dialog = sheet(t.document);
  chooseService(dialog, "1");

  // The numbers of the service, each a labelled checkbox in a labelled group; the first is on.
  const picker = metricsPicker(dialog);
  assert.equal(picker.hasAttribute("hidden"), false);
  assert.equal(picker.tagName, "FIELDSET", "the numbers are one fieldset");
  assert.equal(picker.querySelector("legend").textContent, LABELS.field_metrics);
  assert.equal(picker.querySelector('[role="group"]'), null, "the fieldset is the group, no role needed");
  assert.deepEqual(boxes(dialog).map((b) => b.value), ["status", "latency_ms", "checked"]);
  for (const box of boxes(dialog)) {
    assert.equal(box.closest("label")?.tagName, "LABEL", `${box.value} is inside a label`);
    assert.ok(box.closest("label").textContent.trim().length > 0);
  }
  assert.deepEqual(boxes(dialog).map(boxOn), [true, false, false]);

  setBox(boxes(dialog)[1], true);
  submit(dialog);
  await tick();
  assert.equal(sheet(t.document), null, "the form closed");
  assert.deepEqual(ed.draft().groups[0].cards[0].service, { id: 1, metrics: ["status", "latency_ms"] });
  assert.equal(ed.isDirty(), true);

  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(t.saved.length, 1);
  assert.deepEqual(t.saved[0].groups[0].cards[0].service, { id: 1, metrics: ["status", "latency_ms"] });
  // The other cards were not touched and have no service key at all.
  assert.equal("service" in t.saved[0].groups[0].cards[1], false);
});

test("at_four_numbers_the_other_boxes_are_disabled_with_a_hint_and_come_back_at_three", () => {
  const t = setup({ services: SERVICES });
  t.start();
  click(act(item(t.root, "card", "c1"), "edit"));
  const dialog = sheet(t.document);
  chooseService(dialog, "2");
  assert.equal(boxes(dialog).length, 5);
  const hint = metricsPicker(dialog).querySelector("p.help");
  assert.equal(hint.getAttribute("aria-live"), "polite");
  assert.equal(hint.textContent, LABELS.help_metrics);
  for (const box of boxes(dialog)) assert.equal(box.getAttribute("aria-describedby"), hint.getAttribute("id"));

  for (const box of boxes(dialog).slice(1, 4)) setBox(box, true);
  assert.deepEqual(boxes(dialog).map(boxOn), [true, true, true, true, false]);
  assert.equal(boxes(dialog)[4].hasAttribute("disabled"), true, "the fifth is disabled");
  assert.equal(boxes(dialog)[0].hasAttribute("disabled"), false, "the ticked ones can be cleared");
  assert.equal(hint.textContent, LABELS.metrics_full);

  setBox(boxes(dialog)[0], false);
  assert.equal(boxes(dialog)[4].hasAttribute("disabled"), false);
  assert.equal(hint.textContent, LABELS.help_metrics);
});

test("a_service_with_no_number_ticked_or_five_ticked_is_refused_in_the_form", async () => {
  const t = setup({ services: SERVICES });
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "edit"));
  const dialog = sheet(t.document);
  chooseService(dialog, "2");
  setBox(boxes(dialog)[0], false);
  submit(dialog);
  await tick();
  assert.equal(sheet(t.document), dialog, "the form stays open");
  const message = dialog.querySelector('.field-error[data-for="service"]');
  assert.equal(message.hasAttribute("hidden"), false);
  assert.equal(message.textContent, LABELS.err_service_metrics);
  assert.equal(dialog.querySelector('select[name="service"]').getAttribute("aria-invalid"), "true");
  assert.equal(t.focus.last.getAttribute("name"), "service", "focus goes to the select");
  assert.equal(ed.isDirty(), false);

  // Five at once cannot be ticked from the keyboard, but the rule is the same if it were.
  for (const box of boxes(dialog)) {
    box.removeAttribute("disabled");
    box.checked = true;
  }
  submit(dialog);
  await tick();
  assert.equal(message.textContent, LABELS.err_service_metrics);
  assert.equal(ed.isDirty(), false);
});

test("choosing_none_sends_null_and_a_card_that_never_had_a_service_sends_no_key", async () => {
  const t = setup({ services: SERVICES, layout: layoutWith(bound(1, ["status", "checked"])) });
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "edit"));
  let dialog = sheet(t.document);
  const select = dialog.querySelector('select[name="service"]');
  // The form starts from the card: Blog, with its two numbers ticked.
  assert.equal([...select.querySelectorAll("option")].find((o) => o.hasAttribute("selected")).textContent, "Blog");
  assert.deepEqual(boxes(dialog).map(boxOn), [true, false, true]);

  chooseService(dialog, "");
  assert.ok(metricsPicker(dialog).hasAttribute("hidden"));
  submit(dialog);
  await tick();
  assert.equal(ed.draft().groups[0].cards[0].service, null);

  click(act(bar(t.document), "done"));
  await tick();
  assert.equal(t.saved[0].groups[0].cards[0].service, null);
  assert.ok("service" in t.saved[0].groups[0].cards[0], "an explicit null, not a missing key");

  // A card that never had one: saving the form unchanged is no change, and adds no key.
  const t2 = setup({ services: SERVICES });
  const ed2 = t2.start();
  click(act(item(t2.root, "card", "c2"), "edit"));
  dialog = sheet(t2.document);
  submit(dialog);
  await tick();
  assert.equal(ed2.isDirty(), false);
  assert.equal("service" in ed2.draft().groups[0].cards[1], false);

  // A new card can be bound at once.
  click(act(item(t2.root, "group", "g2"), "add-card"));
  dialog = sheet(t2.document);
  fill(dialog, { title: "Blog", url: "blog.example.com" });
  chooseService(dialog, "1");
  submit(dialog);
  await tick();
  assert.deepEqual(ed2.draft().groups[1].cards.at(-1).service, { id: 1, metrics: ["status"] });
});

test("another_service_starts_with_its_first_number_and_the_same_one_comes_back_as_it_was", () => {
  const t = setup({ services: SERVICES, layout: layoutWith(bound(1, ["latency_ms", "checked"])) });
  t.start();
  click(act(item(t.root, "card", "c1"), "edit"));
  const dialog = sheet(t.document);
  chooseService(dialog, "2");
  assert.deepEqual(boxes(dialog).map((b) => [b.value, boxOn(b)]), [["a", true], ["b", false], ["c", false], ["d", false], ["e", false]]);
  chooseService(dialog, "1");
  assert.deepEqual(boxes(dialog).map(boxOn), [false, true, true]);
});

test("a_card_whose_service_is_gone_says_so_keeps_its_binding_and_lets_the_person_pick_another", async () => {
  const t = setup({ services: SERVICES, layout: layoutWith(bound(99, ["status"])) });
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "edit"));
  let dialog = sheet(t.document);
  const select = dialog.querySelector('select[name="service"]');
  const gone = [...select.querySelectorAll("option")].find((o) => o.hasAttribute("data-gone"));
  assert.ok(gone, "an option says the service is gone");
  assert.equal(gone.textContent, LABELS.service_missing);
  assert.equal(NB.service_missing, "Tjenesten finnes ikke lenger");
  assert.equal(gone.hasAttribute("selected"), true);
  assert.ok(metricsPicker(dialog).hasAttribute("hidden"));
  // None and the real services are offered next to it.
  assert.deepEqual([...select.querySelectorAll("option")].map((o) => o.value), ["", "1", "2", "99"]);

  // Saving as it is changes nothing (the server lets an unchanged binding through).
  submit(dialog);
  await tick();
  assert.deepEqual(ed.draft().groups[0].cards[0].service, { id: 99, metrics: ["status"] });
  assert.equal(ed.isDirty(), false);

  // Another service.
  click(act(item(t.root, "card", "c1"), "edit"));
  dialog = sheet(t.document);
  chooseService(dialog, "1");
  submit(dialog);
  await tick();
  assert.deepEqual(ed.draft().groups[0].cards[0].service, { id: 1, metrics: ["status"] });

  // And back to none, from a gone one.
  const t2 = setup({ services: SERVICES, layout: layoutWith(bound(99, ["status"])) });
  const ed2 = t2.start();
  click(act(item(t2.root, "card", "c1"), "edit"));
  chooseService(sheet(t2.document), "");
  submit(sheet(t2.document));
  await tick();
  assert.equal(ed2.draft().groups[0].cards[0].service, null);
});

test("without_a_services_list_the_form_offers_none_and_the_bound_card_counts_as_gone", () => {
  const t = setup({ layout: layoutWith(bound(1, ["status"])) });
  t.start();
  click(act(item(t.root, "card", "c1"), "edit"));
  const select = sheet(t.document).querySelector('select[name="service"]');
  assert.deepEqual([...select.querySelectorAll("option")].map((o) => o.value), ["", "1"]);
});

test("a_service_name_with_markup_stays_text_in_the_form", () => {
  const evil = [service(5, '<img src=x onerror="alert(1)">', [metric("m", "<b>bold</b>")])];
  const t = setup({ services: evil });
  t.start();
  click(act(item(t.root, "card", "c1"), "edit"));
  const dialog = sheet(t.document);
  chooseService(dialog, "5");
  assert.equal(dialog.querySelector("img"), null);
  assert.equal(dialog.querySelector("b"), null);
  assert.match(dialog.querySelector('select[name="service"]').textContent, /<img src=x/);
});

test("a_422_for_a_service_opens_the_card_form_with_the_message_under_the_select", async () => {
  for (const [path, code, message] of [
    ["groups[0].cards[0].service", "bad_service", LABELS.err_bad_service],
    ["groups[0].cards[0].service.metrics", "bad_metric", LABELS.err_bad_metric],
    ["groups[0].cards[0].service.metrics", "too_many", LABELS.err_service_metrics],
  ]) {
    const t = setup({
      services: SERVICES,
      layout: layoutWith(bound(1, ["status"])),
      saveResult: { status: 422, body: { error: "invalid", fields: [{ path, code }] } },
    });
    t.start();
    click(act(item(t.root, "group", "g2"), "add-card"));
    fill(sheet(t.document), { title: "x", url: "x.example" });
    submit(sheet(t.document));
    await tick();
    click(act(bar(t.document), "done"));
    await tick();
    // The summary names the field, not a list limit (a service problem is not "too many cards").
    assert.match(alertText(t.document), new RegExp(LABELS.field_service));
    assert.doesNotMatch(alertText(t.document), /at most \d+ (groups|cards|bookmarks)/);
    click(t.document.querySelector(".mm-edit-alert button[data-path]"));
    const dialog = sheet(t.document);
    assert.ok(dialog, `${code}: the card's form is open`);
    assert.equal(dialog.querySelector('.field-error[data-for="service"]').textContent, message, code);
    assert.equal(dialog.querySelector('select[name="service"]').getAttribute("aria-invalid"), "true");
  }
});

test("leaving_edit_mode_draws_bound_cards_with_the_services_again", async () => {
  const t = setup({ services: SERVICES, layout: layoutWith(bound(1, ["status"])) });
  t.start();
  click(act(bar(t.document), "cancel"));
  await tick();
  const el = t.root.querySelector(".mm-card.mm-bound");
  assert.ok(el, "the card is drawn as a bound card, not as a plain link");
  assert.equal(el.getAttribute("data-service"), "1");
  // No answer yet (the values come in the next task): the card says it is waiting.
  assert.equal(el.querySelector(".mm-svc-notice").textContent, LABELS.service_waiting);
  assert.equal(el.querySelector(".mm-status").getAttribute("data-status"), "unknown");
});

test("every_label_the_service_field_uses_is_listed_and_translated", () => {
  const source = readFileSync(new URL("../static/magpie/edit.js", import.meta.url), "utf8");
  const listed = source.match(/export const LABEL_KEYS = \[([\s\S]*?)\];/)[1];
  for (const key of ["field_service", "service_none", "service_missing", "help_service", "field_metrics", "help_metrics", "metrics_full", "metrics_gone", "err_service_metrics", "err_bad_service", "err_bad_metric"]) {
    assert.ok(listed.includes(`"${key}"`), `LABEL_KEYS lacks ${key}`);
    assert.ok(source.includes(`L("${key}"`), `${key} is used`);
    assert.ok(LABELS[key] && NB[key] && LABELS[key] !== NB[key], `${key} in both languages`);
  }
});

test("numbers_keep_the_cards_order_and_new_ones_follow_in_the_services_order", async () => {
  // The card shows checked first, then status; latency is added.
  const t = setup({ services: SERVICES, layout: layoutWith(bound(1, ["checked", "status"])) });
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "edit"));
  const dialog = sheet(t.document);
  assert.deepEqual(boxes(dialog).map(boxOn), [true, false, true]);
  setBox(boxes(dialog)[1], true);
  submit(dialog);
  await tick();
  assert.deepEqual(ed.draft().groups[0].cards[0].service.metrics, ["checked", "status", "latency_ms"]);

  // Clearing one in the middle keeps the order of the rest.
  click(act(item(t.root, "card", "c1"), "edit"));
  const again = sheet(t.document);
  setBox(boxes(again)[0], false);
  submit(again);
  await tick();
  assert.deepEqual(ed.draft().groups[0].cards[0].service.metrics, ["checked", "latency_ms"]);

  // Opening and saving a form unchanged is no change, even in a "foreign" order.
  const t2 = setup({ services: SERVICES, layout: layoutWith(bound(1, ["checked", "status"])) });
  const ed2 = t2.start();
  click(act(item(t2.root, "card", "c1"), "edit"));
  submit(sheet(t2.document));
  await tick();
  assert.equal(ed2.isDirty(), false);

  // Another service gets its own order, not the card's.
  click(act(item(t2.root, "card", "c1"), "edit"));
  const other = sheet(t2.document);
  chooseService(other, "2");
  setBox(boxes(other)[2], true);
  submit(other);
  await tick();
  assert.deepEqual(ed2.draft().groups[0].cards[0].service, { id: 2, metrics: ["a", "c"] });
});

test("a_number_the_service_no_longer_has_is_named_in_the_form_and_dropped_on_save", async () => {
  const t = setup({ services: SERVICES, layout: layoutWith(bound(1, ["status", "uptime_old"])) });
  const ed = t.start();
  click(act(item(t.root, "card", "c1"), "edit"));
  const dialog = sheet(t.document);
  const note = metricsPicker(dialog).querySelector('[role="status"]');
  assert.equal(note.hasAttribute("hidden"), false);
  assert.equal(note.textContent, LABELS.metrics_gone.replace("{names}", "uptime_old"));
  assert.deepEqual(boxes(dialog).map(boxOn), [true, false, false]);
  submit(dialog);
  await tick();
  assert.deepEqual(ed.draft().groups[0].cards[0].service, { id: 1, metrics: ["status"] });

  // Nothing missing: no note.
  const t2 = setup({ services: SERVICES, layout: layoutWith(bound(1, ["status"])) });
  t2.start();
  click(act(item(t2.root, "card", "c1"), "edit"));
  assert.equal(metricsPicker(sheet(t2.document)).querySelector('[role="status"]').hasAttribute("hidden"), true);
});

test("the_numbers_picker_has_room_above_its_legend", () => {
  // The picker is a fieldset inside the Service field: without a rule its legend sits right under
  // the hint of the select.
  const css = readFileSync(new URL("../static/magpie.css", import.meta.url), "utf8");
  const start = css.indexOf(".field[data-metrics] {");
  assert.ok(start >= 0, "the rule is in magpie.css");
  assert.match(css.slice(start, css.indexOf("}", start)), /margin-block-start: var\(--sp-3\)/);
});

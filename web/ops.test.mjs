// Tests for static/magpie/ops.js: pure layout operations.
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  addBookmark,
  addCard,
  addGroup,
  moveBookmark,
  moveCard,
  moveGroup,
  newId,
  removeBookmark,
  removeCard,
  removeGroup,
  renameGroup,
  setBackground,
  updateBookmark,
  updateCard,
} from "../static/magpie/ops.js";

function card(id, title = id) {
  return { id, title, description: "", url: `https://${id}.example/`, icon: { emoji: null, colour: "gray" } };
}

function base() {
  return {
    version: 7,
    background: { kind: "preset", name: "hedgerow" },
    groups: [
      { id: "g1", title: "Media", cards: [card("c1"), card("c2"), card("c3")] },
      { id: "g2", title: "Tools", cards: [card("c4")] },
      { id: "g3", title: "Empty", cards: [] },
    ],
    bookmarks: [
      { id: "b1", title: "One", url: "https://one.example/" },
      { id: "b2", title: "Two", url: "https://two.example/" },
      { id: "b3", title: "Three", url: "https://three.example/" },
    ],
  };
}

function ids(layout) {
  const all = [];
  for (const g of layout.groups) {
    all.push(g.id);
    for (const c of g.cards) all.push(c.id);
  }
  for (const b of layout.bookmarks) all.push(b.id);
  return all;
}

const cardIds = (layout, groupId) => layout.groups.find((g) => g.id === groupId).cards.map((c) => c.id);

/** Runs `op` on a deep-frozen layout: a mutation throws (modules are strict). */
function frozen(op) {
  const layout = structuredClone(base());
  const freeze = (o) => {
    if (o && typeof o === "object") {
      Object.freeze(o);
      Object.values(o).forEach(freeze);
    }
  };
  freeze(layout);
  const before = JSON.stringify(layout);
  const out = op(layout);
  assert.equal(JSON.stringify(layout), before, "the input changed");
  return out;
}

test("newId_makes_twelve_url_safe_characters_that_differ", () => {
  const seen = new Set();
  for (let i = 0; i < 500; i++) {
    const id = newId();
    assert.match(id, /^[A-Za-z0-9_-]{12}$/);
    seen.add(id);
  }
  assert.equal(seen.size, 500);
});

test("addGroup_appends_an_empty_group_with_a_fresh_id", () => {
  const out = frozen((l) => addGroup(l, "New"));
  assert.equal(out.groups.length, 4);
  const added = out.groups[3];
  assert.equal(added.title, "New");
  assert.deepEqual(added.cards, []);
  assert.match(added.id, /^[A-Za-z0-9_-]{12}$/);
  assert.equal(out.version, 7);
});

test("renameGroup_changes_only_that_title", () => {
  const out = frozen((l) => renameGroup(l, "g2", "Utilities"));
  assert.deepEqual(out.groups.map((g) => g.title), ["Media", "Utilities", "Empty"]);
  assert.deepEqual(cardIds(out, "g2"), ["c4"]);
});

test("removeGroup_takes_its_cards_with_it", () => {
  const out = frozen((l) => removeGroup(l, "g1"));
  assert.deepEqual(out.groups.map((g) => g.id), ["g2", "g3"]);
  assert.ok(!ids(out).includes("c1"));
  assert.equal(out.bookmarks.length, 3);
});

test("moveGroup_puts_the_group_at_the_final_index", () => {
  assert.deepEqual(frozen((l) => moveGroup(l, "g1", 2)).groups.map((g) => g.id), ["g2", "g3", "g1"]);
  assert.deepEqual(frozen((l) => moveGroup(l, "g3", 0)).groups.map((g) => g.id), ["g3", "g1", "g2"]);
  assert.deepEqual(frozen((l) => moveGroup(l, "g2", 1)).groups.map((g) => g.id), ["g1", "g2", "g3"]);
});

test("moveGroup_clamps_the_index", () => {
  assert.deepEqual(frozen((l) => moveGroup(l, "g1", 99)).groups.map((g) => g.id), ["g2", "g3", "g1"]);
  assert.deepEqual(frozen((l) => moveGroup(l, "g3", -5)).groups.map((g) => g.id), ["g3", "g1", "g2"]);
});

test("addCard_appends_to_the_group_and_gives_the_card_an_id", () => {
  const out = frozen((l) =>
    addCard(l, "g3", { title: "New", description: "", url: "https://n.example/", icon: { emoji: null, colour: "red" } }),
  );
  assert.equal(out.groups[2].cards.length, 1);
  assert.equal(out.groups[2].cards[0].title, "New");
  assert.match(out.groups[2].cards[0].id, /^[A-Za-z0-9_-]{12}$/);
});

test("addCard_keeps_ids_unique_even_when_the_card_brings_a_used_one", () => {
  const out = frozen((l) => addCard(l, "g2", card("c1")));
  const all = ids(out);
  assert.equal(new Set(all).size, all.length);
  assert.equal(out.groups[1].cards.length, 2);
});

test("addCard_to_an_unknown_group_changes_nothing", () => {
  const out = frozen((l) => addCard(l, "nope", card("x")));
  assert.deepEqual(out, base());
});

test("updateCard_merges_the_patch_and_never_changes_the_id", () => {
  const out = frozen((l) =>
    updateCard(l, "c2", { id: "evil", title: "Renamed", icon: { emoji: "x", colour: "red" } }),
  );
  const c = out.groups[0].cards[1];
  assert.equal(c.id, "c2");
  assert.equal(c.title, "Renamed");
  assert.deepEqual(c.icon, { emoji: "x", colour: "red" });
  assert.equal(c.url, "https://c2.example/");
});

test("removeCard_removes_one_card", () => {
  const out = frozen((l) => removeCard(l, "c2"));
  assert.deepEqual(cardIds(out, "g1"), ["c1", "c3"]);
});

test("removeCard_can_empty_a_group", () => {
  const out = frozen((l) => removeCard(l, "c4"));
  assert.deepEqual(cardIds(out, "g2"), []);
  assert.equal(out.groups.length, 3);
});

test("moveCard_inside_a_group_uses_the_final_index", () => {
  assert.deepEqual(cardIds(frozen((l) => moveCard(l, "c1", "g1", 2)), "g1"), ["c2", "c3", "c1"]);
  assert.deepEqual(cardIds(frozen((l) => moveCard(l, "c3", "g1", 0)), "g1"), ["c3", "c1", "c2"]);
});

test("moveCard_to_another_group_at_an_index", () => {
  const out = frozen((l) => moveCard(l, "c2", "g2", 0));
  assert.deepEqual(cardIds(out, "g1"), ["c1", "c3"]);
  assert.deepEqual(cardIds(out, "g2"), ["c2", "c4"]);
  const end = frozen((l) => moveCard(l, "c2", "g2", 1));
  assert.deepEqual(cardIds(end, "g2"), ["c4", "c2"]);
});

test("moveCard_the_last_card_out_of_a_group_leaves_it_empty", () => {
  const out = frozen((l) => moveCard(l, "c4", "g1", 1));
  assert.deepEqual(cardIds(out, "g2"), []);
  assert.deepEqual(cardIds(out, "g1"), ["c1", "c4", "c2", "c3"]);
});

test("moveCard_into_an_empty_group", () => {
  const out = frozen((l) => moveCard(l, "c1", "g3", 0));
  assert.deepEqual(cardIds(out, "g3"), ["c1"]);
  assert.deepEqual(cardIds(out, "g1"), ["c2", "c3"]);
});

test("moveCard_clamps_the_index_and_ignores_unknown_ids", () => {
  assert.deepEqual(cardIds(frozen((l) => moveCard(l, "c1", "g2", 99)), "g2"), ["c4", "c1"]);
  assert.deepEqual(frozen((l) => moveCard(l, "nope", "g2", 0)), base());
  assert.deepEqual(frozen((l) => moveCard(l, "c1", "nope", 0)), base());
});

test("moveCard_keeps_every_card_exactly_once", () => {
  const out = frozen((l) => moveCard(l, "c3", "g2", 0));
  const all = ids(out);
  assert.equal(new Set(all).size, all.length);
  assert.equal(all.length, ids(base()).length);
});

test("bookmark_operations", () => {
  const added = frozen((l) => addBookmark(l, { title: "Four", url: "https://four.example/" }));
  assert.equal(added.bookmarks.length, 4);
  assert.match(added.bookmarks[3].id, /^[A-Za-z0-9_-]{12}$/);

  const updated = frozen((l) => updateBookmark(l, "b2", { id: "evil", title: "Deux" }));
  assert.deepEqual(updated.bookmarks[1], { id: "b2", title: "Deux", url: "https://two.example/" });

  const removed = frozen((l) => removeBookmark(l, "b1"));
  assert.deepEqual(removed.bookmarks.map((b) => b.id), ["b2", "b3"]);

  assert.deepEqual(frozen((l) => moveBookmark(l, "b1", 2)).bookmarks.map((b) => b.id), ["b2", "b3", "b1"]);
  assert.deepEqual(frozen((l) => moveBookmark(l, "b3", 0)).bookmarks.map((b) => b.id), ["b3", "b1", "b2"]);
  assert.deepEqual(frozen((l) => moveBookmark(l, "b1", 50)).bookmarks.map((b) => b.id), ["b2", "b3", "b1"]);
});

test("addBookmark_keeps_ids_unique", () => {
  const out = frozen((l) => addBookmark(l, { id: "g1", title: "x", url: "https://x.example/" }));
  const all = ids(out);
  assert.equal(new Set(all).size, all.length);
});

test("setBackground_replaces_only_the_background", () => {
  const out = frozen((l) => setBackground(l, { kind: "preset", name: "hav" }));
  assert.deepEqual(out.background, { kind: "preset", name: "hav" });
  assert.deepEqual(out.groups, base().groups);
});

test("results_do_not_share_changed_parts_with_the_input", () => {
  const input = base();
  const out = addCard(input, "g1", card("zz"));
  out.groups[0].cards[0].title = "changed";
  assert.equal(input.groups[0].cards[0].title, "c1");
  assert.equal(input.groups[0].cards.length, 3);
});

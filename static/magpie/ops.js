// Mindful Magpie: pure operations on the start page layout.
//
// Every function takes a layout and returns a new one. None touches the DOM
// and none changes its input (the result is a copy). An id that is not in the
// layout is not an error: the layout comes back unchanged. The limits and the
// text rules are not checked here (rules.js and the server do that).

const ID_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
const ID_LENGTH = 12;

/** A random id: 12 characters of `[A-Za-z0-9_-]`. */
export function newId() {
  const bytes = new Uint8Array(ID_LENGTH);
  const crypto = globalThis.crypto;
  if (crypto && typeof crypto.getRandomValues === "function") {
    crypto.getRandomValues(bytes);
  } else {
    for (let i = 0; i < ID_LENGTH; i++) bytes[i] = Math.floor(Math.random() * 256);
  }
  // 64 characters and 256 values: every byte maps evenly, no bias.
  return Array.from(bytes, (b) => ID_CHARS[b & 63]).join("");
}

const copy = (layout) => structuredClone(layout);

function usedIds(layout) {
  const used = new Set();
  for (const g of layout.groups) {
    used.add(g.id);
    for (const c of g.cards) used.add(c.id);
  }
  for (const b of layout.bookmarks) used.add(b.id);
  return used;
}

/** `wanted` if it is free in `layout`, else a fresh id. */
function freeId(layout, wanted) {
  const used = usedIds(layout);
  if (typeof wanted === "string" && wanted !== "" && !used.has(wanted)) return wanted;
  let id = newId();
  while (used.has(id)) id = newId();
  return id;
}

const clamp = (index, length) => Math.max(0, Math.min(length, Number.isFinite(index) ? Math.trunc(index) : length));

/** Merges `patch` into `item`; the id never changes. */
function merge(item, patch) {
  const next = { ...item, ...structuredClone(patch) };
  next.id = item.id;
  return next;
}

export function addGroup(layout, title) {
  const out = copy(layout);
  out.groups.push({ id: freeId(out), title, cards: [] });
  return out;
}

export function renameGroup(layout, groupId, title) {
  const out = copy(layout);
  const group = out.groups.find((g) => g.id === groupId);
  if (group) group.title = title;
  return out;
}

export function removeGroup(layout, groupId) {
  const out = copy(layout);
  out.groups = out.groups.filter((g) => g.id !== groupId);
  return out;
}

/** Moves a group so that it ends at `toIndex` of the new list. */
export function moveGroup(layout, groupId, toIndex) {
  const out = copy(layout);
  const from = out.groups.findIndex((g) => g.id === groupId);
  if (from < 0) return out;
  const [group] = out.groups.splice(from, 1);
  out.groups.splice(clamp(toIndex, out.groups.length), 0, group);
  return out;
}

/** Adds `card` at the end of the group. A missing or used id is replaced. */
export function addCard(layout, groupId, card) {
  const out = copy(layout);
  const group = out.groups.find((g) => g.id === groupId);
  if (!group) return out;
  group.cards.push({ ...structuredClone(card), id: freeId(out, card?.id) });
  return out;
}

export function updateCard(layout, cardId, patch) {
  const out = copy(layout);
  for (const group of out.groups) {
    const at = group.cards.findIndex((c) => c.id === cardId);
    if (at >= 0) group.cards[at] = merge(group.cards[at], patch);
  }
  return out;
}

export function removeCard(layout, cardId) {
  const out = copy(layout);
  for (const group of out.groups) group.cards = group.cards.filter((c) => c.id !== cardId);
  return out;
}

/**
 * Moves a card to the group `toGroupId` so that it ends at `toIndex` of that
 * group's new list (the card is taken out first, then put in).
 */
export function moveCard(layout, cardId, toGroupId, toIndex) {
  const out = copy(layout);
  const target = out.groups.find((g) => g.id === toGroupId);
  const source = out.groups.find((g) => g.cards.some((c) => c.id === cardId));
  if (!target || !source) return out;
  const [card] = source.cards.splice(
    source.cards.findIndex((c) => c.id === cardId),
    1,
  );
  target.cards.splice(clamp(toIndex, target.cards.length), 0, card);
  return out;
}

export function addBookmark(layout, bookmark) {
  const out = copy(layout);
  out.bookmarks.push({ ...structuredClone(bookmark), id: freeId(out, bookmark?.id) });
  return out;
}

export function updateBookmark(layout, id, patch) {
  const out = copy(layout);
  const at = out.bookmarks.findIndex((b) => b.id === id);
  if (at >= 0) out.bookmarks[at] = merge(out.bookmarks[at], patch);
  return out;
}

export function removeBookmark(layout, id) {
  const out = copy(layout);
  out.bookmarks = out.bookmarks.filter((b) => b.id !== id);
  return out;
}

/** Moves a bookmark so that it ends at `toIndex` of the new list. */
export function moveBookmark(layout, id, toIndex) {
  const out = copy(layout);
  const from = out.bookmarks.findIndex((b) => b.id === id);
  if (from < 0) return out;
  const [bookmark] = out.bookmarks.splice(from, 1);
  out.bookmarks.splice(clamp(toIndex, out.bookmarks.length), 0, bookmark);
  return out;
}

export function setBackground(layout, background) {
  const out = copy(layout);
  out.background = structuredClone(background);
  return out;
}

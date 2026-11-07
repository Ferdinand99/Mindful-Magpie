// Mindful Magpie: the start page renderer.
//
// One ES module that draws a layout (groups of link cards, bookmark pills, the
// empty state and the background) into an element. The start page and the
// browser extension both use it, so it makes no assumptions about the page:
//
//   - no fetches, no timers, no event handlers;
//   - it needs nothing from the server-rendered markup except the element it
//     is given (everything inside that element is replaced);
//   - every string it shows comes from `labels` or from the layout;
//   - text is only ever set with textContent and setAttribute, never as HTML;
//   - the only things it touches outside `root` are the `data-bg` attribute on
//     <html> and the `--mm-bg-image` custom property on <html>.
//
// The matching styles are in render.css (copy that file with this one).

const PALETTE = [
  "red",
  "orange",
  "yellow",
  "green",
  "mint",
  "teal",
  "cyan",
  "blue",
  "indigo",
  "purple",
  "pink",
  "brown",
  "gray",
];
const PRESETS = ["hedgerow", "natt", "skumring", "hav"];
const DEFAULT_PRESET = "hedgerow";
const DEFAULT_COLOUR = "gray";
// What the server accepts as an image id; anything else is not drawn, so an id
// can never end the url("...") it is written into.
const IMAGE_ID = /^[A-Za-z0-9_-]{1,64}$/;

/** The first grapheme of `text` (a letter, or a whole emoji sequence). */
function firstGrapheme(text) {
  if (typeof Intl !== "undefined" && typeof Intl.Segmenter === "function") {
    for (const part of new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(text)) {
      return part.segment;
    }
    return "";
  }
  return Array.from(text)[0] ?? "";
}

function upperCase(text, lang) {
  try {
    return text.toLocaleUpperCase(lang || undefined);
  } catch {
    // An unknown language tag: the default rules.
    return text.toUpperCase();
  }
}

/**
 * The letter on a card's tile: the first grapheme of the title, upper-cased,
 * or `?` for an empty title. `lang` (optional) picks the casing rules, for
 * example Turkish dotted capital I.
 */
export function tileLetter(title, lang) {
  const first = firstGrapheme(String(title ?? "").trim());
  if (first === "") return "?";
  // Upper-casing can make two letters (a German sharp s): keep the first.
  return firstGrapheme(upperCase(first, lang)) || "?";
}

/** The URL as a string if it is an http or https link, else null. */
function safeHref(raw) {
  if (typeof raw !== "string") return null;
  try {
    const url = new URL(raw);
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : null;
  } catch {
    return null;
  }
}

function label(labels, key) {
  const value = labels?.[key];
  return typeof value === "string" ? value : "";
}

function text(value) {
  return typeof value === "string" ? value : "";
}

function make(doc, tag, className, content) {
  const el = doc.createElement(tag);
  if (className) el.setAttribute("class", className);
  if (content !== undefined) el.textContent = content;
  return el;
}

/** An element for user text that may run right to left. */
function isolated(doc, tag, className, content) {
  const el = make(doc, tag, className, content);
  el.setAttribute("dir", "auto");
  return el;
}

function invalidMarker(doc, labels) {
  return make(doc, "span", "mm-invalid", label(labels, "invalid_link"));
}

function drawTile(doc, card, lang) {
  const icon = card?.icon ?? {};
  const colour = PALETTE.includes(icon.colour) ? icon.colour : DEFAULT_COLOUR;
  const emoji = typeof icon.emoji === "string" && icon.emoji !== "" ? icon.emoji : null;
  const tile = make(doc, "span", "mm-tile", emoji ?? tileLetter(card?.title, lang));
  tile.setAttribute("data-colour", colour);
  // Decoration: the card's name is its title.
  tile.setAttribute("aria-hidden", "true");
  return tile;
}

function drawCard(doc, card, opts) {
  const href = safeHref(card?.url);
  const el = href
    ? make(doc, "a", "mm-card glass")
    : make(doc, "div", "mm-card mm-card-invalid glass");
  if (href) {
    el.setAttribute("href", href);
    el.setAttribute("rel", "noreferrer");
  }
  el.appendChild(drawTile(doc, card, opts.lang));
  const body = make(doc, "span", "mm-card-text");
  body.appendChild(isolated(doc, "span", "mm-card-title", text(card?.title)));
  const description = text(card?.description);
  if (description !== "") {
    body.appendChild(isolated(doc, "span", "mm-card-desc", description));
  }
  if (!href) body.appendChild(invalidMarker(doc, opts.labels));
  el.appendChild(body);
  return el;
}

function drawGroup(doc, group, opts) {
  const section = make(doc, "section", "mm-group");
  const title = text(group?.title);
  const heading = isolated(doc, "h2", "mm-group-title", title);
  const template = label(opts.labels, "group_label");
  if (template !== "") {
    // split/join, not replace: the title may contain `$&` and the like.
    heading.setAttribute("aria-label", template.split("{title}").join(title));
  }
  section.appendChild(heading);
  const grid = make(doc, "div", "mm-cards");
  for (const card of Array.isArray(group?.cards) ? group.cards : []) {
    if (card && typeof card === "object") grid.appendChild(drawCard(doc, card, opts));
  }
  section.appendChild(grid);
  return section;
}

function drawBookmarks(doc, bookmarks, opts) {
  const nav = make(doc, "nav", "mm-bookmarks");
  const name = label(opts.labels, "bookmarks_label");
  if (name !== "") nav.setAttribute("aria-label", name);
  for (const bookmark of bookmarks) {
    const href = safeHref(bookmark.url);
    let pill;
    if (href) {
      pill = isolated(doc, "a", "mm-pill", text(bookmark.title));
      pill.setAttribute("href", href);
      pill.setAttribute("rel", "noreferrer");
    } else {
      pill = isolated(doc, "span", "mm-pill mm-pill-invalid", text(bookmark.title));
      pill.appendChild(doc.createTextNode(" "));
      pill.appendChild(invalidMarker(doc, opts.labels));
    }
    nav.appendChild(pill);
  }
  return nav;
}

function drawEmpty(doc, opts) {
  const card = make(doc, "div", "mm-empty glass empty-card");
  const inner = make(doc, "div", "empty");
  inner.appendChild(make(doc, "p", "empty-title", label(opts.labels, "empty")));
  card.appendChild(inner);
  return card;
}

/** Sets `data-bg` and `--mm-bg-image` on <html> for the layout's background. */
function applyBackground(doc, background, opts) {
  const html = doc.documentElement;
  let name = DEFAULT_PRESET;
  let image = null;
  if (background?.kind === "preset" && PRESETS.includes(background.name)) {
    name = background.name;
  } else if (
    background?.kind === "image" &&
    typeof background.id === "string" &&
    IMAGE_ID.test(background.id)
  ) {
    name = "image";
    const base = typeof opts.imageBase === "string" ? opts.imageBase.replace(/\/+$/, "") : "";
    image = `url("${base}/start/background/${background.id}")`;
  }
  html.setAttribute("data-bg", name);
  // Through the CSS object model: the page's CSP allows that, not a style attribute.
  if (image) html.style.setProperty("--mm-bg-image", image);
  else html.style.removeProperty("--mm-bg-image");
}

/**
 * Draws `layout` into `root`, replacing what `root` holds.
 *
 * `opts.lang` is the interface language (it picks casing rules), `opts.labels`
 * the interface strings: `empty`, `invalid_link`, `group_label` (with
 * `{title}`) and `bookmarks_label`. `opts.imageBase` (optional) is put before
 * the path of a background image, for a page on another origin.
 */
export function render(root, layout, opts = {}) {
  const doc = root.ownerDocument;
  const settings = { lang: opts.lang, labels: opts.labels ?? {}, imageBase: opts.imageBase };

  applyBackground(doc, layout?.background, settings);

  const groups = (Array.isArray(layout?.groups) ? layout.groups : []).filter(
    (g) => g && typeof g === "object",
  );
  const bookmarks = (Array.isArray(layout?.bookmarks) ? layout.bookmarks : []).filter(
    (b) => b && typeof b === "object",
  );

  const fragment = doc.createDocumentFragment();
  if (groups.length === 0 && bookmarks.length === 0) {
    fragment.appendChild(drawEmpty(doc, settings));
  } else {
    for (const group of groups) fragment.appendChild(drawGroup(doc, group, settings));
    if (bookmarks.length > 0) fragment.appendChild(drawBookmarks(doc, bookmarks, settings));
  }
  while (root.firstChild) root.removeChild(root.firstChild);
  root.appendChild(fragment);
}

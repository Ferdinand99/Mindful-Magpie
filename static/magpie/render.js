// Mindful Magpie: the start page renderer.
//
// One ES module that draws a layout (groups of link cards, bookmark pills, the
// empty state and the background) into an element. The start page and the
// browser extension both use it, so it makes no assumptions about the page:
//
//   - no fetches, no timers, no event handlers;
//   - it needs nothing from the server-rendered markup except the element it
//     is given (everything inside that element is replaced);
//   - every string it shows comes from `labels`, from the layout, or from the
//     service data it is given (names, metric labels, values: all of it text);
//   - text is only ever set with textContent and setAttribute, never as HTML;
//   - the only things it touches outside `root` are the `data-bg` attribute on
//     <html> and the `--mm-bg-image` custom property on <html>; on `root`
//     itself it adds the class `mm-root`.
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

// ---- formatting service values ----------------------------------------------

const DASH = "–";
const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB"];
// The units of a duration, largest first: seconds per unit and the letters in each language.
const DURATION_UNITS = [
  { seconds: 86400, en: "d", nb: "d" },
  { seconds: 3600, en: "h", nb: "t" },
  { seconds: 60, en: "min", nb: "min" },
  { seconds: 1, en: "s", nb: "s" },
];

// One cache per Intl constructor (keyed by the constructor itself), so a replaced Intl is not served
// from an old entry.
const formatterCaches = new WeakMap();

/** A formatter for `lang`, with a language the engine rejects retried in its default language, and an
 * engine that rejects `signDisplay` retried without it. Null when nothing works (callers fall back to plain text). */
function intl(Ctor, lang, options) {
  let cache = formatterCaches.get(Ctor);
  if (!cache) {
    cache = new Map();
    formatterCaches.set(Ctor, cache);
  }
  const key = `${lang ?? ""}|${JSON.stringify(options)}`;
  if (cache.has(key)) return cache.get(key);
  const { signDisplay, ...plain } = options;
  const attempts = [options];
  if (signDisplay !== undefined) attempts.push(plain);
  let formatter = null;
  outer: for (const opts of attempts) {
    for (const tag of [lang || undefined, undefined]) {
      try {
        formatter = new Ctor(tag, opts);
        break outer;
      } catch {
        // Try the next language, then the next option set.
      }
    }
  }
  cache.set(key, formatter);
  return formatter;
}

function isNorwegian(lang) {
  const primary = typeof lang === "string" ? lang.toLowerCase().split(/[-_]/)[0] : "";
  return primary === "nb" || primary === "nn" || primary === "no";
}

/** Milliseconds since the epoch for epoch seconds, epoch milliseconds or an ISO 8601 string; else null. */
function toMs(value) {
  let n = NaN;
  if (typeof value === "number") {
    n = value;
  } else if (typeof value === "string") {
    const trimmed = value.trim();
    if (trimmed === "") return null;
    if (!/^-?\d+(?:\.\d+)?$/.test(trimmed)) {
      const parsed = Date.parse(trimmed);
      return Number.isFinite(parsed) ? parsed : null;
    }
    n = Number(trimmed);
  }
  if (!Number.isFinite(n)) return null;
  // Seconds until about the year 5138, milliseconds beyond that.
  return Math.abs(n) < 1e11 ? n * 1000 : n;
}

/** `n` through Intl.NumberFormat; plain `String(n)` (plus `fallbackSuffix`) if the engine has nothing usable. */
function formatNumber(n, lang, options, fallbackSuffix = "") {
  // signDisplay "negative": a tiny negative number reads 0, not -0.
  const formatter = intl(Intl.NumberFormat, lang, { signDisplay: "negative", ...options });
  return formatter ? formatter.format(n) : `${n}${fallbackSuffix}`;
}

/** The minus sign the language uses ("-" in English, U+2212 in Norwegian). */
function minusSign(lang) {
  return formatNumber(-1, lang).replace(/\d.*$/, "") || "-";
}

function formatBytes(n, lang, suffix) {
  const sign = n < 0 ? -1 : 1;
  const abs = Math.abs(n);
  const top = BYTE_UNITS.length - 1;
  let unit = 0;
  while (unit < top && abs / 1024 ** (unit + 1) >= 1) unit += 1;
  // The decimal is decided after rounding, so 9.97 KiB reads "10 KiB", not "10.0 KiB".
  const digitsFor = (u, scaled) => (u === 0 || Math.round(scaled * 10) / 10 >= 10 ? 0 : 1);
  let scaled = abs / 1024 ** unit;
  let digits = digitsFor(unit, scaled);
  // 1023.97 KiB would show as 1,024 KiB: say 1.0 MiB.
  if (unit < top && Number(scaled.toFixed(digits)) >= 1024) {
    unit += 1;
    scaled = abs / 1024 ** unit;
    digits = digitsFor(unit, scaled);
  }
  const shown = formatNumber(sign * scaled, lang, {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  });
  return `${shown} ${BYTE_UNITS[unit]}${suffix}`;
}

/** `abs` rounded half up to `digits` decimals, on its decimal text ("4.25" gives 4.3, which toFixed would not). */
function roundHalfUp(abs, digits) {
  const text = String(abs);
  // Exponent notation (very small or very large): the plain arithmetic is good enough there.
  if (/e/i.test(text)) return Math.round(abs * 10 ** digits) / 10 ** digits;
  return Number(`${Math.round(Number(`${text}e${digits}`))}e-${digits}`);
}

/**
 * A whole number has no decimals ("100 %", "45 %"); from 99 up to 100 there are two ("99.87 %": an
 * uptime must not read 99), capped at 99.99 so that only an exact 100 reads 100; anything else with
 * a fraction has one ("45.3 %"). Rounded half up, and a number that rounds to a whole one has none.
 */
function formatPercent(n, lang) {
  const abs = Math.abs(n);
  let digits = 0;
  let shown = abs;
  if (!Number.isInteger(abs)) {
    if (abs >= 99 && abs < 100) {
      digits = 2;
      shown = Math.min(roundHalfUp(abs, 2), 99.99);
    } else {
      digits = 1;
      shown = roundHalfUp(abs, 1);
    }
    if (Number.isInteger(shown)) digits = 0;
  }
  return formatNumber(
    n < 0 && shown !== 0 ? -shown : shown,
    lang,
    { style: "unit", unit: "percent", minimumFractionDigits: digits, maximumFractionDigits: digits },
    "%",
  );
}

/** Seconds as the largest unit that is not zero and the next one if that is not zero either ("3 d 4 h", "1 d"); "0 s" for none. */
function formatDuration(n, lang) {
  const letters = isNorwegian(lang) ? "nb" : "en";
  let rest = Math.round(Math.abs(n));
  const counts = DURATION_UNITS.map((unit) => {
    const count = Math.floor(rest / unit.seconds);
    rest -= count * unit.seconds;
    return count;
  });
  const first = counts.findIndex((count) => count > 0);
  if (first === -1) return `0 ${DURATION_UNITS[3][letters]}`;
  const shown = [first, first + 1]
    .filter((i) => i < counts.length && counts[i] > 0)
    .map((i) => `${formatNumber(counts[i], lang)} ${DURATION_UNITS[i][letters]}`);
  return (n < 0 ? minusSign(lang) : "") + shown.join(" ");
}

/** "2 min. ago", "in 3 hr.", "now": the time from `thenMs` to `nowMs` in its biggest whole unit. */
function formatRelative(thenMs, nowMs, lang) {
  const seconds = (thenMs - nowMs) / 1000;
  const abs = Math.abs(seconds);
  let unit = "second";
  let size = 1;
  if (abs >= 86400) [unit, size] = ["day", 86400];
  else if (abs >= 3600) [unit, size] = ["hour", 3600];
  else if (abs >= 60) [unit, size] = ["minute", 60];
  const count = abs < 5 ? 0 : Math.sign(seconds) * Math.floor(abs / size);
  const formatter = intl(Intl.RelativeTimeFormat, lang, { numeric: "auto", style: "short" });
  return formatter ? formatter.format(count, unit) : `${count} ${unit}`;
}

/** The moment a `since` value points at (ms), from its `num` (epoch seconds) or its `text` (ISO); else null. */
function sinceMs(value) {
  if (typeof value === "number") return toMs(value);
  if (!value || typeof value !== "object") return typeof value === "string" ? toMs(value) : null;
  if (typeof value.num === "number" && Number.isFinite(value.num)) return toMs(value.num);
  return toMs(value.text);
}

/**
 * A metric value as the text a card shows.
 *
 * `value` is the server's `{ num, text }` (a bare number or string works too);
 * `format` is the metric's format: number, percent, bytes, bytes_per_sec,
 * duration, since or text (anything else is read as number); `lang` picks the
 * number style and the unit letters (`nb`; anything else is English); `now`
 * (optional: epoch milliseconds, epoch seconds or an ISO string) is what
 * `since` counts from, the wall clock by default.
 *
 *   number         1,234,567: grouped by locale, at most two decimals
 *   percent        0-100 in; none for a whole number ("45%"), two from 99 up to 100 ("99.87%", never
 *                  "100%" unless exactly 100), else one ("45.3%"); rounded half up; not clamped
 *   bytes          binary units B, KiB, MiB, GiB, TiB ("1.5 GiB"); TiB is the top unit
 *   bytes_per_sec  the same with "/s" ("2.5 MiB/s")
 *   duration       seconds in; the largest unit and the next one if not zero ("3 d 4 h", nb "3 d 4 t", "1 d")
 *   since          an ISO timestamp or epoch seconds, relative ("2 min. ago")
 *   text           as is
 *
 * No number and no text, or NaN or Infinity, gives an en dash. A numeric format
 * with no number but a text shows the text. It never throws.
 */
export function formatValue(value, format, lang, now) {
  let num = null;
  let str = "";
  if (typeof value === "number") num = value;
  else if (typeof value === "string") str = value;
  else if (value && typeof value === "object") {
    if (typeof value.num === "number") num = value.num;
    if (typeof value.text === "string") str = value.text;
  }
  const finite = num !== null && Number.isFinite(num);
  const fallback = str !== "" ? str : DASH;

  if (format === "text") {
    if (str !== "") return str;
    return finite ? formatNumber(num, lang, { maximumFractionDigits: 2 }) : DASH;
  }
  if (format === "since") {
    const then = sinceMs(value);
    return then === null ? fallback : formatRelative(then, toMs(now) ?? Date.now(), lang);
  }
  if (!finite) return fallback;
  switch (format) {
    case "percent":
      return formatPercent(num, lang);
    case "bytes":
      return formatBytes(num, lang, "");
    case "bytes_per_sec":
      return formatBytes(num, lang, "/s");
    case "duration":
      return formatDuration(num, lang);
    default:
      return formatNumber(num, lang, { maximumFractionDigits: 2 });
  }
}

// ---- cards bound to a service ---------------------------------------------------

const STATUSES = ["ok", "warn", "bad", "unknown"];
const STALE_FACTOR = 3;

function statusOf(raw) {
  return STATUSES.includes(raw) ? raw : "unknown";
}

function statusText(labels, status) {
  if (status === "ok") return label(labels, "status_ok");
  if (status === "warn") return label(labels, "status_warn");
  if (status === "bad") return label(labels, "status_bad");
  return label(labels, "status_unknown");
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** The card's `service` if it is `{ id: integer, metrics: [...] }`, else null (a plain card). */
function serviceRef(card) {
  const ref = card?.service;
  if (!isObject(ref) || !Number.isSafeInteger(ref.id) || !Array.isArray(ref.metrics)) return null;
  return ref;
}

/** Whether the card has a link of its own typed (an empty one is none). */
function hasOwnUrl(card) {
  return typeof card?.url === "string" && card.url !== "";
}

/**
 * Where a card opens: its own link when it has one (always wins, even a bad
 * one: the card is then drawn as invalid), else, for a card with a service, the
 * service's `link` from `services`. Null when there is nothing to open: a
 * service that is gone, or has no link.
 */
function cardHref(card, services) {
  if (hasOwnUrl(card)) return safeHref(card.url);
  const ref = Array.isArray(services) ? serviceRef(card) : null;
  if (!ref) return null;
  const service = services.find((s) => isObject(s) && s.id === ref.id);
  return service ? safeHref(service.link) : null;
}

/** True when the value is older than three intervals (`fetchedMs`, `nowMs` in ms, `intervalS` in s). */
function isStale(fetchedMs, nowMs, intervalS) {
  return Number.isFinite(intervalS) && intervalS > 0 && nowMs - fetchedMs > STALE_FACTOR * intervalS * 1000;
}

/** The age line: "2 min. ago", or "Last updated 10 min. ago" once the value is stale. */
function ageText(labels, lang, fetchedMs, nowMs, stale) {
  // A value fetched after `now` (clocks differ a little) is not "in 3 s": it is now.
  const when = formatRelative(Math.min(fetchedMs, nowMs), nowMs, lang);
  const template = stale ? label(labels, "updated") : "";
  return template !== "" ? template.split("{when}").join(when) : when;
}

function drawStatus(doc, labels, status) {
  const name = statusText(labels, status);
  const dot = make(doc, "span", "mm-status");
  dot.setAttribute("data-status", status);
  if (name !== "") {
    // Only a dot with a text is an image for a screen reader; without one it is decoration.
    dot.setAttribute("role", "img");
    dot.setAttribute("aria-label", name);
    dot.setAttribute("title", name);
  }
  return dot;
}

// ---- detail blocks ------------------------------------------------------------------
//
// A service's reading may carry `details`: blocks of kind list, bars, strip or facts (see
// src/integrations/block.rs for the shapes). Everything in them is text from outside, so it is set
// with textContent only; a link goes through safeHref; a bar's width is a CSS custom property set
// through the CSS object model. A block that is not well formed is left out, and the counts are
// capped again here, because the extension may talk to a server that does not cap them.

const BLOCK_KINDS = ["list", "bars", "strip", "facts"];
const MAX_BLOCKS = 6;
const MAX_ITEMS = 8;
const MAX_CELLS = 48;
// The lines of the first list a normal card shows.
const NORMAL_LINES = 3;
// The key a block names its title or a row's label by (the same shape the server enforces).
const DETAIL_KEY = /^[a-z0-9_.]{1,64}$/;

/** The text of a detail key in the page's labels (`detail.<key>`), "" when the key is not known. */
function detailLabel(labels, key) {
  return typeof key === "string" && DETAIL_KEY.test(key) ? label(labels, `detail.${key}`) : "";
}

/** The size of a card element: "wide" or "normal". */
function cardSizeOf(el) {
  return el.getAttribute("data-size") === "wide" ? "wide" : "normal";
}

/** An `at` (epoch seconds) as milliseconds, or null when it is not a number. */
function momentMs(at) {
  return typeof at === "number" && Number.isFinite(at) ? at * 1000 : null;
}

/**
 * A block as it is drawn: its kind, title key and the entries that can be drawn (at most the caps),
 * or null when none can (a block with nothing to show is not drawn, not even its title).
 */
function shapeBlock(block, labels) {
  const list = (value) => (Array.isArray(value) ? value.filter(isObject) : []);
  let entries;
  if (block.kind === "list") {
    entries = list(block.items)
      .filter((item) => text(item.text) !== "")
      .slice(0, MAX_ITEMS);
  } else if (block.kind === "bars") {
    entries = list(block.bars).slice(0, MAX_ITEMS);
  } else if (block.kind === "strip") {
    entries = (Array.isArray(block.cells) ? block.cells : []).slice(0, MAX_CELLS);
  } else {
    // A row with no usable label (an unknown key and no text) says nothing: it is left out.
    entries = list(block.rows)
      .map((row) => ({ row, name: detailLabel(labels, row.label_key) || text(row.label) }))
      .filter(({ name }) => name !== "")
      .slice(0, MAX_ITEMS);
  }
  return entries.length === 0 ? null : { kind: block.kind, title_key: block.title_key, entries };
}

/** The blocks of `details` that can be drawn, at most six. */
function wellFormedBlocks(details, labels) {
  if (!Array.isArray(details)) return [];
  const out = [];
  for (const block of details) {
    if (!isObject(block) || !BLOCK_KINDS.includes(block.kind)) continue;
    const shaped = shapeBlock(block, labels);
    if (shaped) out.push(shaped);
    if (out.length === MAX_BLOCKS) break;
  }
  return out;
}

/**
 * What a card of `size` draws of `blocks`: all of them when wide; when normal the first list (cut to
 * three lines) and the first bars or strip. Each is `{ block, cut }`.
 */
function blocksFor(blocks, size) {
  if (size === "wide") return blocks.map((block) => ({ block, cut: false }));
  const picked = [];
  let list = false;
  let visual = false;
  for (const block of blocks) {
    if (block.kind === "list" && !list) {
      list = true;
      picked.push({ block, cut: true });
    } else if ((block.kind === "bars" || block.kind === "strip") && !visual) {
      visual = true;
      picked.push({ block, cut: false });
    }
  }
  return picked;
}

/** A relative age in `className`, counted from the moment `thenMs` (tickAges() keeps it current). */
function drawMoment(doc, className, thenMs, ctx) {
  const el = isolated(doc, "span", className, formatRelative(Math.min(thenMs, ctx.nowMs), ctx.nowMs, ctx.lang));
  el.setAttribute("data-since", String(thenMs));
  return el;
}

function drawListItem(doc, ctx, item) {
  const li = make(doc, "li", "mm-li");
  const href = safeHref(item.href);
  const itemText = text(item.text);
  // What a changed list is matched to the one on the page by: the line's link, else its text.
  li.setAttribute("data-key", href ?? itemText);
  // The slot is always there, so that a line that gains or loses its dot keeps its place.
  const slot = make(doc, "span", "mm-li-slot");
  if (STATUSES.includes(item.status)) {
    li.setAttribute("data-status", item.status);
    slot.appendChild(drawStatus(doc, ctx.labels, item.status));
  }
  li.appendChild(slot);
  const main = make(doc, "span", "mm-li-main");
  let line;
  if (href) {
    line = isolated(doc, "a", "mm-li-text mm-li-link", itemText);
    line.setAttribute("href", href);
    line.setAttribute("rel", "noreferrer");
  } else {
    line = isolated(doc, "span", "mm-li-text", itemText);
  }
  main.appendChild(line);
  const sub = text(item.sub);
  if (sub !== "") main.appendChild(isolated(doc, "span", "mm-li-sub", sub));
  // A badge is a detail key; one the page does not know shows nothing.
  const badge = detailLabel(ctx.labels, item.badge_key);
  if (badge !== "") main.appendChild(make(doc, "span", "mm-badge", badge));
  li.appendChild(main);
  const at = momentMs(item.at);
  const right = text(item.right);
  if (at !== null) li.appendChild(drawMoment(doc, "mm-li-right", at, ctx));
  else if (right !== "") li.appendChild(isolated(doc, "span", "mm-li-right", right));
  return li;
}

function drawList(doc, ctx, block, cut) {
  const ul = make(doc, "ul", "mm-list");
  const shown = cut ? block.entries.slice(0, NORMAL_LINES) : block.entries;
  for (const item of shown) ul.appendChild(drawListItem(doc, ctx, item));
  const more = block.entries.length - shown.length;
  if (more > 0) {
    ul.appendChild(make(doc, "li", "mm-block-more", label(ctx.labels, "block_more").split("{n}").join(String(more))));
  }
  return ul;
}

function drawBars(doc, block) {
  const wrap = make(doc, "div", "mm-bars");
  for (const bar of block.entries) {
    const name = text(bar.label);
    const barText = text(bar.text);
    const pct = Number.isFinite(bar.pct) ? Math.min(100, Math.max(0, bar.pct)) : 0;
    const row = make(doc, "div", "mm-bar-row");
    row.appendChild(isolated(doc, "span", "mm-bar-label", name));
    const track = make(doc, "span", "mm-bar");
    track.setAttribute("role", "progressbar");
    track.setAttribute("aria-valuemin", "0");
    track.setAttribute("aria-valuemax", "100");
    track.setAttribute("aria-valuenow", String(Math.round(pct)));
    // What the bar says in words ("12,3 av 14 TB") is what a screen reader reads.
    if (barText !== "") track.setAttribute("aria-valuetext", barText);
    if (name !== "") track.setAttribute("aria-label", name);
    const fill = make(doc, "span", "mm-bar-fill");
    if (STATUSES.includes(bar.status)) fill.setAttribute("data-status", bar.status);
    // Through the CSS object model: the page's CSP allows that, not a style attribute.
    fill.style.setProperty("--mm-pct", `${pct}%`);
    track.appendChild(fill);
    row.appendChild(track);
    row.appendChild(isolated(doc, "span", "mm-bar-text", barText));
    wrap.appendChild(row);
  }
  return wrap;
}

function drawStrip(doc, ctx, block) {
  const strip = make(doc, "div", "mm-strip");
  const cells = block.entries;
  // The cells are colours: the strip is one picture, named by its title, with the count of what is
  // not well in its description. One that has no name has nothing to say to a screen reader.
  const bad = cells.filter((c) => c === "bad").length;
  const warn = cells.filter((c) => c === "warn").length;
  const parts = [
    detailLabel(ctx.labels, block.title_key),
    bad > 0 ? `${label(ctx.labels, "status_bad")}: ${bad}` : "",
    warn > 0 ? `${label(ctx.labels, "status_warn")}: ${warn}` : "",
  ];
  const name = parts.filter((p) => p !== "").join(", ");
  if (name !== "") {
    strip.setAttribute("role", "img");
    strip.setAttribute("aria-label", name);
  } else {
    strip.setAttribute("aria-hidden", "true");
  }
  for (const cell of cells) {
    const c = make(doc, "span", "mm-cell");
    c.setAttribute("data-status", statusOf(cell));
    strip.appendChild(c);
  }
  return strip;
}

function drawFacts(doc, ctx, block) {
  const wrap = make(doc, "div", "mm-facts");
  for (const { row, name } of block.entries) {
    const fact = make(doc, "div", "mm-fact");
    fact.appendChild(isolated(doc, "span", "mm-fact-label", name));
    const at = momentMs(row.at);
    fact.appendChild(at !== null ? drawMoment(doc, "mm-fact-value", at, ctx) : isolated(doc, "span", "mm-fact-value", text(row.value)));
    wrap.appendChild(fact);
  }
  return wrap;
}

function drawBlock(doc, ctx, block, cut, id) {
  const el = make(doc, "div", `mm-block mm-block-${block.kind}`);
  const titleText = detailLabel(ctx.labels, block.title_key);
  if (titleText !== "") {
    const title = make(doc, "span", "mm-block-title", titleText);
    title.setAttribute("id", id);
    el.appendChild(title);
    // The block is named by its visible title.
    el.setAttribute("role", "group");
    el.setAttribute("aria-labelledby", id);
  }
  if (block.kind === "list") el.appendChild(drawList(doc, ctx, block, cut));
  else if (block.kind === "bars") el.appendChild(drawBars(doc, block));
  else if (block.kind === "strip") el.appendChild(drawStrip(doc, ctx, block));
  else el.appendChild(drawFacts(doc, ctx, block));
  return el;
}

/**
 * The blocks of a reading for a card of `size`, or null when there is nothing to draw. `cardKey`
 * makes the ids of the titles (`mm-bt-<card>-<n>`) different on every card of the page.
 */
function drawBlocks(doc, details, size, opts, cardKey, nowMs) {
  const ctx = { labels: opts.labels, lang: opts.lang, nowMs };
  const picked = blocksFor(wellFormedBlocks(details, ctx.labels), size);
  if (picked.length === 0) return null;
  const wrap = make(doc, "div", "mm-blocks");
  wrap.setAttribute("data-count", String(picked.length));
  picked.forEach(({ block, cut }, i) => wrap.appendChild(drawBlock(doc, ctx, block, cut, `mm-bt-${cardKey}-${i}`)));
  return wrap;
}

/** The service part of a bound card: the notice, or the cells, the blocks, the status and the age. */
function drawService(doc, el, ref, opts) {
  const labels = opts.labels;
  const svc = make(doc, "div", "mm-svc");
  el.setAttribute("data-service", String(ref.id));
  el.setAttribute("data-metrics", JSON.stringify(ref.metrics));

  const service = opts.services.find((s) => isObject(s) && s.id === ref.id);
  if (!service) {
    svc.appendChild(make(doc, "span", "mm-svc-notice", label(labels, "service_missing")));
    return svc;
  }

  const entry = isObject(opts.values?.services) ? opts.values.services[String(ref.id)] : undefined;
  const snap = isObject(entry) ? entry : null;
  const fetchedMs = snap ? toMs(snap.fetched_at) : null;
  const nowMs = toMs(opts.values?.now) ?? Date.now();
  const foot = make(doc, "span", "mm-svc-foot");

  if (!snap || fetchedMs === null) {
    foot.appendChild(drawStatus(doc, labels, "unknown"));
    foot.appendChild(make(doc, "span", "mm-svc-notice", label(labels, "service_waiting")));
    svc.appendChild(foot);
    return svc;
  }

  const intervalS = Number.isFinite(snap.interval_s) ? snap.interval_s : service.interval_s;
  const stale = isStale(fetchedMs, nowMs, intervalS);
  // What tickAges() needs to bring the age and the stale flag up to date later.
  el.setAttribute("data-fetched", String(fetchedMs / 1000));
  if (Number.isFinite(intervalS)) el.setAttribute("data-interval", String(intervalS));
  if (stale) el.setAttribute("data-stale", "");

  const catalogue = Array.isArray(service.metrics) ? service.metrics : [];
  const shown = [];
  for (const id of ref.metrics) {
    const metric = catalogue.find((m) => isObject(m) && m.id === id);
    // A metric listed twice is drawn once.
    if (metric && !shown.includes(metric)) shown.push(metric);
  }
  if (shown.length > 0) {
    const grid = make(doc, "span", "mm-metrics");
    grid.setAttribute("data-count", String(shown.length));
    const stored = isObject(snap.values) ? snap.values : {};
    for (const metric of shown) {
      const name = text(metric.label);
      const value = formatValue(stored[metric.id], metric.format, opts.lang, nowMs);
      const cell = make(doc, "span", "mm-metric");
      // The cell truncates a long value; the title keeps all of it.
      cell.setAttribute("title", `${name}: ${value}`);
      cell.appendChild(isolated(doc, "span", "mm-metric-label", name));
      const valueEl = isolated(doc, "span", "mm-metric-value", value);
      if (metric.format === "since") {
        // The text counts from `now`: keep the moment so that tickAges() can count on.
        const then = sinceMs(stored[metric.id]);
        if (then !== null) valueEl.setAttribute("data-since", String(then));
      }
      cell.appendChild(valueEl);
      grid.appendChild(cell);
    }
    svc.appendChild(grid);
  }

  const blocks = drawBlocks(doc, snap.details, cardSizeOf(el), opts, el.getAttribute("data-card") ?? `s${ref.id}`, nowMs);
  if (blocks) svc.appendChild(blocks);

  foot.appendChild(drawStatus(doc, labels, statusOf(snap.status)));
  foot.appendChild(make(doc, "span", "mm-age", ageText(labels, opts.lang, fetchedMs, nowMs, stale)));
  svc.appendChild(foot);
  const error = text(snap.error_label);
  if (error !== "") svc.appendChild(isolated(doc, "span", "mm-svc-error", error));
  return svc;
}

/**
 * Brings the ages on `root`'s bound cards up to date without drawing again:
 * rewrites each age line and each `since` value, and sets or clears
 * `data-stale`, against `opts.now` (epoch ms, epoch seconds or an ISO string;
 * the wall clock by default).
 * `opts.lang` and `opts.labels` are the ones given to render(). The caller owns
 * the timer; the renderer has none.
 */
export function tickAges(root, opts = {}) {
  const nowMs = toMs(opts.now) ?? Date.now();
  const labels = opts.labels ?? {};
  for (const el of root.querySelectorAll(".mm-card[data-fetched]")) {
    const fetchedMs = toMs(Number(el.getAttribute("data-fetched")));
    const age = el.querySelector(".mm-age");
    if (fetchedMs === null || !age) continue;
    const stale = isStale(fetchedMs, nowMs, Number(el.getAttribute("data-interval")));
    if (stale) el.setAttribute("data-stale", "");
    else el.removeAttribute("data-stale");
    age.textContent = ageText(labels, opts.lang, fetchedMs, nowMs, stale);
  }
  for (const age of root.querySelectorAll(".mm-need-age[data-since]")) {
    const thenMs = Number(age.getAttribute("data-since"));
    if (Number.isFinite(thenMs)) age.textContent = formatRelative(Math.min(thenMs, nowMs), nowMs, opts.lang);
  }
  // The ages inside blocks (a line's "12 min ago", a fact's).
  for (const el of root.querySelectorAll(".mm-li-right[data-since], .mm-fact-value[data-since]")) {
    const thenMs = Number(el.getAttribute("data-since"));
    if (Number.isFinite(thenMs)) el.textContent = formatRelative(Math.min(thenMs, nowMs), nowMs, opts.lang);
  }
  for (const valueEl of root.querySelectorAll(".mm-metric-value[data-since]")) {
    const thenMs = Number(valueEl.getAttribute("data-since"));
    if (!Number.isFinite(thenMs)) continue;
    const shown = formatRelative(thenMs, nowMs, opts.lang);
    valueEl.textContent = shown;
    const cell = valueEl.parentNode;
    const name = cell?.querySelector(".mm-metric-label")?.textContent ?? "";
    cell?.setAttribute("title", `${name}: ${shown}`);
  }
}

/** The size a card has: its own when it names one, else the one of its service's kind, else normal. */
function cardSize(card, services) {
  if (card?.size === "wide" || card?.size === "normal") return card.size;
  const ref = Array.isArray(services) ? serviceRef(card) : null;
  const service = ref ? services.find((s) => isObject(s) && s.id === ref.id) : null;
  return service?.size === "wide" ? "wide" : "normal";
}

function drawCard(doc, card, opts) {
  const href = cardHref(card, opts.services);
  const ref = Array.isArray(opts.services) ? serviceRef(card) : null;
  // A card with a service and no link of its own, whose service has none to
  // lend (it is gone, or has no address), is not invalid: it is just not a link.
  const unlinked = !href && ref !== null && !hasOwnUrl(card);
  // The card is a box, not a link: its title is the link, stretched over the whole card by the
  // style sheet (a card may hold links of its own, in its blocks, and a link cannot hold a link).
  const el = make(doc, "div", href ? "mm-card glass" : unlinked ? "mm-card mm-card-nolink glass" : "mm-card mm-card-invalid glass");
  el.appendChild(drawTile(doc, card, opts.lang));
  const body = make(doc, "span", "mm-card-text");
  if (href) {
    const title = isolated(doc, "a", "mm-card-title mm-card-link", text(card?.title));
    title.setAttribute("href", href);
    title.setAttribute("rel", "noreferrer");
    body.appendChild(title);
  } else {
    body.appendChild(isolated(doc, "span", "mm-card-title", text(card?.title)));
  }
  const description = text(card?.description);
  if (description !== "") {
    body.appendChild(isolated(doc, "span", "mm-card-desc", description));
  }
  if (!href && !unlinked) body.appendChild(invalidMarker(doc, opts.labels));
  el.appendChild(body);
  if (ref) {
    const size = cardSize(card, opts.services);
    el.setAttribute("class", `${el.getAttribute("class")} mm-bound${size === "wide" ? " mm-card-wide" : ""}`);
    el.setAttribute("data-size", size);
    // What updateValues() finds the card by.
    if (typeof card.id === "string" && card.id !== "") el.setAttribute("data-card", card.id);
    el.appendChild(drawService(doc, el, ref, opts));
  }
  return el;
}

/** Whether two nodes can be one: text with text, or elements of the same tag and class. */
function sameKind(a, b) {
  if (a.nodeType === 3 && b.nodeType === 3) return true;
  return (
    a.nodeType === 1 &&
    b.nodeType === 1 &&
    a.nodeName === b.nodeName &&
    a.getAttribute("class") === b.getAttribute("class")
  );
}

/**
 * Makes `oldEl` (on the page) the same as `freshEl` (just drawn, not on the page) by changing it in
 * place: attributes and text are rewritten, and the children are matched. A child with a `data-key`
 * (a line of a list) is matched to the old child with the same key, wherever that stands; the others
 * are matched by position among the others. What cannot be matched (another tag or class, or one
 * more or one fewer) is replaced, added or removed. An element that stays is the same element, so a
 * focused link in a block keeps its focus, also when the list around it is reordered: the element
 * with the focus is never moved, the others are moved around it. The one style the renderer sets on
 * elements, `--mm-pct` of a bar, goes through the CSS object model.
 */
function morph(oldEl, freshEl) {
  for (const attr of [...oldEl.attributes]) {
    if (attr.name !== "style" && !freshEl.hasAttribute(attr.name)) oldEl.removeAttribute(attr.name);
  }
  for (const attr of [...freshEl.attributes]) {
    if (attr.name !== "style" && oldEl.getAttribute(attr.name) !== attr.value) oldEl.setAttribute(attr.name, attr.value);
  }
  const pct = freshEl.style?.getPropertyValue?.("--mm-pct") ?? "";
  if (pct !== "" && oldEl.style.getPropertyValue("--mm-pct") !== pct) oldEl.style.setProperty("--mm-pct", pct);
  const olds = [...oldEl.childNodes];
  const keyOf = (node) => (node.nodeType === 1 ? node.getAttribute("data-key") : null);
  // Old children with a key wait for the fresh child with the same key (in order, for a repeated
  // one); the others wait their turn by position.
  const byKey = new Map();
  const unkeyed = [];
  for (const old of olds) {
    const key = keyOf(old);
    if (key === null) unkeyed.push(old);
    else byKey.set(key, [...(byKey.get(key) ?? []), old]);
  }
  let turn = 0;
  const wanted = [...freshEl.childNodes].map((next) => {
    const key = keyOf(next);
    const prev = key === null ? (unkeyed[turn++] ?? null) : (byKey.get(key)?.shift() ?? null);
    if (!prev || !sameKind(prev, next)) return next;
    if (next.nodeType === 3) {
      if (prev.nodeValue !== next.nodeValue) prev.nodeValue = next.nodeValue;
    } else {
      morph(prev, next);
    }
    return prev;
  });
  const kept = new Set(wanted);
  for (const old of olds) if (!kept.has(old)) oldEl.removeChild(old);
  // Put them in order without moving the one that has the focus (moving an element takes its focus).
  const active = oldEl.ownerDocument?.activeElement;
  const anchor = wanted.findIndex((node) => node.nodeType === 1 && active && (node === active || node.contains?.(active)));
  let before = anchor >= 0 ? wanted[anchor] : null;
  for (let i = anchor + 1; i < wanted.length; i += 1) {
    const expected = before ? before.nextSibling : oldEl.firstChild;
    if (expected !== wanted[i]) oldEl.insertBefore(wanted[i], expected);
    before = wanted[i];
  }
  for (let i = anchor - 1; i >= 0; i -= 1) {
    if (wanted[i].nextSibling !== wanted[i + 1]) oldEl.insertBefore(wanted[i], wanted[i + 1]);
  }
}

/**
 * Brings the service part of every bound card up to date after new `opts.values`
 * (or `opts.services`) without drawing the page again. Per card it changes only
 * `.mm-svc` and the `data-fetched`, `data-interval` and `data-stale` attributes, in
 * place (see morph): the card's own element, its tile and its text stay, and so do
 * the elements of the service part that still fit (a list line, a bar), so a focused
 * link, also one inside a block, keeps focus. Only what cannot be matched (a block
 * that changed its kind, a line more or fewer) is drawn anew. A card whose service,
 * metrics or size changed is redrawn on its own (it loses focus). Plain cards are
 * not touched.
 *
 * `layout` is the one given to the last render(); `opts` the same as for
 * render(). Returns true when every bound card of the layout was found on the
 * page and every bound card on the page is in the layout. False means the page
 * and the layout differ in more than values (a card added, removed, bound or
 * unbound): the caller should render() again, when it is safe to.
 */
export function updateValues(root, layout, opts = {}) {
  const doc = root.ownerDocument;
  const settings = {
    lang: opts.lang,
    labels: opts.labels ?? {},
    imageBase: opts.imageBase,
    services: opts.services,
    values: opts.values,
    canAck: opts.canAck === true,
  };
  const onPage = new Map();
  for (const el of root.querySelectorAll(".mm-bound[data-card]")) {
    onPage.set(el.getAttribute("data-card"), el);
  }
  const handled = new Set();
  let complete = true;
  for (const group of Array.isArray(layout?.groups) ? layout.groups : []) {
    for (const card of Array.isArray(group?.cards) ? group.cards : []) {
      if (!card || typeof card !== "object") continue;
      const ref = Array.isArray(settings.services) ? serviceRef(card) : null;
      if (!ref) continue;
      const el = typeof card.id === "string" ? onPage.get(card.id) : undefined;
      if (!el || handled.has(el)) {
        complete = false;
        continue;
      }
      handled.add(el);
      if (
        el.getAttribute("data-service") !== String(ref.id) ||
        el.getAttribute("data-metrics") !== JSON.stringify(ref.metrics) ||
        el.getAttribute("data-size") !== cardSize(card, settings.services) ||
        // The service's link changed (or came or went) under a card with no link of its own.
        (el.querySelector(".mm-card-link")?.getAttribute("href") ?? null) !== (cardHref(card, settings.services) ?? null)
      ) {
        el.parentNode.replaceChild(drawCard(doc, card, settings), el);
        continue;
      }
      for (const name of ["data-fetched", "data-interval", "data-stale"]) el.removeAttribute(name);
      const old = el.querySelector(".mm-svc");
      const fresh = drawService(doc, el, ref, settings);
      if (old) morph(old, fresh);
      else el.appendChild(fresh);
    }
  }
  updateNeeds(root, layout, settings);
  return complete && handled.size === onPage.size;
}

// ---- «Trenger deg» -----------------------------------------------------------------

const NEED_LEVELS = ["warn", "bad"];
const NEEDS_TITLE_ID = "mm-needs-title";
const NEEDS_ACKED_ID = "mm-needs-acked";

function isNeed(item) {
  return (
    isObject(item) &&
    Number.isSafeInteger(item.service_id) &&
    typeof item.key === "string" &&
    item.key !== "" &&
    NEED_LEVELS.includes(item.level) &&
    typeof item.label === "string" &&
    typeof item.service_name === "string"
  );
}

/** The well-formed needs of `values.needs`, or null when the values carry no needs list. */
function needList(values) {
  if (!isObject(values) || !Array.isArray(values.needs)) return null;
  return values.needs.filter(isNeed);
}

/** The address of the first card of the layout that is bound to the service and opens an http(s) link. */
function needHref(layout, serviceId, services) {
  for (const group of Array.isArray(layout?.groups) ? layout.groups : []) {
    for (const card of Array.isArray(group?.cards) ? group.cards : []) {
      if (!card || typeof card !== "object") continue;
      const ref = serviceRef(card);
      if (!ref || ref.id !== serviceId) continue;
      const href = cardHref(card, services);
      if (href) return href;
    }
  }
  return null;
}

function separator(doc) {
  const sep = make(doc, "span", "mm-need-sep", " · ");
  sep.setAttribute("aria-hidden", "true");
  return sep;
}

function needButton(doc, className, text, template, item) {
  const button = make(doc, "button", className, text);
  button.setAttribute("type", "button");
  // The name holds the visible word and says which service and need it is for.
  if (template !== "") {
    button.setAttribute("aria-label", template.split("{name}").join(item.service_name).split("{text}").join(item.label));
  }
  return button;
}

/** One need: the chip (a link to the bound card when there is one), then its button. */
function drawNeed(doc, item, href, opts, nowMs) {
  const labels = opts.labels;
  const li = make(doc, "li", item.acked ? "mm-need mm-need-acked" : "mm-need");
  li.setAttribute("data-level", item.level);
  li.setAttribute("data-service", String(item.service_id));
  li.setAttribute("data-key", item.key);
  const chip = make(doc, href ? "a" : "span", "mm-need-chip");
  if (href) {
    chip.setAttribute("href", href);
    chip.setAttribute("rel", "noreferrer");
  }
  // The separator stays with the part before it, so when the chip is too narrow for one line it
  // breaks between the parts (the name, the text, the age), never inside the name or at a lone dot.
  const lead = make(doc, "span", "mm-need-lead");
  lead.appendChild(drawStatus(doc, labels, item.level));
  lead.appendChild(isolated(doc, "span", "mm-need-name", item.service_name));
  lead.appendChild(separator(doc));
  chip.appendChild(lead);
  const sinceMs = toMs(item.since);
  const main = make(doc, "span", "mm-need-main");
  main.appendChild(isolated(doc, "span", "mm-need-text", item.label));
  if (sinceMs !== null) main.appendChild(separator(doc));
  chip.appendChild(main);
  if (sinceMs !== null) {
    const age = make(doc, "span", "mm-need-age", formatRelative(Math.min(sinceMs, nowMs), nowMs, opts.lang));
    // What tickAges() counts from.
    age.setAttribute("data-since", String(sinceMs));
    chip.appendChild(age);
  }
  li.appendChild(chip);
  if (opts.canAck) {
    li.appendChild(
      item.acked
        ? needButton(doc, "mm-need-unack", label(labels, "need_unack"), label(labels, "need_unack_for"), item)
        : needButton(doc, "mm-need-ack", label(labels, "need_ack"), label(labels, "need_ack_for"), item),
    );
  }
  return li;
}

function needItems(doc, className, items, hrefs, opts, nowMs) {
  const list = make(doc, "ul", className);
  items.forEach((item) => list.appendChild(drawNeed(doc, item, hrefs.get(item.service_id) ?? null, opts, nowMs)));
  return list;
}

/** What decides whether the row on the page is the row to draw: everything it shows. */
function needsSignature(items, hrefs, opts) {
  return JSON.stringify([
    opts.canAck === true,
    items.map((i) => [i.service_id, i.service_name, i.key, i.level, i.label, i.since ?? null, i.acked === true, hrefs.get(i.service_id) ?? null]),
  ]);
}

/** The row: the title, the chips that still need the person, the toggle and the acknowledged ones. */
function drawNeeds(doc, items, layout, opts, open) {
  const nowMs = toMs(opts.values?.now) ?? Date.now();
  const hrefs = new Map();
  for (const item of items) hrefs.set(item.service_id, needHref(layout, item.service_id, opts.services));
  const section = make(doc, "section", "mm-needs glass");
  const title = label(opts.labels, "needs");
  section.setAttribute("data-open", open ? "true" : "false");
  section.setAttribute("data-sig", needsSignature(items, hrefs, opts));
  const heading = make(doc, "h2", "mm-needs-title", title);
  // The row is named by its visible heading (there is one row on a page, so the ids are fixed).
  if (title !== "") {
    heading.setAttribute("id", NEEDS_TITLE_ID);
    section.setAttribute("aria-labelledby", NEEDS_TITLE_ID);
  }
  section.appendChild(heading);
  const showing = items.filter((i) => i.acked !== true);
  const hidden = items.filter((i) => i.acked === true);
  if (showing.length > 0) section.appendChild(needItems(doc, "mm-needs-list", showing, hrefs, opts, nowMs));
  if (hidden.length > 0) {
    const toggle = make(
      doc,
      "button",
      "mm-needs-toggle",
      label(opts.labels, "needs_acked").split("{n}").join(String(hidden.length)),
    );
    toggle.setAttribute("type", "button");
    toggle.setAttribute("aria-expanded", open ? "true" : "false");
    toggle.setAttribute("aria-controls", NEEDS_ACKED_ID);
    const hint = label(opts.labels, "needs_show_acked");
    if (hint !== "") toggle.setAttribute("title", hint);
    section.appendChild(toggle);
    const list = needItems(doc, "mm-needs-list mm-needs-acked", hidden, hrefs, opts, nowMs);
    list.setAttribute("id", NEEDS_ACKED_ID);
    if (!open) list.setAttribute("hidden", "");
    section.appendChild(list);
  }
  return section;
}

/** The region a screen reader hears new needs from. It is on the page before anything is put in it. */
function drawNeedsLive(doc) {
  const live = make(doc, "div", "mm-needs-live mm-vh");
  live.setAttribute("role", "status");
  live.setAttribute("aria-live", "polite");
  return live;
}

const child = (root, className) => [...root.children].find((el) => el.classList.contains(className));

/** The keys of the needs the row shows as still needing the person. */
function showingKeys(section) {
  const keys = new Set();
  if (!section) return keys;
  for (const li of section.querySelectorAll(".mm-needs-list:not(.mm-needs-acked) .mm-need")) {
    keys.add(`${li.getAttribute("data-service")}:${li.getAttribute("data-key")}`);
  }
  return keys;
}

/** What the row's focus stands on, so that it can be given back to the same thing in a new row. */
function focusMark(doc, section) {
  const active = doc.activeElement;
  if (!section || !active || active === section || !section.contains(active)) return null;
  const item = active.closest?.(".mm-need");
  const kind = ["mm-need-chip", "mm-need-ack", "mm-need-unack", "mm-needs-toggle"].find((c) => active.classList.contains(c));
  if (!kind) return null;
  const shown = [...section.querySelectorAll(".mm-needs-list:not(.mm-needs-acked) .mm-need")];
  return {
    kind,
    service: item?.getAttribute("data-service"),
    key: item?.getAttribute("data-key"),
    // Where it stood among the chips that need the person, for when it is gone.
    index: item ? shown.indexOf(item) : -1,
  };
}

/**
 * Gives the focus back to what it stood on, in the new row (`section`, null when
 * the row is gone). When that is gone (the need went, or was acknowledged
 * elsewhere), the focus goes where a press of its button would have sent it: the
 * chip that took its place, the one before it, the toggle, and when the row has
 * nothing left the first link of the page. It is never left to fall on the body.
 */
function giveFocusBack(root, section, mark) {
  if (!mark) return;
  let target = null;
  if (section && mark.kind === "mm-needs-toggle") {
    target = section.querySelector(".mm-needs-toggle");
  } else if (section) {
    for (const li of section.querySelectorAll(".mm-need")) {
      if (li.getAttribute("data-service") === mark.service && li.getAttribute("data-key") === mark.key) {
        target = li.querySelector(`.${mark.kind}`);
      }
    }
  }
  if (!target && section) {
    const shown = [...section.querySelectorAll(".mm-needs-list:not(.mm-needs-acked) .mm-need")];
    const near = shown[mark.index] ?? shown[mark.index - 1];
    target = near?.querySelector("a.mm-need-chip") ?? near?.querySelector("button") ?? section.querySelector(".mm-needs-toggle");
  }
  if (!target) target = root.querySelector("a[href], button");
  target?.focus?.();
}

/**
 * Brings the row up to date with `settings.values.needs` without drawing the page
 * again: nothing is touched when the row shows what it should; otherwise the row is
 * replaced (its toggle keeps its state, and focus that stood in it goes back to the same
 * chip or button if it is still there) and the needs that are new are put into the live
 * region for a screen reader. No other focus is moved.
 */
function updateNeeds(root, layout, settings) {
  const items = needList(settings.values);
  // Values that carry no needs list say nothing about them (an older server).
  if (items === null) return;
  const doc = root.ownerDocument;
  const old = child(root, "mm-needs");
  let live = child(root, "mm-needs-live");
  if (items.length === 0) {
    const gone = focusMark(doc, old);
    if (old) old.parentNode.removeChild(old);
    if (live) live.textContent = "";
    giveFocusBack(root, null, gone);
    return;
  }
  const fresh = drawNeeds(doc, items, layout, settings, old?.getAttribute("data-open") === "true");
  if (old && old.getAttribute("data-sig") === fresh.getAttribute("data-sig")) return;
  const mark = focusMark(doc, old);
  const before = showingKeys(old);
  if (old) {
    root.replaceChild(fresh, old);
  } else {
    root.insertBefore(fresh, live ? live.nextSibling : root.firstChild);
  }
  if (!live) {
    live = drawNeedsLive(doc);
    root.insertBefore(live, root.firstChild);
  }
  live.textContent = items
    .filter((i) => i.acked !== true && !before.has(`${i.service_id}:${i.key}`))
    .map((i) => `${i.service_name}: ${i.label}`)
    .join(". ");
  giveFocusBack(root, fresh, mark);
}

/**
 * The toggle «N kvittert» of the row: shows or hides the needs that are
 * acknowledged. `button` is the toggle (`.mm-needs-toggle`); the renderer has no
 * handlers, the page wires this to a click.
 */
export function toggleAckedNeeds(button) {
  const section = button?.closest?.(".mm-needs");
  if (!section) return;
  const open = button.getAttribute("aria-expanded") !== "true";
  button.setAttribute("aria-expanded", open ? "true" : "false");
  section.setAttribute("data-open", open ? "true" : "false");
  const list = section.querySelector(".mm-needs-acked");
  if (!list) return;
  if (open) list.removeAttribute("hidden");
  else list.setAttribute("hidden", "");
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

// A host the browser's URL parser accepts can still hold a quote, a bracket or a
// semicolon, which would end the url("...") the origin is written into. Only
// plain host names, IPv4 and IPv6 literals pass, with an optional port.
const PLAIN_ORIGIN = /^https?:\/\/(?:[A-Za-z0-9.-]+|\[[0-9A-Fa-f:.]+\])(?::\d+)?$/;

/**
 * `value` as an origin (`https://host:port`, no trailing slash) if it is an
 * http or https address with a plain host, else "" (a relative path). It is
 * written into url("..."), so nothing but a checked origin may get in.
 */
function originOf(value) {
  if (typeof value !== "string") return "";
  try {
    const url = new URL(value);
    if (url.protocol !== "http:" && url.protocol !== "https:") return "";
    return PLAIN_ORIGIN.test(url.origin) ? url.origin : "";
  } catch {
    return "";
  }
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
    const base = originOf(opts.imageBase);
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
 * `{title}`), `bookmarks_label` and the service strings listed below. `opts.imageBase` (optional, an http or https
 * address; only its origin is used, anything else is ignored) is put before the
 * path of a background image, for a page on another origin.
 *
 * Cards may be bound to a service (`card.service` is `{ id, metrics: [ids] }`,
 * null or absent for a plain link card; `card.size` is "normal" or "wide", or absent for the
 * size of the service). Two more options draw them; without
 * `services` a bound card is drawn as a plain one:
 *
 *   - `opts.services`: `[{ id, name, kind, interval_s, link, size, metrics: [{ id,
 *     label, format }] }]`, the services the viewer can see (`size` is the size a card
 *     of the service has when it names none; absent reads normal). A bound id that is
 *     not in the list gets the notice `service_missing`; the card keeps its own
 *     link, and a card that has none is drawn as a plain element, not a link.
 *     A card with a service and an empty `url` opens the service's `link`; the
 *     card's own `url` always wins.
 *   - `opts.values`: `{ now, services: { "<id>": { name, kind, interval_s,
 *     status, values: { "<metric>": { num, text } }, details?, fetched_at,
 *     error_label? } } }`. `details` is the reading's blocks (see below). `now` and `fetched_at` are ISO 8601 strings or epoch
 *     seconds; ages count from `now`, never from the clock, so a page can draw
 *     the same thing twice. A service with no entry shows `service_waiting`.
 *     `status` is ok, warn, bad or unknown (anything else reads unknown).
 *
 * A bound card gets `data-service` (the id), a status dot (`.mm-status`, its
 * text in `aria-label`), a cell for each chosen metric (`.mm-metric`: label and
 * value; two columns on a normal card, up to five on a wide one), the blocks, the
 * age (`.mm-age`) and, older than three intervals, `data-stale`. A card is
 * `data-size="normal"` or `"wide"` (class `mm-card-wide`, two grid columns on a
 * wide screen). The extra labels: `service_missing`, `service_waiting`,
 * `status_ok`, `status_warn`, `status_bad`, `status_unknown`, `updated` (with
 * `{when}`) and `block_more` (with `{n}`). tickAges() keeps the ages current
 * later, updateValues() changes the new values into the page in place (so focus
 * stays, also on a link inside a block), and formatValue() is the number
 * formatting.
 *
 * Blocks: `details` is a list of `{ kind: "list" | "bars" | "strip" | "facts",
 * title_key, ... }` (see src/integrations/block.rs). A title is the label
 * `detail.<title_key>` (the `[detail]` section of the locale files; a key that is
 * not there shows no title). A normal card draws the first list, cut to three
 * lines and "+ N more", and the first bars or strip; a wide card draws all of them
 * (at most six), in two columns on a wide screen. Everything in a block is text,
 * a link goes through safeHref and opens in the same tab, a bar's width is a CSS
 * custom property. A list line may have `at` (epoch seconds; a fact too): it is drawn as
 * an age ("12 min ago", counted again by tickAges()) in place of `right` (`value`), and
 * `badge_key` (a detail key; an unknown one shows nothing); a bar may have `status`
 * (its fill is green, yellow or red). A card is a box and its title is the link,
 * stretched over the box by render.css, so links in the blocks can be links of their own.
 *
 * «Trenger deg»: `opts.values.needs` is `[{ service_id, service_name, key, level
 * (warn|bad), label, since (epoch seconds), acked }]`, the server's list for this
 * person (it applies the acknowledgements and leaves out what the person may not see).
 * When the list is there, an empty live region (`.mm-needs-live`, role status) comes
 * first in `root`; when it holds anything, so does the row (`.mm-needs`) right after
 * it, before the groups. A need that is not acknowledged is a chip, "service · text ·
 * age" with the status shape of its level, a link to the first card bound to the
 * service (same tab) or plain text, and, with `opts.canAck`, a «Kvitter» button
 * after it (`.mm-need-ack`; off unless `canAck` is true: the extension is read-only).
 * Acknowledged ones are behind the toggle «N kvittert» (`.mm-needs-toggle`, wired with
 * toggleAckedNeeds()) with an undo button each (`.mm-need-unack`, canAck only). The
 * row is absent when there is no need at all. Labels: `needs`, `need_ack`,
 * `need_ack_for` (with `{name}` and `{text}`), `need_unack`, `need_unack_for` (the same), `needs_acked`
 * (with `{n}`) and `needs_show_acked`. updateValues() brings the row up to date in
 * place and announces new needs in the live region ("service: text"); focus is never
 * moved, except that focus that stood in the row and has lost its place (the need went)
 * goes to the next chip, the toggle or the first link, never to the body.
 */
export function render(root, layout, opts = {}) {
  const doc = root.ownerDocument;
  const settings = {
    lang: opts.lang,
    labels: opts.labels ?? {},
    imageBase: opts.imageBase,
    services: opts.services,
    values: opts.values,
    canAck: opts.canAck === true,
  };

  applyBackground(doc, layout?.background, settings);

  const groups = (Array.isArray(layout?.groups) ? layout.groups : []).filter(
    (g) => g && typeof g === "object",
  );
  const bookmarks = (Array.isArray(layout?.bookmarks) ? layout.bookmarks : []).filter(
    (b) => b && typeof b === "object",
  );

  const fragment = doc.createDocumentFragment();
  // «Trenger deg» comes first: the region for a screen reader (empty, so that nothing is
  // announced on load, and there before a need appears), then the row if there is a need.
  const needs = needList(settings.values);
  if (needs !== null) {
    fragment.appendChild(drawNeedsLive(doc));
    if (needs.length > 0) fragment.appendChild(drawNeeds(doc, needs, layout, settings, false));
  }
  if (groups.length === 0 && bookmarks.length === 0) {
    fragment.appendChild(drawEmpty(doc, settings));
  } else {
    for (const group of groups) fragment.appendChild(drawGroup(doc, group, settings));
    if (bookmarks.length > 0) fragment.appendChild(drawBookmarks(doc, bookmarks, settings));
  }
  // The hook the styles hang the background on (see render.css).
  root.classList.add("mm-root");
  while (root.firstChild) root.removeChild(root.firstChild);
  root.appendChild(fragment);
}

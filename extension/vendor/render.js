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

const formatters = new Map();

/** A cached Intl formatter; a language tag the browser rejects gets its default language. */
function intl(Ctor, lang, options) {
  const key = `${Ctor.name}|${lang ?? ""}|${JSON.stringify(options)}`;
  let formatter = formatters.get(key);
  if (!formatter) {
    try {
      formatter = new Ctor(lang || undefined, options);
    } catch {
      formatter = new Ctor(undefined, options);
    }
    formatters.set(key, formatter);
  }
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

function formatNumber(n, lang, options) {
  // signDisplay "negative": a tiny negative number reads 0, not -0.
  return intl(Intl.NumberFormat, lang, { signDisplay: "negative", ...options }).format(n);
}

function formatBytes(n, lang, suffix) {
  const sign = n < 0 ? -1 : 1;
  const abs = Math.abs(n);
  const top = BYTE_UNITS.length - 1;
  let unit = 0;
  while (unit < top && abs / 1024 ** (unit + 1) >= 1) unit += 1;
  const digitsFor = (u, scaled) => (u === 0 || scaled >= 10 ? 0 : 1);
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

function formatPercent(n, lang) {
  const digits = Math.round(Math.abs(n) * 10) / 10 < 10 ? 1 : 0;
  return formatNumber(n, lang, {
    style: "unit",
    unit: "percent",
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  });
}

/** Seconds as the two largest units that are not zero ("3 d 4 h"); "0 s" for none. */
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
  return (n < 0 ? "-" : "") + shown.join(" ");
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
  return intl(Intl.RelativeTimeFormat, lang, { numeric: "auto", style: "short" }).format(count, unit);
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
 *   percent        0-100 in; one decimal under 10 ("4.3%"), none from 10; not clamped
 *   bytes          binary units B, KiB, MiB, GiB, TiB ("1.5 GiB"); TiB is the top unit
 *   bytes_per_sec  the same with "/s" ("2.5 MiB/s")
 *   duration       seconds in; the two largest units ("3 d 4 h", nb "3 d 4 t")
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
    const then = finite ? toMs(num) : toMs(str);
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

/** True when the value is older than three intervals (`fetchedMs`, `nowMs` in ms, `intervalS` in s). */
function isStale(fetchedMs, nowMs, intervalS) {
  return Number.isFinite(intervalS) && intervalS > 0 && nowMs - fetchedMs > STALE_FACTOR * intervalS * 1000;
}

/** The age line: "2 min. ago", or "Last updated 10 min. ago" once the value is stale. */
function ageText(labels, lang, fetchedMs, nowMs, stale) {
  const when = formatRelative(fetchedMs, nowMs, lang);
  const template = stale ? label(labels, "updated") : "";
  return template !== "" ? template.split("{when}").join(when) : when;
}

function drawStatus(doc, labels, status) {
  const name = statusText(labels, status);
  const dot = make(doc, "span", "mm-status");
  dot.setAttribute("data-status", status);
  dot.setAttribute("role", "img");
  if (name !== "") {
    dot.setAttribute("aria-label", name);
    dot.setAttribute("title", name);
  }
  return dot;
}

/** The service part of a bound card: the notice, or the cells, the status and the age. */
function drawService(doc, el, ref, opts) {
  const labels = opts.labels;
  const svc = make(doc, "span", "mm-svc");
  el.setAttribute("data-service", String(ref.id));

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
    if (shown.length === 4) break;
    const metric = catalogue.find((m) => isObject(m) && m.id === id);
    if (metric) shown.push(metric);
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
      cell.appendChild(isolated(doc, "span", "mm-metric-value", value));
      grid.appendChild(cell);
    }
    svc.appendChild(grid);
  }

  foot.appendChild(drawStatus(doc, labels, statusOf(snap.status)));
  foot.appendChild(make(doc, "span", "mm-age", ageText(labels, opts.lang, fetchedMs, nowMs, stale)));
  svc.appendChild(foot);
  const error = text(snap.error_label);
  if (error !== "") svc.appendChild(isolated(doc, "span", "mm-svc-error", error));
  return svc;
}

/**
 * Brings the ages on `root`'s bound cards up to date without drawing again:
 * rewrites each age line and sets or clears `data-stale` against `opts.now`
 * (epoch ms, epoch seconds or an ISO string; the wall clock by default).
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
}

function drawCard(doc, card, opts) {
  const href = safeHref(card?.url);
  const ref = Array.isArray(opts.services) ? serviceRef(card) : null;
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
  if (ref) {
    el.setAttribute("class", `${el.getAttribute("class")} mm-bound`);
    el.appendChild(drawService(doc, el, ref, opts));
  }
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
 * null or absent for a plain link card). Two more options draw them; without
 * `services` a bound card is drawn as a plain one:
 *
 *   - `opts.services`: `[{ id, name, kind, interval_s, metrics: [{ id, label,
 *     format }] }]`, the services the viewer can see. A bound id that is not in
 *     the list gets the notice `service_missing`; the card keeps its link.
 *   - `opts.values`: `{ now, services: { "<id>": { name, kind, interval_s,
 *     status, values: { "<metric>": { num, text } }, needs, fetched_at,
 *     error_label? } } }`. `now` and `fetched_at` are ISO 8601 strings or epoch
 *     seconds; ages count from `now`, never from the clock, so a page can draw
 *     the same thing twice. A service with no entry shows `service_waiting`.
 *     `status` is ok, warn, bad or unknown (anything else reads unknown).
 *
 * A bound card gets `data-service` (the id), a status dot (`.mm-status`, its
 * text in `aria-label`), up to four cells (`.mm-metric`: label and value), the
 * age (`.mm-age`) and, older than three intervals, `data-stale`. The extra
 * labels: `service_missing`, `service_waiting`, `status_ok`, `status_warn`,
 * `status_bad`, `status_unknown` and `updated` (with `{when}`). tickAges()
 * keeps the ages current later; formatValue() is the number formatting.
 */
export function render(root, layout, opts = {}) {
  const doc = root.ownerDocument;
  const settings = {
    lang: opts.lang,
    labels: opts.labels ?? {},
    imageBase: opts.imageBase,
    services: opts.services,
    values: opts.values,
  };

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
  // The hook the styles hang the background on (see render.css).
  root.classList.add("mm-root");
  while (root.firstChild) root.removeChild(root.firstChild);
  root.appendChild(fragment);
}

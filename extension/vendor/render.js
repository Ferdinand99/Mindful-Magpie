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

function formatPercent(n, lang) {
  const digits = Math.round(Math.abs(n) * 10) / 10 < 10 ? 1 : 0;
  return formatNumber(
    n,
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
 *   percent        0-100 in; one decimal under 10 ("4.3%"), none from 10; not clamped
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

/** The service part of a bound card: the notice, or the cells, the status and the age. */
function drawService(doc, el, ref, opts) {
  const labels = opts.labels;
  const svc = make(doc, "span", "mm-svc");
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
    if (shown.length === 4) break;
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
    // What updateValues() finds the card by.
    if (typeof card.id === "string" && card.id !== "") el.setAttribute("data-card", card.id);
    el.appendChild(drawService(doc, el, ref, opts));
  }
  return el;
}

/**
 * Brings the service part of every bound card up to date after new `opts.values`
 * (or `opts.services`) without drawing the page again. Per card it replaces only
 * `.mm-svc` and the `data-fetched`, `data-interval` and `data-stale` attributes;
 * the card's own element, its tile and its text stay, so a focused link keeps
 * focus. A card whose service or metrics changed is redrawn on its own (it loses
 * focus). Plain cards are not touched.
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
        el.getAttribute("data-metrics") !== JSON.stringify(ref.metrics)
      ) {
        el.parentNode.replaceChild(drawCard(doc, card, settings), el);
        continue;
      }
      for (const name of ["data-fetched", "data-interval", "data-stale"]) el.removeAttribute(name);
      const old = el.querySelector(".mm-svc");
      const fresh = drawService(doc, el, ref, settings);
      if (old) el.replaceChild(fresh, old);
      else el.appendChild(fresh);
    }
  }
  updateNeeds(root, layout, settings);
  return complete && handled.size === onPage.size;
}

// ---- «Trenger deg» -----------------------------------------------------------------

const NEED_LEVELS = ["warn", "bad"];

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

/** The address of the first card of the layout that is bound to the service and has an http(s) link. */
function needHref(layout, serviceId) {
  for (const group of Array.isArray(layout?.groups) ? layout.groups : []) {
    for (const card of Array.isArray(group?.cards) ? group.cards : []) {
      if (!card || typeof card !== "object") continue;
      const ref = serviceRef(card);
      if (!ref || ref.id !== serviceId) continue;
      const href = safeHref(card.url);
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
  // The name holds the visible word and says which need it is for.
  if (template !== "") button.setAttribute("aria-label", template.split("{text}").join(item.label));
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
  chip.appendChild(drawStatus(doc, labels, item.level));
  chip.appendChild(isolated(doc, "span", "mm-need-name", item.service_name));
  chip.appendChild(separator(doc));
  chip.appendChild(isolated(doc, "span", "mm-need-text", item.label));
  const sinceMs = toMs(item.since);
  if (sinceMs !== null) {
    chip.appendChild(separator(doc));
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
  for (const item of items) hrefs.set(item.service_id, needHref(layout, item.service_id));
  const section = make(doc, "section", "mm-needs glass");
  const title = label(opts.labels, "needs");
  if (title !== "") section.setAttribute("aria-label", title);
  section.setAttribute("data-open", open ? "true" : "false");
  section.setAttribute("data-sig", needsSignature(items, hrefs, opts));
  section.appendChild(make(doc, "h2", "mm-needs-title", title));
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
    const hint = label(opts.labels, "needs_show_acked");
    if (hint !== "") toggle.setAttribute("title", hint);
    section.appendChild(toggle);
    const list = needItems(doc, "mm-needs-list mm-needs-acked", hidden, hrefs, opts, nowMs);
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
  return { kind, service: item?.getAttribute("data-service"), key: item?.getAttribute("data-key") };
}

function giveFocusBack(section, mark) {
  if (!mark) return;
  let target = null;
  if (mark.kind === "mm-needs-toggle") {
    target = section.querySelector(".mm-needs-toggle");
  } else {
    for (const li of section.querySelectorAll(".mm-need")) {
      if (li.getAttribute("data-service") === mark.service && li.getAttribute("data-key") === mark.key) {
        target = li.querySelector(`.${mark.kind}`);
      }
    }
  }
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
    if (old) old.parentNode.removeChild(old);
    if (live) live.textContent = "";
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
    .map((i) => i.label)
    .join(". ");
  giveFocusBack(fresh, mark);
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
 * keeps the ages current later, updateValues() swaps new values in without
 * touching the cards' own elements (so focus stays), and formatValue() is the
 * number formatting.
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
 * `need_ack_for` (with `{text}`), `need_unack`, `need_unack_for`, `needs_acked`
 * (with `{n}`) and `needs_show_acked`. updateValues() brings the row up to date in
 * place and announces new needs in the live region; focus is never moved.
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

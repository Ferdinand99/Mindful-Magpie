// Mindful Magpie: the checks edit mode makes before it saves. They mirror the
// server's rules (src/layout/model.rs), so a form can mark a field at once. The
// server decides: it still checks everything, and a 422 is shown the same way.
//
// Pure: no DOM. Each check returns an object from field name to the server's
// error code (`required`, `too_long`, `bad_url`, `bad_scheme`, `bad_emoji`,
// `bad_colour`); an empty object means the values pass. Lengths count
// characters (scalar values), as the server does, not UTF-16 units.

export const LIMITS = {
  groups: 30,
  cards: 300,
  bookmarks: 100,
  title: 80,
  groupTitle: 60,
  description: 160,
  serviceMetrics: 4,
  metricId: 40,
  url: 2048,
  emojiBytes: 64,
};

export const PALETTE = [
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

/** The length in characters (scalar values). */
export const length = (text) => [...text].length;

// Rust's `str::trim` removes Unicode White_Space: JS `\s` plus U+0085.
const BLANK = /^[\s\u0085]*$/;

function checkText(value, max, required) {
  if (required && BLANK.test(value)) return "required";
  if (length(value) > max) return "too_long";
  return null;
}

/**
 * What a person typed as a link, made ready to check: surrounding blanks are
 * taken off, and an address without a scheme (`example.com`, `localhost:3000`,
 * `example.com:8080/path`) gets `https://` in front. Anything that starts with
 * a scheme (`mailto:a@b.c`, `javascript:...`) is left as typed, so that the
 * check refuses it as the wrong scheme instead of turning it into a host.
 */
export function normalizeUrl(raw) {
  const value = String(raw ?? "").trim();
  if (value === "" || value.includes("://") || /\s/.test(value)) return value;
  const scheme = /^[A-Za-z][A-Za-z0-9+.-]*:(.*)$/.exec(value);
  // `host:port` also looks like `scheme:rest`: a port is only digits, and
  // then the path, query or fragment.
  if (scheme && !/^\d{1,5}(?:[/?#]|$)/.test(scheme[1])) return value;
  return `https://${value}`;
}

/** The code for a link that would be refused, or null. */
export function urlProblem(value) {
  if (BLANK.test(value)) return "required";
  if (length(value) > LIMITS.url) return "too_long";
  if (value !== value.trim() || /\p{Cc}/u.test(value)) return "bad_url";
  let url;
  try {
    url = new URL(value);
  } catch {
    return "bad_url";
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return "bad_scheme";
  if (url.hostname === "") return "bad_url";
  // `https:example.com` and `https:///example.com` parse as links too: keep
  // only the plain `scheme://host` shape.
  const rest = value.slice(value.indexOf(":") + 1);
  if (!rest.startsWith("//") || /^[/\\]/.test(rest.slice(2))) return "bad_url";
  return null;
}

const GLUE = new Set(["‍", "︎", "️"]);
const isTag = (c) => c >= "\u{e0020}" && c <= "\u{e007f}";

function graphemeCount(text) {
  if (typeof Intl !== "undefined" && typeof Intl.Segmenter === "function") {
    let n = 0;
    for (const _ of new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(text)) n++;
    return n;
  }
  return length(text) === 0 ? 0 : 1;
}

/** The code for an emoji the server would refuse, or null. Empty is fine. */
export function emojiProblem(emoji) {
  if (emoji === "" || emoji === null || emoji === undefined) return null;
  if (new TextEncoder().encode(emoji).length > LIMITS.emojiBytes) return "bad_emoji";
  if (graphemeCount(emoji) !== 1) return "bad_emoji";
  const chars = [...emoji];
  const flag =
    chars[0] === "\u{1f3f4}" &&
    chars.at(-1) === "\u{e007f}" &&
    chars.length > 2 &&
    chars.slice(1).every(isTag);
  for (const c of chars) {
    if (/[\s\u0085]/.test(c) || /\p{Cc}/u.test(c)) return "bad_emoji";
    if (/\p{Cf}/u.test(c) && !GLUE.has(c) && !(flag && isTag(c))) return "bad_emoji";
  }
  if (!chars.some((c) => !GLUE.has(c) && !isTag(c))) return "bad_emoji";
  return null;
}

/**
 * The code for a service binding the server would refuse, or null (none is
 * fine). `service` is `{ id, metrics }` or null. Mirrors `validate` in
 * `src/layout/model.rs`. Whether the service exists and may be used, and
 * whether the metrics are its kind's, is for the server to say (`bad_service`,
 * `bad_metric`).
 */
export function serviceProblem(service) {
  if (service === null || service === undefined) return null;
  if (!Number.isInteger(service.id) || service.id < 1) return "bad_service";
  const metrics = Array.isArray(service.metrics) ? service.metrics : [];
  if (metrics.length === 0) return "required";
  if (metrics.length > LIMITS.serviceMetrics) return "too_many";
  const shaped = (id) => typeof id === "string" && id.length >= 1 && id.length <= LIMITS.metricId && /^[A-Za-z0-9_-]+$/.test(id);
  if (!metrics.every(shaped)) return "bad_id";
  if (new Set(metrics).size !== metrics.length) return "duplicate_id";
  return null;
}

function collect(found) {
  const out = {};
  for (const [field, code] of Object.entries(found)) if (code) out[field] = code;
  return out;
}

export function checkGroup({ title }) {
  return collect({ title: checkText(title, LIMITS.groupTitle, true) });
}

/**
 * `values`: `title`, `description`, `url`, `emoji` ("" for none), `colour`, and `service` (`{ id, metrics }` or null).
 * The link may be empty when there is a service, and only then.
 */
export function checkCard(values) {
  return collect({
    title: checkText(values.title, LIMITS.title, true),
    description: checkText(values.description, LIMITS.description, false),
    // A card with a service may have no link of its own: it opens the service's.
    url: values.service && values.url === "" ? null : urlProblem(values.url),
    emoji: emojiProblem(values.emoji),
    colour: PALETTE.includes(values.colour) ? null : "bad_colour",
    service: serviceProblem(values.service),
  });
}

export function checkBookmark(values) {
  return collect({
    title: checkText(values.title, LIMITS.title, true),
    url: urlProblem(values.url),
  });
}

/**
 * Reads a server field path (`groups[0].cards[2].url`, `bookmarks[1].title`,
 * `groups`, `cards`, `background.name`) into where it points:
 * `{ kind: "group" | "card" | "bookmark" | "list" | "background", group, card,
 * bookmark, field }` (indexes are numbers; absent ones are undefined).
 */
export function parsePath(path) {
  let m = /^groups\[(\d+)\]\.cards\[(\d+)\]\.(\w+)(?:\.(\w+))?$/.exec(path);
  if (m) return { kind: "card", group: +m[1], card: +m[2], field: m[4] ? `${m[3]}.${m[4]}` : m[3] };
  m = /^groups\[(\d+)\]\.(\w+)$/.exec(path);
  if (m) return { kind: "group", group: +m[1], field: m[2] };
  m = /^bookmarks\[(\d+)\]\.(\w+)$/.exec(path);
  if (m) return { kind: "bookmark", bookmark: +m[1], field: m[2] };
  if (path.startsWith("background")) return { kind: "background", field: path };
  return { kind: "list", field: path };
}

// The start-up and refresh sequence of the new-tab page without any page code, so node --test can
// drive it with fakes. storage is browser.storage; draw(view, { serverUrl, token }) paints the
// status line and the whole body; update(view, conn) swaps in new numbers without redrawing the
// cards (so focus stays where it is); tick(nowMs) moves the ages on. The connection is for what the
// body fetches itself (image backgrounds). A view is
//
//   { data, status, services, values }
//
// where data is the layout, services is its catalogue (or undefined for an old server) and values
// is the values document with `now` set to the estimated server clock (or null).
//
// Order of events: the cached layout and cached values are drawn at once; then the layout and the
// values are fetched together and drawn; then, while the tab is visible, the values are fetched
// every 30 s (an EventSource cannot send the token, so there is no stream). Those refreshes leave
// the curves (`history`) out and keep the ones the page has; the curves are fetched with the
// values on load and at most every 5 minutes after. Hiding the tab stops the timer, showing it
// refreshes at once and starts it again.
//
// The cache belongs to one server and one token. Values and the old-server back-off are tagged with
// the server address and trusted only for that server. Before anything is written, the saved
// server and token are read again: a tab that outlives a change of either stops and writes nothing,
// so it cannot put another server's numbers into the cache. `connection.onChange(cb)` lets the page
// tell the sequence about a change at once.
//
// A storage failure never leaves the page blank: a failed read draws the "not connected" state, a
// failed write is ignored because the page is already drawn.
import {
  fetchData,
  fetchValues,
  isValidData,
  keepCurves,
  isValidValues,
  nextValues,
  nextView,
  serverNowMs,
  statusWithValues,
} from "./state.js";
import { pickLabels } from "./labels.js";

export const REFRESH_MS = 30_000;
// How often a refresh takes the curves (the values' `history`) as well.
export const HISTORY_MS = 5 * 60_000;
// A server that answers 404 to the values request is from before the values API. It is not asked
// again for this long (also across new tabs, through storage).
export const MISSING_BACKOFF_MS = 5 * 60_000;
// Showing the tab again refreshes at once, unless the last attempt is this recent.
const MIN_GAP_MS = 10_000;
// The values cache is not written when it is bigger than this many characters of JSON.
export const MAX_CACHED_VALUES = 256 * 1024;

const defaultTimers = {
  set: (fn, ms) => setInterval(fn, ms),
  clear: (id) => clearInterval(id),
};

// True when some card of the layout is bound to a service, so values matter for the page.
function hasBound(data) {
  const groups = Array.isArray(data?.groups) ? data.groups : [];
  return groups.some(
    (group) =>
      Array.isArray(group?.cards) &&
      group.cards.some((card) => card && typeof card.service === "object" && card.service !== null),
  );
}

export async function start({
  storage,
  fetchImpl: rawFetch,
  language = null,
  draw,
  update = draw,
  tick = () => {},
  visibility = null,
  connection = null,
  timers = defaultTimers,
  now = Date.now,
}) {
  // The server writes some texts in the request's language (the services' metric labels, the
  // needs, the reasons of a failed reading), the page draws its own (and the detail titles) in the
  // browser's language (labels.js). Ask the server for that same language, so a card never mixes
  // two; without a language the browser's own Accept-Language goes.
  const asked = language ? pickLabels(language).lang : null;
  const fetchImpl = asked
    ? (url, opts = {}) => rawFetch(url, { ...opts, headers: { ...(opts.headers ?? {}), "Accept-Language": asked } })
    : rawFetch;
  let stored = {};
  try {
    stored =
      (await storage.local.get(["last", "lastValues", "valuesMissing", "serverUrl", "token"])) ?? {};
  } catch {
    stored = {};
  }
  const cached = isValidData(stored.last) ? stored.last : null;
  const conn = { serverUrl: stored.serverUrl, token: stored.token };
  const connected = Boolean(conn.serverUrl && conn.token);

  // ---- Control of the refresh (declared first: everything below may stop it) ----
  let stopped = false;
  let timer = null;
  let inFlight = null;
  let lastAttempt = now();
  // When the curves were last fetched (null: not yet, so the next fetch takes them).
  let historyAt = null;
  let blocked = false; // the token was rejected: no point in asking again
  let bound = false;
  const unschedule = () => {
    if (timer !== null) timers.clear(timer);
    timer = null;
  };
  const stop = () => {
    stopped = true;
    unschedule();
  };
  if (connection) connection.onChange(stop);

  // The saved server and token, read again: false when either differs from what this page started
  // with (or cannot be read).
  async function sameConnection() {
    try {
      const current = await storage.local.get(["serverUrl", "token"]);
      return current?.serverUrl === conn.serverUrl && current?.token === conn.token;
    } catch {
      return false;
    }
  }

  // Values, with the offset between the server's clock and this device's clock (server minus
  // device, in ms). A cache from another server, or without a usable offset, is not trusted: its
  // numbers belong elsewhere, or its ages would be wrong.
  const cv = stored.lastValues;
  const haveCache =
    connected && cv?.serverUrl === conn.serverUrl && isValidValues(cv?.values) && Number.isFinite(cv?.offsetMs);
  let values = haveCache ? cv.values : null;
  let offsetMs = haveCache ? cv.offsetMs : 0;
  const vm = stored.valuesMissing;
  let missingAt = connected && vm?.serverUrl === conn.serverUrl && Number.isFinite(vm?.at) ? vm.at : null;
  let baseStatus;
  let status;
  let data = cached;

  const inBackoff = () => missingAt !== null && now() - missingAt < MISSING_BACKOFF_MS;
  const clock = () => now() + offsetMs;
  const buildView = () => ({
    data,
    status,
    services: Array.isArray(data?.services) ? data.services : undefined,
    // The renderer counts ages from values.now, so a cached document must not claim it is from
    // now: its clock is moved on to the estimated server time.
    values: values ? { ...values, now: Math.floor(clock() / 1000) } : null,
  });

  // First paint from the cache, before any network.
  baseStatus = status = nextView(cached, undefined).status;
  draw(buildView(), conn);

  const asking = connected && !inBackoff();
  const [startResult, valuesResult] = await Promise.all([
    fetchData(fetchImpl, conn.serverUrl, conn.token),
    asking ? fetchValues(fetchImpl, conn.serverUrl, conn.token) : undefined,
  ]);
  const controller = { refresh: () => refresh(), stop };
  // The settings changed while the requests ran: the answers belong to the old connection.
  if (stopped || !(await sameConnection())) {
    stop();
    return controller;
  }
  const view = nextView(cached, startResult);
  data = view.data;
  baseStatus = view.status;
  bound = hasBound(data);

  // Takes in the answer of a values fetch: keeps, stores or drops the values and returns the kind
  // (fresh, cached, network, unauthorized, missing, or stale when the settings changed meanwhile).
  // `withHistory` false: the answer has no curves, and the page keeps the ones it has.
  async function absorb(result, withHistory = true) {
    if (!(await sameConnection())) {
      stop();
      return "stale";
    }
    if (result?.ok && !withHistory) result = { ...result, values: keepCurves(values, result.values) };
    if (result?.ok && withHistory) historyAt = now();
    const next = nextValues(values, result);
    values = next.values;
    try {
      if (next.kind === "fresh") {
        offsetMs = serverNowMs(values) - now();
        const entry = { serverUrl: conn.serverUrl, values, offsetMs };
        if (JSON.stringify(entry).length <= MAX_CACHED_VALUES) await storage.local.set({ lastValues: entry });
        else await storage.local.remove("lastValues"); // too big to keep; an older copy would be wrong
        if (missingAt !== null) await storage.local.remove("valuesMissing");
        missingAt = null;
      } else if (next.kind === "missing") {
        offsetMs = 0;
        missingAt = now();
        await storage.local.remove("lastValues");
        await storage.local.set({ valuesMissing: { serverUrl: conn.serverUrl, at: missingAt } });
      }
    } catch {
      // The cache is an optimisation; the page is already drawn.
    }
    if (next.kind === "unauthorized") blocked = true;
    return next.kind;
  }

  // The back-off must be recorded even for a layout with no bound cards: an old server has none.
  // Only a page with bound cards is marked or polled because of the answer.
  let kind;
  if (asking) {
    const answer = await absorb(valuesResult);
    if (bound) kind = answer;
  } else if (bound && connected) {
    // Inside the back-off of an old server: draw without values, as for any 404.
    values = null;
    kind = "missing";
  }
  if (stopped) return controller;
  status = statusWithValues(baseStatus, kind);
  draw(buildView(), conn);
  if (view.status === "fresh" && (await sameConnection())) {
    try {
      await storage.local.set({ last: view.data });
    } catch {
      // The cache is an optimisation; the page is already drawn.
    }
  }
  if (status === "unauthorized") blocked = true;
  lastAttempt = now();

  // ---- Refresh while the tab is visible ----
  function polling() {
    return !stopped && bound && connected && !blocked;
  }
  function hidden() {
    return visibility ? visibility.isHidden() : false;
  }
  function schedule() {
    if (timer === null && polling() && !hidden()) {
      timer = timers.set(() => refresh().catch(() => {}), REFRESH_MS);
    }
  }

  async function run() {
    lastAttempt = now();
    if (inBackoff()) return; // an old server: nothing to draw, nothing to ask
    // The ages move on even while the request is slow or fails.
    tick(clock());
    const withHistory = historyAt === null || now() - historyAt >= HISTORY_MS;
    const result = await fetchValues(fetchImpl, conn.serverUrl, conn.token, undefined, { history: withHistory });
    if (stopped) return;
    const answer = await absorb(result, withHistory);
    if (answer === "stale" || stopped) return;
    status = statusWithValues(baseStatus, answer);
    if (blocked) unschedule();
    update(buildView(), conn);
  }

  function refresh() {
    if (!polling()) return Promise.resolve();
    if (inFlight) return inFlight;
    inFlight = run().finally(() => {
      inFlight = null;
    });
    return inFlight;
  }

  if (visibility) {
    visibility.onChange(async () => {
      try {
        if (stopped) return;
        if (hidden()) {
          unschedule();
          return;
        }
        if (!polling()) return;
        if (now() - lastAttempt >= MIN_GAP_MS) await refresh();
      } catch {
        // A failed refresh leaves the page as it is; the timer tries again.
      }
      schedule();
    });
  }
  schedule();

  return controller;
}

/**
 * Whether a `storage.onChanged` event is a real change of the saved server or token: a save of the
 * same values (the options page saving again, or the connect flow reconnecting to the same server
 * with the same token) fires the event too, and must not stop the tabs that are open.
 */
export function connectionChanged(changes, area) {
  if (area !== "local" || !changes) return false;
  return ["serverUrl", "token"].some((key) => changes[key] && changes[key].oldValue !== changes[key].newValue);
}

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
// every 30 s (an EventSource cannot send the token, so there is no stream). Hiding the tab stops
// the timer, showing it refreshes at once and starts it again.
//
// A storage failure never leaves the page blank: a failed read draws the "not connected" state, a
// failed write is ignored because the page is already drawn.
import {
  fetchData,
  fetchValues,
  isValidData,
  isValidValues,
  nextValues,
  nextView,
  serverNowMs,
  statusWithValues,
} from "./state.js";

export const REFRESH_MS = 30_000;
// A server that answers 404 to the values request is from before the values API. It is not asked
// again for this long (also across new tabs, through storage).
export const MISSING_BACKOFF_MS = 5 * 60_000;
// Showing the tab again refreshes at once, unless the last attempt is this recent.
const MIN_GAP_MS = 10_000;

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
  fetchImpl,
  draw,
  update = draw,
  tick = () => {},
  visibility = null,
  timers = defaultTimers,
  now = Date.now,
}) {
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

  // Values, with the offset between the server's clock and this device's clock (server minus
  // device, in ms). A cache without a usable offset is not trusted: its ages would be wrong.
  const cv = stored.lastValues;
  const haveCache = isValidValues(cv?.values) && Number.isFinite(cv?.offsetMs);
  let values = haveCache ? cv.values : null;
  let offsetMs = haveCache ? cv.offsetMs : 0;
  let missingAt = Number.isFinite(stored.valuesMissing) ? stored.valuesMissing : null;
  let baseStatus;
  let status;
  let data = cached;
  let blocked = false; // the token was rejected: no point in asking again

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
  const view = nextView(cached, startResult);
  data = view.data;
  baseStatus = view.status;
  const bound = hasBound(data);

  // Takes in the answer of a values fetch: keeps, stores or drops the values and returns the kind
  // (fresh, cached, network, unauthorized, missing).
  async function absorb(result) {
    const next = nextValues(values, result);
    values = next.values;
    try {
      if (next.kind === "fresh") {
        offsetMs = serverNowMs(values) - now();
        await storage.local.set({ lastValues: { values, offsetMs } });
        if (missingAt !== null) await storage.local.remove("valuesMissing");
        missingAt = null;
      } else if (next.kind === "missing") {
        offsetMs = 0;
        missingAt = now();
        await storage.local.remove("lastValues");
        await storage.local.set({ valuesMissing: missingAt });
      }
    } catch {
      // The cache is an optimisation; the page is already drawn.
    }
    if (next.kind === "unauthorized") blocked = true;
    return next.kind;
  }

  let kind;
  if (bound && asking) {
    kind = await absorb(valuesResult);
  } else if (bound && connected) {
    // Inside the back-off of an old server: draw without values, as for any 404.
    values = null;
    kind = "missing";
  }
  status = statusWithValues(baseStatus, kind);
  draw(buildView(), conn);
  if (view.status === "fresh") {
    try {
      await storage.local.set({ last: view.data });
    } catch {
      // The cache is an optimisation; the page is already drawn.
    }
  }
  if (status === "unauthorized") blocked = true;

  // ---- Refresh while the tab is visible ----
  let stopped = false;
  let timer = null;
  let inFlight = null;
  let lastAttempt = now();

  const polling = () => !stopped && bound && connected && !blocked;
  const hidden = () => (visibility ? visibility.isHidden() : false);
  const unschedule = () => {
    if (timer !== null) timers.clear(timer);
    timer = null;
  };
  const schedule = () => {
    if (timer === null && polling() && !hidden()) timer = timers.set(refresh, REFRESH_MS);
  };

  async function run() {
    lastAttempt = now();
    if (inBackoff()) return; // an old server: nothing to draw, nothing to ask
    // The ages move on even while the request is slow or fails.
    tick(clock());
    const result = await fetchValues(fetchImpl, conn.serverUrl, conn.token);
    if (stopped) return;
    const answer = await absorb(result);
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
      if (stopped) return;
      if (hidden()) {
        unschedule();
        return;
      }
      if (!polling()) return;
      if (now() - lastAttempt >= MIN_GAP_MS) await refresh();
      schedule();
    });
  }
  schedule();

  return {
    refresh,
    stop() {
      stopped = true;
      unschedule();
    },
  };
}

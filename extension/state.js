// Pure view logic for the new-tab page and the options page. No browser APIs here, so it is
// testable with node --test.
//
// The token is only ever put in the Authorization header. Every failure below is mapped to a
// fixed kind, never to the original error, so the token cannot leak through an error string.

// The Norwegian status line of the new-tab page, by view status.
export const statusText = {
  fresh: "Oppdatert",
  cached: "Oppdaterer …",
  offline: "Frakoblet – viser sist lagrede",
  unauthorized: "Ugyldig token – se innstillingene",
  empty: "Ikke koblet til",
};

// A payload is usable only if it has the layout document's shape; anything else is treated as
// absent (cache) or as a network failure (response). Deeper checks belong to the renderer.
export function isValidData(d) {
  return (
    d !== null &&
    typeof d === "object" &&
    typeof d.version === "number" &&
    Array.isArray(d.groups) &&
    Array.isArray(d.bookmarks)
  );
}

export function nextView(cached, fetchResult) {
  const have = cached ?? null;
  if (fetchResult === undefined) {
    return { data: have, status: have ? "cached" : "empty" };
  }
  if (fetchResult.ok) {
    return { data: fetchResult.data, status: "fresh" };
  }
  if (fetchResult.kind === "unauthorized") {
    return { data: have, status: "unauthorized" };
  }
  return { data: have, status: have ? "offline" : "empty" };
}

// Maps an HTTP status and parsed body to a fetch result. 401 (no or revoked token) and 403 (a
// token without the read scope) both mean the user must fix the token.
export function classifyResponse(status, body) {
  if (status === 401 || status === 403) return { ok: false, kind: "unauthorized" };
  if (status === 200 && isValidData(body)) return { ok: true, data: body };
  return { ok: false, kind: "network" };
}

export function startUrl(serverUrl) {
  return `${serverUrl.replace(/\/+$/, "")}/api/v1/me/start`;
}

export function valuesUrl(serverUrl) {
  return `${serverUrl.replace(/\/+$/, "")}/api/v1/me/values`;
}

// One GET with the token in the Authorization header and nothing else. `classify(status, body)`
// maps the answer. Every failure, including timeout and abort, maps to "network", never to the
// original error, so the token cannot leak through an error string.
async function getJson(fetchImpl, url, token, timeoutMs, classify) {
  try {
    const res = await fetchImpl(url, {
      headers: { Authorization: `Bearer ${token}` },
      credentials: "omit",
      redirect: "error",
      cache: "no-store",
      signal: AbortSignal.timeout(timeoutMs),
    });
    let body = null;
    if (res.status === 200) body = await res.json();
    return classify(res.status, body);
  } catch {
    return { ok: false, kind: "network" };
  }
}

// fetchImpl is injectable for tests.
export async function fetchData(fetchImpl, serverUrl, token, timeoutMs = 5000) {
  if (!serverUrl || !token) return { ok: false, kind: "network" };
  return getJson(fetchImpl, startUrl(serverUrl), token, timeoutMs, classifyResponse);
}

// ---- Service values (GET /api/v1/me/values) ----

// The values document is `{ now, services: { "<id>": entry } }`. Entries are checked by the
// renderer; here only the shape the extension relies on (a usable clock and an object of entries).
export function isValidValues(v) {
  return (
    v !== null &&
    typeof v === "object" &&
    v.services !== null &&
    typeof v.services === "object" &&
    !Array.isArray(v.services) &&
    serverNowMs(v) !== null
  );
}

/**
 * The server clock of a values document in epoch milliseconds, or null. `now` is epoch seconds
 * (what the server sends), epoch milliseconds or an ISO 8601 string.
 */
export function serverNowMs(v) {
  const now = v?.now;
  if (typeof now === "number" && Number.isFinite(now)) return now < 1e11 ? now * 1000 : now;
  if (typeof now === "string") {
    const ms = Date.parse(now);
    return Number.isNaN(ms) ? null : ms;
  }
  return null;
}

// 401 and 403 both mean the user must fix the token. 404 is a server from before the values API.
export function classifyValuesResponse(status, body) {
  if (status === 401 || status === 403) return { ok: false, kind: "unauthorized" };
  if (status === 404) return { ok: false, kind: "missing" };
  if (status === 200 && isValidValues(body)) return { ok: true, values: body };
  return { ok: false, kind: "network" };
}

export async function fetchValues(fetchImpl, serverUrl, token, timeoutMs = 5000) {
  if (!serverUrl || !token) return { ok: false, kind: "network" };
  return getJson(fetchImpl, valuesUrl(serverUrl), token, timeoutMs, classifyValuesResponse);
}

/**
 * The values to show after a fetch. `cached` is the last stored document (or null), `result` is
 * what fetchValues returned, or undefined while it is still running. A failure keeps the cache;
 * a server without the values API (missing) drops it.
 */
export function nextValues(cached, result) {
  const have = cached ?? null;
  if (result === undefined) return { values: have, kind: "cached" };
  if (result.ok) return { values: result.values, kind: "fresh" };
  if (result.kind === "missing") return { values: null, kind: "missing" };
  return { values: have, kind: result.kind };
}

// The status line after the values answered. A rejected token wins; a failed values fetch marks an
// otherwise fresh page as showing the last stored numbers.
export function statusWithValues(status, kind) {
  if (kind === "unauthorized") return "unauthorized";
  if (kind === "network" && status === "fresh") return "offline";
  return status;
}

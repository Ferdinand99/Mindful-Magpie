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
  empty: "Ikke koblet til – åpne innstillingene",
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

// fetchImpl is injectable for tests. Every failure, including timeout and abort, maps to
// "network".
export async function fetchData(fetchImpl, serverUrl, token, timeoutMs = 5000) {
  if (!serverUrl || !token) return { ok: false, kind: "network" };
  try {
    const res = await fetchImpl(startUrl(serverUrl), {
      headers: { Authorization: `Bearer ${token}` },
      credentials: "omit",
      redirect: "error",
      cache: "no-store",
      signal: AbortSignal.timeout(timeoutMs),
    });
    let body = null;
    if (res.status === 200) body = await res.json();
    return classifyResponse(res.status, body);
  } catch {
    return { ok: false, kind: "network" };
  }
}

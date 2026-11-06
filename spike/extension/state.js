// Pure view logic for the new-tab page. No browser APIs here, so it is testable with node --test.

// A payload is usable only if it has the documented shape; anything else is treated as absent or as a network failure.
export function isValidData(d) {
  return (
    d !== null &&
    typeof d === "object" &&
    typeof d.generated_at === "string" &&
    typeof d.counter === "number" &&
    Array.isArray(d.items) &&
    d.items.every(
      (i) => i !== null && typeof i === "object" && typeof i.label === "string" && typeof i.value === "string",
    )
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

// Maps an HTTP status and parsed body to a fetch result.
export function classifyResponse(status, body) {
  if (status === 401) return { ok: false, kind: "unauthorized" };
  if (status === 200 && isValidData(body)) return { ok: true, data: body };
  return { ok: false, kind: "network" };
}

// fetchImpl is injectable for tests. Every failure, including timeout and abort, maps to "network".
export async function fetchData(fetchImpl, serverUrl, token, timeoutMs = 5000) {
  if (!serverUrl || !token) return { ok: false, kind: "network" };
  try {
    const res = await fetchImpl(`${serverUrl.replace(/\/+$/, "")}/api/spike/data`, {
      headers: { Authorization: `Bearer ${token}` },
      signal: AbortSignal.timeout(timeoutMs),
    });
    let body = null;
    if (res.status === 200) body = await res.json();
    return classifyResponse(res.status, body);
  } catch {
    return { ok: false, kind: "network" };
  }
}

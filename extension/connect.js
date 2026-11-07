// The "Koble til Magpie" connect flow without any page code, so node --test can drive it with
// fakes. The flow is OAuth-style with PKCE (RFC 7636): the extension opens the Magpie consent page
// in browser.identity.launchWebAuthFlow, gets a one-time code back on the redirect, and swaps it
// for a read-scoped API token at the token endpoint.
//
// Rules this file keeps:
// - The code, the verifier and the token are never part of an error, a status text or a log
//   line. Every failure is mapped to a fixed code or kind, never to the original error.
// - The server and the redirect are checked before anything is exchanged: the state must match.

const b64url = (bytes) =>
  btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

const randomToken = (byteLength) => b64url(crypto.getRandomValues(new Uint8Array(byteLength)));

// 32 random bytes give 43 URL-safe characters, the minimum PKCE length, with 256 bits of entropy.
export function makeVerifier() {
  return randomToken(32);
}

// BASE64URL(SHA256(ASCII(verifier))) without padding (RFC 7636, section 4.2).
export async function challengeS256(verifier) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier));
  return b64url(new Uint8Array(digest));
}

export function makeState() {
  return randomToken(16);
}

// server is the normalised address without a trailing slash (see parseServerUrl).
export function connectUrl(server, redirectUri, state, challenge, deviceName) {
  const query = [
    ["redirect_uri", redirectUri],
    ["state", state],
    ["code_challenge", challenge],
    ["code_challenge_method", "S256"],
    ["device", deviceName],
  ]
    .map(([key, value]) => `${key}=${encodeURIComponent(value)}`)
    .join("&");
  return `${server.replace(/\/+$/, "")}/connect/extension?${query}`;
}

// The only thing parseRedirect throws. The message is the fixed code, nothing from the URL.
export class ConnectError extends Error {
  constructor(code) {
    super(code);
    this.name = "ConnectError";
    this.code = code;
  }
}

// Returns { code } for a good redirect, or throws a ConnectError with one of the fixed codes
// "state_mismatch", "denied" or "no_code".
export function parseRedirect(url, expectedState) {
  let params;
  try {
    params = new URL(url).searchParams;
  } catch {
    throw new ConnectError("no_code");
  }
  const state = params.get("state");
  if (state !== null && state !== expectedState) throw new ConnectError("state_mismatch");
  if (params.get("error") === "access_denied") throw new ConnectError("denied");
  if (state === null) throw new ConnectError("state_mismatch");
  const code = params.get("code");
  if (!code) throw new ConnectError("no_code");
  return { code };
}

export function tokenUrl(server) {
  return `${server.replace(/\/+$/, "")}/api/v1/extension/token`;
}

// Returns { ok: true, token } or { ok: false, kind }. kind is "rejected" (the server refused the
// code or the verifier: HTTP 400), "network" (no answer in time, redirect, or a failed request)
// or "server" (any other answer).
export async function exchangeCode(fetchImpl, server, code, verifier, timeoutMs = 10000) {
  let res;
  try {
    res = await fetchImpl(tokenUrl(server), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code, code_verifier: verifier }),
      credentials: "omit",
      redirect: "error",
      cache: "no-store",
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch {
    return { ok: false, kind: "network" };
  }
  if (res.status === 400) return { ok: false, kind: "rejected" };
  if (res.status !== 200) return { ok: false, kind: "server" };
  try {
    const body = await res.json();
    if (typeof body?.token === "string" && body.token.length > 0) return { ok: true, token: body.token };
  } catch {
    // fall through to the fixed kind
  }
  return { ok: false, kind: "server" };
}

const systems = {
  win: "Windows",
  mac: "macOS",
  linux: "Linux",
  android: "Android",
  cros: "ChromeOS",
  openbsd: "OpenBSD",
};

// platformInfo is the answer of browser.runtime.getPlatformInfo(). The name is shown on the
// consent page and on the API token that the flow creates.
export function deviceName(platformInfo) {
  const system = systems[platformInfo?.os];
  return system ? `Firefox på ${system}` : "Firefox";
}

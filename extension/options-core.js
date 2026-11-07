// Logic of the options page without any page code, so node --test can drive it with fakes.
//
// Rules this file keeps:
// - The host permission is requested before any other await, inside the user gesture (a click on
//   a button), or Firefox refuses the prompt.
// - Only http and https addresses are accepted.
// - Nothing is saved unless the endpoint answered with a valid layout document.
// - The token is never part of a message, a URL or a log line.
import { fetchData } from "./state.js";

export const messages = {
  "invalid-url": "Adressen må være en http:// eller https://-adresse til Magpie.",
  "no-token": "Lim inn et token.",
  denied: "Tilgang til serveren ble ikke gitt.",
  unauthorized: "Serveren avviste token. Lag et nytt lese-token under Innstillinger → API-tokens.",
  network: "Fikk ikke kontakt med Magpie på den adressen.",
  tested: "Tilkoblingen virker.",
  saved: "Lagret. Tilkoblingen virker.",
};

// Returns { ok: true, serverUrl, pattern } or { ok: false }. serverUrl is the address without
// query, fragment and trailing slashes. pattern is the host permission pattern: match patterns
// take no port, so it is built from protocol and hostname only.
export function parseServerUrl(input) {
  if (typeof input !== "string") return { ok: false };
  let url;
  try {
    url = new URL(input.trim());
  } catch {
    return { ok: false };
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return { ok: false };
  if (!url.hostname || url.username || url.password) return { ok: false };
  return {
    ok: true,
    serverUrl: `${url.origin}${url.pathname.replace(/\/+$/, "")}`,
    pattern: `${url.protocol}//${url.hostname}/*`,
  };
}

const fail = (kind) => ({ ok: false, kind, message: messages[kind] });

// Tests the connection. Saves nothing. permissions.request is called synchronously: there is no
// await before it.
export function checkConnection({ permissions, fetchImpl }, { serverUrl, token }) {
  const parsed = parseServerUrl(serverUrl);
  if (!parsed.ok) return Promise.resolve(fail("invalid-url"));
  const trimmedToken = typeof token === "string" ? token.trim() : "";
  if (!trimmedToken) return Promise.resolve(fail("no-token"));
  const request = permissions.request({ origins: [parsed.pattern] });
  return finishCheck(request, fetchImpl, parsed, trimmedToken);
}

async function finishCheck(request, fetchImpl, parsed, token) {
  let granted = false;
  try {
    granted = await request;
  } catch {
    granted = false;
  }
  if (!granted) return fail("denied");
  const result = await fetchData(fetchImpl, parsed.serverUrl, token);
  if (result.ok) return { ok: true, kind: "tested", message: messages.tested, serverUrl: parsed.serverUrl };
  return fail(result.kind);
}

// Tests the connection and saves the settings only when the test succeeds. The cache belongs to
// one server, so it is dropped when the server changes.
export async function saveConnection(deps, input) {
  const checked = await checkConnection(deps, input);
  if (!checked.ok) return checked;
  const { storage } = deps;
  const { serverUrl: previous } = await storage.local.get("serverUrl");
  if (previous !== checked.serverUrl) {
    await storage.local.remove("last");
  }
  await storage.local.set({ serverUrl: checked.serverUrl, token: input.token.trim() });
  return { ok: true, kind: "saved", message: messages.saved };
}

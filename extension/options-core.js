// Logic of the options page without any page code, so node --test can drive it with fakes.
//
// Rules this file keeps:
// - The host permission is requested before any other await, inside the user gesture (a click on
//   a button), or Firefox refuses the prompt.
// - Only http and https addresses are accepted.
// - Nothing is saved unless the endpoint answered with a valid layout document.
// - The token is never part of a message, a URL or a log line.
import { fetchData } from "./state.js";
import {
  challengeS256,
  ConnectError,
  connectUrl,
  deviceName,
  exchangeCode,
  makeState,
  makeVerifier,
  parseRedirect,
} from "./connect.js";

export const messages = {
  "invalid-url": "Adressen må være en http:// eller https://-adresse til Magpie.",
  "no-token": "Lim inn et token.",
  denied: "Tilgang til serveren ble ikke gitt.",
  unauthorized: "Serveren avviste token. Lag et nytt lese-token under Innstillinger → API-tokens.",
  network: "Fikk ikke kontakt med Magpie på den adressen.",
  tested: "Tilkoblingen virker.",
  saved: "Lagret. Tilkoblingen virker.",
  connected: "Koblet til ✓",
  cancelled: "Tilkoblingen ble avbrutt, eller Magpie svarte ikke.",
  declined: "Du avslo tilgangen i Magpie.",
  "state-mismatch": "Svaret fra Magpie hørte ikke til denne tilkoblingen. Prøv igjen.",
  "no-code": "Magpie ga ikke noen tilgangskode. Prøv igjen.",
  rejected: "Magpie avviste tilgangskoden. Den kan ha utløpt. Prøv igjen.",
  server: "Magpie svarte med en feil. Prøv igjen senere.",
  storage: "Kunne ikke lagre innstillingene.",
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

// Saves server and token. The cache belongs to one server and one token, so it is dropped when
// either changes.
async function persist(storage, serverUrl, token) {
  const previous = await storage.local.get(["serverUrl", "token"]);
  if (previous.serverUrl !== serverUrl || previous.token !== token) {
    await storage.local.remove("last");
  }
  await storage.local.set({ serverUrl, token });
}

// Tests the connection and saves the settings only when the test succeeds.
export async function saveConnection(deps, input) {
  const checked = await checkConnection(deps, input);
  if (!checked.ok) return checked;
  await persist(deps.storage, checked.serverUrl, input.token.trim());
  return { ok: true, kind: "saved", message: messages.saved };
}

// The "Koble til Magpie" button. Like checkConnection it asks for the host permission before any
// other await, inside the click. serverUrl comes from the address field (the options page fills
// it with the saved address). The code, verifier and token never leave this function except as
// the request body and the stored token.
export function connectMagpie(deps, { serverUrl }) {
  const parsed = parseServerUrl(serverUrl);
  if (!parsed.ok) return Promise.resolve(fail("invalid-url"));
  const request = deps.permissions.request({ origins: [parsed.pattern] });
  return finishConnect(request, deps, parsed);
}

const redirectKinds = { state_mismatch: "state-mismatch", denied: "declined", no_code: "no-code" };

async function finishConnect(request, { storage, fetchImpl, identity, runtime }, parsed) {
  let granted = false;
  try {
    granted = await request;
  } catch {
    granted = false;
  }
  if (!granted) return fail("denied");

  let platformInfo = null;
  try {
    platformInfo = await runtime.getPlatformInfo();
  } catch {
    platformInfo = null;
  }
  const verifier = makeVerifier();
  const state = makeState();
  const url = connectUrl(
    parsed.serverUrl,
    identity.getRedirectURL(),
    state,
    await challengeS256(verifier),
    deviceName(platformInfo),
  );

  let redirected;
  try {
    redirected = await identity.launchWebAuthFlow({ url, interactive: true });
  } catch {
    return fail("cancelled");
  }
  let code;
  try {
    ({ code } = parseRedirect(redirected, state));
  } catch (error) {
    return fail(error instanceof ConnectError ? redirectKinds[error.code] : "no-code");
  }

  const exchanged = await exchangeCode(fetchImpl, parsed.serverUrl, code, verifier);
  if (!exchanged.ok) return fail(exchanged.kind);
  try {
    await persist(storage, parsed.serverUrl, exchanged.token);
  } catch {
    return fail("storage");
  }
  return { ok: true, kind: "connected", message: messages.connected };
}

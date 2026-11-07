// The start-up sequence of the new-tab page without any page code, so node --test can drive it
// with fakes. storage is browser.storage, draw(view, { serverUrl, token }) paints the status line
// and the body; the connection is for what the body fetches itself (image backgrounds).
//
// A storage failure never leaves the page blank: a failed read draws the "not connected" state, a
// failed write is ignored because the page is already drawn.
import { fetchData, isValidData, nextView } from "./state.js";

export async function start({ storage, fetchImpl, draw }) {
  let stored = {};
  try {
    stored = (await storage.local.get(["last", "serverUrl", "token"])) ?? {};
  } catch {
    stored = {};
  }
  const cached = isValidData(stored.last) ? stored.last : null;
  const conn = { serverUrl: stored.serverUrl, token: stored.token };
  // First paint from the cache, before any network.
  draw(nextView(cached, undefined), conn);

  const result = await fetchData(fetchImpl, stored.serverUrl, stored.token);
  const view = nextView(cached, result);
  draw(view, conn);
  if (view.status === "fresh") {
    try {
      await storage.local.set({ last: view.data });
    } catch {
      // The cache is an optimisation; the page is already drawn.
    }
  }
}

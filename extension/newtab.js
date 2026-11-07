import { fetchData, isValidData, nextView } from "./state.js";
import { renderPlaceholder, renderStatus } from "./placeholder.js";

const statusEl = document.getElementById("status");
const bodyEl = document.getElementById("body");

function draw(view) {
  renderStatus(statusEl, view.status, () => browser.runtime.openOptionsPage());
  renderPlaceholder(bodyEl, view.data);
}

async function start() {
  const stored = await browser.storage.local.get(["last", "serverUrl", "token"]);
  const cached = isValidData(stored.last) ? stored.last : null;
  // First paint from the cache, before any network.
  draw(nextView(cached, undefined));

  const result = await fetchData(fetch, stored.serverUrl, stored.token);
  const view = nextView(cached, result);
  draw(view);
  if (view.status === "fresh") {
    await browser.storage.local.set({ last: view.data });
  }
}

document.addEventListener("DOMContentLoaded", start);

import { start } from "./newtab-core.js";
import { createView } from "./newtab-view.js";

document.addEventListener("DOMContentLoaded", () => {
  const draw = createView({
    doc: document,
    statusEl: document.getElementById("status"),
    root: document.getElementById("groups"),
    language: navigator.language,
    fetchImpl: fetch,
    openOptions: () => browser.runtime.openOptionsPage(),
    objectUrl: { create: (blob) => URL.createObjectURL(blob), revoke: (url) => URL.revokeObjectURL(url) },
  });
  // The values refresh runs only while the tab is visible (see newtab-core.js).
  const visibility = {
    isHidden: () => document.hidden,
    onChange: (callback) => document.addEventListener("visibilitychange", callback),
  };
  // A new server or token makes this tab's data stale: stop refreshing at once (newtab-core.js also
  // checks the saved settings before every write).
  const connection = {
    onChange: (callback) =>
      browser.storage.onChanged.addListener((changes, area) => {
        if (area === "local" && (changes.serverUrl || changes.token)) callback();
      }),
  };
  start({
    storage: browser.storage,
    fetchImpl: fetch,
    draw,
    update: draw.update,
    tick: draw.tick,
    visibility,
    connection,
  });
});

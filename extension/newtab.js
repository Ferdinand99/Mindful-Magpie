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
  start({
    storage: browser.storage,
    fetchImpl: fetch,
    draw,
    update: draw.update,
    tick: draw.tick,
    visibility,
  });
});

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
  start({ storage: browser.storage, fetchImpl: fetch, draw });
});

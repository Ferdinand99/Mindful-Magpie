import { renderPlaceholder, renderStatus } from "./placeholder.js";
import { start } from "./newtab-core.js";

const statusEl = document.getElementById("status");
const bodyEl = document.getElementById("body");

function draw(view) {
  renderStatus(statusEl, view.status, () => browser.runtime.openOptionsPage());
  renderPlaceholder(bodyEl, view.data);
}

document.addEventListener("DOMContentLoaded", () => {
  start({ storage: browser.storage, fetchImpl: fetch, draw });
});

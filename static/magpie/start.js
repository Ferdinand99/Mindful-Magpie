// Mindful Magpie: the start page script. It reads the layout and the interface
// strings the server embedded in the page as data (#mm-layout, #mm-labels) and
// draws them with the shared renderer. It never moves focus.
import { render } from "./render.js";

function readJson(id) {
  const block = document.getElementById(id);
  if (!block) return null;
  try {
    return JSON.parse(block.textContent);
  } catch {
    return null;
  }
}

function draw() {
  const root = document.getElementById("groups");
  if (!root) return;
  // An unreadable layout draws as the empty page; the notice above says why.
  const layout = readJson("mm-layout");
  const labels = readJson("mm-labels") ?? {};
  render(root, layout, { lang: document.documentElement.lang, labels });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", draw);
} else {
  draw();
}
// A boosted visit to the start page from the start page swaps in a fresh body,
// and htmx does not run scripts it swaps in: draw again.
document.addEventListener("htmx:afterSettle", draw);

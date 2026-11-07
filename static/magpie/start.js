// Mindful Magpie: the start page script. It reads the layout and the interface
// strings the server embedded in the page as data (#mm-layout, #mm-labels) and
// draws them with the shared renderer. It never moves focus.
import { render } from "./render.js";

function readJson(doc, id) {
  const block = doc.getElementById(id);
  if (!block) return null;
  try {
    return JSON.parse(block.textContent);
  } catch {
    return null;
  }
}

/**
 * Draws the layout embedded in `doc` into #groups. On a page without #groups
 * (the body of another page was swapped in) it takes the background off <html>
 * instead, because htmx swaps only the body and <html> would keep it.
 */
export function draw(doc) {
  const root = doc.getElementById("groups");
  if (!root) {
    doc.documentElement.removeAttribute("data-bg");
    doc.documentElement.style.removeProperty("--mm-bg-image");
    return;
  }
  // An unreadable layout draws as the empty page; the notice above says why.
  const layout = readJson(doc, "mm-layout");
  const labels = readJson(doc, "mm-labels") ?? {};
  render(root, layout, { lang: doc.documentElement.lang, labels });
}

/**
 * After an htmx swap: draws only when #groups is there but empty (a fresh
 * body of the start page), and cleans up when it is gone. Content that is
 * already drawn is never drawn over (edit mode works on it).
 */
export function drawIfNeeded(doc) {
  const root = doc.getElementById("groups");
  if (root && root.firstElementChild) return;
  draw(doc);
}

if (typeof document !== "undefined") {
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => draw(document));
  } else {
    draw(document);
  }
  // A boosted visit to the start page swaps in a fresh body, and htmx does not
  // run scripts it swaps in. A boosted visit to another page takes the body away.
  document.addEventListener("htmx:afterSettle", () => drawIfNeeded(document));
}

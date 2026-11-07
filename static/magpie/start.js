// Mindful Magpie: the start page script. It reads the layout and the interface
// strings the server embedded in the page as data (#mm-layout, #mm-labels) and
// draws them with the shared renderer. It never moves focus. Edit mode (edit.js)
// is loaded only when the person presses the Edit button.
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
  // Edit mode owns the element while it is open.
  if (root?.hasAttribute("data-editing")) return;
  // The Edit button is hidden until a script can make it work.
  doc.querySelector(".start-actions")?.removeAttribute("hidden");
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
 * After an htmx swap or a history restore: draws when #groups is there but
 * empty (a fresh body of the start page), or when <html> has lost its
 * background (the body was restored from history after a visit to another page
 * cleared it), and cleans up when #groups is gone. Content that is drawn and
 * has its background is never drawn over, and while edit mode is open
 * (`data-editing` on #groups) nothing is drawn at all: edit mode works on it.
 */
export function drawIfNeeded(doc) {
  const root = doc.getElementById("groups");
  if (root?.hasAttribute("data-editing")) return;
  if (root && root.firstElementChild && doc.documentElement.hasAttribute("data-bg")) return;
  draw(doc);
}

/**
 * A `save` for edit mode: posts the layout as JSON to `/start/layout` (the
 * `HX-Request` header is the CSRF proof the server asks for besides the cookie)
 * and resolves to `{ status, body }`, `body` being the parsed answer or null. A
 * network failure rejects. When the server accepts it, the layout block in the
 * page is replaced, so that the page and the next edit start from what is now
 * stored (and from the new version).
 */
export function makeSave(doc, fetchFn = (...args) => fetch(...args)) {
  return async function save(layout) {
    const response = await fetchFn("/start/layout", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", "HX-Request": "true" },
      body: JSON.stringify(layout),
    });
    let body = null;
    try {
      body = await response.json();
    } catch {
      // Not JSON (a proxy's error page): the status says enough.
    }
    if (response.status === 200 && Number.isFinite(body?.version)) {
      const block = doc.getElementById("mm-layout");
      if (block) {
        // `<` as an escape: the block lives in HTML, and htmx saves it as HTML
        // for the history, so no text in it may be able to end it early.
        block.textContent = JSON.stringify({ ...layout, version: body.version }).replace(/</g, "\\u003c");
      }
    }
    return { status: response.status, body };
  };
}

/**
 * Wires the Edit button (`#mm-edit`, found at click time, because htmx swaps
 * the body) to edit mode. The module is loaded on the first press. Pressing the
 * button while editing is the same as «Done». `deps`: `load` (gives the module),
 * `save`, `reload`. Returns `{ leave, check }`: `leave()` closes edit mode
 * without asking (the page is about to be snapshotted or left), `check()` drops
 * an editor whose page was swapped away.
 */
export function initEditing(doc, deps = {}) {
  const load = deps.load ?? (() => import("./edit.js"));
  let editor = null;
  let loading = false;

  doc.addEventListener("click", async (event) => {
    const button = event.target.closest?.("#mm-edit");
    if (!button) return;
    if (editor?.isActive()) {
      editor.done();
      return;
    }
    const root = doc.getElementById("groups");
    const layout = readJson(doc, "mm-layout");
    if (!root || !layout || loading) return;
    loading = true;
    try {
      const module = await load();
      editor = module.startEditing(root, () => readJson(doc, "mm-layout") ?? layout, deps.save ?? makeSave(doc), {
        labels: readJson(doc, "mm-labels") ?? {},
        lang: doc.documentElement.lang,
        editButton: button,
        reload: deps.reload,
      });
    } finally {
      loading = false;
    }
  });

  return {
    leave() {
      if (editor?.isActive()) editor.stop({ focus: false });
    },
    check() {
      if (editor && editor.isActive() && !editor.root.isConnected) editor.dispose();
    },
  };
}

if (typeof document !== "undefined") {
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => draw(document));
  } else {
    draw(document);
  }
  const editing = initEditing(document);
  // htmx saves the body for the history after a click on a link: edit mode
  // (nothing unsaved by then: the page asked) gives way to the plain page.
  document.addEventListener("htmx:beforeHistorySave", () => editing.leave());
  // A boosted visit to the start page swaps in a fresh body, and htmx does not
  // run scripts it swaps in. A boosted visit to another page takes the body away.
  document.addEventListener("htmx:afterSettle", () => {
    editing.check();
    drawIfNeeded(document);
  });
  // Back to the start page: htmx puts the saved body back, drawn but without
  // the background that the visit to the other page cleared.
  document.addEventListener("htmx:historyRestore", () => drawIfNeeded(document));
}

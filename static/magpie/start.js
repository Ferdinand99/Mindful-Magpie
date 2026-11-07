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

// htmx does not keep script elements when it puts a saved body back (Back and
// Forward), so the data blocks are gone from such a page. What the page showed
// last is remembered here, for edit mode, which needs it again.
const remembered = new Map();

/** The block `id` as data; the remembered copy when the page has no such block. */
function dataOf(doc, id) {
  if (!doc.getElementById(id)) return remembered.get(id) ?? null;
  const value = readJson(doc, id);
  if (value !== null) remembered.set(id, value);
  return value;
}

/**
 * Draws the layout embedded in `doc` into #groups. On a page without #groups
 * (the body of another page was swapped in) it takes the background off <html>
 * instead, because htmx swaps only the body and <html> would keep it.
 * `fromMemory` (a body htmx put back, whose data blocks are gone) draws what the
 * page showed last instead of an empty page.
 */
export function draw(doc, fromMemory = false) {
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
  let layout = readJson(doc, "mm-layout");
  let labels = readJson(doc, "mm-labels") ?? {};
  if (fromMemory && !doc.getElementById("mm-layout")) layout = remembered.get("mm-layout") ?? null;
  if (fromMemory && !doc.getElementById("mm-labels")) labels = remembered.get("mm-labels") ?? {};
  dataOf(doc, "mm-layout");
  dataOf(doc, "mm-labels");
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
  draw(doc, true);
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
      remembered.set("mm-layout", { ...layout, version: body.version });
      if (block) {
        // `<` as an escape: the block lives in HTML, and htmx saves it as HTML
        // for the history, so no text in it may be able to end it early.
        block.textContent = JSON.stringify({ ...layout, version: body.version }).replace(/</g, "\\u003c");
      }
    }
    return { status: response.status, body };
  };
}

// An unsaved draft is kept in sessionStorage when the person leaves edit mode in
// a way that cannot be stopped (Back and Forward). The page offers it again the
// next time the start page shows. Every access is guarded: storage can be off,
// full or blocked, and the page must work without it.
const STASH_KEY = "mm-unsaved-draft";

/** The tab's sessionStorage, or null where it is not there or not allowed. */
function storageOf(doc, deps) {
  try {
    return deps.storage ?? doc.defaultView?.sessionStorage ?? null;
  } catch {
    return null;
  }
}

/** Keeps `draft` (its `version` is the one it was loaded at). */
export function stashDraft(storage, draft) {
  try {
    storage?.setItem(STASH_KEY, JSON.stringify({ version: draft.version, draft }));
  } catch {
    // Nothing to do: the draft is lost, as it was before this existed.
  }
}

/** The kept draft, or null when there is none or it does not look like a layout. */
export function readStash(storage) {
  try {
    const kept = JSON.parse(storage?.getItem(STASH_KEY) ?? "null");
    const draft = kept?.draft;
    const shaped =
      draft &&
      typeof draft === "object" &&
      Number.isFinite(kept.version) &&
      Array.isArray(draft.bookmarks) &&
      Array.isArray(draft.groups) &&
      draft.groups.every((g) => g && typeof g === "object" && Array.isArray(g.cards));
    return shaped ? draft : null;
  } catch {
    return null;
  }
}

export function clearStash(storage) {
  try {
    storage?.removeItem(STASH_KEY);
  } catch {
    // See stashDraft.
  }
}

/**
 * Wires the Edit button (`#mm-edit`, found at click time, because htmx swaps
 * the body) to edit mode. The module is loaded on the first press. Pressing the
 * button while editing is the same as «Done». `deps`: `load` (gives the module),
 * `save`, `reload`, `navigate`, `storage`. Returns `{ leave, check, offerRestore }`:
 *
 * - `leave()`: the page is about to be saved to the history or left by Back or
 *   Forward, which cannot be stopped. Unsaved changes are kept (see above) and
 *   edit mode closes, so that the history holds the plain page.
 * - `check()`: drops an editor whose page was swapped away.
 * - `offerRestore()`: shows the notice for a kept draft, if there is one.
 */
export function initEditing(doc, deps = {}) {
  const load = deps.load ?? (() => import("./edit.js"));
  const storage = storageOf(doc, deps);
  const baseSave = deps.save ?? makeSave(doc);
  // A saved page has no use for an older draft.
  const save = async (layout) => {
    const result = await baseSave(layout);
    if (result?.status === 200) clearStash(storage);
    return result;
  };
  let editor = null;
  let loading = false;

  async function begin(button, restored) {
    const root = doc.getElementById("groups");
    const layout = dataOf(doc, "mm-layout");
    if (!root || !layout || loading) return;
    loading = true;
    try {
      const module = await load();
      doc.getElementById("mm-restore")?.remove();
      editor = module.startEditing(root, () => dataOf(doc, "mm-layout") ?? layout, save, {
        labels: dataOf(doc, "mm-labels") ?? {},
        lang: doc.documentElement.lang,
        editButton: button,
        reload: deps.reload,
        navigate: deps.navigate,
        draft: restored ?? undefined,
        onDiscard: () => clearStash(storage),
      });
    } catch (error) {
      // A kept draft that edit mode cannot use is not offered again.
      if (restored) clearStash(storage);
      else throw error;
    } finally {
      loading = false;
    }
  }

  doc.addEventListener("click", (event) => {
    const button = event.target.closest?.("#mm-edit");
    if (!button) return;
    if (editor?.isActive()) editor.done();
    else begin(button, null);
  });

  function offerRestore() {
    const root = doc.getElementById("groups");
    if (!root || editor?.isActive() || doc.getElementById("mm-restore")) return;
    const kept = readStash(storage);
    if (!kept) return;
    const labels = dataOf(doc, "mm-labels") ?? {};
    // Without the strings the notice would say nothing: leave the draft alone.
    if (typeof labels.restore_notice !== "string") return;
    const make = (tag, props, text) => {
      const el = doc.createElement(tag);
      for (const [name, value] of Object.entries(props)) el.setAttribute(name, value);
      if (text !== undefined) el.textContent = text;
      return el;
    };
    const notice = make("section", { class: "glass mm-notice", id: "mm-restore" });
    const message = make("div", {});
    message.appendChild(make("p", {}, labels.restore_notice));
    const actions = make("div", { class: "mm-notice-actions" });
    const restore = make("button", { type: "button", class: "btn btn-primary" }, labels.restore_yes ?? "");
    const forget = make("button", { type: "button", class: "btn btn-secondary" }, labels.restore_discard ?? "");
    restore.addEventListener("click", () => begin(doc.getElementById("mm-edit"), kept));
    forget.addEventListener("click", () => {
      clearStash(storage);
      notice.remove();
    });
    actions.appendChild(restore);
    actions.appendChild(forget);
    notice.appendChild(message);
    notice.appendChild(actions);
    root.parentNode.insertBefore(notice, root);
  }

  return {
    leave() {
      // Its buttons would be dead when htmx puts the saved body back.
      doc.getElementById("mm-restore")?.remove();
      if (!editor?.isActive()) return;
      if (editor.isDirty()) stashDraft(storage, editor.draft());
      editor.stop({ focus: false });
    },
    check() {
      if (editor && editor.isActive() && !editor.root.isConnected) editor.dispose();
    },
    offerRestore,
  };
}

if (typeof document !== "undefined") {
  const editing = initEditing(document);
  const start = () => {
    draw(document);
    editing.offerRestore();
  };
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start);
  } else {
    start();
  }
  // htmx saves the body for the history before it leaves the page, for a click
  // on a link and also for Back and Forward (popstate, which cannot be stopped).
  // Links and forms inside the app were asked about already while edit mode held
  // unsaved changes (edit.js); what is left is Back and Forward, so the draft is
  // kept, and edit mode closes so that the history holds the plain page.
  document.addEventListener("htmx:beforeHistorySave", () => editing.leave());
  // A boosted visit to the start page swaps in a fresh body, and htmx does not
  // run scripts it swaps in. A boosted visit to another page takes the body away.
  document.addEventListener("htmx:afterSettle", () => {
    editing.check();
    drawIfNeeded(document);
    editing.offerRestore();
  });
  // Back to the start page: htmx puts the saved body back, drawn but without
  // the background that the visit to the other page cleared.
  document.addEventListener("htmx:historyRestore", () => {
    drawIfNeeded(document);
    editing.offerRestore();
  });
}

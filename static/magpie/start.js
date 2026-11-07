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

// An unsaved draft is kept in sessionStorage when the person leaves edit mode in
// a way that cannot be stopped (Back and Forward). The page offers it again the
// next time the start page shows. A tab can be used by more than one person in
// turn, so the kept draft carries the number of the user it belongs to (the page
// has it in data-user on #groups) and the time it was kept: it is offered only
// to that user and only for a day. Every access is guarded: storage can be off,
// full or blocked, and the page must work without it.
const STASH_KEY = "mm-unsaved-draft";
const STASH_TTL_MS = 24 * 60 * 60 * 1000;

/** The signed-in user's number from the page, or null. */
function userOf(doc) {
  const value = doc.getElementById("groups")?.getAttribute("data-user");
  return typeof value === "string" && /^\d+$/.test(value) ? Number(value) : null;
}

/** The tab's sessionStorage, or null where it is not there or not allowed. */
function storageOf(doc, deps) {
  try {
    return deps.storage ?? doc.defaultView?.sessionStorage ?? null;
  } catch {
    return null;
  }
}

/** Keeps `draft` (its `version` is the one it was loaded at) for `user`. */
export function stashDraft(storage, draft, user, now = Date.now()) {
  try {
    storage?.setItem(STASH_KEY, JSON.stringify({ user, at: now, version: draft.version, draft }));
  } catch {
    // Nothing to do: the draft is lost, as it was before this existed.
  }
}

/**
 * The draft kept for `user`, or null. Anything that is not offered (another
 * user's, older than a day, or not a layout at all) is deleted on the way.
 */
export function readStash(storage, user, now = Date.now()) {
  let raw;
  try {
    raw = storage?.getItem(STASH_KEY) ?? null;
  } catch {
    return null;
  }
  if (raw === null) return null;
  let draft = null;
  try {
    const kept = JSON.parse(raw);
    const shaped =
      kept?.draft &&
      typeof kept.draft === "object" &&
      Number.isFinite(kept.version) &&
      Array.isArray(kept.draft.bookmarks) &&
      Array.isArray(kept.draft.groups) &&
      kept.draft.groups.every((g) => g && typeof g === "object" && Array.isArray(g.cards));
    const mine = user !== null && kept?.user === user;
    const fresh = Number.isFinite(kept?.at) && now - kept.at >= 0 && now - kept.at < STASH_TTL_MS;
    if (shaped && mine && fresh) draft = kept.draft;
  } catch {
    // Not JSON: not offered.
  }
  if (draft === null) clearStash(storage);
  return draft;
}

export function clearStash(storage) {
  try {
    storage?.removeItem(STASH_KEY);
  } catch {
    // See stashDraft.
  }
}

const RELOAD_KEY = "mm-reload-at";
const RELOAD_GAP_MS = 10 * 1000;

/**
 * htmx does not keep script elements in the content it swaps in, and the start
 * page's data blocks are scripts: a start page that htmx put back (Back, or a
 * boosted visit from the start page to itself) has its groups element but no
 * layout, and could only be drawn empty. The page is not kept in htmx's history
 * cache (hx-history), so it came from the server for the right user: a full
 * reload gives it with its data. A page that is still without data soon after a
 * reload is left alone, so that this cannot loop: the mark of a reload is taken
 * off as soon as a start page with its data is seen (call this on every load as
 * well). Returns whether it reloaded.
 */
export function reloadIfDataLost(doc, deps = {}) {
  if (!doc.getElementById("groups")) return false;
  const storage = storageOf(doc, deps);
  if (doc.getElementById("mm-layout")) {
    try {
      storage?.removeItem(RELOAD_KEY);
    } catch {
      // See below.
    }
    return false;
  }
  const now = (deps.now ?? (() => Date.now()))();
  try {
    const raw = storage?.getItem(RELOAD_KEY);
    const at = raw === null || raw === undefined ? Number.NaN : Number(raw);
    if (now - at >= 0 && now - at < RELOAD_GAP_MS) return false;
    storage?.setItem(RELOAD_KEY, String(now));
  } catch {
    // No storage, no guard: a reload still gives the page its data.
  }
  (deps.reload ?? (() => doc.defaultView?.location.reload()))();
  return true;
}

/**
 * Wires the Edit button (`#mm-edit`, found at click time, because htmx swaps
 * the body) to edit mode. The module is loaded on the first press. Pressing the
 * button while editing is the same as «Done». `deps`: `load` (gives the module),
 * `save`, `reload`, `navigate`, `storage`, `now` (a clock, for tests). Returns `{ leave, check, offerRestore }`:
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
  const now = deps.now ?? (() => Date.now());
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
    const layout = readJson(doc, "mm-layout");
    if (!root || !layout || loading) return;
    loading = true;
    let module;
    try {
      // A failure here (the network, say) says nothing about a kept draft: it
      // stays, and the person can try again.
      module = await load();
    } finally {
      loading = false;
    }
    doc.getElementById("mm-restore")?.remove();
    try {
      editor = module.startEditing(root, () => readJson(doc, "mm-layout") ?? layout, save, {
        labels: readJson(doc, "mm-labels") ?? {},
        lang: doc.documentElement.lang,
        editButton: button,
        reload: deps.reload,
        navigate: deps.navigate,
        draft: restored ?? undefined,
        onDiscard: () => clearStash(storage),
      });
    } catch (error) {
      // Only a draft that edit mode cannot use is dropped.
      if (restored) clearStash(storage);
      else throw error;
    }
  }

  /** `begin` for a click: a failure is reported, not left unhandled. */
  const begun = (button, restored) =>
    begin(button, restored).catch((error) => globalThis.console?.error?.(error));

  doc.addEventListener("click", (event) => {
    const button = event.target.closest?.("#mm-edit");
    if (!button) return;
    if (editor?.isActive()) editor.done();
    else begun(button, null);
  });

  function offerRestore() {
    const root = doc.getElementById("groups");
    if (!root || editor?.isActive() || doc.getElementById("mm-restore")) return;
    const kept = readStash(storage, userOf(doc), now());
    if (!kept) return;
    const labels = readJson(doc, "mm-labels") ?? {};
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
    restore.addEventListener("click", () => begun(doc.getElementById("mm-edit"), kept));
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

  function leave() {
    doc.getElementById("mm-restore")?.remove();
    if (!editor?.isActive()) return;
    // Without a user number on the page the draft could not be tied to anyone.
    if (editor.isDirty() && userOf(doc) !== null) stashDraft(storage, editor.draft(), userOf(doc), now());
    editor.stop({ focus: false });
  }

  // Back and Forward cannot be stopped. htmx takes over an entry it made
  // (its state says so) and swaps the body, so the draft is kept now, while the
  // editor is still there. An entry of another kind is a real page load, which
  // the browser's own prompt covers (edit.js); a change of the address hash
  // keeps the page, and so keeps edit mode. This page asks htmx not to keep a
  // snapshot of it (hx-history), so htmx's beforeHistorySave never comes for it.
  // Capture phase: before htmx's own listener swaps the body.
  doc.defaultView?.addEventListener(
    "popstate",
    (event) => {
      if (event.state?.htmx) leave();
    },
    true,
  );

  return {
    leave,
    check() {
      if (editor && editor.isActive() && !editor.root.isConnected) editor.dispose();
    },
    offerRestore,
  };
}

if (typeof document !== "undefined") {
  const editing = initEditing(document);
  const start = () => {
    // A start page with its data takes off the mark of an earlier reload.
    reloadIfDataLost(document);
    draw(document);
    editing.offerRestore();
  };
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start);
  } else {
    start();
  }
  // The start page asks htmx not to keep a snapshot of it (hx-history="false"
  // on #groups): it holds one person's layout and htmx's cache outlives a
  // sign-out in the same tab. Back to the page fetches it from the server (for
  // whoever is signed in now); the draft kept by a dirty Back is in initEditing.
  //
  // A body htmx swaps in has no data blocks (see reloadIfDataLost): reload.
  document.addEventListener("htmx:afterSettle", () => {
    if (reloadIfDataLost(document)) return;
    editing.check();
    drawIfNeeded(document);
    editing.offerRestore();
  });
  document.addEventListener("htmx:historyRestore", () => {
    if (reloadIfDataLost(document)) return;
    drawIfNeeded(document);
    editing.offerRestore();
  });
}

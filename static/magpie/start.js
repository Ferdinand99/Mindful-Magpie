// Mindful Magpie: the start page script. It reads the layout and the interface
// strings the server embedded in the page as data (#mm-layout, #mm-labels) and
// draws them with the shared renderer. It never moves focus. Edit mode (edit.js)
// is loaded only when the person presses the Edit button. Numbers of bound
// cards come from #mm-values and stay current through an event stream (see
// initLive); so does the «Trenger deg» row, whose «Kvitter» buttons are wired in
// initNeeds.
import { render, tickAges, toggleAckedNeeds, updateValues } from "./render.js";

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
 * The services block of the page (`#mm-services`: what this user may see of the
 * services, never an address or a key) as the renderer's `services` option. A
 * page without the block, or with a block that is not a list, gives no option,
 * and the renderer then draws bound cards as plain link cards.
 */
function servicesOpt(doc) {
  const services = readJson(doc, "mm-services");
  return Array.isArray(services) ? { services } : {};
}

/**
 * The values block of the page (`#mm-values`: the latest reading of each service
 * a card of this page is bound to) as the renderer's `values` option, or no
 * option when the block is missing or not an object.
 */
function valuesOpt(doc) {
  const values = readJson(doc, "mm-values");
  return values && typeof values === "object" && !Array.isArray(values) ? { values } : {};
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
  render(root, layout, { lang: doc.documentElement.lang, labels, canAck: true, ...servicesOpt(doc), ...valuesOpt(doc) });
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

/** Replaces the data block `id` of the page with `value` as JSON. */
function writeBlock(doc, id, value) {
  const block = doc.getElementById(id);
  if (!block) return;
  // `<` as an escape: the block lives in HTML, and htmx saves it as HTML for the
  // history, so no text in it may be able to end it early.
  block.textContent = JSON.stringify(value).replace(/</g, "\\u003c");
}

/** Replaces the layout block of the page, so that the page and the next edit start from it. */
function writeLayoutBlock(doc, layout) {
  writeBlock(doc, "mm-layout", layout);
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
      writeLayoutBlock(doc, { ...layout, version: body.version });
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
 * The two calls of the background picture for edit mode, each resolving to
 * `{ status, body }` (a network failure rejects):
 *
 * - `upload(file)`: `POST /start/background`; the picture is on the server at
 *   once, the layout points at it only when the layout is saved;
 * - `remove(id)`: `DELETE /start/background?id=`; the server removes that
 *   picture only when it is still the user's current one and the stored layout
 *   does not use it, and answers 204 either way. It never changes a layout, so
 *   nothing on the page changes with it, and the layout block is written only
 *   from a save.
 */
export function makeBackground(doc, fetchFn = (...args) => fetch(...args)) {
  const headers = { "HX-Request": "true" };
  const answer = async (response) => {
    let body = null;
    try {
      body = await response.json();
    } catch {
      // Not JSON (a 204 has no body): the status says enough.
    }
    return { status: response.status, body };
  };
  return {
    async upload(file) {
      const form = new FormData();
      form.append("image", file);
      // No Content-Type: the browser writes it, with the boundary.
      return answer(await fetchFn("/start/background", { method: "POST", credentials: "same-origin", headers, body: form }));
    },
    async remove(id) {
      // keepalive: it may be sent as the page is left.
      return answer(
        await fetchFn(`/start/background?id=${encodeURIComponent(id)}`, {
          method: "DELETE",
          credentials: "same-origin",
          headers,
          keepalive: true,
        }),
      );
    },
  };
}

/**
 * Wires the Edit button (`#mm-edit`, found at click time, because htmx swaps
 * the body) to edit mode. The module is loaded on the first press. Pressing the
 * button while editing is the same as «Done». `deps`: `load` (gives the module),
 * `save`, `background`, `reload`, `navigate`, `storage`, `now` (a clock, for tests). Returns `{ leave, check, offerRestore }`:
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
  const background = deps.background ?? makeBackground(doc);
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
        ...servicesOpt(doc),
        lang: doc.documentElement.lang,
        editButton: button,
        reload: deps.reload,
        navigate: deps.navigate,
        background,
        draft: restored ?? undefined,
        onDiscard: () => clearStash(storage),
        // The page is drawn with the numbers again when edit mode ends.
        getValues: () => readJson(doc, "mm-values") ?? undefined,
        onEnd: () => deps.onEditingChange?.(),
      });
      // The stream is closed while the page is being edited.
      deps.onEditingChange?.();
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
    // A draft kept at another version than the page has now would give a
    // conflict when it is saved: still offered (it is the person's work), with a
    // note saying so.
    const pageVersion = readJson(doc, "mm-layout")?.version;
    if (pageVersion !== kept.version && typeof labels.restore_stale === "string") {
      message.appendChild(make("p", { class: "help" }, labels.restore_stale));
    }
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

// ---- live values ------------------------------------------------------------

export const LIVE_URL = "/start/live";
/** How often the ages on the cards are brought up to date. */
export const TICK_MS = 30 * 1000;
/** How long to wait before opening the stream again after the server refused it. */
export const RETRY_MS = 30 * 1000;

/** A time as epoch milliseconds: epoch seconds, epoch milliseconds or an ISO string; else null. */
function toMs(value) {
  let n = Number.NaN;
  if (typeof value === "number") {
    n = value;
  } else if (typeof value === "string" && value.trim() !== "") {
    n = Number(value);
    if (Number.isNaN(n)) return Number.isFinite(Date.parse(value)) ? Date.parse(value) : null;
  }
  if (!Number.isFinite(n)) return null;
  // Seconds until about the year 5138, milliseconds beyond that.
  return Math.abs(n) < 1e11 ? n * 1000 : n;
}

/**
 * Merges a stream event into the values the page has. A `full` event replaces
 * them; any other event adds and replaces the entries it carries and drops the
 * ids in `removed`. The needs list (the person's «Trenger deg», acknowledgements
 * applied) is replaced when the event carries one and kept when it does not.
 * Returns `{ values, removed }`, `removed` being every id whose values the person
 * no longer gets (the service was deleted, hidden or unbound). It says nothing of
 * the services list the editor offers: that follows `services_visible` (see
 * mergeServices). Neither argument is changed.
 */
export function mergeValues(current, update) {
  const before = current && typeof current.services === "object" && current.services ? current.services : {};
  const sent = update && typeof update.services === "object" && update.services ? update.services : {};
  const listed = Array.isArray(update?.removed) ? update.removed.map(String) : [];
  const services = update?.full ? {} : { ...before };
  for (const id of listed) delete services[id];
  Object.assign(services, sent);
  const removed = new Set(listed);
  if (update?.full) {
    for (const id of Object.keys(before)) if (!(id in sent)) removed.add(id);
  }
  const needs = Array.isArray(update?.needs) ? update.needs : Array.isArray(current?.needs) ? current.needs : [];
  return { values: { now: update?.now ?? current?.now, services, needs }, removed: [...removed] };
}

/**
 * The services list after an event (the list the editor's picker offers): when
 * the event says which services the person may see (`visible`, its
 * `services_visible`: every id, sent when the set changed), the others gone;
 * the views the event adds (`services_add`: a service that became visible while
 * the page was open) put in, or replacing the one the list has, and names and
 * intervals as the event's entries have them. By name, as the server lists them.
 * Without `visible` nobody is dropped: a service that is visible but not bound to
 * a card (its values were removed) is still to be offered.
 */
export function mergeServices(list, values, visible, add = []) {
  const keep = Array.isArray(visible) ? new Set(visible.map(String)) : null;
  const out = list.filter((s) => keep === null || keep.has(String(s?.id)));
  for (const view of Array.isArray(add) ? add : []) {
    if (!view || typeof view !== "object" || view.id === undefined) continue;
    const at = out.findIndex((s) => String(s?.id) === String(view.id));
    if (at >= 0) out[at] = view;
    else out.push(view);
  }
  const byName = (a, b) => String(a?.name ?? "").localeCompare(String(b?.name ?? ""), undefined, { sensitivity: "base" });
  return out.sort(byName).map((s) => {
    const entry = values.services[String(s?.id)];
    if (!entry) return s;
    return { ...s, name: entry.name ?? s.name, interval_s: entry.interval_s ?? s.interval_s };
  });
}

/** Whether any card of the layout shows a service. */
function hasBoundCards(layout) {
  return (Array.isArray(layout?.groups) ? layout.groups : []).some(
    (g) => Array.isArray(g?.cards) && g.cards.some((c) => c && typeof c === "object" && c.service),
  );
}

/**
 * Keeps the numbers on the cards current. The page holds the latest readings in
 * `#mm-values`; while the page is visible and not being edited, an EventSource
 * on `/start/live` brings changes (the first event of a stream is the whole
 * document, so a stream opened after a long hidden spell, a reconnect or a
 * save catches up at once). Each event is merged into the block, and the cards
 * are updated in place (`updateValues`: no element a person may be standing on
 * is replaced, so focus stays); only when the page and the layout differ in
 * more than numbers is it drawn again. Nothing is redrawn while edit mode is
 * open (it owns the page); the stream is closed then, and opens again when edit
 * mode ends.
 *
 * A timer every 30 s brings the ages up to date, counted on the server's clock:
 * the offset between it and this one is taken from each event's `now`.
 *
 * The stream is closed when the page is hidden, when the body is swapped
 * (`htmx:beforeSwap`, a boosted visit) or when `#groups` is no longer in the
 * document, so nothing stays open across navigation. A server that refuses a
 * stream (an `EventSource` does not retry those) is asked again after
 * `RETRY_MS`, unless the answer shows the person is signed out: then nothing
 * more is tried until the page is shown again or the body is swapped.
 *
 * `deps` (for tests): `EventSource`, `now`, `setInterval`, `clearInterval`,
 * `setTimeout`, `clearTimeout`, `fetch` (the probe after a refusal). Returns `{ sync, close, tick, apply, isOpen }`:
 * `sync()` opens or closes the stream as the page's state asks (call it when
 * anything that matters changed), `tick()` is the timer's work.
 */
export function initLive(doc, deps = {}) {
  const win = doc.defaultView;
  const EventSourceCtor = deps.EventSource ?? win?.EventSource ?? globalThis.EventSource;
  const now = deps.now ?? (() => Date.now());
  const every = deps.setInterval ?? ((fn, ms) => globalThis.setInterval(fn, ms));
  const stopEvery = deps.clearInterval ?? ((id) => globalThis.clearInterval(id));
  const later = deps.setTimeout ?? ((fn, ms) => globalThis.setTimeout(fn, ms));
  const stopLater = deps.clearTimeout ?? ((id) => globalThis.clearTimeout(id));
  const request = deps.fetch ?? ((...args) => (win?.fetch ?? globalThis.fetch)(...args));
  let source = null;
  // Set after a refusal because the person is signed out: no more tries until
  // the page is shown again or the body is swapped.
  let signedOut = false;
  let probing = false;
  let timer = null;
  let retry = null;
  // Server clock minus this one, in ms; null until a page or an event says.
  let offset = null;

  const rootEl = () => doc.getElementById("groups");
  const editing = () => rootEl()?.hasAttribute("data-editing") === true;

  function learnClock(values) {
    const server = toMs(values?.now);
    if (server !== null) offset = server - now();
  }
  learnClock(readJson(doc, "mm-values"));

  function wanted() {
    const root = rootEl();
    if (!root || !root.isConnected || editing() || typeof EventSourceCtor !== "function") return false;
    if (doc.visibilityState !== "visible") return false;
    if (!readJson(doc, "mm-values")) return false;
    return hasBoundCards(readJson(doc, "mm-layout"));
  }

  function close() {
    if (source) {
      const old = source;
      source = null;
      old.close();
    }
    if (timer !== null) {
      stopEvery(timer);
      timer = null;
    }
    if (retry !== null) {
      stopLater(retry);
      retry = null;
    }
  }

  function ages() {
    const labels = readJson(doc, "mm-labels") ?? {};
    tickAges(rootEl(), { now: now() + (offset ?? 0), lang: doc.documentElement.lang, labels });
  }

  function tick() {
    // The page may have gone while the timer ran.
    if (!wanted()) return sync();
    ages();
  }

  /**
   * Whether the server turns this session away (an `EventSource` does not say
   * why it failed): asks once for the stream and looks at the answer's head.
   */
  async function turnedAway() {
    try {
      const controller = typeof AbortController === "function" ? new AbortController() : null;
      const res = await request(LIVE_URL, {
        credentials: "same-origin",
        redirect: "manual",
        headers: { Accept: "text/event-stream" },
        signal: controller?.signal,
      });
      controller?.abort();
      return res.type === "opaqueredirect" || [0, 301, 302, 303, 307, 308, 401, 403].includes(res.status);
    } catch {
      return false;
    }
  }

  async function refused() {
    probing = true;
    const out = await turnedAway();
    probing = false;
    if (source !== null || !wanted()) return;
    if (out) {
      signedOut = true;
      return;
    }
    if (retry === null) {
      retry = later(() => {
        retry = null;
        sync();
      }, RETRY_MS);
    }
  }

  function open() {
    const mine = new EventSourceCtor(LIVE_URL);
    source = mine;
    mine.addEventListener("values", (event) => {
      if (source !== mine) return;
      let update = null;
      try {
        update = JSON.parse(event.data);
      } catch {
        // Not ours to read: the next event will do.
      }
      if (update && typeof update === "object") apply(update);
    });
    mine.addEventListener("error", () => {
      if (source !== mine) return;
      // The browser reconnects by itself unless the server answered with
      // something other than a stream (a refusal, a sign-out): that is final.
      if (mine.readyState === 2) {
        source = null;
        refused();
      }
    });
  }

  function sync() {
    if (!wanted()) return close();
    if (!source && retry === null && !probing && !signedOut) open();
    if (timer === null) {
      timer = every(tick, TICK_MS);
      // The page may have been hidden for a while: the ages are old.
      ages();
    }
  }

  /** Takes an update (an event's data) into the page: the blocks first, then the cards. */
  function apply(update) {
    let { values } = mergeValues(readJson(doc, "mm-values"), update);
    // An update without a `now` (the answer to an acknowledgement) keeps the block's old
    // one: ages would be counted from it, and look too young. Count them from the server's
    // clock as it is now, as this page learned it; the clock itself is only learned from an
    // update that says what time it is.
    const stamped = toMs(update?.now) !== null;
    if (!stamped && offset !== null) values = { ...values, now: (now() + offset) / 1000 };
    writeBlock(doc, "mm-values", values);
    const listed = readJson(doc, "mm-services");
    if (Array.isArray(listed)) {
      writeBlock(doc, "mm-services", mergeServices(listed, values, update.services_visible, update.services_add));
    }
    if (stamped) learnClock(values);
    // Edit mode owns the page; the blocks are current for when it ends.
    if (editing()) return;
    const root = rootEl();
    const layout = readJson(doc, "mm-layout");
    if (!root || !layout) return;
    const options = {
      lang: doc.documentElement.lang,
      labels: readJson(doc, "mm-labels") ?? {},
      canAck: true,
      ...servicesOpt(doc),
      values,
    };
    if (!updateValues(root, layout, options)) draw(doc);
  }

  doc.addEventListener("visibilitychange", () => {
    signedOut = false;
    sync();
  });
  // A boosted visit swaps the body: nothing may stay open for the old page. A
  // response htmx does not swap (an error answer, say) leaves the page as it is.
  doc.addEventListener("htmx:beforeSwap", (event) => {
    if (event?.detail?.shouldSwap !== true) return;
    signedOut = false;
    close();
  });

  return { sync, close, tick, apply, isOpen: () => source !== null };
}

// ---- «Trenger deg» --------------------------------------------------------------

export const NEEDS_ACK_URL = "/start/needs/ack";
export const NEEDS_UNACK_URL = "/start/needs/unack";

/**
 * Wires the buttons of the «Trenger deg» row (found at click time, one listener on
 * the document, so a row that is drawn again needs no new wiring): «Kvitter» and
 * the undo button post `{ service_id, key }` to `/start/needs/ack` and
 * `/start/needs/unack` (with the `HX-Request` header, the same-origin proof the
 * server asks for besides the cookie), and the toggle «N kvittert» shows and hides
 * the acknowledged ones. The server answers with the person's new list; it goes
 * through `deps.apply` (initLive's, which writes `#mm-values` and updates the row
 * in place) at once, and the person's other tabs get it from their streams. A
 * refusal or a failure changes nothing and is said in the row's live region
 * («already cleared» when the need is gone, else «try again»).
 *
 * The one place focus is moved: the person pressed the button, so it is given to
 * what took its place (the next chip, or the toggle when none is left), not left on
 * a button that is gone. A second press while one is on its way does nothing.
 *
 * `deps` (for tests): `apply` (required for any change), `fetch`.
 */
export function initNeeds(doc, deps = {}) {
  const win = doc.defaultView;
  const request = deps.fetch ?? ((...args) => (win?.fetch ?? globalThis.fetch)(...args));
  let busy = false;

  function fail(key) {
    const labels = readJson(doc, "mm-labels") ?? {};
    const live = doc.querySelector("#groups .mm-needs-live");
    if (live) live.textContent = typeof labels[key] === "string" ? labels[key] : "";
  }

  /** Gives the focus to what took the place of the need the person pressed the button of. */
  function refocus(index, key, service, acknowledged) {
    const root = doc.getElementById("groups");
    if (!root) return;
    const shown = [...root.querySelectorAll(".mm-needs-list:not(.mm-needs-acked) .mm-need")];
    const inner = (li) => li?.querySelector("a.mm-need-chip") ?? li?.querySelector("button");
    let target = null;
    if (acknowledged) {
      target = inner(shown[index] ?? shown[index - 1]) ?? root.querySelector(".mm-needs-toggle");
    } else {
      const back = shown.find((li) => li.getAttribute("data-key") === key && li.getAttribute("data-service") === service);
      target = inner(back) ?? root.querySelector(".mm-needs-toggle");
    }
    target?.focus?.();
  }

  async function change(button, url, acknowledged) {
    const item = button.closest(".mm-need");
    const service = item?.getAttribute("data-service") ?? "";
    const key = item?.getAttribute("data-key") ?? "";
    const serviceId = Number(service);
    if (!item || !Number.isSafeInteger(serviceId) || key === "" || busy || typeof deps.apply !== "function") return;
    const index = [...doc.querySelectorAll("#groups .mm-needs-list:not(.mm-needs-acked) .mm-need")].indexOf(item);
    busy = true;
    button.setAttribute("aria-disabled", "true");
    try {
      const response = await request(url, {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", "HX-Request": "true" },
        body: JSON.stringify({ service_id: serviceId, key }),
      });
      let body = null;
      try {
        body = await response.json();
      } catch {
        // Not JSON (a proxy's error page): the status says enough.
      }
      if (response.status === 200 && Array.isArray(body?.needs)) {
        deps.apply({ full: false, services: {}, removed: [], needs: body.needs });
        refocus(index, key, service, acknowledged);
      } else {
        // 404: the need is gone (it cleared, or the service is no longer the person's).
        fail(response.status === 404 ? "need_gone" : "need_ack_failed");
      }
    } catch {
      fail("need_ack_failed");
    } finally {
      busy = false;
      button.removeAttribute("aria-disabled");
    }
  }

  doc.addEventListener("click", (event) => {
    const target = event.target;
    if (!target?.closest) return;
    const toggle = target.closest(".mm-needs-toggle");
    if (toggle) return toggleAckedNeeds(toggle);
    const ack = target.closest(".mm-need-ack");
    const unack = target.closest(".mm-need-unack");
    const button = ack ?? unack;
    if (!button || button.getAttribute("aria-disabled") === "true") return;
    change(button, ack ? NEEDS_ACK_URL : NEEDS_UNACK_URL, Boolean(ack)).catch((error) => globalThis.console?.error?.(error));
  });

  return { change };
}

/**
 * Wires a page: edit mode, the first draw and what htmx does to the body. Returns
 * `{ editing, start, live, needs }`; `start()` draws the page (call it when the document is
 * ready). `deps` goes to `initEditing` and `reloadIfDataLost` (tests give it a
 * clock, a storage and a `reload`).
 *
 * The start page asks htmx not to keep a snapshot of it (hx-history="false" on
 * #groups): it holds one person's layout and htmx's cache outlives a sign-out in
 * the same tab. Back to the page fetches it from the server (for whoever is
 * signed in now); the draft kept by a dirty Back is in `initEditing`.
 *
 * A body htmx swaps in has no data blocks (see `reloadIfDataLost`): the page is
 * reloaded in full.
 */
export function wirePage(doc, deps = {}) {
  // `deps.live` goes to `initLive`; edit mode tells it when it opens and closes.
  const live = initLive(doc, deps.live ?? {});
  const editing = initEditing(doc, { ...deps, onEditingChange: () => live.sync() });
  const needs = initNeeds(doc, { ...deps.needs, apply: live.apply });
  const start = () => {
    // A start page with its data takes off the mark of an earlier reload.
    reloadIfDataLost(doc, deps);
    draw(doc);
    editing.offerRestore();
    live.sync();
  };
  doc.addEventListener("htmx:afterSettle", () => {
    if (reloadIfDataLost(doc, deps)) return;
    editing.check();
    drawIfNeeded(doc);
    editing.offerRestore();
    live.sync();
  });
  doc.addEventListener("htmx:historyRestore", () => {
    if (reloadIfDataLost(doc, deps)) return;
    drawIfNeeded(doc);
    editing.offerRestore();
    live.sync();
  });
  return { editing, start, live, needs };
}

if (typeof document !== "undefined") {
  const { start } = wirePage(document);
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start);
  } else {
    start();
  }
}

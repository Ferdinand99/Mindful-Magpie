// Drawing of the new-tab page without any page code, so node --test can drive it with fakes.
//
// The layout is drawn by the start page's own renderer (vendor/render.js, copied from
// static/magpie/ by web/build-extension.mjs). This file adds what is specific to the extension:
// the status line, the language of the labels, and image backgrounds (see background.js).
import { render, tickAges, updateValues } from "./vendor/render.js";
import { fetchBackground, imageId, serverOrigin } from "./background.js";
import { pickLabels } from "./labels.js";
import { renderStatus } from "./status.js";

const FALLBACK_PRESET = "hedgerow";

/**
 * Makes the draw function for start(). `doc` is the page's document; `statusEl` and `root` are the
 * status line and the element the renderer fills. `language` is navigator.language, `openOptions`
 * opens the options page, `objectUrl` is { create, revoke } (URL.createObjectURL and
 * URL.revokeObjectURL). The result is draw(view, { serverUrl, token }); it returns a promise that
 * settles when the background is final.
 *
 * A view is { data, status, services, values } (see newtab-core.js). draw() draws everything.
 * draw.update(view, conn) is for a refresh of the numbers: it keeps the card elements (so the
 * address bar keeps focus) and falls back to draw() when the page and the layout differ in more
 * than values. draw.tick(nowMs) moves the ages on without drawing. `renderer` is the vendored
 * renderer's { render, updateValues, tickAges }; tests pass spies.
 */
export function createView({
  doc,
  statusEl,
  root,
  language,
  fetchImpl,
  openOptions,
  objectUrl,
  renderer = { render, updateValues, tickAges },
}) {
  const { lang, labels } = pickLabels(language);
  doc.documentElement.setAttribute("lang", lang);
  const html = doc.documentElement;

  // The blob: URL of the image that is shown (or being fetched), kept so that the cached layout
  // and the fresh layout, which name the same image, load it once.
  let shown = { id: null, url: null };
  let pending = null; // { id, promise } of a fetch in flight
  let generation = 0; // grows with every draw; a late answer for an older draw is dropped

  function release() {
    if (shown.url) objectUrl.revoke(shown.url);
    shown = { id: null, url: null };
  }

  function fallBack() {
    // The image that was shown is gone from the page, so its blob URL goes too.
    release();
    html.setAttribute("data-bg", FALLBACK_PRESET);
    html.style.removeProperty("--mm-bg-image");
  }

  async function applyImage(id, conn, mine) {
    // The renderer wrote a url() that points at the server. The page's content security policy
    // blocks that (and it would carry no token), so replace it before the first paint.
    if (shown.id === id) {
      html.style.setProperty("--mm-bg-image", `url("${shown.url}")`);
      return;
    }
    html.style.setProperty("--mm-bg-image", "none");
    if (!pending || pending.id !== id) {
      pending = { id, promise: fetchBackground(fetchImpl, conn.serverUrl, conn.token, id) };
    }
    const own = pending;
    const blob = await own.promise;
    if (pending === own) pending = null;
    if (mine !== generation) return;
    if (!blob) {
      fallBack();
      return;
    }
    release();
    shown = { id, url: objectUrl.create(blob) };
    html.style.setProperty("--mm-bg-image", `url("${shown.url}")`);
  }

  // What the renderer was last given, so update() and tick() can reuse it.
  let drawn = null; // { data, opts }

  function renderOptions(view, conn) {
    return {
      lang,
      labels,
      imageBase: serverOrigin(conn.serverUrl) ?? undefined,
      services: view.services,
      values: view.values ?? undefined,
      // The extension's token is read-only, so no acknowledge button is drawn here. (Older
      // renderers ignore the option.)
      canAck: false,
    };
  }

  function draw(view, conn = {}) {
    generation += 1;
    renderStatus(statusEl, view.status, openOptions);
    if (!view.data) {
      // Not connected, or a token that no cache can stand in for: no layout, so no page. The
      // renderer would draw "your start page is empty", which is wrong here.
      root.replaceChildren();
      html.removeAttribute("data-bg");
      html.style.removeProperty("--mm-bg-image");
      release();
      drawn = null;
      return Promise.resolve();
    }
    const opts = renderOptions(view, conn);
    renderer.render(root, view.data, opts);
    drawn = { data: view.data, opts };
    const id = imageId(view.data.background);
    if (id === null) {
      release();
      return Promise.resolve();
    }
    return applyImage(id, conn, generation);
  }

  draw.update = function update(view, conn = {}) {
    if (!view.data || !drawn || drawn.data !== view.data) return draw(view, conn);
    const opts = renderOptions(view, conn);
    renderStatus(statusEl, view.status, openOptions);
    if (!renderer.updateValues(root, view.data, opts)) return draw(view, conn);
    drawn = { data: view.data, opts };
    return Promise.resolve();
  };

  draw.tick = function tick(nowMs) {
    if (!drawn) return;
    renderer.tickAges(root, { now: nowMs, lang, labels });
  };

  return draw;
}

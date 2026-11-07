// Drawing of the new-tab page without any page code, so node --test can drive it with fakes.
//
// The layout is drawn by the start page's own renderer (vendor/render.js, copied from
// static/magpie/ by web/build-extension.mjs). This file adds what is specific to the extension:
// the status line, the language of the labels, and image backgrounds (see background.js).
import { render } from "./vendor/render.js";
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
 */
export function createView({ doc, statusEl, root, language, fetchImpl, openOptions, objectUrl }) {
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

  return function draw(view, conn = {}) {
    generation += 1;
    renderStatus(statusEl, view.status, openOptions);
    if (!view.data) {
      // Not connected, or a token that no cache can stand in for: no layout, so no page. The
      // renderer would draw "your start page is empty", which is wrong here.
      root.replaceChildren();
      html.removeAttribute("data-bg");
      html.style.removeProperty("--mm-bg-image");
      release();
      return Promise.resolve();
    }
    render(root, view.data, { lang, labels, imageBase: serverOrigin(conn.serverUrl) ?? undefined });
    const id = imageId(view.data.background);
    if (id === null) {
      release();
      return Promise.resolve();
    }
    return applyImage(id, conn, generation);
  };
}

// Image backgrounds for the new tab. The renderer points --mm-bg-image at
// {origin}/start/background/{id}, but an extension page cannot send the token to a CSS url(), and
// the page's cookie is not sent either. So the image is fetched here with the token, turned into a
// blob: URL (the extension's content security policy allows img-src blob:), and that URL replaces
// the property. Any failure falls back to the default "hedgerow" preset.
//
// The token goes only into the Authorization header. Every failure maps to null, never to the
// original error, so the token cannot leak through an error string.

// What the server accepts as an image id (the renderer checks the same).
const IMAGE_ID = /^[A-Za-z0-9_-]{1,64}$/;

/** The image id of a layout background, or null when it is not an image background. */
export function imageId(background) {
  if (background?.kind !== "image") return null;
  return typeof background.id === "string" && IMAGE_ID.test(background.id) ? background.id : null;
}

/** The origin of the server address, or null for anything that is not an http(s) address. */
export function serverOrigin(serverUrl) {
  try {
    const url = new URL(serverUrl);
    return url.protocol === "http:" || url.protocol === "https:" ? url.origin : null;
  } catch {
    return null;
  }
}

/** The address of a background image on the server. */
export function backgroundUrl(serverUrl, id) {
  const origin = serverOrigin(serverUrl);
  return origin ? `${origin}/start/background/${id}` : null;
}

/**
 * Fetches a background image with the token. Resolves to a Blob, or to null when the request
 * fails, times out, is redirected, is not a 200 answer, or does not hold an image.
 */
export async function fetchBackground(fetchImpl, serverUrl, token, id, timeoutMs = 10000) {
  const url = backgroundUrl(serverUrl, id);
  if (!url || !token) return null;
  try {
    const res = await fetchImpl(url, {
      headers: { Authorization: `Bearer ${token}` },
      credentials: "omit",
      redirect: "error",
      signal: AbortSignal.timeout(timeoutMs),
    });
    if (res.status !== 200) return null;
    const blob = await res.blob();
    return typeof blob?.type === "string" && blob.type.startsWith("image/") ? blob : null;
  } catch {
    return null;
  }
}

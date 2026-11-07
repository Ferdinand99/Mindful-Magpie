# Mindful Magpie for Firefox

A Firefox extension (Manifest V3, Firefox 140 or newer) that replaces the new tab page with your
Mindful Magpie start page. It keeps the last layout in `browser.storage.local`, so a new tab
paints at once, and then fetches `GET {server}/api/v1/me/start` with a read-scoped API token.
Cards that are bound to a service also show its live numbers (see "Service values").

The new tab draws the layout with the same renderer as the web start page (`static/magpie/render.js`
and `render.css`), copied into `extension/vendor/` by `web/build-extension.mjs`. Links open in the
same tab. Interface texts come from `labels.js` (English and Norwegian, picked from
`navigator.language`).

## Try it

1. In Firefox open `about:debugging#/runtime/this-firefox`.
2. Choose "Load Temporary Add-on" and pick `manifest.json` in this folder (or a zip of this folder).
3. The options page opens in a tab: enter the address of your Magpie and press "Koble til Magpie".
   Firefox opens the Magpie consent page; after "Godkjenn" the extension gets a read-scoped token
   by itself (needs a Magpie server with the connect flow).
4. Fallback under "Avansert": paste a read token (Magpie, Settings, API tokens). "Test tilkobling"
   checks the connection; "Lagre" saves only when the test succeeds.

## Connect flow

`connect.js` implements the "Koble til Magpie" flow: PKCE (S256) verifier and challenge, a random
`state`, the consent url `{server}/connect/extension?...`, the redirect check and the code
exchange at `POST {server}/api/v1/extension/token`. The flow runs in
`browser.identity.launchWebAuthFlow`, so the manifest asks for the `identity` permission. The code,
verifier and token never appear in an error or a status text.

## The vendored renderer

Mozilla's add-on review forbids remote code, so the renderer ships inside the package.
`extension/vendor/` holds three files, all committed:

- `render.js` and `render.css`: copies of `static/magpie/render.js` and `render.css`.
- `tokens.css`: the Hedgerow kit's design tokens that `render.css` reads (custom properties, light and
  dark), from `web/kit-tokens.css`, which has the values of the kit's `static/style.css` at tag
  `vole-kit-v0.8.0`.

Never edit them by hand. After a change to the renderer (or to `web/kit-tokens.css`), run
`npm run build:extension` in `web/` and commit the result. CI runs the script and fails on any
difference (`git diff --exit-code`), and `web/extension-vendor.test.mjs` checks the same.

## Service values

Cards bound to a service show its numbers, status dot and age, as on the web
start page. `/api/v1/me/start` carries the service catalogue (`services`); `GET
{server}/api/v1/me/values` carries the numbers. Both are fetched with the same token.

- A new tab draws the cached layout, catalogue and values at once, then fetches the layout and the
  values together. While the tab is visible the values are fetched again every 30 seconds (an
  `EventSource` cannot send the token, so there is no stream); a hidden tab does not fetch.
- A refresh uses the renderer's `updateValues`, which keeps the card elements, so the address bar
  keeps focus. The renderer counts ages from the server's clock (`values.now`); the extension keeps
  the offset between that clock and the device clock and moves ages on with it.
- When the values fetch fails, the cached numbers stay and the status line reads «Frakoblet – viser
  sist lagrede»; the renderer dims numbers that are older than three intervals. A 401 or 403 reads
  «Ugyldig token – se innstillingene» and stops the refresh until the next new tab.
- A server from before the values API answers 404: the page is drawn without numbers and the server
  is asked again after five minutes at the earliest.
- The extension cannot acknowledge anything: its token is read-only, and a card with numbers
  opens its link like any other card.

## Privacy and storage

`browser.storage.local` holds the server address, the token, the last layout, the service catalogue
that came with it (service names and metric labels), the last service values (names, numbers and
status) and the clock offset. The values and the clock offset are tagged with the server address
and used only for that server. The storage is private to the extension and is not synced. The
cached layout, catalogue and values are dropped when the server address or the token changes, and
a tab that is still open then stops refreshing and writes nothing. A values cache bigger than
256 KiB is not kept. The extension sends nothing anywhere except to the configured server, and it
collects no data (`data_collection_permissions` is `none`).

## Image backgrounds

The renderer points `--mm-bg-image` at `{origin}/start/background/{id}`. An extension page cannot
send the token to a CSS `url()`, so `newtab-view.js` replaces it: `background.js` fetches the image
with `Authorization: Bearer` (no cookie, no redirect, 10 second timeout), makes a `blob:` URL of it
and sets that. The content security policy allows it with `img-src 'self' blob:`. When the fetch
fails, the background is the `hedgerow` preset.

## Rules

- The new tab page must keep the address bar focused: no `autofocus`, no `.focus()`, no input
  elements, no inline script or style.
- The content security policy has no `upgrade-insecure-requests`, so a plain-http LAN instance
  works (see `docs/spike-findings.md`).
- No remote code. Every script ships inside the extension.
- The token lives only in `browser.storage.local`. It is sent only in the `Authorization` header
  and is never put in a URL, a log line or a message.
- Host permission is optional and asked for the configured server only (`http(s)://host/*`, no
  port).

## Layout

- `state.js`: view logic, response handling and the fetches of the layout and the values (pure,
  tested).
- `connect.js`: the connect flow's pure functions (tested).
- `options-core.js`: address check, permission request, connection test, save and connect (tested).
- `newtab-core.js`: the new-tab start-up and 30 s values refresh with injected dependencies
  (tested).
- `newtab-view.js`: draws the status line and the layout with the vendored renderer, updates the
  numbers in place, and the image background (tested).
- `status.js`: the status line, with the «Koble til Magpie» button when not connected (tested).
- `background.js`: fetches an image background with the token (tested).
- `labels.js`: the renderer's interface strings, English and Norwegian (tested against the server's).
- `vendor/`: the renderer, its styles and its tokens (generated, see above).
- `newtab.*`, `options.*`, `shell.css`: the pages.
- Tests: `web/extension-*.test.mjs`, run with `npm ci && node --test` in `web/`.

## Signing and installing

Firefox only installs a permanent extension that Mozilla has signed. Magpie signs it on the
unlisted channel (addons.mozilla.org, "On your own"): it is not in the public catalogue.

To release a new version:

1. Change `version` in `extension/manifest.json` (for example `0.2.0` to `0.3.0`) and merge it to
   `main`. Mozilla refuses to sign a version twice, so every release needs a new number.
2. Tag the commit `extension-vX.Y.Z` (the same number) and push the tag.
   `.github/workflows/extension.yml` then lints, builds, signs and publishes. It fails when the tag
   and the manifest version differ. A manual run (Actions, Extension, Run workflow, with the
   version) does the same without a pushed tag.

The signed file is `mindful-magpie-X.Y.Z.xpi` on the release `extension-vX.Y.Z`
(https://github.com/Ferdinand99/Mindful-Magpie/releases). To install it, download the file and drag it
into a Firefox window, then accept the prompt. Opening the `.xpi` link in Firefox works too, but a
browser may download the file instead of installing it. Firefox updates are not automatic: install the
next `.xpi` over the old one.

The release job can be re-run on its own. When signing worked and only creating the release failed,
use "Re-run failed jobs": it takes the signed `.xpi` from the first run's artifact (kept 14 days) and
does not sign again. Re-running the whole workflow would sign again, and Mozilla refuses the same
version twice. A manual run stops before signing when the tag `extension-vX.Y.Z` already exists on
another commit. The workflow signs only for a tag or for `main`.

In the workflow, the "Build (check only)" step only proves that the package builds; the signed file
is made by `web-ext sign` from the same folder.

The repository secrets `AMO_JWT_ISSUER` and `AMO_JWT_SECRET` (an API key from addons.mozilla.org,
Developer Hub, Manage API Keys) are used by the sign step only. Nothing signs locally.

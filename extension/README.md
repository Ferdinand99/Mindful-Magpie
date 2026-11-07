# Mindful Magpie for Firefox

A Firefox extension (Manifest V3, Firefox 128 or newer) that replaces the new tab page with your
Mindful Magpie start page. It keeps the last layout in `browser.storage.local`, so a new tab
paints at once, and then fetches `GET {server}/api/v1/me/start` with a read-scoped API token.

This is the shell (plan 3, task A). The page body is a placeholder list of group and card titles;
the shared renderer of the start page replaces it in a later task.

## Try it

1. In Firefox open `about:debugging#/runtime/this-firefox`.
2. Choose "Load Temporary Add-on" and pick `manifest.json` in this folder (or a zip of this folder).
3. The options page opens in a tab: enter the address of your Magpie and a read token (Magpie,
   Settings, API tokens). "Test tilkobling" checks the connection; "Lagre" saves only when the
   test succeeds.

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

- `state.js`: view logic, response handling and the fetch (pure, tested).
- `options-core.js`: address check, permission request, connection test and save (tested).
- `placeholder.js`: placeholder drawing and status line (tested).
- `newtab.*`, `options.*`, `shell.css`: the pages.
- Tests: `web/extension-*.test.mjs`, run with `npm ci && node --test` in `web/`.

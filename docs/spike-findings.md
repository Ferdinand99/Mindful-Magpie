# Spike findings

A spike before the real build: does a Firefox extension's new-tab page behave like a start page should, with the
address bar keeping focus and the page loading fast from a self-hosted server? It was tested on 2026-11-07.

## Windows PC, Firefox

- **Address-bar focus:** with the extension's new-tab page, Ctrl+T leaves the focus in the address bar (placeholder
  and caret visible). The page takes no focus.
- **First open:** with no cache and no options set, the page renders its empty state in 34 ms.
- **Repeat opens:** the cached first paint shows at once (67 ms in the test), then the fresh data from the server
  replaces it.
- **Plain-http server:** the first fetch to a LAN instance over `http://` failed. Firefox's default MV3 content
  security policy for extension pages includes `upgrade-insecure-requests`, so the request was upgraded to https.
  Fix: set a custom `content_security_policy.extension_pages` without it. After the fix, data loaded from the LAN
  server.

## What follows for the extension

- A plain-http LAN instance needs the custom CSP in the extension manifest. An https instance (behind a reverse
  proxy) needs nothing.
- The new-tab page must never move focus: no `autofocus`, no `.focus()`. The start page of the server follows the
  same rule (a test guards it).

## Not covered by the spike

The offline and unauthorised-state checks and the Mac checks were not done in the spike. They are left to the tests of the
real extension and its browsers.

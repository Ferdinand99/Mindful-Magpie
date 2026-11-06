# Magpie spike

Throwaway spike for Mindful Magpie. It answers two questions:

1. Does a Firefox extension that overrides the new tab keep the address bar focused, where a normal web page steals focus?
2. Does cached data show instantly when a new tab opens?

This branch (`spike/extension`) is never merged.

## Run the server

```sh
docker build -t magpie-spike-server:dev spike/server
docker run -d --name magpie-spike-server --restart unless-stopped \
  -p 8099:8099 -e MM_SPIKE_TOKEN=<32 hex characters> magpie-spike-server:dev
```

Endpoints: `GET /api/spike/data` (needs `Authorization: Bearer <token>`), `GET /` (the same data as a plain web page, for comparison), `GET /healthz`.
Environment: `MM_SPIKE_TOKEN` (required), `MM_SPIKE_PORT` (default 8099).

The token for the running spike instance is in `spike/README.local` (git-ignored, never committed).

## Load the extension in Firefox

1. Open `about:debugging#/runtime/this-firefox`, click "Load Temporary Add-on", and pick `spike/extension/manifest.json` (or the packaged `magpie-spike.zip`).
2. Open the extension's options page, enter the server URL and token, and save. Accept the host permission prompt.
3. Open a new tab.

## What to check (Windows and Mac)

- Open a new tab and start typing at once. The text should land in the address bar.
- Compare with the plain web page: set it as the home/new-tab page (about:preferences#home, "Custom URLs") and repeat.
- Read the "vist på N ms" figure in the corner of the extension page on the second and later tabs; it shows when the cached data was painted.
- Stop the server and open a tab: cached data stays visible with an offline note.

## Logic test

```sh
cd spike/extension && node --test
```

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/magpie-logo-dark.svg">
    <img src="assets/brand/magpie-logo-light.svg" alt="Mindful Magpie" width="420">
  </picture>
</p>

<p align="center"><strong>A self-hosted start page for your links and services.</strong></p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-blue.svg" alt="License: AGPL-3.0"></a>
</p>

> **Status: under utvikling (in development).** Mindful Magpie ("Magpie") is a self-hosted start page for your
> links and services. Today it has first-run setup, sign-in, users and roles, two-factor sign-in and passkeys, API
> tokens, and a start page you build yourself in the browser, in the Hedgerow design: groups of link cards, bookmark
> pills, a clock and one of four backgrounds or your own picture, with an edit mode on the page (drag and drop, or
> the keyboard). A Firefox extension (`extension/`) shows the same page in the new tab. An admin sets up
> services under Settings → Services; a card can show a service's live numbers, which stay current through an
> event stream, and the «Trenger deg» row lists what needs you, with acknowledgements. A service may have a link
> on its cards (empty means the service's address up to the port). The kinds of service are: «Nettside» (a website
> check), Vigilant Vole, Keen Kestrel, Wise Wren, Proxmox VE, Unraid and Tofa, which an admin sets up with a
> read-only key (a self-signed certificate is trusted by pinning its fingerprint), and GitHub, which each person
> sets up for themselves under Settings → My integrations with their own token. More kinds follow. It is part of the Hedgerow family and is built on
> [vole-kit](https://github.com/Ferdinand99/Vigilant-Vole), the shared kit that brings sign-in, users, setup,
> translations and the design system. Nothing is released yet. The spike that settled the new-tab approach is in
> [`docs/spike-findings.md`](docs/spike-findings.md).

## Run it

There is no release yet, so the image `ghcr.io/ferdinand99/mindful-magpie` does not exist until the first one. Build
it from a checkout:

```bash
docker build -t mindful-magpie:dev .
docker run --rm mindful-magpie:dev --help   # every flag and its MM_ variable
```

The first start logs a setup link (`/setup?token=...`); open it to create the first admin. `docker-compose.yml` runs
the released image on `http://localhost:3010`, and has a commented `build: .` line to use your own build.

The image is a single static binary on Alpine. Data lives in `/data` (the database `magpie.sqlite`, the session key
and pre-migration backups). The session key also seals the keys of your services, so back up `/data` as a
whole: a database restored without its session key keeps the services but can no longer read their keys (you
type them again). The container starts as root only to hand `/data` to its `magpie` user; set `PUID` and
`PGID` to run as another uid and gid.

| Flag | Variable | Default | What |
|---|---|---|---|
| `--data-dir` | `MM_DATA_DIR` | `/data` | `magpie.sqlite`, the session key, pre-migration backups |
| `--bind` | `MM_BIND` | `0.0.0.0` | address to listen on |
| `--port` | `MM_PORT` | `3010` | port to listen on |
| `--public-url` | `MM_PUBLIC_URL` | (none) | the address people use; stored as the General page's public URL |
| `--extension-ids` | `MM_EXTENSION_IDS` | (none) | extra browser extension IDs, comma separated, that may use "Koble til Magpie" (a development build); the official extension always may |
| | `MM_TRUST_PROXY` | off | honour `X-Forwarded-*` from a reverse proxy (read by vole-kit) |
| | `MM_SETUP_TOKEN` | (random, logged at the first start) | `/setup` only opens with `?token=<value>` until the first user exists (read by vole-kit) |

## Behind a reverse proxy

Set the public address and tell Magpie to trust the proxy's forwarded headers:

```yaml
environment:
  - MM_PUBLIC_URL=https://start.example.com
  - MM_TRUST_PROXY=1
```

`MM_PUBLIC_URL` must be the root of its own host name (a path like `https://example.com/start` is refused at
startup). Without `MM_TRUST_PROXY=1` Magpie sees every request as coming from the proxy: the sign-in rate limits are
keyed to the proxy's address, so one person's failed attempts lock out everyone (the same goes for the rate limit of the
browser extension's token endpoint, which is per client address), and Magpie cannot tell that the
request came in over HTTPS, so it sends no HSTS header.

The live numbers and the «Trenger deg» row reach the page through a server-sent event stream (`/start/live`),
so the proxy must not buffer that response: turn proxy buffering off (nginx: `proxy_buffering off;` for the
location, or for the whole site). With buffering on, the page still works but updates late or in bursts.

## Develop

See [`docs/development.md`](docs/development.md). All builds and tests run in Docker.

## Licence

[AGPL-3.0](LICENSE).

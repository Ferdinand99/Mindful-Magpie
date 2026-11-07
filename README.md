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

> **Status: skeleton.** Mindful Magpie ("Magpie") is in its first steps. The server starts, serves a health probe
> and its static files, and keeps its data in SQLite. Sign-in, setup and the start page itself follow. It is part of
> the Hedgerow family and is built on [vole-kit](https://github.com/Ferdinand99/Vigilant-Vole), the shared kit that
> brings sign-in, users, setup, translations and the design system.

## Run it

```bash
docker compose up -d      # http://localhost:3010
docker run --rm ghcr.io/ferdinand99/mindful-magpie:latest --help   # every flag and its MM_ variable
```

The image is a single static binary on Alpine. Data lives in `/data` (the database `magpie.sqlite`, the session key
and pre-migration backups). The container starts as root only to hand `/data` to its `magpie` user; set `PUID` and
`PGID` to run as another uid and gid.

| Flag | Variable | Default | What |
|---|---|---|---|
| `--data-dir` | `MM_DATA_DIR` | `/data` | `magpie.sqlite`, the session key, pre-migration backups |
| `--bind` | `MM_BIND` | `0.0.0.0` | address to listen on |
| `--port` | `MM_PORT` | `3010` | port to listen on |
| `--public-url` | `MM_PUBLIC_URL` | (none) | the address people use; stored as the General page's public URL |
| | `MM_TRUST_PROXY` | off | honour `X-Forwarded-*` from a reverse proxy (read by vole-kit) |
| | `MM_SETUP_TOKEN` | (random, logged at the first start) | `/setup` only opens with `?token=<value>` until the first user exists (read by vole-kit) |

## Develop

See [`docs/development.md`](docs/development.md). All builds and tests run in Docker.

## Licence

[AGPL-3.0](LICENSE).

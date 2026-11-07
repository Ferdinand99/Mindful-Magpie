# Development

Mindful Magpie is one Cargo package (`mindful-magpie`, binary `mindful-magpie`) built on
[vole-kit](https://github.com/Ferdinand99/Vigilant-Vole), pinned by git tag in `Cargo.toml`. The kit brings
sign-in, setup, users and roles, API tokens, 2FA, Security, General settings, the API reference, i18n, the
database plumbing and the design system.

## Layout

```
Cargo.toml            # the package; vole-kit by git tag
build.rs              # copies the kit's templates into templates/kit/ (git-ignored)
rust-toolchain.toml   # Rust 1.99
src/
  main.rs             # flags/env, logging, server
  lib.rs              # start() (database, state) and router()
  product.rs          # KitConfig: names, cookies, token prefixes, nav, settings tabs, locales, static files
  pages.rs            # Magpie's own pages: the start page and the Settings redirect
  api.rs              # REST routes for the extension: GET /api/v1/me/start (the layout plus `services`), POST /api/v1/extension/token
  connect.rs          # the extension's consent page (GET/POST /connect/extension) and its one-time codes
  config.rs           # flags and MM_* environment variables
  server.rs           # listener, graceful shutdown
  db.rs               # Magpie's migration set (after the kit's core set)
  layout/             # the start page layout: model and validation, per-user versioned storage, uploaded backgrounds (background.rs)
  integrations/       # the integration contract (mod.rs: the Integration trait, Snapshot, FetchCtx, the guarded HTTP client, the registry), the built-in kinds (http_kind.rs: "Nettside") and the background poller (poller.rs: fetches every service on its interval, keeps the latest snapshot in memory and in service_snapshots, follows the service events)
  services/           # admin-managed services: store.rs (tables, sealed secrets, who sees what), pages.rs (Settings → Services and «Test»), mod.rs (types and checks)
templates/            # Magpie's pages (extend kit/layout.html)
static/               # favicon, icons, web manifest, magpie.css, clock.js
static/magpie/        # the start page scripts (ES modules: render.js is the shared renderer, edit.js is edit mode with ops.js and rules.js) and render.css
web/                  # JavaScript tests (node --test, linkedom): render*, ops, rules, edit (edit mode, the page wiring in start.js), extension-*; build-extension.mjs and kit-tokens.css; dev-only
extension/            # the Firefox extension; extension/vendor/ is generated from static/magpie/ and web/kit-tokens.css
migrations/           # Magpie's migrations (0001 init, 0002 layouts, 0003 extension codes, 0004 backgrounds, 0005 kept background, 0006 services, 0007 snapshots)
locales/              # Magpie's strings (en, nb)
tests/                # boot.rs, auth.rs, start.rs, layout.rs, layout_http.rs, cards.rs (cards bound to services: the save checks, the `#mm-services` embed, what a hidden service leaks), api_start.rs, background.rs (uploads, the kept picture, token limits, upgrades from older schemas), connect.rs, services.rs (the services pages, sealed secrets, «Test» and the http kind against a stub server run in the test), poller.rs (the poller on a paused clock with a stub kind; hostile answers and secrets against a stub web server); common/ builds an in-process app on a temporary data directory
Dockerfile            # static musl binary on Alpine
docker/               # the entrypoint (PUID/PGID)
about.toml, about.hbs # cargo-about: licence allowlist and template for THIRD-PARTY-NOTICES
scripts/              # notices.sh (make or check the notices), test-notices-fail.sh
legal/                # licence texts of the Rust standard library and musl, added to the notices
deny.toml             # cargo-deny (advisories), run by the Audit workflow
release-please-*.json # release automation
.github/workflows/    # ci.yml, audit.yml, release-please.yml, docker-publish.yml
```

## Build and test

Builds and tests run in Docker on a build host (a shared Docker daemon), not on a workstation. These are the
cargo commands that run inside a Rust 1.99 container with clippy and rustfmt installed.

```bash
cargo test --locked                                  # 228 Rust tests: boot, auth and users, start page, layout, backgrounds and migrations, extension API and connect flow, services, the poller
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all --check
cd web && npm ci && node --test                     # 305 JavaScript tests: renderer, operations, rules, edit mode and page wiring, the extension (node:22 container; linkedom stands in for the DOM)
cd web && npm run build:extension                    # copies the renderer into extension/vendor/; git must show no change
docker build -t magpie:dev .
docker run --rm magpie:dev --help     # every flag and its MM_ variable
```

Keep the cargo registry and the target directory in named volumes so that rebuilds are incremental, and limit the
container (for example `--cpus 4 --memory 6g`).

The in-process tests need no environment variable: they build with vole-kit's `test-support` feature (a
`[dev-dependencies]` entry), which opens `/setup` by default.

To work against a local checkout of the kit, create `.cargo/config.toml` (git-ignored):

```toml
[patch."https://github.com/Ferdinand99/Vigilant-Vole"]
vole-kit = { path = "../Vigilant-Vole/kit" }
vole-kit-build = { path = "../Vigilant-Vole/kit/kit-build" }
```

## Third-party notices

`THIRD-PARTY-NOTICES` holds the licence texts of the Rust crates in the binary. It is made from the dependencies, so
a changed dependency (including a Dependabot bump) makes it stale and CI fails. Regenerate it with cargo-about 0.9.3
(CI installs that version with a pinned checksum; the same version is in `scripts/notices.sh`):

```bash
scripts/notices.sh           # write THIRD-PARTY-NOTICES
scripts/notices.sh --check   # what CI runs
scripts/test-notices-fail.sh # proves the check stops on a licence that is not allowed
```

A new licence must be added to `accepted` in `about.toml` on purpose. The image carries the file and `LICENSE` in
`/usr/share/doc/mindful-magpie/`.

## Signing the extension

`extension/` is the Firefox extension. `web/` holds its test tooling, including the pinned `web-ext`. CI runs
`npx web-ext lint --source-dir ../extension --self-hosted` in the `Web tests` job on every pull
request. Run the same commands on the build host:

```bash
cd web && npm ci
node --test
npm run build:extension       # copy the shared renderer into extension/vendor/ (commit the result)
npx web-ext lint --source-dir ../extension --self-hosted
npx web-ext build --source-dir ../extension --artifacts-dir /tmp/out   # a zip, not signed
```

`extension/vendor/` holds copies of `static/magpie/render.js` and `render.css` plus the kit tokens they need
(`web/kit-tokens.css`), because the add-on package must hold all its code. Change the renderer, then run
`npm run build:extension` and commit `extension/vendor/`. The `web` job in `ci.yml` runs the script and fails on any
difference (`git diff --exit-code`).

Signing happens only in `extension.yml`, never on a workstation. To release: bump `version` in
`extension/manifest.json`, merge, then push the tag `extension-vX.Y.Z` (same number). The workflow checks the
manifest version against the tag, builds, signs on the unlisted channel with the secrets `AMO_JWT_ISSUER` and
`AMO_JWT_SECRET`, and attaches `mindful-magpie-X.Y.Z.xpi` to the release `extension-vX.Y.Z`. That release is not
marked as latest, so the app release stays the latest one. The sign job runs only for a tag or on `main`, and the
signing secrets go in as `WEB_EXT_API_KEY` and `WEB_EXT_API_SECRET` in the environment of the sign step. A manual run
stops before signing when the tag already exists on another commit. The "Build (check only)" step only proves that
the package builds. Mozilla signs a version once: when signing itself fails after the upload was accepted, bump the
version and tag again. When only the release job fails, re-run that job alone ("Re-run failed jobs"): it uses the
signed file from the first run's artifact and does not sign again. Install: download the `.xpi` and drag it into a
Firefox window (opening the URL in Firefox can also work, but a browser may only download the file).

## CI and releases

- `ci.yml`: format, clippy with `-D warnings` and tests; the `web` job (`npm ci`, `node --test` in `web/`, the vendored-renderer check and `web-ext lint`); the notices check; on pull requests a Docker build for
  linux/amd64 and linux/arm64 (nothing is pushed).
- `audit.yml`: RustSec advisories (`cargo deny check advisories`) on dependency changes and weekly.
- `release-please.yml`: keeps a release pull request open on `main` (version and changelog from Conventional
  Commits). Merging it tags `vX.Y.Z`, creates the release and calls `docker-publish.yml`. The first release is 0.1.0.
- `docker-publish.yml`: publishes the multi-arch image `ghcr.io/ferdinand99/mindful-magpie` (`:X.Y.Z`, `:X.Y`, `:X`,
  `:latest`) on release tags only, never from `main`.
- `extension.yml`: on a tag `extension-vX.Y.Z` (or a manual run), signs the Firefox extension with Mozilla and
  attaches the `.xpi` to a release (see "Signing the extension").

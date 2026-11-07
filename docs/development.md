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
  config.rs           # flags and MM_* environment variables
  server.rs           # listener, graceful shutdown
  db.rs               # Magpie's migration set (after the kit's core set)
templates/            # Magpie's pages (extend kit/layout.html)
static/               # favicon, icons, web manifest, magpie.css, clock.js
migrations/           # Magpie's migrations
locales/              # Magpie's strings (en, nb)
tests/                # boot.rs, auth.rs, start.rs; common/ builds an in-process app on a temporary data directory
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
cargo test --locked                                  # 19 tests: boot, auth and users, start page
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all --check
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

## CI and releases

- `ci.yml`: format, clippy with `-D warnings` and tests; the notices check; on pull requests a Docker build for
  linux/amd64 and linux/arm64 (nothing is pushed).
- `audit.yml`: RustSec advisories (`cargo deny check advisories`) on dependency changes and weekly.
- `release-please.yml`: keeps a release pull request open on `main` (version and changelog from Conventional
  Commits). Merging it tags `vX.Y.Z`, creates the release and calls `docker-publish.yml`. The first release is 0.1.0.
- `docker-publish.yml`: publishes the multi-arch image `ghcr.io/ferdinand99/mindful-magpie` (`:X.Y.Z`, `:X.Y`, `:X`,
  `:latest`) on release tags only, never from `main`.

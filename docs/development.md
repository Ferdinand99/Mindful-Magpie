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
  config.rs           # flags and MM_* environment variables
  server.rs           # listener, graceful shutdown
  db.rs               # Magpie's migration set (after the kit's core set)
templates/            # Magpie's pages (extend kit/layout.html)
static/               # favicon, icons, web manifest, magpie.css
migrations/           # Magpie's migrations
locales/              # Magpie's strings (en, nb)
tests/                # boot.rs; common/ builds an in-process app on a temporary data directory
Dockerfile            # static musl binary on Alpine
docker/               # the entrypoint (PUID/PGID)
```

## Build and test

Builds and tests run in Docker on a build host (a shared Docker daemon), not on a workstation. These are the
cargo commands that run inside a Rust 1.99 container with clippy and rustfmt installed.

```bash
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all --check
docker build -t magpie:dev .
docker run --rm magpie:dev --help     # every flag and its MM_ variable
```

The in-process tests need no environment variable: they build with vole-kit's `test-support` feature (a
`[dev-dependencies]` entry), which opens `/setup` by default.

To work against a local checkout of the kit, create `.cargo/config.toml` (git-ignored):

```toml
[patch."https://github.com/Ferdinand99/Vigilant-Vole"]
vole-kit = { path = "../Vigilant-Vole/kit" }
vole-kit-build = { path = "../Vigilant-Vole/kit/kit-build" }
```

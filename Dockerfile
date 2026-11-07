# syntax=docker/dockerfile:1
#
# Mindful Magpie: a static musl binary on Alpine, rustls only (no OpenSSL in
# the binary).
#
#   docker build -t mindful-magpie:latest .
#
# Needs BuildKit (the default since Docker 23). The cargo registry, the git
# checkout of vole-kit and the target directory live in cache mounts, so
# rebuilds after a source change only recompile Magpie itself.

ARG RUST_VERSION=1.99
ARG ALPINE_VERSION=3.22

# --- Builder -----------------------------------------------------------------
FROM rust:${RUST_VERSION}-alpine AS builder

# build-base: C toolchain for the bundled SQLite and aws-lc-rs (rustls crypto).
RUN apk add --no-cache build-base

WORKDIR /app

# Smaller binaries without touching Cargo.toml: no symbols, and link-time
# optimisation across all crates in one unit.
ENV CARGO_PROFILE_RELEASE_STRIP=symbols \
    CARGO_PROFILE_RELEASE_LTO=true \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
    CARGO_TERM_COLOR=never

# Everything the build reads: templates, static files, migrations and locales
# are compiled into the server. vole-kit comes from its git tag (Cargo.lock).
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY templates ./templates
COPY static ./static
COPY migrations ./migrations
COPY locales ./locales

# BUILD_JOBS (build arg): cap parallel compile jobs on a shared machine.
ARG BUILD_JOBS=
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo build --release --locked ${BUILD_JOBS:+--jobs $BUILD_JOBS} \
    && cp target/release/mindful-magpie /usr/local/bin/

# --- Server ------------------------------------------------------------------
FROM alpine:${ALPINE_VERSION} AS server

ARG VERSION=0.0.0
ARG REVISION=unknown
ARG CREATED=unknown

LABEL org.opencontainers.image.title="Mindful Magpie" \
      org.opencontainers.image.description="A self-hosted start page for your links and services: one Rust binary, SQLite" \
      org.opencontainers.image.url="https://github.com/Ferdinand99/Mindful-Magpie" \
      org.opencontainers.image.source="https://github.com/Ferdinand99/Mindful-Magpie" \
      org.opencontainers.image.documentation="https://github.com/Ferdinand99/Mindful-Magpie#readme" \
      org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.created="${CREATED}"

# ca-certificates: TLS trust store for outgoing HTTPS.
# tzdata: time zones for display.
# su-exec: the entrypoint drops from root to `magpie` after fixing /data ownership.
# Unprivileged user `magpie` (uid/gid 1000) owns /data and runs the app.
RUN apk add --no-cache ca-certificates tzdata su-exec \
    && addgroup -S -g 1000 magpie \
    && adduser -S -D -H -u 1000 -G magpie -h /data -s /sbin/nologin magpie \
    && mkdir -p /data /usr/share/doc/mindful-magpie \
    && chown magpie:magpie /data

COPY --from=builder /usr/local/bin/mindful-magpie /usr/local/bin/mindful-magpie
COPY --chmod=755 docker/entrypoint.sh /usr/local/bin/docker-entrypoint.sh

# The licence texts: Magpie's own (AGPL-3.0) and the notices of the crates in the
# binary (scripts/notices.sh makes the file; CI checks that it is up to date).
COPY --chmod=644 LICENSE THIRD-PARTY-NOTICES /usr/share/doc/mindful-magpie/

ENV MM_DATA_DIR=/data \
    MM_PORT=3010

# No `USER` here on purpose: the entrypoint starts as root only to chown /data,
# then runs the app as `magpie` via su-exec. Setting `user:` / `--user` skips
# that step entirely.
WORKDIR /data
VOLUME ["/data"]
EXPOSE 3010

# The server answers /healthz (busybox wget, no curl needed).
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD wget -q -O /dev/null "http://127.0.0.1:${MM_PORT:-3010}/healthz" || exit 1

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]

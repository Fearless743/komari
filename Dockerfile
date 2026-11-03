# syntax=docker/dockerfile:1.7

# Komari Rust backend image.
#
# Build:
#   docker build -t komari:local .
#   docker build \
#     --build-arg KOMARI_VERSION=1.2.3 \
#     --build-arg KOMARI_VERSION_HASH=$(git rev-parse HEAD) \
#     -t komari:local .
#
# The frontend is owned by task B14. Its production build is copied from
# `KOMARI_WEB_DIST` (relative to the build context) into /app/web. The
# committed placeholder is web/dist.

ARG RUST_VERSION=1.98

# ---------------------------------------------------------------------------
# Dependency cache: cargo-chef builds third-party crates in a separate layer.
# ---------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---------------------------------------------------------------------------
# Builder
# ---------------------------------------------------------------------------
FROM chef AS builder

COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json --package komari-cli

# Frontend static assets (B14). Override with a real build output directory.
ARG KOMARI_WEB_DIST=web/dist
COPY ${KOMARI_WEB_DIST}/ /app/web/

COPY . .

# Version injection mirrors the Go release workflow's ldflags:
#   -X .../utils.CurrentVersion  -> KOMARI_VERSION
#   -X .../utils.VersionHash     -> KOMARI_VERSION_HASH
ARG KOMARI_VERSION=dev
ARG KOMARI_VERSION_HASH=unknown
ENV KOMARI_VERSION=${KOMARI_VERSION} \
    KOMARI_VERSION_HASH=${KOMARI_VERSION_HASH}

RUN cargo build --release --locked -p komari-cli \
    && strip target/release/komari

# ---------------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

ARG KOMARI_VERSION=dev
ARG KOMARI_VERSION_HASH=unknown

LABEL org.opencontainers.image.title="Komari" \
      org.opencontainers.image.description="A simple server monitoring tool" \
      org.opencontainers.image.version="${KOMARI_VERSION}" \
      org.opencontainers.image.revision="${KOMARI_VERSION_HASH}" \
      org.opencontainers.image.source="https://github.com/komari-monitor/komari" \
      org.opencontainers.image.licenses="MIT"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 komari \
    && useradd --system --uid 10001 --gid komari --home-dir /app --shell /usr/sbin/nologin komari \
    && mkdir -p /app/data /app/web \
    && chown -R komari:komari /app

COPY --from=builder /src/target/release/komari /usr/local/bin/komari
COPY --from=builder /app/web /app/web

RUN chmod 0755 /usr/local/bin/komari \
    && chown -R komari:komari /app

# Runtime defaults match the Go image. `KOMARI_LISTEN` and `KOMARI_DB_FILE`
# can be overridden from docker-compose or `docker run -e`.
ENV KOMARI_LISTEN=0.0.0.0:25774 \
    KOMARI_DB_TYPE=sqlite \
    KOMARI_DB_FILE=/app/data/komari.db \
    KOMARI_DB_HOST=localhost \
    KOMARI_DB_PORT=3306 \
    KOMARI_DB_USER=root \
    KOMARI_DB_PASS= \
    KOMARI_DB_NAME=komari \
    KOMARI_WEB_DIR=/app/web

WORKDIR /app

# SQLite database, downloaded GeoIP database (data/GeoLite2-Country.mmdb) and
# themes all live under /app/data, matching the Go backend's working directory.
VOLUME ["/app/data"]

USER komari
EXPOSE 25774

ENTRYPOINT ["/usr/local/bin/komari"]
CMD ["server"]

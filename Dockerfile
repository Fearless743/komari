# syntax=docker/dockerfile:1.7

# Komari Rust backend image.
#
# Build (default: embed the placeholder theme committed by B14):
#   docker build -t komari:local .
#
# Build with a real frontend checkout (the directory must contain `dist/`,
# `komari-theme.json` and `preview.png`/`perview.png`):
#   docker build \
#     --build-arg KOMARI_THEME_SRC=.komari-theme \
#     --build-arg KOMARI_DEFAULT_THEME_DIR=/src/theme \
#     --build-arg KOMARI_VERSION=1.2.3 \
#     --build-arg KOMARI_VERSION_HASH=$(git rev-parse HEAD) \
#     -t komari:local .

ARG RUST_VERSION=1.98

# ---------------------------------------------------------------------------
# Dependency cache: cargo-chef builds third-party crates in a separate layer.
# ---------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-bookworm AS chef
RUN cargo install cargo-chef --version 0.1.78 --locked
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

# Frontend theme source, relative to the build context. The default is B14's
# committed placeholder; CI and release builds pass a real komari-web checkout.
ARG KOMARI_THEME_SRC=crates/komari-web/defaultTheme
COPY ${KOMARI_THEME_SRC}/ /src/theme/

COPY . .

# B14's komari-web/build.rs reads KOMARI_DEFAULT_THEME_DIR at compile time and
# embeds that directory with rust-embed. The default path exists after
# `COPY . .` on a tree that includes B14. A real theme copied to /src/theme is
# selected by passing --build-arg KOMARI_DEFAULT_THEME_DIR=/src/theme.
ARG KOMARI_DEFAULT_THEME_DIR=/src/crates/komari-web/defaultTheme
ENV KOMARI_DEFAULT_THEME_DIR=${KOMARI_DEFAULT_THEME_DIR} \
    CARGO_PROFILE_RELEASE_STRIP=symbols

# Version injection mirrors the Go release workflow's ldflags:
#   -X .../utils.CurrentVersion  -> KOMARI_VERSION
#   -X .../utils.VersionHash     -> KOMARI_VERSION_HASH
ARG KOMARI_VERSION=dev
ARG KOMARI_VERSION_HASH=unknown
ENV KOMARI_VERSION=${KOMARI_VERSION} \
    KOMARI_VERSION_HASH=${KOMARI_VERSION_HASH}

# Build the whole workspace so komari-web (B14) is compiled and its embedded
# theme is available to the HTTP integration. The final `komari` binary is
# still produced by komari-cli.
RUN cargo build --release --locked --workspace \
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
      org.opencontainers.image.source="https://github.com/Fearless743/komari" \
      org.opencontainers.image.licenses="MIT"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 komari \
    && useradd --system --uid 10001 --gid komari --home-dir /app --shell /usr/sbin/nologin komari \
    && mkdir -p /app/data \
    && chown -R komari:komari /app

COPY --from=builder /src/target/release/komari /usr/local/bin/komari

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
    KOMARI_DB_NAME=komari

WORKDIR /app

# SQLite database, downloaded GeoIP database (data/GeoLite2-Country.mmdb) and
# installed themes all live under /app/data, matching the Go backend.
VOLUME ["/app/data"]

USER komari
EXPOSE 25774

ENTRYPOINT ["/usr/local/bin/komari"]
CMD ["server"]

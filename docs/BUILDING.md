# Building Komari (Rust backend)

This document covers the build, test, cross-compilation, container and release
infrastructure for the Rust rewrite. The Go implementation in the sibling
`komari` repository remains the reference for runtime behaviour.

## Prerequisites

| Tool | Version | Notes |
| --- | --- | --- |
| Rust | 1.85+ (edition 2024) | Stable toolchain; CI uses `dtolnay/rust-toolchain@stable` |
| Cargo | bundled with Rust | `Cargo.lock` is committed; builds use `--locked` |
| Docker | 24+ with BuildKit | Optional, for container builds |
| `cross` | latest | Optional, for Linux musl cross-compilation |
| Node.js | 20+ | Optional, only for the B14 frontend assets |

```sh
rustup toolchain install stable
rustup component add rustfmt clippy
```

## Quick start

```sh
make check          # cargo fmt --check + clippy -D warnings + tests
make build          # debug workspace build
make release        # release build with version injection -> dist/komari
./dist/komari --version
```

`make help` lists every target.

## Version injection

The Go pipeline injected two variables with linker flags:

```sh
go build -ldflags "-X github.com/Fearless743/komari/utils.CurrentVersion=$VERSION \
                   -X github.com/Fearless743/komari/utils.VersionHash=$VERSION_HASH"
```

The Rust port reads the same values at compile time from environment variables
in `crates/komari-cli/src/version.rs`:

| Go variable | Rust source | Environment variable | Default |
| --- | --- | --- | --- |
| `utils.CurrentVersion` | `version::VERSION` | `KOMARI_VERSION` | `CARGO_PKG_VERSION` (`0.1.0`) |
| `utils.VersionHash` | `version::VERSION_HASH` | `KOMARI_VERSION_HASH` | `unknown` |

`KOMARI_VERSION_HASH=unknown` marks a development build and selects debug
logging by default, matching the Go `main.go` behaviour. `--version` prints the
injected version:

```sh
KOMARI_VERSION=9.9.9 KOMARI_VERSION_HASH=abc ./scripts/build.sh
./dist/komari --version
# komari 9.9.9
```

`option_env!` values are not tracked by Cargo's fingerprint, so both
`scripts/build.sh` and the CI workflows touch
`crates/komari-cli/src/version.rs` before compiling. If you invoke `cargo build`
directly after changing the variables, run `cargo clean -p komari-cli` or touch
that file first.

### `scripts/build.sh`

| Variable | Default | Meaning |
| --- | --- | --- |
| `KOMARI_VERSION` | `git describe --tags --always --dirty` | Version string |
| `KOMARI_VERSION_HASH` | `git rev-parse HEAD` | Commit hash |
| `PACKAGE` | `komari-cli` | Cargo package to build |
| `PROFILE` | `release` | Cargo profile (`release` or `dev`) |
| `TARGET` | host | Rust target triple |
| `OUT_DIR` | `dist` | Output directory |
| `CARGO` | `cargo` | Cargo executable |

```sh
# Local development build with an explicit version
KOMARI_VERSION=dev KOMARI_VERSION_HASH="$(git rev-parse HEAD)" ./scripts/build.sh

# Cross build
rustup target add aarch64-unknown-linux-musl
TARGET=aarch64-unknown-linux-musl ./scripts/build.sh
```

`make release VERSION=1.2.3 VERSION_HASH=abcdef0` is equivalent to invoking the
script with the two variables.

## Cross-compilation

Release artifacts use these targets:

| Artifact | Target | Runner |
| --- | --- | --- |
| `komari-linux-amd64` | `x86_64-unknown-linux-musl` | Ubuntu + `cross` |
| `komari-linux-arm64` | `aarch64-unknown-linux-musl` | Ubuntu + `cross` |
| `komari-darwin-amd64` | `x86_64-apple-darwin` | macOS 14 |
| `komari-darwin-arm64` | `aarch64-apple-darwin` | macOS 14 |
| `komari-windows-amd64.exe` | `x86_64-pc-windows-msvc` | Windows Server |

Linux binaries are statically linked against musl, mirroring the Go release
builds. `cross` (https://github.com/cross-rs/cross) supplies the musl
toolchain and runs the build in a container:

```sh
cargo install cross --locked
cross build --locked --release -p komari-cli --target aarch64-unknown-linux-musl
```

macOS builds run on an Apple Silicon runner; the Xcode SDK contains both slices,
so `x86_64-apple-darwin` is built with `cargo build --target ...` without an
Intel runner.

## Docker

The `Dockerfile` is a multi-stage build:

1. `chef` installs `cargo-chef`.
2. `planner` records the dependency recipe from `Cargo.toml` / `Cargo.lock`.
3. `builder` caches third-party crates, copies the frontend assets, compiles
   `komari-cli` in release mode with the version variables and strips the
   binary.
4. `runtime` is `debian:bookworm-slim` with `ca-certificates`, `tzdata`, a
   non-root user (uid/gid `10001`) and the `/app/data` volume.

```sh
docker build -t komari:local .

docker build \
  --build-arg KOMARI_VERSION=1.2.3 \
  --build-arg KOMARI_VERSION_HASH="$(git rev-parse HEAD)" \
  -t komari:local .

docker run -d --name komari \
  -p 25774:25774 \
  -v komari-data:/app/data \
  komari:local
```

### Build arguments

| Argument | Default | Meaning |
| --- | --- | --- |
| `RUST_VERSION` | `1.98` | Rust image tag (`rust:<version>-bookworm`) |
| `KOMARI_VERSION` | `dev` | Version baked into the binary and OCI labels |
| `KOMARI_VERSION_HASH` | `unknown` | Commit hash baked into the binary and labels |
| `KOMARI_WEB_DIST` | `web/dist` | Frontend output directory, relative to the build context |

### Frontend assets (B14)

Frontend integration belongs to task **B14**. Until it lands, `web/dist`
contains only a `.gitkeep`, so Docker builds succeed without a UI. B14 should
write its production output (Vite/NPM `dist`) to `web/dist`, or pass another
path:

```sh
docker build --build-arg KOMARI_WEB_DIST=path/to/dist .
```

The assets are copied to `/app/web` in the image and `KOMARI_WEB_DIR` points at
that directory. CI runs an optional `npm install && npm run build` step inside
`web/` only when `web/package.json` exists.

### Runtime configuration

| Variable | Default | Notes |
| --- | --- | --- |
| `KOMARI_LISTEN` | `0.0.0.0:25774` | Same as Go |
| `KOMARI_DB_TYPE` | `sqlite` | `sqlite` or `mysql` |
| `KOMARI_DB_FILE` | `/app/data/komari.db` | SQLite file; the Go image also used `/app/data` |
| `KOMARI_DB_HOST` / `_PORT` / `_USER` / `_PASS` / `_NAME` | Go defaults | MySQL settings |
| `KOMARI_LOG_LEVEL` | `info` | Release default |
| `KOMARI_THEME` | `default` | Theme directory name |
| `KOMARI_WEB_DIR` | `/app/web` | Copied frontend assets |

The working directory is `/app`, so downloaded GeoIP data
(`data/GeoLite2-Country.mmdb`) and themes live under the `/app/data` volume,
matching the Go backend's relative paths.

`docker-compose.yml` starts the same image with a named volume, restart policy,
port mapping and environment examples:

```sh
docker compose up -d --build
docker compose logs -f komari
```

The container runs as uid/gid `10001`. A named volume inherits that ownership
from the image. For a bind mount, create the directory and fix ownership first:

```sh
mkdir -p ./data
sudo chown -R 10001:10001 ./data
# then uncomment the bind mount in docker-compose.yml
```

## CI and releases

`.github/workflows/build.yml` runs on pushes to `main`, pull requests and
manual dispatch:

- `lint`: `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets --locked -- -D warnings`.
- `test`: `cargo test --workspace --locked`.
- `build`: the five-target matrix above, with `Swatinem/rust-cache@v2` and
  version injection from `github.sha` (matching the Go build workflow).
  Artifacts are uploaded as `komari-<os>-<arch>` plus a `.sha256` file.
- `docker`: builds the image without pushing to validate the Dockerfile.

`.github/workflows/release.yml` runs when a GitHub release is published (or
manually with a tag). It runs the test suite, builds the same matrix with
`KOMARI_VERSION=<tag>` and `KOMARI_VERSION_HASH=<commit>`, uploads every asset
to the release with `gh release upload --clobber`, and publishes a multi-arch
(`linux/amd64`, `linux/arm64`) image to `ghcr.io/<repository>` tagged with the
release tag and `latest`.

### Release checklist

1. Update versions if needed and merge to `main`.
2. Create a tag (`git tag v1.2.3 && git push origin v1.2.3`).
3. Publish a GitHub release for the tag.
4. Wait for `Release` to finish; verify the five binaries, checksums and the
   GHCR image.
5. Optional: `docker run --rm ghcr.io/<repository>:v1.2.3 --version`.

## Troubleshooting

- **`--version` still shows the old version.** `option_env!` changes are not
  tracked by Cargo. Use `scripts/build.sh`/`make release`, or run
  `cargo clean -p komari-cli` before `cargo build`.
- **`cross` fails to start.** It needs a working Docker daemon. For a local
  Linux arm64 build without Docker, install the musl toolchain and set
  `CC_aarch64_unknown_linux_musl`, or build on an arm64 host.
- **Docker build cannot find `web/dist`.** The placeholder should be committed;
  otherwise pass `--build-arg KOMARI_WEB_DIST=<existing-dir>`.
- **SQLite permission denied with a bind mount.** The image runs as uid 10001;
  `chown -R 10001:10001 ./data` or use the named volume.

# Building Komari (Rust backend)

This document covers the build, test, cross-compilation, container and release
infrastructure for the Rust rewrite. The Go implementation in the sibling
`komari` repository remains the reference for runtime behaviour.

## Prerequisites

| Tool | Version | Notes |
| --- | --- | --- |
| Rust | 1.85+ (edition 2024) | Stable toolchain; CI uses `dtolnay/rust-toolchain@stable` |
| Cargo | bundled with Rust | `Cargo.lock` is committed; builds use `--locked` |
| Node.js | 20+ | Only for building the `komari-web` frontend |
| Docker | 24+ with BuildKit | Optional, for container builds |
| `cross` | latest | Optional, for Linux musl cross-compilation |

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

The default `make release` embeds B14's committed placeholder theme. For a real
UI, build the frontend, normalize the output into a theme directory and pass it:

```sh
make release DEFAULT_THEME_DIR=/path/to/defaultTheme
```

`make help` lists every target and variable.

## Default theme (B14 contract)

`komari-web` embeds the default theme at **compile time** with `rust-embed`.
Its `build.rs` reads `KOMARI_DEFAULT_THEME_DIR`; when the variable is unset it
falls back to `crates/komari-web/defaultTheme` and prints a warning. The
directory must contain:

```text
<theme>/
  komari-theme.json
  preview.png          # or perview.png (historical spelling)
  dist/
    index.html
    assets/...
```

A real frontend build can be produced with:

```sh
git clone https://github.com/Fearless743/komari-web
(cd komari-web && npm install && npm run build)

# The current Vite config writes to public/defaultTheme/dist; collect the
# dist output and the metadata into one directory.
mkdir -p defaultTheme
cp -a public/defaultTheme/dist defaultTheme/
cp komari-web/komari-theme.json komari-web/preview.png defaultTheme/

KOMARI_DEFAULT_THEME_DIR="$PWD/defaultTheme" cargo build --release --workspace
```

If a checkout already has `dist/` at its root (the layout described by the B14
README), it can be passed directly. The CI action handles both layouts.

Through the build script:

```sh
KOMARI_DEFAULT_THEME_DIR=/path/to/defaultTheme ./scripts/build.sh
make release DEFAULT_THEME_DIR=/path/to/defaultTheme
```

Relative paths passed to `scripts/build.sh`/`make` are resolved to absolute
paths because B14 resolves relative paths against `crates/komari-web`, not the
repository root. The embedded theme is fixed for the lifetime of the binary;
changing it requires a rebuild.

The placeholder is **not** a usable UI. Release builds and the CI matrix must
set `KOMARI_DEFAULT_THEME_DIR`; CI clones and builds
`https://github.com/Fearless743/komari-web` in the reusable
`.github/actions/prepare-theme` action, then shares the result as the
`komari-theme` artifact.

`cargo build --workspace` is used deliberately so `komari-web` is compiled and
its theme embedding is validated even while the HTTP server wiring (B09) is
still landing. `cargo test -p komari-web` with the real theme is part of the CI
theme job. The final `komari` binary is produced by `komari-cli`; linking the
embedded assets into the HTTP server is the responsibility of the server
integration task.

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

`--version` prints the injected version:

```sh
KOMARI_VERSION=9.9.9 KOMARI_VERSION_HASH=abc ./scripts/build.sh
./dist/komari --version
# komari 9.9.9
```

`KOMARI_VERSION_HASH=unknown` marks a development build and selects **debug**
logging by default; when a hash is present (release builds) the default log
level is **info**, matching the Go `main.go` behaviour. An explicit
`KOMARI_LOG_LEVEL` still wins.

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
| `KOMARI_DEFAULT_THEME_DIR` | unset (B14 placeholder) | Theme directory embedded at compile time |
| `PACKAGE` | unset (`--workspace`) | Optional package to build instead of the workspace |
| `PROFILE` | `release` | Cargo profile (`release` or `dev`) |
| `TARGET` | host | Rust target triple |
| `CROSS` | `cross` if on `PATH` | Cross runner used when `TARGET` differs from the host |
| `OUT_DIR` | `dist` | Output directory |
| `CARGO` | `cargo` | Cargo executable |

Release builds set `CARGO_PROFILE_RELEASE_STRIP=symbols`, equivalent to the Go
release workflow's `-ldflags "-s -w"`. The build script, Dockerfile and CI also
run `strip` explicitly on non-Windows binaries so `cross`/musl and custom
linkers are covered. Set `CARGO_PROFILE_RELEASE_STRIP=none` to keep symbols
(the explicit `strip` step still applies).

When `TARGET` is not the host triple, the script requires `cross` (or a custom
`CROSS` executable) and fails with installation instructions otherwise. Native
runners are recommended for macOS and Windows targets.

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
TARGET=aarch64-unknown-linux-musl ./scripts/build.sh
# or:
cross build --locked --release --workspace --target aarch64-unknown-linux-musl
```

macOS builds run on an Apple Silicon runner; the Xcode SDK contains both slices,
so `x86_64-apple-darwin` is built with `cargo build --target ...` without an
Intel runner.

### Difference from the Go matrix

The Go workflows also shipped `linux/386`, `linux/riscv64`, `windows/386` and
`windows/arm64`. The Rust matrix intentionally omits them for now:

- `riscv64` is not covered by the current `ring`/rustls and bundled-SQLite
  dependency set;
- 32-bit Linux/Windows targets have not been validated with the Rust
  dependencies and are not worth advertising until they are tested;
- macOS `amd64`/`arm64` are added because the Rust workspace supports them.

This deviation is intentional and should be revisited once the dependency
support and test coverage exist.

## Docker

The `Dockerfile` is a multi-stage build:

1. `chef` installs `cargo-chef` (pinned to `0.1.78`).
2. `planner` records the dependency recipe from `Cargo.toml` / `Cargo.lock`.
3. `builder` caches third-party crates, copies the theme source to
   `/src/theme`, compiles the **workspace** in release mode with
   `KOMARI_DEFAULT_THEME_DIR`, and strips symbols.
4. `runtime` is `debian:bookworm-slim` with `ca-certificates`, `tzdata`, a
   non-root user (uid/gid `10001`) and the `/app/data` volume.

```sh
docker build -t komari:local .

docker build \
  --build-arg KOMARI_THEME_SRC=.komari-theme \
  --build-arg KOMARI_DEFAULT_THEME_DIR=/src/theme \
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
| `KOMARI_THEME_SRC` | `crates/komari-web/defaultTheme` | Theme source directory, relative to the build context |
| `KOMARI_DEFAULT_THEME_DIR` | `/src/crates/komari-web/defaultTheme` | Absolute path passed to B14's `build.rs`; use `/src/theme` for a custom source |

`KOMARI_THEME_SRC` is copied to `/src/theme` inside the builder. The default
`KOMARI_DEFAULT_THEME_DIR` points at B14's committed placeholder after
`COPY . .`; when supplying a real frontend through `KOMARI_THEME_SRC`, also set
`KOMARI_DEFAULT_THEME_DIR=/src/theme`.

### Runtime configuration

| Variable | Default | Notes |
| --- | --- | --- |
| `KOMARI_LISTEN` | `0.0.0.0:25774` | Same as Go |
| `KOMARI_DB_TYPE` | `sqlite` | `sqlite` or `mysql` |
| `KOMARI_DB_FILE` | `/app/data/komari.db` | SQLite file; the Go image also used `/app/data` |
| `KOMARI_DB_HOST` / `_PORT` / `_USER` / `_PASS` / `_NAME` | Go defaults | MySQL settings |
| `KOMARI_LOG_LEVEL` | `info` | Release default; `debug` when the version hash is unknown |
| `KOMARI_THEME` | `default` | Installed theme directory name |
| `ADMIN_USERNAME` | `admin` | Initial admin, read by `komari-db` on first database creation |
| `ADMIN_PASSWORD` | random | Initial admin password; random value is printed in the startup logs |

The working directory is `/app`, so downloaded GeoIP data
(`data/GeoLite2-Country.mmdb`) and installed themes live under the `/app/data`
volume, matching the Go backend's relative paths. The default theme is embedded
in the binary; no separate web-assets directory is shipped in the image.

`docker-compose.yml` starts the same image with a named volume, restart policy,
port mapping and environment examples:

```sh
docker compose up -d --build
docker compose logs -f komari
```

To build the image with a real frontend through Compose:

```sh
KOMARI_THEME_SRC=.komari-theme \
KOMARI_DEFAULT_THEME_DIR=/src/theme \
docker compose build
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
- `theme`: clones/builds `Fearless743/komari-web` with
  `.github/actions/prepare-theme`, normalizes the Vite output into
  `.komari-theme`, runs `cargo test -p komari-web --locked` and verifies the
  built asset bytes are present in the `komari-web` rlib, then uploads the
  `komari-theme` artifact.
- `build`: the five-target matrix above, downloads the theme artifact, sets
  `KOMARI_DEFAULT_THEME_DIR`, builds `--workspace`, strips symbols, and
  uploads `komari-<os>-<arch>` plus a `.sha256` file. The `linux-amd64` job
  also checks whether `komari-cli` depends on `komari-web`; once the B09 HTTP
  integration lands, it verifies the final binary contains the theme asset
  marker. Until then it prints a warning and skips that binary-level check
  (the `komari-web` rlib embedding is still verified in the `theme` job).
- `docker`: downloads the theme artifact and builds the image without pushing,
  passing `KOMARI_THEME_SRC=.komari-theme` and
  `KOMARI_DEFAULT_THEME_DIR=/src/theme`.

`.github/workflows/release.yml` runs when a GitHub release is published (or
manually with a tag):

- `version` checks out the tag and records `git rev-parse HEAD`; both the
  binaries and the Docker image use that commit hash, not the branch
  `github.sha`.
- `theme` builds and validates the real frontend once.
- `build` builds the matrix with `KOMARI_VERSION=<tag>` and
  `KOMARI_VERSION_HASH=<checked-out commit>`, then uploads every asset to the
  release with `gh release upload --clobber`.
- `docker` publishes a multi-arch (`linux/amd64`, `linux/arm64`) image to
  `ghcr.io/<repository>` tagged with the release tag and `latest`, using the
  same theme artifact and build arguments.

### Release checklist

1. Update versions if needed and merge to `main`.
2. Create a tag (`git tag v1.2.3 && git push origin v1.2.3`).
3. Publish a GitHub release for the tag.
4. Wait for `Release` to finish; verify the five binaries, checksums and the
   GHCR image.
5. Optional: `docker run --rm ghcr.io/<repository>:v1.2.3 --version`.

## Troubleshooting

- **`komari-web` fails with "cannot embed the default theme".** Set
  `KOMARI_DEFAULT_THEME_DIR` to a directory containing `dist/index.html`,
  `komari-theme.json` and `preview.png`/`perview.png`, or unset it to use the
  committed placeholder.
- **`--version` still shows the old version.** `option_env!` changes are not
  tracked by Cargo. Use `scripts/build.sh`/`make release`, or run
  `cargo clean -p komari-cli` before `cargo build`.
- **`cross` fails to start.** It needs a working Docker daemon. For a local
  Linux arm64 build without Docker, install the musl toolchain or build on an
  arm64 host; macOS/Windows targets should use native runners.
- **Docker build cannot find the theme source.** The default
  `crates/komari-web/defaultTheme` requires B14 to be merged; otherwise pass
  `--build-arg KOMARI_THEME_SRC=<existing-dir>` and
  `--build-arg KOMARI_DEFAULT_THEME_DIR=/src/theme`.
- **SQLite permission denied with a bind mount.** The image runs as uid 10001;
  `chown -R 10001:10001 ./data` or use the named volume.

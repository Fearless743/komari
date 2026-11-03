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
git clone https://github.com/Fearless743/komari-web .komari-web-src
(cd .komari-web-src && npm install && npm run build)

# The current Vite config writes to public/defaultTheme/dist; collect the
# dist output and the metadata into one directory.
mkdir -p .komari-theme
cp -a public/defaultTheme/dist .komari-theme/
cp .komari-web-src/komari-theme.json .komari-web-src/preview.png .komari-theme/

KOMARI_DEFAULT_THEME_DIR="$PWD/.komari-theme" cargo build --release --workspace
```

If a checkout already has `dist/` at its root (the layout described by the B14
README), it can be passed directly. The CI action handles both layouts.

Through the build script:

```sh
KOMARI_DEFAULT_THEME_DIR=/path/to/defaultTheme ./scripts/build.sh
make release DEFAULT_THEME_DIR=/path/to/defaultTheme
```

Relative paths passed to `scripts/build.sh`/`make` are resolved against the
repository root for native builds. For Linux musl cross builds the script
passes a path relative to `crates/komari-web` (for example
`../../.komari-theme`) so the file is visible inside the `cross` container;
an absolute host path is not. The embedded theme is fixed for the lifetime of
the binary; changing it requires a rebuild.

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
release workflow's `-ldflags "-s -w"`. The Dockerfile and native CI builds also
run the host `strip` explicitly. Cross builds do **not** run the host `strip`
(the host binutils cannot strip a foreign-architecture binary) and rely on
`CARGO_PROFILE_RELEASE_STRIP`. Set it to `none` to keep symbols.

Only Linux musl targets (`*-unknown-linux-musl`) use `cross` when `TARGET`
differs from the host. Same-OS cross builds (for example macOS x86_64 on Apple
Silicon) use the native cargo/linker, matching the CI matrix. A `CROSS`
override can force `cargo` or another runner.

## Cross-compilation

Release artifacts use these targets:

| Artifact | Target | Runner |
| --- | --- | --- |
| `komari-linux-amd64` | `x86_64-unknown-linux-musl` | Ubuntu + `cross` |
| `komari-linux-arm64` | `aarch64-unknown-linux-musl` | Ubuntu + `cross` |
| `komari-darwin-amd64` | `x86_64-apple-darwin` | macOS 14 |
| `komari-darwin-arm64` | `aarch64-apple-darwin` | macOS 14 |
| `komari-windows-amd64.exe` | `x86_64-pc-windows-msvc` | Windows Server |

The Rust Linux release artifacts target musl so they are self-contained. This
is not identical to the Go release workflow: Go's `release.yml` built
linux/amd64 with CGO/glibc, other Linux architectures with CGO disabled, and
used Zig/musl only for the Docker image. `cross`
(https://github.com/cross-rs/cross) supplies the musl toolchain and runs the
build in a container:

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
3. `builder` cooks dependencies against the stable committed placeholder, then
   copies the requested theme source to `/src/theme`, compiles the
   **workspace** in release mode with `KOMARI_DEFAULT_THEME_DIR=/src/theme`,
   and strips symbols. Theme changes therefore do not invalidate the dependency
   cook layer.
4. `runtime` is `debian:bookworm-slim` with `ca-certificates`, `tzdata`, a
   non-root user (uid/gid `10001`) and the `/app/data` volume.

```sh
docker build -t komari:local .

docker build \
  --build-arg KOMARI_THEME_SRC=.komari-theme \
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
| `KOMARI_DEFAULT_THEME_DIR` | `/src/theme` | Absolute path passed to B14's `build.rs` |

`KOMARI_THEME_SRC` is copied to `/src/theme` inside the builder and
`KOMARI_DEFAULT_THEME_DIR` defaults to that single copy, so supplying a real
frontend only requires `KOMARI_THEME_SRC`. The dependency cook uses the stable
committed placeholder before the real theme is copied.

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
KOMARI_THEME_SRC=.komari-theme docker compose build
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
- `theme` builds and validates the real frontend once and **fails** if B14's
  `crates/komari-web/build.rs` or the embedded theme bytes are missing.
- `build` builds the matrix with `KOMARI_VERSION=<tag>` and
  `KOMARI_VERSION_HASH=<checked-out commit>`, then uploads every asset to the
  release with `gh release upload --clobber`. The `linux-amd64` job **fails**
  if `komari-cli` does not depend on `komari-web`, because the release would
  otherwise ship without a UI; this is a hard prerequisite on the B09 HTTP
  integration.
- `docker` builds an amd64 image for verification and **fails** if the binary
  inside the image does not contain the theme asset marker, then publishes a
  multi-arch (`linux/amd64`, `linux/arm64`) image to `ghcr.io/<repository>`
  tagged with the release tag and `latest`.

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
  arm64 host; macOS/Windows targets use native cargo on native runners, not
  `cross`.
- **Cross build cannot read the theme.** Pass a theme directory inside the
  repository (for example `.komari-theme`) so the script can hand B14 a
  path relative to `crates/komari-web`; absolute host paths do not exist
  inside the `cross` container.
- **Docker build cannot find the theme source.** The default
  `crates/komari-web/defaultTheme` is committed by B14. To use another theme,
  pass only `--build-arg KOMARI_THEME_SRC=<existing-dir>`;
  `KOMARI_DEFAULT_THEME_DIR` already defaults to the single `/src/theme` copy.
- **SQLite permission denied with a bind mount.** The image runs as uid 10001;
  `chown -R 10001:10001 ./data` or use the named volume.

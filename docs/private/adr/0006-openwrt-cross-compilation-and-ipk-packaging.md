# ADR-0006: OpenWrt Cross-Compilation and IPK Packaging

- **Status**: Proposed
- **Date**: 2026-05-12
- **Deciders**: Project owner, Sisyphus

## Context

tollgate-rs must compile for OpenWrt routers as a `.ipk` package, serving as a drop-in replacement for the Go v1 `tollgate-module-basic-go`. The Go v1 build system (CI, packaging, SDK integration) is mature and we should be inspired by it rather than invent from scratch.

### Current State

- tollgate-rs builds on macOS (aarch64) with Rust 1.85.0 MSRV
- No cross-compilation config exists (no `.cargo/config.toml`, no Makefile, no CI)
- No `packaging/` directory
- All dependencies are pure Rust except potential transitive C deps (OpenSSL, SQLite)

### Go v1 Reference Build System

The Go v1 module uses a two-stage pipeline:

```
Stage 1: Native cross-compile (Ubuntu CI)
  → Go binaries for arm64, armv7, mipsle, mips
  → UPX compression (optional, 4 levels)

Stage 2: Package into .ipk / .apk
  → IPK: ar + tar script (no SDK needed)
  → APK: Docker container with OpenWrt SDK 25.12.0
```

**Go v1 cross-compilation targets:**

| SDK Tag | OpenWrt Arch | Go Config |
|---------|-------------|-----------|
| mediatek-filogic-25.12.0 | aarch64_cortex-a53 | arm64 |
| ath79-generic-25.12.0 | mips_24kc | mips GOMIPS=softfloat |
| ramips-mt7621-25.12.0 | mipsel_24kc | mipsle GOMIPS=softfloat |
| bcm27xx-bcm2711-25.12.0 | aarch64_cortex-a72 | arm64 |
| bcm27xx-bcm2709-25.12.0 | arm_cortex-a7 | arm GOARM=7 |

**Go v1 packaging files:**
- `.github/workflows/build-package.yml` — matrix CI (13 variants)
- `packaging/Makefile` — OpenWrt package recipe (deps: nodogsplash, luci, jq)
- `packaging/build-ipk.sh` — standalone IPK builder (ar + tar, no SDK)
- `scripts/build-sdk-package.sh` — SDK Docker build for APK format
- Procd init script (`/etc/init.d/tollgate-wrt`)
- Firewall rules, hotplug restart, UCI defaults, captive portal HTML

### Key Differences: Go vs Rust Cross-Compilation

| Aspect | Go | Rust |
|--------|-----|------|
| Cross-compilation | Built-in (`GOARCH`/`GOOS`) | Requires target toolchain + linker |
| C dependencies | None (no CGO) | OpenSSL, SQLite (transitive via reqwest, cdk-sqlite) |
| Static linking | Default | Requires musl target + vendored deps |
| Binary size | ~10-15MB | ~5-10MB (stripped, musl) |
| UPX support | Yes (used in CI) | Yes (same approach works) |
| Target triple pattern | `GOARCH` + flags | `arch-vendor-os-abi` (e.g., `mipsel-unknown-linux-musl`) |

## Decision

### Phase 1: IPK-Only (OpenWrt ≤ 24.10.x)

Focus on `.ipk` packaging first. This covers existing hardware and avoids APK SDK complexity. Use the same `build-ipk.sh` approach as Go v1.

### Phase 2: APK Support (OpenWrt ≥ 25.x)

Add APK packaging via OpenWrt SDK Docker container once Phase 1 is proven.

## Rust Cross-Compilation Targets

For musl-based static linking (no runtime C library dependency on the router):

| Target Triple | OpenWrt Arch | Use Case |
|---------------|-------------|----------|
| `aarch64-unknown-linux-musl` | aarch64_cortex-a53/a72 | Modern WiFi routers (Filogic, Raspberry Pi 4) |
| `armv7-unknown-linux-musleabihf` | arm_cortex-a7 | Older routers (BCM2709) |
| `mipsel-unknown-linux-musl` | mipsel_24kc | MT7621 routers (common OpenWrt target) |
| `mips-unknown-linux-musl` | mips_24kc | ATH79 routers |

**Note on MIPS soft-float**: OpenWrt's MIPS targets use soft-float. Rust's `mips*-unknown-linux-musl` targets default to soft-float, which aligns with OpenWrt's expectations. No special configuration needed.

## Dependency Strategy for Cross-Compilation

### Problem Dependencies

1. **reqwest** (HTTP client) — depends on OpenSSL via `native-tls` by default
2. **cdk-sqlite** — depends on `libsqlite3-sys` which needs C compilation

### Solution: Feature Flags and Vendored Dependencies

**reqwest → rustls:**
```toml
reqwest = { version = "0.12", default-features = false, features = [
    "rustls-tls",      # Pure Rust TLS, no OpenSSL
    "json",
    "http1",
] }
```

This eliminates OpenSSL from the dependency tree entirely. `rustls` + `aws-lc-rs` (or `ring`) compile cleanly for all musl targets.

**cdk-sqlite → bundled SQLite:**
```toml
cdk-sqlite = { version = "0.16", features = ["bundled"] }
```

The `bundled` feature compiles SQLite from C source using `cc` crate, which supports cross-compilation via standard `TARGET_CC` environment variables. This avoids needing libsqlite3 installed on the build host or target.

**Alternative for constrained MIPS routers**: Use `cdk-redb` instead of `cdk-sqlite`. Redb is pure Rust with no C dependencies, but we lose SQLite compatibility. Decision: start with bundled SQLite, evaluate redb if binary size is problematic on 16MB flash routers.

### Dependency Audit (Pure Rust, No Concerns)

These compile cleanly for all targets:
- `tokio` — async runtime
- `axum` — HTTP server
- `clap` — CLI parsing
- `tracing` + `tracing-subscriber` — logging
- `minicbor` — CBOR codec
- `nostr` — Nostr event signing
- `thiserror` — error types
- `serde_json` — JSON
- `sha2` — hashing
- `async-trait` — async trait macros

## Build Pipeline Design

### CI Workflow (`.github/workflows/build-package.yml`)

Inspired by Go v1's matrix approach:

```yaml
strategy:
  matrix:
    include:
      - target: aarch64-unknown-linux-musl
        arch: aarch64_cortex-a53
      - target: armv7-unknown-linux-musleabihf
        arch: arm_cortex-a7
      - target: mipsel-unknown-linux-musl
        arch: mipsel_24kc
      - target: mips-unknown-linux-musl
        arch: mips_24kc

steps:
  - uses: dtolnay/rust-toolchain@stable
    with:
      targets: ${{ matrix.target }}
  - uses: rust-cross/cross-action@v0
    with:
      command: build
      target: ${{ matrix.target }}
  - run: ./packaging/build-ipk.sh ${{ matrix.target }} ${{ matrix.arch }}
```

### `cross` vs Manual Toolchain

**Recommendation: Use `cross` (cross-rs) for CI.**

- `cross` provides Docker containers with pre-configured cross-compilation toolchains
- Handles musl linker, C cross-compilers for bundled deps, and QEMU for tests
- Go v1 achieves the same via Go's built-in cross-compilation; `cross` is Rust's equivalent
- Local development can use `rustup target add` + manual linker config for faster iteration

### Local `.cargo/config.toml`

For developers who want to cross-compile without Docker:

```toml
[target.aarch64-unknown-linux-musl]
linker = "aarch64-linux-musl-gcc"

[target.armv7-unknown-linux-musleabihf]
linker = "arm-linux-musleabihf-gcc"

[target.mipsel-unknown-linux-musl]
linker = "mipsel-linux-musl-gcc"

[target.mips-unknown-linux-musl]
linker = "mips-linux-musl-gcc"
```

Musl cross-make toolchains: https://musl.cc/

## Package Structure

```
packaging/
├── Makefile                          # OpenWrt package recipe (adapted from Go v1)
├── build-ipk.sh                      # Standalone IPK builder (adapted from Go v1)
├── files/
│   ├── etc/
│   │   ├── init.d/
│   │   │   └── tollgate-wrt          # Procd service init script
│   │   ├── config/
│   │   │   └── firewall-tollgate     # Firewall rules for port 2121
│   │   ├── uci-defaults/
│   │   │   └── 99-tollgate-setup     # First-boot setup
│   │   └── hotplug.d/iface/
│   │       └── 95-tollgate-restart   # Restart on WAN up
│   └── lib/upgrade/keep.d/
│       └── tollgate                  # Preserve config across sysupgrade
└── preinst                           # Pre-install checks
```

### IPK Control File

```
Package: tollgate-wrt
Version: <from git tag or branch.height.sha>
Depends: libc, libpthread
Provides: tollgate-wrt
Section: net
Category: Network
Title: TollGate v2 — Rust payment router
Maintainer: Amperstrand
Source: https://github.com/Amperstrand/tollgate-rs-ai-research-and-experiments
Architecture: <arch-specific>
Description: Autonomous device-to-device payment for metered network access.
 Built on Cashu ecash and Spilman payment channels. Drop-in replacement for
 tollgate-module-basic-go (Go v1).
Installed-Size: <auto>
```

**Note**: Unlike Go v1, we don't depend on `nodogsplash` or `luci`. Our captive portal is built into the binary (or will be). This simplifies the dependency chain.

### Procd Init Script

```sh
#!/bin/sh /etc/rc.common
USE_PROCD=1
START=95
STOP=10

PROG=/usr/bin/tollgate-wrt
CONF=/etc/config/tollgate-wrt

start_service() {
    procd_open_instance
    procd_set_param command "$PROG" server \
        --port "$(uci_get tollgate_wrt @server[0] port 2121)" \
        --metric "$(uci_get tollgate_wrt @server[0] metric bytes)" \
        --step-size "$(uci_get tollgate_wrt @server[0] step_size 5000)"
    procd_set_param respawn
    procd_set_param stdout 1
    procd_set_param stderr 1
    procd_close_instance
}
```

## Versioning Strategy

Adopt Go v1's approach:
- **Release tags** (`vX.Y.Z`): exact version
- **Branch builds** (`<branch>.<height>.<sha>`): development builds
- **APK version normalization**: handled by `normalize-apk-version.sh` (Phase 2)

## Testing Strategy

1. **CI cross-compilation test**: `cross build --target <musl-target>` for all 4 targets
2. **CI IPK generation**: Build `.ipk` artifacts, verify with `opkg info`
3. **Hardware testing**: Deploy to physical OpenWrt routers via `physical-router-test-automation`
4. **Go v1 interop**: tollgate-rs server must respond identically to Go v1 server for all HTTP endpoints

## Open Questions

1. **Binary size on MIPS**: musl static binaries for MIPS can be 5-15MB. Routers with 16MB flash may need UPX compression. Measure first, compress if needed.
2. **SQLite vs redb**: Bundled SQLite adds ~2MB to binary. If flash is tight, consider `cdk-redb` (pure Rust). Measure and decide.
3. **cdk-sqlite bundled on MIPS**: The `cc` crate cross-compiles C code. Verify that SQLite's C source compiles cleanly for MIPS soft-float with `cross`.
4. **nodogsplash dependency**: Go v1 depends on nodogsplash for captive portal. We may need to provide our own captive portal HTML or integrate with nodogsplash. Decision deferred to M4.

## Consequences

- **Simpler than Go v1 for IPK**: Rust musl static linking means one binary, no runtime dependencies. Go v1 also does this but Rust's approach is more predictable.
- **CI complexity**: `cross` adds Docker-in-CI overhead. Acceptable for the build reliability it provides.
- **MIPS support is critical**: MT7621 (mipsel) and ATH79 (mips) are the most common OpenWrt routers. These MUST work.
- **Feature flag discipline**: Dependencies that pull in C libraries must be behind feature flags with vendored alternatives. This is an ongoing maintenance concern.
- **Phase 1 scope**: IPK only, 4 architectures, no APK, no UPX, no captive portal integration. Ship the basics, iterate.

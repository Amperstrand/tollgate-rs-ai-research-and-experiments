# TollGate OpenWrt Package

This directory builds and installs the Rust **TollGate** node on OpenWrt
routers.

**Primary target: OpenWrt 25.x (latest stable).** 25.12+ dropped opkg
entirely — packages are `apk`-format (`build-apk.sh`). The `.ipk` path is
**LEGACY, for already-deployed 24.10 devices only**: it works, but it is
no longer the focus of testing and will be removed when 24.10 leaves the
bench.

| Path | Target | When to use | Tool |
| --- | --- | --- | --- |
| [`build-apk.sh`](build-apk.sh) | **25.12+ (primary)** | Standalone, no SDK | `cargo-zigbuild` cross-compile + `apk mkpkg` (apk-tools 3) |
| [`build-ipk.sh`](build-ipk.sh) | 24.10 (legacy) | Updating existing deployments | `cargo-zigbuild` cross-compile + manual `.ipk` assembly |
| [`Makefile`](Makefile) | Inside the OpenWrt SDK | Standard `make package/tollgate/compile` | |

## Package contents

| Installed path | Purpose |
| --- | --- |
| `/usr/bin/tollgate` | TollGate node daemon (HTTP/WS transport, port 4747) |
| `/usr/bin/tolltop` | Live TUI dashboard |
| `/etc/init.d/tollgate` | procd service (auto-start, crash respawn) |
| `/etc/tollgate/tollgate.yaml` | Node configuration (conffile — preserved on upgrade) |
| `/etc/tollgate/tollgate.yaml.example` | Reference configuration with all options documented |
| `/etc/config/tollgate` | Optional UCI runtime knobs (overrides for listen/port/metric) |
| `/etc/config/firewall-tollgate` | UCI firewall rules (lan-zone accept on port 4747) |
| `/etc/hotplug.d/iface/95-tollgate-restart` | Restart TollGate when WAN gains connectivity |
| `/etc/uci-defaults/90-tollgate-firewall` | First-boot firewall + kernel-module setup |
| `/etc/uci-defaults/99-tollgate-setup` | First-boot config bootstrap |
| `/lib/upgrade/keep.d/tollgate` | Preserves `/etc/tollgate/` across `sysupgrade` |

## Requirements

### Build host

| Requirement | Notes |
| --- | --- |
| `cargo-zigbuild` | For standalone builds: `cargo install cargo-zigbuild` |
| Rust target for your router | Added automatically by `build-ipk.sh` |
| OpenWrt SDK 22.03+ (SDK path only) | Older versions lack fw4 / nftables support |

### Router

| Requirement | Notes |
| --- | --- |
| `nftables` | TollGate's enforcing firewall installs nft forward chains + paid_peers sets |
| `kmod-nft-tproxy` | Transparent proxy support |
| `ip-full` | Full `ip` tooling |

All three are listed as package dependencies (`DEPENDS`) and installed
automatically by `opkg`.

## Target architectures

`build-ipk.sh` and the `Makefile` map the architecture to a Rust musl target:

| `--arch` | OpenWrt arch | Rust target | Standalone | SDK |
| --- | --- | --- | --- | --- |
| `aarch64` *(default)* | `aarch64_cortex-a53` | `aarch64-unknown-linux-musl` | ✅ | ✅ |
| `x86_64` | `x86_64` | `x86_64-unknown-linux-musl` | ✅ | ✅ |
| `arm` | `arm_cortex-a7` | `arm-unknown-linux-musleabihf` | ✅ | ✅ |
| `mipsel` | `mipsel_24kc` | `mipsel-unknown-linux-musl` | ✅ [`build-apk-mipsel.sh`](build-apk-mipsel.sh) | ✅ |
| `mips` | `mips_24kc` | `mips-unknown-linux-musl` | ❌ Tier 3 | ✅ |
| `arm_cortex-a7_neon-vfpv4` (ipq40xx) | `arm_cortex-a7_neon-vfpv4` | `armv7-unknown-linux-musleabihf` | ✅ [`build-apk-armv7.sh`](build-apk-armv7.sh) | ✅ |

### armv7 (ipq40xx): plain cargo-zigbuild

`armv7-unknown-linux-musleabihf` is a Rust **tier-2** target (prebuilt
std via rustup), so — unlike MIPS — no SDK container or `-Z build-std`
is needed: [`build-apk-armv7.sh`](build-apk-armv7.sh) runs
`cargo zigbuild` on the host (zig cc defines `__ARM_ARCH`, which ring
0.17.14's C/asm sources require on ARM32), then packages via
`build-apk.sh --arch arm_cortex-a7_neon-vfpv4` (passed through verbatim —
ipq40xx's packages arch; the plain `arm` alias maps to `arm_cortex-a7`,
which apk rejects on ipq40xx devices). Verified end-to-end 2026-11-18:
build in ~2.5 min with no flag iteration; output is a fully static
32-bit ARM EABI5 hard-float ELF (`Tag_CPU_arch: v7`, no PT_INTERP, no
NEEDED); boots under `qemu-arm-static` — serve on 127.0.0.1:4814,
`GET /admin/api/status` → 200, `GET /v1/sessions/unknown` → 404; the
`.apk` payload hashes equal the smoke-tested binaries (apk-tools 3
adbdump).

### MIPS: the SDK-container + build-std path

Rust classifies `mips-unknown-linux-musl` and `mipsel-unknown-linux-musl` as
**Tier 3** targets: no pre-compiled `rust-std` is available via `rustup`, so
`cargo-zigbuild` cannot produce a standalone binary for MIPS (zig also fails
on both endians — compiler_rt intrinsics on big-endian, std heap-allocator
symbols on little-endian; see issue #94).

For **mipsel** (ramips/mt7621), use [`build-apk-mipsel.sh`](build-apk-mipsel.sh):
a docker wrapper around a digest-pinned `openwrt/sdk:ramips-mt7621-25.12.0`
container that cross-compiles with the SDK's musl gcc and nightly
`-Z build-std`, then packages the `.apk` via `build-apk.sh --arch mipsel`.
Verified end-to-end 2026-11-18 (issue #94): build in ~3 min with no flag
iteration (ring 0.17.14 compiles via its portable-C source set; sqlite is
bundled via cc; secp256k1 via cc), and the binary boots under
`qemu-mipsel-static` — serve on 127.0.0.1:4813, `GET /admin/api/status` →
200, `GET /v1/sessions/unknown` → 404, correct musl interpreter
(`/lib/ld-musl-mipsel-sf.so.1`, NEEDED `libc.so` + `libgcc_s.so.1` — both
stock on OpenWrt). Requires: docker, apk-tools 3 + fakeroot (as for
`build-apk.sh`); a host `qemu-mipsel-static` enables the emulated
`--version` smoke.

| Approach | MIPS support | Build time | Toolchain |
| --- | --- | --- | --- |
| `build-ipk.sh` (standalone) | ❌ No Tier 3 std | Minutes | cargo-zigbuild |
| `build-apk-mipsel.sh` (SDK container) | ✅ mipsel, verified | Minutes (first run downloads nightly) | docker + OpenWrt SDK gcc + nightly build-std |
| `Makefile` (OpenWrt SDK) | ✅ Full (both endians) | Hours (rust/host compiles from source) | OpenWrt toolchain |

Big-endian `mips` (ath79) is not covered by the container script yet — the
same recipe applies with an `ath79/generic` SDK image (digest-pinned the
same way); the `Makefile` path works today at the cost of a from-source
rust/host build.

**Future improvement**: When Rust stabilizes `-Z build-std` or promotes MIPS
musl to Tier 2, the standalone path will work for MIPS without changes. The
OpenWrt TIER-1 Rust PR ([openwrt/openwrt#22748](https://github.com/openwrt/openwrt/pull/22748))
may also add pre-built host compilers that include MIPS std.

To add a missing architecture, add a mapping in both `Makefile` (the `ifeq`
block) and `build-ipk.sh` (the `case` block).

## Building standalone (no SDK)

```bash
# From the repo root — default arch is aarch64:
./packaging/openwrt-ipk/build-ipk.sh

# Pick an architecture:
./packaging/openwrt-ipk/build-ipk.sh --arch x86_64

# Override cargo features (default: v1-compat):
./packaging/openwrt-ipk/build-ipk.sh --features v1-compat
TOLLGATE_FEATURES="v1-compat,spilman" ./packaging/openwrt-ipk/build-ipk.sh

# Use prebuilt binaries (skip compilation, e.g. from CI):
./packaging/openwrt-ipk/build-ipk.sh --bin-dir target/aarch64-unknown-linux-musl/release
```

Output: `dist/tollgate_<version>_<arch>.ipk`.

## Building with the OpenWrt SDK

1. Download the SDK for your target from
   [downloads.openwrt.org](https://downloads.openwrt.org) and extract it.
2. Symlink this directory into the SDK `package/` tree:

   ```bash
   ln -s /path/to/tollgate-rs/packaging/openwrt-ipk package/tollgate
   ```

   Or add the repo as a feed in `feeds.conf`:

   ```
   src-git-full tollgate https://github.com/OpenTollGate/tollgate-rs.git
   ```

   ```bash
   ./scripts/feeds update tollgate
   ./scripts/feeds install -a -p tollgate
   ```

3. Build:

   ```bash
   make package/tollgate/compile V=s
   ```

   The resulting `.ipk` is in `bin/packages/<arch>/`.

4. To change the cargo features built by the SDK, override
   `TOLLGATE_FEATURES`:

   ```bash
   make package/tollgate/compile V=s TOLLGATE_FEATURES="v1-compat"
   ```

## Installing on the router

```bash
scp -O dist/tollgate_0.1.0_aarch64_cortex-a53.ipk root@192.168.1.1:/tmp/
ssh root@192.168.1.1 opkg install /tmp/tollgate_0.1.0_aarch64_cortex-a53.ipk
```

`postinst` runs the uci-defaults scripts and enables + starts the service.

## First-time configuration

Edit `/etc/tollgate/tollgate.yaml` before (or after) first start:

```bash
ssh root@192.168.1.1
vi /etc/tollgate/tollgate.yaml
```

The full option reference is in the shipped `tollgate.yaml.example` and in
`docs/design/core/tollgate-configuration.md`. Key fields:

| Field | Default | Purpose |
| --- | --- | --- |
| `listen` | `0.0.0.0:4747` | HTTP/WS transport bind address |
| `secret_key_file` | `/etc/tollgate/identity.hex` | Node signing key (auto-generated) |
| `firewall` | `enforcing` | nft forward-chain enforcement |
| `mints` | `[]` | Accepted Cashu mints |
| `products` | `[]` | Static product offers |
| `v1_compat` | — | Go v1 router interop (with `v1-compat` feature) |

## Service management

```bash
/etc/init.d/tollgate start
/etc/init.d/tollgate stop
/etc/init.d/tollgate restart
/etc/init.d/tollgate enable    # start at boot (already enabled by postinst)
/etc/init.d/tollgate disable
```

## Inspection and logs

```bash
# Live TUI dashboard
tolltop

# Daemon logs (OpenWrt syslog)
logread | grep tollgate
```

## Upgrading

Install the new `.ipk` over the existing one:

```bash
opkg install --force-reinstall /tmp/tollgate_<new-version>_<arch>.ipk
```

The config `/etc/tollgate/tollgate.yaml` and the identity key
`/etc/tollgate/identity.hex` are preserved by `opkg` (the yaml is a conffile;
the key is not a package file). Both survive `sysupgrade` via
`/lib/upgrade/keep.d/tollgate`.

# V1 Parity Porting Plan

Port features from `tollgate-module-basic-go` (local: `/Users/macbook/src/tollgate-module-basic-go/src/`) to `tollgate-rs` to reach feature parity with the Go v1 production implementation.

## Current State Summary

**Already at parity** (verified by reading both codebases):
- All 7 HTTP endpoints (GET /, POST /, GET /usage, GET /balance, GET /whoami, POST /ln-invoice, GET /ln-invoice)
- CORS (private-origin-only)
- Session store (in-memory + SQLite, Go only has in-memory)
- Janitor cleanup
- Payout task (profit sharing)
- Client: ad fetch, pricing, budget validation, payment, usage polling, renewal, recovery, multi-gateway manager, throttling
- Nostr event formatting (kind 10021/1022/21023)
- MAC resolution (dhcp.leases → /proc/net/arp)
- NDS valve + customer data tracking (behind `nds` feature flag — `NdsValve` in `valve.rs` is a complete port of Go's `valve.go` + `customer_data_tracker.go`)
- Config (JSON load/save/validate/migrate, identities)
- CLI socket (wallet balance/info/fund/drain, status, version)

## Porting Work Items

Ordered by dependency and priority. Each item is independently testable.

---

### PORT-1: Mint Health Tracker + Degraded Mode

**Go source**: `merchant/mint_health_tracker.go` (234 lines), `merchant/merchant_degraded.go` (98 lines)
**Rust target**: `crates/tollgate-net/src/v1/server/mint_health_tracker.rs` (new), `crates/tollgate-net/src/v1/server/merchant_degraded.rs` (new)

**What to port**:
- `MintHealthTracker` struct: probes each configured mint's `/v1/info` endpoint
  - Initial synchronous probe on startup
  - Background periodic probe every 5 minutes
  - Hysteresis: 3 consecutive successes to mark as reachable, single failure resets counter
  - Callbacks: `on_first_reachable` (fires once when first mint recovers from total outage), `on_reachable_set_changed`
  - `GetReachableMintConfigs()` returns only reachable mints
- `MerchantDegraded` null-object: implements the same merchant interface but returns `errDegraded` for all payment/session operations. Only `GetAdvertisement()` and `CreateNoticeEvent()` work (they don't need mints)
- Startup flow in `main.rs`: attempt normal merchant creation → if mints unreachable, create degraded merchant → start health tracker → on recovery, swap to real merchant via provider

**Go patterns to translate**:
- `sync.RWMutex` → `tokio::sync::RwLock`
- `time.Ticker` + goroutine → `tokio::time::interval` + `tokio::spawn`
- `sync.Once` for stop → `CancellationToken`
- `net/http.Client{Timeout}` → `reqwest` with timeout (already a dependency)
- `MerchantInterface` (Go interface) → existing merchant trait or new `MerchantProvider` wrapper that holds `Arc<RwLock<dyn Merchant>>`

**Testing**: Unit test with mock HTTP server returning 200/500 for different mints. Test hysteresis (3 successes needed). Test degraded mode returns errors. Test recovery swaps merchant.

**Effort**: Medium (~400 lines Rust)

---

### ~~PORT-2: Nostr Payment Event on POST /~~ — ALREADY DONE

Already implemented in `handlers.rs:325-339` (`extract_payment_token()`). Parses Nostr kind 21000 events and extracts `["payment", "<token>"]` tags, falling back to raw body. Matches Go v1 exactly.

**One minor gap**: Go caps POST body at 1MB (`http.MaxBytesReader`). Rust has no body size limit — should add `axum::extract::DefaultBodyLimit` or explicit limit.

---

### PORT-3: LN Client Payment Path

**Go source**: `upstream_session_manager/session.go`, `lightning/lightning.go`
**Rust target**: `crates/tollgate-net/src/v1/http.rs` (add client methods), `crates/tollgate-net/src/v1/mod.rs` (add LN payment branch)

**What to port**:
- `V1HttpClient` methods:
  - `POST /ln-invoice` — create a Lightning invoice quote on upstream server (sends `{amount, mint_url}`)
  - `GET /ln-invoice?quote=<id>` — poll quote status (UNPAID → PAID → ISSUED)
- `V1Client` connect/renew: add LN payment branch alongside existing Cashu token payment
  - When upstream only supports LN (no matching mint for Cashu), or when explicitly chosen
  - Create quote → get invoice → user pays invoice (or auto-pay if wallet supports LN) → poll until ISSUED → session starts
- Lightning Address resolution for payouts (Go: `lightning/lightning.go` uses LNURL-p)

**Note**: This is a significant feature. The server-side LN is already implemented in Rust. The client just needs HTTP calls to the upstream server's LN endpoints, which are the same as our server's.

**Testing**: Mock HTTP server returning LN quote lifecycle states. Test client correctly polls and establishes session.

**Effort**: Medium (~300 lines Rust)

---

### PORT-4: CLI Socket Completeness

**Go source**: `cli/server.go` (892 lines), `cli/network.go`, `cli/config.go`
**Rust target**: `crates/tollgate-net/src/v1/cli/commands.rs` (modify), `crates/tollgate-net/src/v1/cli/mod.rs` (modify)

**What to port**:
1. **`health` command** — return `{status, version, config_ok, wallet_ok, uptime}`. Simple: Rust CLI already has all pieces, just needs a handler.
2. **`config` command** — `config get <key>`, `config set <key> <value>`, `config save`. Requires config manager access in CLI server.
3. **`upstream scan`** — needs wireless gateway manager (blocked by PORT-6, but can stub)
4. **`upstream connect <ssid> [passphrase]`** — needs wireless gateway manager (blocked by PORT-6)
5. **`upstream list-upstream`** — needs connector (blocked by PORT-6)
6. **`upstream remove-upstream <ssid>`** — needs connector (blocked by PORT-6)
7. **`network` command** — private SSID management (blocked by PORT-6)

**Can do now (no blockers)**:
- `health` command — trivial addition
- `config` command — needs config manager passed to CLI server constructor

**Deferred to PORT-6**:
- `upstream scan/connect/list/remove`
- `network` commands

**Testing**: Unit tests for health/config commands.

**Effort**: Low for health+config (~100 lines). Medium+ for upstream/network (requires PORT-6).

---

### PORT-5: Netlink Upstream Network Monitor

**Go source**: `upstream_detector/main.go` (359 lines), `upstream_detector/network_monitor.go` (505 lines), Linux-only (`//go:build linux`)
**Rust target**: `crates/tollgate-net/src/v1/upstream_detector_netlink.rs` (new, behind `linux` cfg)

**What to port**:
- `NetworkMonitor` using netlink subscriptions:
  - Subscribe to link changes (interface up/down) → `rtnetlink::new_connection()`, listen for `RTM_NEWLINK`/`RTM_DELLINK`
  - Subscribe to address changes (IP added/removed) → listen for `RTM_NEWADDR`/`RTM_DELADDR`
  - Event deduplication: 2-second throttle per (interface, event_type) key
  - Interface filtering: `ignore_interfaces` / `only_interfaces` from config
- `UpstreamDetector` event loop:
  - Processes `InterfaceUp` → infer gateway → report to session manager
  - Processes `InterfaceDown` → notify session manager of disconnect
  - Processes `AddressAdded/Deleted` → report gateway or disconnect
  - Periodic gateway check every 30 seconds
  - Initial interface scan after 2-second delay
- Gateway inference (3 methods):
  1. Check interface-specific default route
  2. Check global routing table for routes using this interface
  3. Infer from IP address (network.1 or network.254)

**Rust crates**: `rtnetlink` (async netlink), `netlink-packet-route` (route parsing). Both are mature. Alternatively: `neli` crate.

**Architecture**: Replace `crowsnest.rs` (polling-based) with netlink event-driven detection. Keep `crowsnest.rs` as fallback for non-Linux platforms.

**Testing**: Integration test using network namespaces (`ip netns`) to simulate interface up/down. Unit test for gateway inference logic.

**Effort**: High (~600 lines Rust)

---

### PORT-6: Wireless Gateway Manager

**Go source**: `wireless_gateway_manager/` (7 files: `connector.go`, `scanner.go`, `upstream_manager.go`, `types.go`, `interfaces.go`, `logger.go`, `vendor_element_manager.go`)
**Rust target**: `crates/tollgate-net/src/v1/wireless_gateway_manager/` (new module, behind `linux` cfg)

**What to port**:
- `Connector` — OpenWrt UCI command wrapper:
  - `EnsureRadiosEnabled()` — check/enable Wi-Fi radios
  - `EnsureWWANSetup()` — create wwan interface if missing
  - `FindOrCreateSTAForSSID(ssid, passphrase, encryption, radio)` — UCI config for STA mode
  - `SwitchUpstream(current_iface, new_iface, ssid)` — switch default route
  - `GetActiveSTA()` — find current upstream STA
  - `GetSTASections()` — list configured STA sections
  - `RemoveDisabledSTA(ssid)` — remove stale STA config
- `Scanner` — Wi-Fi scan wrapper:
  - `ScanAllRadios()` — run `iwinfo <radio> scan` on all radios, parse output
  - `FindBestRadioForSSID(ssid, networks)` — pick radio with strongest signal
  - `DetectEncryption(encryption_str)` — map iwinfo encryption to UCI encryption type
- `UpstreamManager` — automatic upstream selection with:
  - Periodic signal quality monitoring
  - Hysteresis: only switch if new network is `HysteresisDB` stronger
  - Blacklist with TTL for failed networks
  - Circuit breaker: `MaxConsecutiveFailures` before emergency blacklist
  - Switch cooldown to prevent flapping
  - Reseller mode: auto-connect to SSIDs matching reseller config
  - Post-switch verification: DHCP lease acquisition + internet connectivity check

**Go patterns to translate**:
- UCI commands via `exec.Command("uci", ...)` → `tokio::process::Command::new("uci")`
- `iwinfo` scan parsing → regex/line parsing of `iwinfo` output
- `context.Context` for cancellation → `CancellationToken`
- Timers and intervals → `tokio::time::interval`

**Testing**: Integration test using mock UCI/iwinfo shell scripts (same pattern as `NdsValve` mock tests). Cannot unit test real hardware.

**Effort**: High (~800 lines Rust)

---

### PORT-7: MerchantProvider (Atomic Merchant Swap)

**Go source**: `merchant/merchant.go` (`MerchantProvider` type), `main.go` lines 140-177
**Rust target**: `crates/tollgate-net/src/v1/server/merchant_provider.rs` (new)

**What to port**:
- `MerchantProvider` — thread-safe wrapper that allows atomic merchant swap:
  - `GetMerchant()` → returns current merchant (Arc<dyn Merchant>)
  - `SetMerchant(new)` → atomically swaps (used by health tracker recovery)
- This is needed for PORT-1 (degraded mode recovery) — when mints come back online, the health tracker callback swaps the degraded merchant for a real one without restarting the server

**Go pattern**:
```go
type MerchantProvider struct {
    mu       sync.RWMutex
    merchant MerchantInterface
}
func (p *MerchantProvider) GetMerchant() MerchantInterface {
    p.mu.RLock()
    defer p.mu.RUnlock()
    return p.merchant
}
func (p *MerchantProvider) SetMerchant(m MerchantInterface) {
    p.mu.Lock()
    defer p.mu.Unlock()
    p.merchant = m
}
```

**Rust**: `Arc<RwLock<Arc<dyn MerchantTrait>>>` or `arc_swap::ArcSwap` for lock-free reads.

**Effort**: Low (~50 lines Rust). Should be done before PORT-1.

---

### PORT-8: OpenWrt Packaging

**Go source**: `packaging/` directory (Makefile, preinst, postinst, init.d, uci-defaults, firewall config), `scripts/build-sdk-package.sh`
**Rust target**: New `packaging/` directory at repo root

**What to port**:
- `packaging/files/etc/config/firewall-tollgate` — firewall allow rule for TCP port 2121
- `packaging/files/etc/uci-defaults/99-tollgate-setup` — first-boot setup (NoDogSplash config, Wi-Fi AP, uhttpd, DNS, hostname)
- `packaging/files/etc/uci-defaults/90-tollgate-captive-portal-symlink` — symlink captive portal into NDS servedir
- `packaging/files/etc/init.d/tollgate-wrt` — OpenWrt procd service script (depends on nodogsplash, logs to `/tmp/tollgate-debug.log`)
- `packaging/preinst` — timestamp `install.json`
- `packaging/postinst` — run uci-defaults, reload services
- `packaging/Makefile` — package staging/install recipe (adapt for Rust binary)
- `scripts/build-sdk-package.sh` — cross-compile for target arch, stage into OpenWrt SDK, produce `.apk` (25.x) and `.ipk` (≤24.10)

**Key differences from Go**:
- Go cross-compiles with `GOARCH`. Rust uses `--target <triple>` (e.g., `aarch64-unknown-linux-musl` for OpenWrt ARM)
- Binary name changes: `tollgate-wrt` instead of Go binary
- May need `musl` static linking for OpenWrt compatibility

**Testing**: Build package, install on test OpenWrt router (manual).

**Effort**: Medium (~300 lines shell/Makefile)

---

### PORT-9: Config: install.json + Backup-on-Drift

**Go source**: `config_manager/config_manager_install.go`, `config_manager/config_manager.go` (backup logic)
**Rust target**: `crates/tollgate-net/src/v1/server/config.rs` (modify)

**What to port**:
- `install.json` — separate config file tracking:
  - `install_timestamp` — when the package was first installed
  - `ip_address_randomized` — whether IP randomization has been done
  - Schema version
- Config backup on version drift: when loading a config with an older schema version, back up the existing file to `/etc/tollgate/config_backups/config_<timestamp>.json` before migrating

**Effort**: Low (~100 lines Rust)

---

## Dependency Graph

```
PORT-7 (MerchantProvider) ──── prerequisite for ──── PORT-1 (Mint Health + Degraded)
PORT-2 (Nostr Payment Event) ──── ✅ ALREADY DONE (handlers.rs:325-339)
PORT-3 (LN Client Payment) ──── independent
PORT-4 (CLI: health + config) ──── independent (partial)
PORT-5 (Netlink Monitor) ──── independent (Linux-only)
PORT-6 (Wireless Gateway) ──── enables PORT-4 CLI upstream/network commands
PORT-8 (OpenWrt Packaging) ──── independent, but needs PORT-1 and real valve for production
PORT-9 (install.json + backup) ──── mostly done (config.rs already has backup+migrate), only install.json lifecycle is new
```

## Execution Order

**Phase 1 — Independent, high-value, no platform dependencies** (can test on macOS):
1. PORT-7: MerchantProvider (~50 lines) — 30 min
2. ~~PORT-2: Nostr payment event on POST /~~ — ✅ ALREADY DONE
3. PORT-1: Mint health tracker + degraded mode (~400 lines) — 1-2 days
4. PORT-4 (partial): CLI health + config commands (~100 lines) — 2 hours
5. PORT-3: LN client payment path (~300 lines) — 1-2 days
6. PORT-9: install.json lifecycle only (~60 lines, backup+migrate already exist) — 1 hour

**Phase 2 — Linux/OpenWrt-specific** (need Linux environment to test):
7. PORT-5: Netlink upstream monitor (~600 lines) — 2-3 days
8. PORT-6: Wireless gateway manager (~800 lines) — 3-5 days
9. PORT-4 (complete): Wire CLI upstream/network commands to PORT-6 — 1 day
10. PORT-8: OpenWrt packaging (~300 lines shell) — 1-2 days

**Total estimated effort**: ~2,950 lines Rust + ~300 lines shell (reduced from original by removing already-done PORT-2 and nearly-done PORT-9)

## What We Get at Each Phase

**After Phase 1**: Full v1 protocol parity. Server handles all payment types (Cashu tokens, Nostr events, Lightning invoices). Graceful degradation when mints are down. Client can pay via LN. Ready for testing against Go v1 routers.

**After Phase 2**: Full v1 deployment parity. Can run on OpenWrt hardware. Auto-discovers upstream TollGates. Auto-switches Wi-Fi upstreams. Installable package.

# V1 Go→Rust Parity Roadmap

Living document tracking the gap between `tollgate-module-basic-go` (Go v1) and `tollgate-rs` (Rust).
Updated as gaps are closed. When all items are ✅, the Rust binary is a viable drop-in replacement.

**Go source**: `tollgate-module-basic-go/src/`
**Rust source**: `tollgate-rs/crates/tollgate-net/src/`
**Last audit**: 2025-06-08

---

## Status Summary

| Milestone | Severity | Status | Description |
|-----------|----------|--------|-------------|
| [R1](#r1-data-usage-monitoring) | CRITICAL | ✅ Done | Byte-metric session auto-close |
| [R2](#r2-degraded-mode--mint-health-tracker) | CRITICAL | ✅ Done | Degraded boot + auto-recovery |
| [R3](#r3-config-management-parity) | HIGH | 🔲 Open | Missing dot-path get/set, save-identities, hot reload |
| [R4](#r4-session-atomicity--extensions) | HIGH | ✅ Done | Snapshot/restore on valve failure, session extension verified |
| [R5](#r5-wallet-parity) | HIGH | ✅ Done | Overpayment via OnlineTolerance, signal handling, multi-mint init |
| [R6](#r6-physical-router-testing) | CRITICAL | 🔲 Open | Zero real-hardware validation |
| [R7](#r7-packaging-polish) | MEDIUM | 🔲 Open | Dependency declarations, vendor IE, install state |
| [R8](#r8-integration-test-parity) | MEDIUM | 🔲 Open | Run Go's pytest suite against Rust binary |

---

## R1: Data Usage Monitoring

**Severity**: CRITICAL
**Status**: ✅ Done

### Implementation

- **File**: `v1/server/data_monitor.rs` (209 lines)
- **Function**: `spawn_data_monitor(sessions, valve, interval)` — spawns a tokio task
- **Wired in**: `v1/server/mod.rs:199-203` — `V1Server::run()` spawns it with 2s interval
- **Tests**: 3 tests — closes on exceed, skips milliseconds, keeps under-quota sessions

### What it does

Every 2 seconds:
1. `sessions.list_all()` — get all active sessions
2. Filter to `metric != "milliseconds"` (byte-based sessions only)
3. For each: `valve.get_client_usage_since_baseline(mac)` — get current byte usage
4. If `usage >= session.allotment`: remove session + close gate + log

### Go Parity

Matches `merchant.go:154-222` (`StartDataUsageMonitoring` + `checkDataUsage`). The only difference is Go logs progress every ~10MB — Rust doesn't have that periodic logging yet (LOW priority).

---

## R2: Degraded Mode & Mint Health Tracker

**Severity**: CRITICAL
**Status**: ✅ Done

### Implementation

**DegradedWallet** — `v1/server/degraded_wallet.rs` (105 lines)
- Implements `Wallet` trait — all payment ops return `WalletError::TokenRejected("service degraded...")`
- `balance()` returns `Ok(Amount(0))`
- `mint_reachable()` returns `Ok(false)`
- 4 tests

**MintHealthTracker** — `v1/server/mint_health_tracker.rs` (665 lines)
- `new(mint_urls)` — creates tracker with defaults (5min interval, 5s timeout, 3 consecutive)
- `run_initial_probe()` / `run_initial_probe_async()` — synchronous/async initial probing
- `start(self: Arc<Self>)` — background probing loop with concurrent mint probes
- `set_on_first_reachable(cb)` — one-shot recovery callback
- `reset_first_reachable()` — retry after failed recovery
- `set_on_reachable_set_changed(cb)` — callback when reachable set changes
- `get_reachable_mint_urls()` / `get_reachable_count()`
- `stop()` — clean shutdown via CancellationToken
- 12 tests: probe, hysteresis, callbacks, recovery swap, stop

**Wiring** — `main.rs:487-521`
- If CdkWallet::new() fails → create DegradedWallet + MerchantProvider
- Create MintHealthTracker with all accepted mint URLs
- `set_on_first_reachable` callback: tries CdkWallet::new(), on success calls `merchant.swap(new_wallet)`, on failure calls `tracker.reset_first_reachable()`
- Server runs with degraded merchant until recovery fires

### Go Parity

Full parity with `merchant_degraded.go` + `mint_health_tracker.go` + `main.go:145-177` recovery flow.

---

## R3: Config Management Parity

**Severity**: HIGH
**Status**: 🔲 Open

### Problem

Go CLI has full runtime config management:
- `config set <dot.path> <value>` — sets nested values like `accepted_mints.0.url`
- `config save-identities <json>` — updates identities.json separately
- `ReloadConfig()` / `ReloadIdentities()` — hot reload without restart
- Config backup on save (`config.json.bak`)
- Config migration (`config_version` stamping)

Rust has `config get`, `config set <key> <value>`, `config schema`, `config save <json>` but:
- `config set` does not support dot-path access to nested fields (no `config_manager/config_dotpath.go` equivalent)
- No `config save-identities` command
- No hot reload — requires restart
- No backup on save
- No config migration

### Go Reference

- **Dot-path system**: `config_manager/config_dotpath.go` — `SetDotPath(cm, key, value)` walks nested JSON using dot-separated path segments. Supports array indexing (`accepted_mints.0.url`), object fields, and type coercion.
- **Save identities**: `cli/config.go:198-237` — `handleIdentitiesSave(jsonStr)` deserializes identities JSON, calls `SaveIdentities()`, then `ReloadIdentities()`.
- **Hot reload**: `config_manager/config_manager.go` — `ReloadConfig()` re-reads from file and replaces in-memory config. `ReloadIdentities()` same for identities.
- **Backup**: `config_manager/config_manager.go` — `SaveConfig()` copies existing file to `.bak` before writing.
- **Schema**: `config_manager/config_schema.go:17-163` — full field schemas with types, defaults, enums, min/max, editable flags.

### Rust Implementation Plan

**Files to modify**:
- MODIFY: `crates/tollgate-net/src/v1/cli/commands.rs` — add `save-identities` subcommand to config handler
- MODIFY: `crates/tollgate-net/src/v1/server/config.rs` — add:
  ```rust
  fn set_dot_path(config: &mut serde_json::Value, path: &str, value: &str) -> Result<()>;
  fn backup_config(path: &str) -> Result<()>;
  fn reload_config(&self) -> Result<()>;  // on CliConfig trait
  fn reload_identities(&self) -> Result<()>;  // on CliConfig trait
  fn migrate_config(config: &mut serde_json::Value) -> bool;
  ```
- MODIFY: `crates/tollgate-net/src/v1/cli/config_schema.rs` — add identities schema to `config schema` response

**Implementation notes**:
- `set_dot_path` needs to handle: array index (`accepted_mints.0`), nested object (`upstream_detector.probe_timeout`), and type coercion (string → u64, string → float, string → bool, string → duration)
- Backup: `std::fs::copy(path, format!("{path}.bak"))` before write
- Hot reload: re-read file, parse, replace in-memory config. CLI server holds `Arc<Mutex<ConfigManager>>` or similar
- Migration: check `config_version` field, apply any schema changes, update version

---

## R4: Session Atomicity & Extensions

**Severity**: HIGH
**Status**: ✅ Done

### Implementation

- **Snapshot/restore** (`handlers.rs`): `open_gate_for_session` returns `Result<(), ValveError>`. New `rollback_session()` helper restores prior session or removes new one on valve failure. Both `handle_post_payment` and `handle_get_ln_invoice` (LN grant path) snapshot before `add_allotment` and rollback on valve failure.
- **Session extension parity**: Rust `add_allotment` already matches Go's `extendSessionEvent` — `existing_allotment + additional_allotment` with `start_time` reset to now (forgives consumed time).
- **Tests**: 2 new integration tests — `v1_server_rollback_session_on_valve_failure` and `v1_server_restores_prior_session_on_valve_failure`.

### Go Parity

Full parity with `merchant.go:855-877` (`snapshotSession`/`restoreSession`) and `merchant/lightning.go:348-362` (`grantSessionAccess`).
- **grantSessionAccess**: `merchant/lightning.go:348-362` — atomic: snapshot → allotment → gate → restore on fail
- **extendSessionEvent**: `merchant.go:562-647` — for time metric, calculates `leftover = existing - elapsed`, then `new_total = existing + additional` (NOT leftover + additional — Go uses existing allotment as the base, which means already-consumed time is "forgiven")

### Rust Files to Audit

- `crates/tollgate-net/src/v1/server/merchant_provider.rs` — `add_allotment()` — check if session modification is rolled back on valve failure
- `crates/tollgate-net/src/v1/server/handlers.rs` — `handle_post_payment()`, `handle_post_ln_invoice()` — check the error handling after allotment is added
- `crates/tollgate-net/src/v1/server/valve.rs` — NdsValve gate operations

### Implementation Plan

1. **Audit**: Read `handlers.rs` payment flow — trace what happens if valve fails after allotment added
2. **Fix**: If needed, add snapshot/restore pattern: save session state before `add_allotment`, restore on valve failure
3. **Verify**: `extendSessionEvent` equivalent — check if session extension handles leftover time correctly
4. **Wire upstream**: Verify `upstream_detector.rs` → session manager → `V1ClientAuto` CLI path is connected end-to-end

---

## R5: Wallet Parity

**Severity**: HIGH
**Status**: ✅ Done

### Implementation

- **Overpayment** (`cdk_wallet.rs`): `create_token_with_overpayment()` added to `Wallet` trait (default delegates to `create_token`). `CdkWallet` overrides with `SendKind::OnlineTolerance(100 sat)` + `include_fee: true`, matching Go's `100 sat` absolute buffer. V1Client `connect()` and `renew()` both use it.
- **Signal handling** (`v1/server/mod.rs`, `v1/mod.rs`): `tokio::signal::ctrl_c()` added to `V1Server::run()` select and `V1Client::run()` loop. CDK 0.16 has no `shutdown()` method (in-memory store, no flush needed).
- **Multi-mint init** (`cdk_wallet.rs`): `CdkWallet::try_mints(&[String], seed)` loops through accepted mints, returns first success, matching Go's `TollWallet.New()`. Wired in `main.rs` V1Server startup and degraded-mode recovery callback.
- **Mint safety guard** (`config.rs`): `ServerConfig::validate()` rejects any mint URL whose hostname doesn't contain "test" (case-insensitive). Prevents accidental use of real Bitcoin-backed mints during development. `main.rs` calls `validate()` before proceeding.
- **Tests**: 523 tests, 0 failures.

### Go Parity

| Go Feature | Rust Status |
|------------|-------------|
| `SendWithOverpayment` (10000%/100 sat) | ✅ `SendKind::OnlineTolerance(100 sat)` + `include_fee` |
| `wallet.Shutdown()` | ⚠️ CDK 0.16 has no shutdown API; in-memory store needs no flush |
| Multi-mint init loop | ✅ `CdkWallet::try_mints()` |

---

## R6: Physical Router Testing

**Severity**: CRITICAL
**Status**: 🔲 Open

### Problem

424+ tests, all against mock servers. Zero real-hardware validation. The Rust binary has never run on a real OpenWrt router with real NDS, real Cashu mint, or real Lightning.

### Test Plan

1. Deploy Rust .ipk to x86_64 test router via cloud lab (`physical-router-test-automation/scripts/deploy-rust-ci.sh`)
2. Smoke test all 7 API endpoints with real ndsctl
3. Byte-metric session: auth → data flows → auto-close on allotment
4. Time-metric session: timed gate open → auto-close on expiry
5. Lightning invoice: POST → BOLT11 → pay externally → GET → access granted
6. Payout: profit share to real Lightning address
7. Degraded mode: boot with unreachable mint → verify notice events → recover
8. CLI: wallet, upstream, network commands on real OpenWrt
9. Fix all issues found

### Dependencies

- R1 ✅ (data usage monitoring)
- R2 ✅ (degraded mode)

---

## R7: Packaging Polish

**Severity**: MEDIUM
**Status**: 🔲 Open

### Gaps

1. **Package dependencies** — Go has `DEPENDS:=+nodogsplash +luci +jq` in packaging/Makefile. Rust packaging scripts don't declare these.
2. **Vendor IE emission** — Go emits TollGate vendor IE in local AP beacons when `vendor_ie_discovery: true` (main.go:235-253). Rust has `VendorElementProcessor` in `upstream_manager.rs` but it may not be wired into server startup.
3. **Install state** — Go tracks `install.json` (IP randomization, first-boot state). Rust has `install_config.rs` but it's not clear if it's used.

### Files

- MODIFY: `packaging/build-ipk.sh` — add dependency declarations
- MODIFY: `packaging/build-apk.sh` — add dependency declarations
- MODIFY: `crates/tollgate-net/src/main.rs` — wire vendor IE emission when config has `vendor_ie_discovery: true`

---

## R8: Integration Test Parity

**Severity**: MEDIUM
**Status**: 🔲 Open

### Problem

Go has a pytest suite (`tests/`) for real hardware testing:
- `test_ecash_payment.py` — E2E buy-internet flow
- `test_ecash_functionality.py` — Wallet-level Cashu operations
- `test_data_measurement.py` — Byte accounting across data-metered session

Rust has 68 API parity tests and 7 E2E lifecycle tests, all against mock servers.

### Plan

1. Run Go's pytest suite against Rust binary on real hardware
2. Port Go-specific test assumptions to Rust
3. Add regression tests for all issues found in R6

---

## Completed Items

| Milestone | Completed | What was done |
|-----------|-----------|---------------|
| R1: Data Usage Monitoring | Pre-existing | `v1/server/data_monitor.rs` — 2s ticker, byte session auto-close, 3 tests |
| R2: Degraded Mode + Mint Health | Pre-existing | `degraded_wallet.rs` + `mint_health_tracker.rs` + wired in `main.rs:487-521` |
| R4: Session Atomicity | 2025-06-08 | Snapshot/restore in `handlers.rs`, `rollback_session` helper, 2 integration tests |
| R5: Wallet Parity | 2025-06-08 | Overpayment via `OnlineTolerance`, signal handling, `try_mints()`, mint safety guard |

---

## Changelog

| Date | Change |
|------|--------|
| 2025-06-08 | Initial creation from comprehensive Go→Rust audit |
| 2025-06-08 | Updated: R1 and R2 were already implemented — marked ✅, removed implementation plans, added actual implementation details |
| 2025-06-08 | R4 complete: snapshot/restore on valve failure, session extension verified |
| 2025-06-08 | R5 complete: overpayment, signal handling, multi-mint init, mint safety guard |

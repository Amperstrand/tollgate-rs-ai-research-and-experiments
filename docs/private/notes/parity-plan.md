# Go Feature-Parity Plan (`tollgate-module-basic-go` → `tollgate-rs`)

Status: living document. Owner: parity workstream. Spilman/v2 CBOR work is **frozen**
until parity ships on hardware-equivalent tests (per decision 2026-05-31).

## Definition of done

> `tollgate-net`, deployed as the `tollgate-wrt` package, passes the
> `physical-router-test-automation` cloud-lab plans (`gcloud-api` →
> `gcloud-captive-portal` → `all`), selling both **bytes** and **milliseconds**,
> gating via `ndsctl` (and an nftables-native backend), in both single-hop
> seller and reseller modes, packaged as `ipk` **and** `apk`.

The test harness already supports a Rust backend (`--backend rust`,
`TOLLGATE_BACKEND=rust`, CI workflow "Build and Package", branch `experimental`).
Parity = the Rust binary passes the same pytest suite that gates the Go module.

## Scope decisions (interview 2026-05-31)

- Parity target: **drop-in binary replacement** on real OpenWrt hardware.
- Metrics: **both** bytes and milliseconds must be production-correct.
- Reseller mode: **in scope** (Wi-Fi gateway mgmt + netlink detection + WIFI-01 beacons).
- Valve: **both** NoDogSplash/`ndsctl` (match Go) **and** nftables-native behind a flag.
- Packaging: **both** `ipk` (≤24.10) and `apk` (25.x).
- Spilman/v2 CBOR: **frozen**.
- Hardware: none on hand; validate via gcloud cloud-lab (`tollgate-test-lab` GCP project).

## Go module → Rust mapping

| Go module | Rust equivalent | Parity status |
|---|---|---|
| `merchant` | `v1/server/handlers.rs`, `payout.rs`, `merchant.rs` | good (time); needs mint-health/degraded |
| `merchant/mint_health_tracker` + `merchant_degraded` | `mint_reachable` stub=true | MISSING |
| `valve` (gate) | `NdsValve` (feature `nds`); `StubValve` default | partial |
| `valve` (data tracker) | `/usage` bytes path returns 0 | MISSING |
| `upstream_session_manager` (was `chandler`) | `v1` client, `session_manager.rs`, `usage_tracker.rs` | time done; bytes missing |
| `upstream_detector` (was `crowsnest`) | `crowsnest.rs`, `upstream_detector.rs` (static poll) | partial; no netlink |
| `wireless_gateway_manager` | `v1/cli/commands.rs` stubs | MISSING |
| `config_manager` | `config.rs` flat JSON | partial; no schema/migrations/identities/backups |
| `tollwallet` | `cdk_wallet.rs` | good |
| `lightning` (LNURL-p) | `melt_to_lightning_address` | partial |
| `cli` + `cmd/tollgate-cli` | `v1/cli` CliServer (not started) | partial |
| `tollgate_protocol` | `core/protocol.rs`, `v1/nostr_events.rs` | good |
| packaging | `ipk` only; init-script subcommand bug | partial |

## Phases (each gated by a cloud-lab plan)

### Phase 0 — Close the test loop  ← IN PROGRESS
- [ ] Fix init script: `tollgate-wrt server` → `v1-server`; add `--wallet cdk`.
- [ ] Add **x86_64** to the build matrix (cloud-lab OpenWrt QEMU is x86_64; today CI
      only builds arm64/armv7/mips so the lab can consume nothing).
- [ ] Wire `DhcpLeasesResolver` + `MintQuoteWallet` (CdkWallet) into `V1Server::run`.
- [ ] Use the config's first accepted-mint URL for the CDK wallet connection.
- [ ] Green `plans/gcloud-api-quick.yaml` published.

### Phase 1 — config_manager parity
- [ ] `config.json` schema `v0.0.7` + `identities.json` `v0.0.1` (owned/public identities).
- [ ] Load, validate, **migrate**, **backup**. FieldSchema introspection for LuCI.
- [ ] Wire `SqliteSessionStore` to a config-driven data dir (persistence across restart).

### Phase 2 — Real valve + byte metering (both backends)
- [ ] `NdsValve` default when `ndsctl` present; port `customer_data_tracker.go`
      (baseline + `ndsctl json` byte counters).
- [ ] nftables-native backend behind config/feature flag.
- [ ] Real bytes usage tracker → `/usage` + `/balance` return true byte counts.

### Phase 3 — Merchant robustness
- [ ] `mint_health_tracker` (5-min probe, 3-consecutive, onFirstReachable) + degraded mode.
- [ ] Multi-mint `accepted_mints`; payout via `profit_share` + `identities.json` + LNURL-p.

### Phase 4 — CLI + captive portal + LuCI
- [ ] Start `CliServer` (`status/start/stop/restart/logs/version`); SSL apply/remove.
- [ ] Serve captive-portal site; LuCI config UI parity via schema endpoint.

### Phase 5 — Reseller mode
- [ ] `upstream_detector`: netlink event-driven WAN monitoring.
- [ ] `wireless_gateway_manager`: scan/connect/reconnect with `upstream_wifi` scoring;
      WIFI-01 beacon/vendor-element discovery.
- [ ] Wire crowsnest → session_manager; client-side LN-invoice payment; recovery wired.

### Phase 6 — Packaging both formats + full CI
- [ ] Produce `ipk` (≤24.10) and `apk` (25.x); align with SDK build recipe.
- [ ] CI artifacts consumable by `download-rust-ci-artifact.sh`.
- [ ] Full `plans/all.yaml` green in cloud-lab.

## Module-rename note
Go renamed `chandler → upstream_session_manager` and `crowsnest → upstream_detector`.
Keep the Rust names for now but document the mapping; consider renaming in Phase 5.
